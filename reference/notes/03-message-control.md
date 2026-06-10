# 03 — Message state: enable/disable resolution and inline pragma handling

Pinned sources:
- pylint 4.0.5: `/Users/adamraudonis/Desktop/Projects/prylint/reference/pylint/pylint`
- astroid 4.0.4: `/Users/adamraudonis/Desktop/Projects/prylint/reference/astroid/astroid`
- Runtime: CPython 3.12.12 (verified `.venv-pylint/bin/python --version` = 3.12.12, `pylint.__version__` = 4.0.5)

Target invocation: `pylint . -E --disable=C0301,...,E0110,...,E0611,...,E1101,...,E0401` (see
`harness/flags.txt`). All file:line references below are to the pinned sources.

This document is the exact spec for:
1. The global/per-module/per-line message-state machine (`_MessageStateHandler`, `FileState`).
2. `--errors-only` (`-E`) semantics.
3. The inline pragma grammar (`pylint/utils/pragma_parser.py`) and `process_tokens`.
4. `is_message_enabled` resolution order and confidence filtering.
5. Which in-scope messages are py-version gated.
6. skip-file / disable-all semantics, including interaction with E0001.

---

## 1. Core data structures and constants

### 1.1 Constants (`pylint/constants.py`)

```python
MSG_STATE_CONFIDENCE = 2          # constants.py:25
MSG_STATE_SCOPE_CONFIG = 0        # constants.py:27
MSG_STATE_SCOPE_MODULE = 1        # constants.py:28

MSG_TYPES: dict[str, MessageTypesFullName] = {   # constants.py:33-40
    "I": "info",
    "C": "convention",
    "R": "refactor",
    "W": "warning",
    "E": "error",
    "F": "fatal",
}
MSG_TYPES_LONG: dict[str, str] = {v: k for k, v in MSG_TYPES.items()}  # constants.py:41
# => {"info": "I", "convention": "C", "refactor": "R", "warning": "W", "error": "E", "fatal": "F"}
# NOTE: keys are LOWERCASE. See §3.2 for the resulting long-name lookup bug.

class WarningScope:               # constants.py:55-57
    LINE = "line-based-msg"
    NODE = "node-based-msg"
```

`MSG_TYPES` is a regular dict; iteration order is the literal order `I, C, R, W, E, F`
(matters for `disable("all")` expansion, §3.2).

### 1.2 `_MessageStateHandler` state (`pylint/lint/message_state_handler.py:38-64`)

```python
self._msgs_state: dict[str, bool] = {}        # GLOBAL ("package"-scope) state, keyed by msgid
self._options_methods = {
    "enable": self.enable,
    "disable": self.disable,
    "disable-next": self.disable_next,
}
self._bw_options_methods = {                  # deprecated pragma keywords
    "disable-msg": self._options_methods["disable"],
    "enable-msg": self._options_methods["enable"],
}
self._pragma_lineno: dict[str, int] = {}      # msgid/symbol-string -> line of last control pragma
self._stashed_messages: defaultdict[tuple[str, str], list[tuple[str | None, str]]] = defaultdict(list)
```

`default_enabled_messages` (message_state_handler.py:40-44) is computed from the **main
checker's** `msgs` only (`self.linter.msgs` is `PyLinter.msgs = MSGS`, pylinter.py:281):

```python
self.default_enabled_messages: dict[str, MessageDefinitionTuple] = {
    k: v
    for k, v in self.linter.msgs.items()
    if len(v) == 3 or v[3].get("default_enabled", True)
}
```

Given `MSGS` (pylinter.py:103-254), this set is exactly:
`F0001, F0002, F0010, F0011, E0001, E0011, W0012, R0022, E0013, E0014, E0015`
(everything in `MSGS` except `I0001, I0010, I0011, I0013, I0020, I0021, I0022`, which all carry
`"default_enabled": False`). These are "pylint's own warnings" exempt from `disable=all` (§3.2).

### 1.3 `FileState` state (`pylint/utils/file_state.py:30-54`)

```python
self._module_msgs_state: MessageStateDict = {}      # dict[msgid, dict[lineno, bool]] — EXPANDED state
self._raw_module_msgs_state: MessageStateDict = {}  # dict[msgid, dict[lineno, bool]] — RAW pragma lines
self._ignored_msgs: defaultdict[tuple[str, int], set[int]] = collections.defaultdict(set)
self._suppression_mapping: dict[tuple[str, int], int] = {}   # (msgid, line) -> pragma line that disabled it
self._module = node                                  # nodes.Module or None
if node:
    self._effective_max_line_number = node.tolineno  # file_state.py:47
else:
    self._effective_max_line_number = None
```

A **base** `FileState("", msgs_store, is_base_filestate=True)` is installed at
`PyLinter.__init__` (pylinter.py:361). It has `_module = None`, `_effective_max_line_number = None`,
and empty state dicts. It remains `linter.file_state` until the first `_lint_file` runs — this
matters for messages emitted during AST building (§10).

A fresh `FileState(file.modpath, self.msgs_store, module)` is created per linted module in
`_lint_file` (pylinter.py:815) and `_check_file` (pylinter.py:859).

### 1.4 `MessageDefinition` (`pylint/message/message_definition.py`)

Relevant fields: `msgid`, `symbol`, `msg` (the `%`-template), `scope`
(`WarningScope.LINE` or `WarningScope.NODE`), `minversion`, `maxversion`, `old_names`,
`default_enabled`.

Default scope (base_checker.py:182-207): `WarningScope.LINE` if the checker is a
`BaseTokenChecker`/`BaseRawFileChecker`, else `WarningScope.NODE`; an explicit `"scope"` in the
4th tuple element overrides (`options.setdefault("scope", default_scope)`, base_checker.py:206).
All of the main-checker `MSGS` set `scope: WarningScope.LINE` explicitly.

```python
def may_be_emitted(self, py_version) -> bool:        # message_definition.py:75-81
    if self.minversion is not None and self.minversion > py_version:
        return False
    if self.maxversion is not None and self.maxversion <= py_version:
        return False
    return True
```

Tuple comparison; `py_version` is `linter.config.py_version`, default `sys.version_info[:2]`
(base_options.py:355-366) = `(3, 12)` on the pinned runtime. `--py-version` strings are parsed by
`_py_version_transformer` (argument.py:95-103): `tuple(int(v) for v in value.replace(",", ".").split("."))`.

`check_message_definition(line, node)` (message_definition.py:109-132) — raises
`InvalidMessageError` (a crash, not a lint message) if:
- msgid category not in `_SCOPE_EXEMPT = "FR"` (constants.py:31), AND
- scope==LINE and (`line is None` or `node is not None`), or
- scope==NODE and `node is None` (a NODE-scoped message MAY carry an override line).
F-category messages are exempt from the line/node check entirely.

### 1.5 py-version-gated messages — full list

`grep minversion|maxversion` over `pylint/checkers` + `pylint/extensions` yields exactly six:

| msgid | symbol | gate | emittable at py_version=(3,12)? | in -E scope? |
|---|---|---|---|---|
| E0106 | return-arg-in-generator | `maxversion (3, 3)` (basic_error_checker.py:202) | **NO** (`(3,3) <= (3,12)`) | yes (E) — but gated off |
| E0118 | used-prior-global-declaration | `minversion (3, 6)` (basic_error_checker.py:261) | yes | yes |
| E1700 | yield-inside-async-function | `minversion (3, 5)` (async_checker.py:33) | yes | yes |
| E1701 | not-async-context-manager | `minversion (3, 5)` (async_checker.py:40) | yes | yes |
| W1502 | boolean-datetime | `maxversion (3, 5)` (stdlib.py:508) | no | no (W) |
| W1514 | unspecified-encoding | `maxversion (3, 15)` (stdlib.py:583) | yes | no (W) |

Gating is applied in `PyLinter.initialize()` (pylinter.py:624-634), called at the start of
`check()` (pylinter.py:677), AFTER all config/CLI parsing:

```python
def initialize(self) -> None:
    self._ignore_paths = self.config.ignore_paths
    # initialize msgs_state now that all messages have been registered into the store
    for msg in self.msgs_store.messages:
        if not msg.may_be_emitted(self.config.py_version):
            self._msgs_state[msg.msgid] = False
```

Note: this writes `_msgs_state` directly (no `config.enable/disable` re-sync) and
**unconditionally overrides** any prior `--enable` of a gated message. Under the pinned defaults,
the only effect is `_msgs_state["E0106"] = False` → **E0106 can never fire**. (The store
iteration order is registration order; irrelevant since assignment is idempotent.)

Additionally `MessageDefinitionStore.__init__` receives `self.config.py_version`
(pylinter.py:356) but only uses it for `find_emittable_messages` / `list_messages`, not for
emission gating.

---

## 2. is_message_enabled — full resolution order

`pylint/lint/message_state_handler.py:315-345`:

```python
def is_message_enabled(self, msg_descr, line=None, confidence=None) -> bool:
    if confidence and confidence.name not in self.linter.config.confidence:
        return False
    try:
        msgids = self.linter.msgs_store.message_id_store.get_active_msgids(msg_descr)
    except exceptions.UnknownMessageError:
        # The linter checks for messages that are not registered
        # due to version mismatch, just treat them as message IDs for now.
        msgids = [msg_descr]
    return any(self._is_one_message_enabled(msgid, line) for msgid in msgids)
```

Resolution steps:

1. **Confidence filter.** `confidence` is `None` in many call sites (e.g.
   `prepare_checkers`); `None` skips the filter (`confidence and ...`). Default
   `config.confidence` = `CONFIDENCE_LEVEL_NAMES` =
   `["HIGH", "CONTROL_FLOW", "INFERENCE", "INFERENCE_FAILURE", "UNDEFINED"]`
   (interfaces.py:36-37, base_options.py:173-183). **All five levels are shown by default**, so
   under the pinned invocation the confidence filter never rejects anything.
   `_confidence_transformer` (argument.py:39-49): empty string → all levels; invalid name →
   `argparse.ArgumentTypeError` (startup failure, not a lint message).
2. **id/symbol/old-name normalization** via `get_active_msgids` (§2.1). Unknown descriptor →
   treat verbatim as a single msgid (conservatism: lookup of unknown id then falls back to
   `_msgs_state.get(msgid, True)` → enabled).
3. `any()` over the resolved msgids of `_is_one_message_enabled` (§2.2). For old msgids that map
   to several current messages (e.g. `E0012` → `[W0012, R0022]`), enabled-if-any-enabled.

### 2.1 `get_active_msgids` (`pylint/message/message_id_store.py:121-160`)

```python
if msgid_or_symbol[1:].isdigit():
    # Only msgid can have a digit as second letter
    msgid = msgid_or_symbol.upper()
    symbol = self.__msgid_to_symbol.get(msgid)
    if not symbol:
        deletion_reason = is_deleted_msgid(msgid)
        if deletion_reason is None:
            moved_reason = is_moved_msgid(msgid)
else:
    symbol = msgid_or_symbol
    msgid = self.__symbol_to_msgid.get(msgid_or_symbol)
    if not msgid:
        deletion_reason = is_deleted_symbol(symbol)
        if deletion_reason is None:
            moved_reason = is_moved_symbol(symbol)
if not (msgid and symbol):
    if deletion_reason is not None:
        raise DeletedMessageError(msgid_or_symbol, deletion_reason)
    if moved_reason is not None:
        raise MessageBecameExtensionError(msgid_or_symbol, moved_reason)
    error_msg = f"No such message id or symbol '{msgid_or_symbol}'."
    raise UnknownMessageError(error_msg)
ids = self.__old_names.get(msgid, [msgid])
```

- Numeric ids are **upper-cased** (`e0602` works); symbols are case-sensitive.
- `__old_names` maps OLD msgid → list of NEW msgids (one entry appended per registration,
  message_id_store.py:76-88). Old msgids/symbols are registered into the same
  `__msgid_to_symbol`/`__symbol_to_msgid` maps, so an old symbol resolves to the old msgid and
  then through `__old_names`.
- Deleted/moved tables: `pylint/message/_deleted_message_ids.py:19-131`
  (`DELETED_MESSAGES_IDS`: py3k checker W16xx/E16xx batch, W0312, C0326(+C0322/C0323/C0324),
  C0330, R0921, R0922, W0142, W0232, W0111; `MOVED_TO_EXTENSIONS`: R0201 no-self-use).
  Exception strings (exceptions.py:16-35):
  - `DeletedMessageError`: `'{msgid_or_symbol}' was removed from pylint, see {explanation}.`
  - `MessageBecameExtensionError`: `'{msgid_or_symbol}' was moved to an optional extension, see {explanation}.`
- Results are memoized in `__active_msgids` (only on success).

### 2.2 `_is_one_message_enabled` (`message_state_handler.py:279-313`) — quoted verbatim

```python
def _is_one_message_enabled(self, msgid: str, line: int | None) -> bool:
    if line is None:
        return self._msgs_state.get(msgid, True)
    try:
        return self.linter.file_state._module_msgs_state[msgid][line]
    except KeyError:
        # Check if the message's line is after the maximum line existing in ast tree.
        # This line won't appear in the ast tree and won't be referred in
        # self.file_state._module_msgs_state
        # This happens for example with a commented line at the end of a module.
        max_line_number = self.linter.file_state.get_effective_max_line_number()
        if max_line_number and line > max_line_number:
            fallback = True
            lines = self.linter.file_state._raw_module_msgs_state.get(msgid, {})

            # Doesn't consider scopes, as a 'disable' can be in a
            # different scope than that of the current line.
            closest_lines = reversed(
                [
                    (message_line, enable)
                    for message_line, enable in lines.items()
                    if message_line <= line
                ]
            )
            _, fallback_iter = next(closest_lines, (None, None))
            if fallback_iter is not None:
                fallback = fallback_iter
            return self._msgs_state.get(msgid, fallback)
        return self._msgs_state.get(msgid, True)
```

Resolution order per msgid:
1. `line is None` → global `_msgs_state` (default True).
2. Exact `(msgid, line)` hit in `file_state._module_msgs_state` → that boolean wins.
   **Per-line/block pragma state overrides global CLI disables and enables** when there is an
   exact hit (e.g. `# pylint: enable=E1102` overrides nothing here under -E because CLI disables
   are global, but a pragma `enable` DOES re-enable a globally-disabled message on those lines —
   conversely an exact `True` hit beats `_msgs_state[msgid] == False`).
3. KeyError → "past-end-of-AST" fallback: only when `max_line_number` is truthy (module's
   `tolineno`, 0/None falsy!) **and** `line > max_line_number`. Take entries of
   `_raw_module_msgs_state[msgid]` (RAW pragma lines, **dict insertion order** = the order
   `set_msg_status` was called = token order = ascending file order), filter
   `message_line <= line`, take the **last inserted** one. If found, that pragma's state is the
   *default* for `_msgs_state.get(msgid, fallback)` — note global `_msgs_state` still wins if the
   msgid has a global entry. Order dependency: "closest" is really "last-inserted ≤ line", which
   equals numerically-largest only because pragmas are processed in ascending line order.
4. Otherwise → `_msgs_state.get(msgid, True)`.

### 2.3 `_get_message_state_scope` (`message_state_handler.py:261-277`)

Used only to decide whether a *suppressed* message should be recorded for
useless-suppression accounting:

```python
if confidence is None:
    confidence = interfaces.UNDEFINED
if confidence.name not in self.linter.config.confidence:
    return MSG_STATE_CONFIDENCE          # 2
try:
    if line in self.linter.file_state._module_msgs_state[msgid]:
        return MSG_STATE_SCOPE_MODULE    # 1
except (KeyError, TypeError):
    return MSG_STATE_SCOPE_CONFIG        # 0
return None
```

---

## 3. disable() / enable() semantics

### 3.1 Entry points (`message_state_handler.py:189-232`)

```python
def disable(self, msgid, scope="package", line=None, ignore_unknown=False):
    self._set_msg_status(msgid, enable=False, scope=scope, line=line, ignore_unknown=ignore_unknown)
    self._register_by_id_managed_msg(msgid, line)

def disable_next(self, msgid, _="package", line=None, ignore_unknown=False):
    if not line:
        raise exceptions.NoLineSuppliedError
    self._set_msg_status(msgid, enable=False, scope="line", line=line + 1, ignore_unknown=ignore_unknown)
    self._register_by_id_managed_msg(msgid, line + 1)

def enable(self, msgid, scope="package", line=None, ignore_unknown=False):
    self._set_msg_status(msgid, enable=True, scope=scope, line=line, ignore_unknown=ignore_unknown)
    self._register_by_id_managed_msg(msgid, line, is_disabled=False)
```

`disable_next` **ignores the scope argument** and always uses `scope="line"`, `line+1`
(single-line, no block expansion).

`_register_by_id_managed_msg` (message_state_handler.py:171-187): only when
`msgid_or_symbol[1:].isdigit()` (numeric id); resolves symbol via
`message_id_store.get_symbol`, **silently returns on UnknownMessageError**, else appends
`ManagedMessage(current_name, msgid, symbol, line, is_disabled)` to
`linter._by_id_managed_msgs`. Sole consumer: `ByIdManagedMessagesChecker` in
`pylint/checkers/misc.py` emitting I0023 `use-symbolic-message-instead` — I-category, disabled
under -E ⇒ pure bookkeeping side effect; ignore for the port (but the list grows).

### 3.2 `_get_messages_to_set` — expansion rules (`message_state_handler.py:82-140`)

Order of checks (first match wins):

1. **`msgid == "all"`** (exact, case-sensitive): recursively expand every category letter in
   `MSG_TYPES` order `I, C, R, W, E, F`. For `enable=False` only, filter OUT pylint's own
   default-enabled main-checker messages:

   ```python
   if not enable:
       # "all" should not disable pylint's own warnings
       message_definitions = list(filter(
           lambda m: m.msgid not in self.default_enabled_messages, message_definitions))
   ```
   ⇒ `disable=all` never disables F0001, F0002, F0010, F0011, E0001, E0011, W0012, R0022,
   E0013, E0014, E0015.

2. **Category letter**: `category_id = msgid.upper()`; if in `MSG_TYPES` → expand
   `msgs_store._msgs_by_category[category_id]` (list of msgids, insertion order =
   checker-registration order, each checker's msgs sorted by msgid — see §3.4), recursing per
   msgid. **Long-name bug**: if not a letter, `MSG_TYPES_LONG.get(category_id)` is looked up with
   the *upper-cased* string while `MSG_TYPES_LONG` keys are lowercase, so it always returns
   `None`. Empirically verified on the pinned build:
   `_get_messages_to_set("error", enable=False)` raises
   `UnknownMessageError("No such message id or symbol 'error'.")`, while `"E"` expands to 130
   messages. ⇒ `--disable=error`/`# pylint: disable=convention` do NOT work as category
   disables in 4.0.5; the former becomes an `unknown-option-value` stash, the latter a W0012.

3. **Checker name**: `msgid.lower() in self.linter._checkers` → expand every `msgid` in
   `checker.msgs` for every registered checker of that name (dict iteration order of the `msgs`
   class dict). E.g. `disable("miscellaneous")` used by error mode.

4. **Report id**: `msgid.lower().startswith("rp")` → enable_report/disable_report, returns `[]`.

5. **Plain id/symbol** → `msgs_store.get_message_definitions(msgid)`
   (message_definition_store.py:61-72; an `@cache`-decorated method) → list of
   `MessageDefinition` via `get_active_msgids`. `UnknownMessageError` propagates unless
   `ignore_unknown=True` (never set in the paths in scope).

### 3.3 `_set_msg_status` and `_set_one_msg_status` (`message_state_handler.py:66-169`)

```python
def _set_msg_status(self, msgid, enable, scope="package", line=None, ignore_unknown=False):
    assert scope in {"package", "module", "line"}
    message_definitions = self._get_messages_to_set(msgid, enable, ignore_unknown)
    for message_definition in message_definitions:
        self._set_one_msg_status(scope, message_definition, line, enable)
    # sync configuration object
    self.linter.config.enable = []
    self.linter.config.disable = []
    for msgid_or_symbol, is_enabled in self._msgs_state.items():
        symbols = [m.symbol
                   for m in self.linter.msgs_store.get_message_definitions(msgid_or_symbol)]
        if is_enabled:
            self.linter.config.enable += symbols
        else:
            self.linter.config.disable += symbols
```

```python
def _set_one_msg_status(self, scope, msg, line, enable):
    if scope in {"module", "line"}:
        assert isinstance(line, int)
        self.linter.file_state.set_msg_status(msg, line, enable, scope)
        if not enable and msg.symbol != "locally-disabled":
            self.linter.add_message("locally-disabled", line=line, args=(msg.symbol, msg.msgid))
    else:
        msgs = self._msgs_state
        msgs[msg.msgid] = enable
```

- `"package"` scope (CLI/config) → global `_msgs_state[msgid] = enable`.
- `"module"`/`"line"` scope (pragmas) → `FileState.set_msg_status` (§6); a *disable* also emits
  I0011 `locally-disabled` ("Locally disabling %s (%s)" % (symbol, msgid)) at the pragma line —
  I-category, default-disabled, never visible under -E.
- The `config.enable`/`config.disable` lists are rebuilt from `_msgs_state` after EVERY call —
  iteration order of `_msgs_state` (insertion order of first state change per msgid). These lists
  are only consulted by run.py's "No files to lint" heuristic (run.py:202-211) and reporting; the
  authoritative state is `_msgs_state`.

### 3.4 `_msgs_by_category` population order

`MessageDefinitionStore.register_message` (message_definition_store.py:49-55) appends
`message.msgid` to `_msgs_by_category[msgid[0]]`. Registration happens per checker via
`register_messages_from_checker`, iterating `checker.messages` =
`sorted(self.msgs.items())` (base_checker.py:209-214 — **sorted by msgid within each
checker**). The PyLinter registers itself first (pylinter.py:368), then
`load_default_plugins` → `checkers.initialize` registers the rest. Resulting category key
order (verified empirically): `['E', 'F', 'I', 'R', 'W', 'C']`.

---

## 4. `-E` / `--errors-only`

### 4.1 Option definition

base_options.py:550-561: `"errors-only"`, `"action": _ErrorsOnlyModeAction`, `"short": "E"`,
`hide_from_config_file: True`. The argparse action (callback_actions.py:266-284) only does:

```python
self.run.linter._error_mode = True
```

(`_error_mode` initialized False at pylinter.py:340.)

### 4.2 `_parse_error_mode` (`pylinter.py:558-570`)

Called once from `_config_initialization` (config_initialization.py:145), AFTER config-file
parsing, command-line parsing, `_emit_stashed_messages`, `load_plugin_configuration` and
`enable_fail_on_messages` — i.e. error-mode disables are applied LAST regardless of where `-E`
appeared on the command line:

```python
def _parse_error_mode(self) -> None:
    if not self._error_mode:
        return
    self.disable_noerror_messages()
    self.disable("miscellaneous")
    self.set_option("reports", False)
    self.set_option("persistent", False)
    self.set_option("score", False)
```

### 4.3 `disable_noerror_messages` (`message_state_handler.py:234-239`)

```python
def disable_noerror_messages(self) -> None:
    """Disable message categories other than `error` and `fatal`."""
    for msgcat in self.linter.msgs_store._msgs_by_category:
        if msgcat in {"E", "F"}:
            continue
        self.disable(msgcat)
```

So -E is implemented as **category disables of every registered category except E and F**, in
`_msgs_by_category` key order (`E, F, I, R, W, C` → disables `I`, then `R`, then `W`, then `C`).
Each `disable(<letter>)` is a package-scope `_msgs_state[msgid] = False` for every message of
that category. **Fatal (F) and Error (E) remain enabled** — F is never disabled by -E.
`disable("miscellaneous")` additionally disables that checker's msgs (W0511 fixme — W anyway).

Consequences for the port:
- I/C/R/W messages are *globally* disabled but can still be re-enabled per line by an inline
  `# pylint: enable=...` pragma (exact-line hit in `_module_msgs_state` wins over `_msgs_state`,
  §2.2 step 2). E.g. `# pylint: enable=unused-import` in a file makes W0611 fire on those lines
  even under -E (subject to the explicit `--disable=W0611` in flags.txt: CLI `--disable` also
  only writes `_msgs_state`, so an inline enable STILL wins on exact-line hits).
- `--disable=...` from the command line is processed during argument parsing (each
  `_DisableAction.__call__` → `linter.disable(msgid)` per CSV item, callback_actions.py:352-395),
  i.e. BEFORE `_parse_error_mode`; since both write `_msgs_state`, order is irrelevant for
  disjoint sets and idempotent for overlapping disables.
- Unknown values in `--disable`/`--enable` do NOT abort: `_XableAction._call`
  (callback_actions.py:352-372) catches `DeletedMessageError`/`MessageBecameExtensionError` →
  stash `("useless-option-value", (option_string, str(e)))`, `UnknownMessageError` → stash
  `("unknown-option-value", (option_string, msgid))`, keyed by `linter.current_name` (which is
  `"Command line"` during CLI parsing, config_initialization.py:79). Stashes are flushed by
  `_emit_stashed_messages` (pylinter.py:1346-1357) with `line=0, confidence=HIGH` after
  `set_current_module(modname)`. Both are R/W category → invisible under -E, but they still run
  through `add_message` (stats counters for R/W are incremented only if enabled — they aren't).

`--disable=all`/`--enable=all` ordering: `_order_all_first` (config_initialization.py:164-206)
moves any `--enable=all`/`--disable=all` argument (and its value) to the FRONT of the arg list
(both config-file args, joined=False, and CLI args, joined=True), and raises
`ArgumentPreprocessingError` if both are present. Not used by the pinned invocation.

### 4.4 run.py guard

run.py:202-211: after config init, pylint exits 32 with `"No files to lint: exiting."` if no
positional args, or if `len(config.enable) == 0 and set(config.disable) ==` (all symbols minus
default-enabled main-checker symbols) — i.e. a pure `--disable=all` run. Not triggered by -E
(config.disable contains category expansions but config.enable is rebuilt from `_msgs_state`
which contains no True entries... note: `_msgs_state` only gets True entries from explicit
enables; under the pinned flags `len(config.enable)==0` is true but `set(config.disable)` ≠ the
disable-all set since E/F msgs are absent, so the guard passes).

---

## 5. Pragma grammar (`pylint/utils/pragma_parser.py`) — COMPLETE

### 5.1 Comment-recognition regex (verbatim, pragma_parser.py:14-27)

```python
# Allow stopping after the first semicolon/hash encountered,
# so that an option can be continued with the reasons
# why it is active or disabled.
OPTION_RGX = r"""
    (?:^\s*\#.*|\s*|               # Comment line, or whitespaces,
       \s*\#.*(?=\#.*?\bpylint:))  # or a beginning of an inline comment
                                   # followed by "pylint:" pragma
    (\#                            # Beginning of comment
    .*?                            # Anything (as little as possible)
    \bpylint:                      # pylint word and column
    \s*                            # Any number of whitespaces
    ([^;#\n]+))                    # Anything except semicolon or hash or
                                   # newline (it is the second matched group)
                                   # and end of the first matched group
    [;#]{0,1}                      # From 0 to 1 repetition of semicolon or hash
"""
OPTION_PO = re.compile(OPTION_RGX, re.VERBOSE)
```

Applied with `OPTION_PO.search(content)` where `content` is a COMMENT token's text (starts with
`#`, no trailing newline). `match.group(2)` = the pragma payload after `pylint:`, terminated by
the first `;`, `#`, or newline (trailing spaces included, e.g. `'disable=unused-import '`).
`\bpylint:` requires word boundary, so `# nopylint: disable=X` does NOT match but
`# comment # pylint: disable=X` does. Only the FIRST pragma per comment is parsed
(a second `; pylint: enable=...` in the same comment is cut off by `[^;#\n]+`).

### 5.2 Keywords and tokenizer (verbatim, pragma_parser.py:35-58)

```python
ATOMIC_KEYWORDS = frozenset(("disable-all", "skip-file"))
MESSAGE_KEYWORDS = frozenset(("disable-next", "disable-msg", "enable-msg", "disable", "enable"))
# sorted is necessary because sets are unordered collections and ALL_KEYWORDS
# string should not vary between executions
# reverse is necessary in order to have the longest keywords first, so that, for example,
# 'disable' string should not be matched instead of 'disable-all'
ALL_KEYWORDS = "|".join(sorted(ATOMIC_KEYWORDS | MESSAGE_KEYWORDS, key=len, reverse=True))

TOKEN_SPECIFICATION = [
    ("KEYWORD", rf"\b({ALL_KEYWORDS:s})\b"),
    ("MESSAGE_STRING", r"[0-9A-Za-z\-\_]{2,}"),  # Identifiers
    ("ASSIGN", r"="),                            # Assignment operator
    ("MESSAGE_NUMBER", r"[CREIWF]{1}\d*"),
]
TOK_REGEX = "|".join(f"(?P<{token_name:s}>{token_rgx:s})"
                     for token_name, token_rgx in TOKEN_SPECIFICATION)
```

Empirically on the pinned build:
`ALL_KEYWORDS = disable-next|disable-all|disable-msg|enable-msg|skip-file|disable|enable`
(ties of length 11 between `disable-all`/`disable-msg` depend on set order/hash seed but are
behaviorally equivalent in the alternation), and

```
TOK_REGEX = (?P<KEYWORD>\b(disable-next|disable-all|disable-msg|enable-msg|skip-file|disable|enable)\b)|(?P<MESSAGE_STRING>[0-9A-Za-z\-\_]{2,})|(?P<ASSIGN>=)|(?P<MESSAGE_NUMBER>[CREIWF]{1}\d*)
```

Tokenization facts (important for a port):
- `re.finditer(TOK_REGEX, payload)` — characters not matching ANY alternative (whitespace,
  commas, single non-`[CREIWF]` letters like `x`, punctuation) are **silently skipped**; commas
  are not tokens at all. The `else: raise RuntimeError("Token not recognized")` branch
  (pragma_parser.py:127-128) is dead code.
- A msgid like `E0602` matches MESSAGE_STRING (len ≥ 2), not MESSAGE_NUMBER.
  MESSAGE_NUMBER only ever matches a single category letter (`E`, `C`, `R`, `I`, `W`, `F`,
  optionally length-1 since `{2,}` strings are taken by MESSAGE_STRING first) — this is how
  `# pylint: disable=E` (single letter, category expansion) tokenizes.
- KEYWORD wins over MESSAGE_STRING at the same position due to alternation order;
  `\b...\b` prevents `disablefoo` matching KEYWORD (it becomes one MESSAGE_STRING).
  Note `disable-next-foo`: `\b` boundaries are satisfied around `disable-next` (hyphen is a
  non-word char), so it tokenizes as KEYWORD `disable-next` + MESSAGE_STRING `foo`... actually
  `foo` is preceded by `-`, MESSAGE_STRING `[0-9A-Za-z\-\_]{2,}` would match `-foo` — port must
  replicate finditer's leftmost-longest-per-alternative behavior exactly: at the position of
  `disable-next-foo`, MESSAGE_STRING could match the WHOLE string and it is listed after KEYWORD;
  Python's `re` alternation tries KEYWORD first and KEYWORD matches, so the rest `-foo` is
  scanned next, matching MESSAGE_STRING `-foo`. (Behavior verified for the analogous case below.)

### 5.3 `parse_pragma` state machine (verbatim, pragma_parser.py:89-135)

```python
def parse_pragma(pylint_pragma: str) -> Generator[PragmaRepresenter]:
    action: str | None = None
    messages: list[str] = []
    assignment_required = False
    previous_token = ""

    for mo in re.finditer(TOK_REGEX, pylint_pragma):
        kind = mo.lastgroup
        value = mo.group()

        if kind == "ASSIGN":
            if not assignment_required:
                if action:
                    # A keyword has been found previously but doesn't support assignment
                    raise UnRecognizedOptionError("The keyword doesn't support assignment", action)
                if previous_token:
                    # Something found previously but not a known keyword
                    raise UnRecognizedOptionError("The keyword is unknown", previous_token)
                # Nothing at all detected before this assignment
                raise InvalidPragmaError("Missing keyword before assignment", "")
            assignment_required = False
        elif assignment_required:
            raise InvalidPragmaError("The = sign is missing after the keyword", action or "")
        elif kind == "KEYWORD":
            if action:
                yield emit_pragma_representer(action, messages)
            action = value
            messages = []
            assignment_required = action in MESSAGE_KEYWORDS
        elif kind in {"MESSAGE_STRING", "MESSAGE_NUMBER"}:
            messages.append(value)
            assignment_required = False
        else:
            raise RuntimeError("Token not recognized")

        previous_token = value

    if action:
        yield emit_pragma_representer(action, messages)
    else:
        raise UnRecognizedOptionError("The keyword is unknown", previous_token)
```

with (pragma_parser.py:61-66):

```python
def emit_pragma_representer(action: str, messages: list[str]) -> PragmaRepresenter:
    if not messages and action in MESSAGE_KEYWORDS:
        raise InvalidPragmaError("The keyword is not followed by message identifier", action)
    return PragmaRepresenter(action, messages)
```

Multiple pragmas per comment are supported: `disable=C0103 enable=E0602` yields
`[("disable", ["C0103"]), ("enable", ["E0602"])]` (the second KEYWORD flushes the first).
The generator is lazy — errors raised mid-iteration occur after earlier representers were already
**processed** by `process_tokens` (the `for pragma_repr in parse_pragma(...)` loop is inside the
`try`; earlier disables stay applied).

### 5.4 Verified error matrix (payload → outcome)

| payload after `pylint:` | result |
|---|---|
| `disable=E0602` | `disable, ["E0602"]` |
| `disable = missing-docstring, C0103` | `disable, ["missing-docstring", "C0103"]` (whitespace/comma skipping) |
| `disable=unused-import ; reason` | group(2) stops at `;` → `disable, ["unused-import"]` |
| `skip-file` | `skip-file, []` |
| `disable-all` | `disable-all, []` |
| `disable=all` | `disable, ["all"]` |
| `disable=E` | `disable, ["E"]` (MESSAGE_NUMBER) |
| `disable` | InvalidPragmaError token=`disable` ("not followed by message identifier") |
| `disable=` | InvalidPragmaError token=`disable` (same; ASSIGN consumed, no messages) |
| `skip-file=foo` | UnRecognizedOptionError token=`skip-file` ("doesn't support assignment") |
| `foo=bar` | UnRecognizedOptionError token=`foo` ("keyword is unknown") |
| `foobar` | UnRecognizedOptionError token=`foobar` (end-of-input, no action) |
| `=foo` | InvalidPragmaError token=`""` ("Missing keyword before assignment") |
| `disable foo` | InvalidPragmaError token=`disable` ("The = sign is missing after the keyword") |
| `disable=x enable=y` | InvalidPragmaError token=`disable` (`x` matches no token → messages empty when `enable` flushes) |
| `disable=C0103 enable=E0602` | two representers |

---

## 6. `process_tokens` (`pylint/lint/message_state_handler.py:347-444`) — FULL

Called from `_check_astroid_module` (pylinter.py:1096) with
`tokens = utils.tokenize_module(node)` where (pylint/utils/utils.py:151-154):

```python
def tokenize_module(node: nodes.Module) -> list[tokenize.TokenInfo]:
    with node.stream() as stream:
        readline = stream.readline
        return list(tokenize.tokenize(readline))
```

— bytes-mode `tokenize.tokenize` over the module's source stream; the list starts with an
ENCODING token; COMMENT token `start` is `(row, col)` with 1-based row. `tokenize.TokenError`
during this call is caught in `_check_astroid_module` (pylinter.py:1080-1089) and converted to
E0001 `syntax-error` with `line=ex.args[1][0]`, `col_offset=ex.args[1][1]`,
`args=ex.args[0]`, `confidence=HIGH`, then `return None` (no pragma processing, no checkers).

Verbatim body:

```python
def process_tokens(self, tokens: list[tokenize.TokenInfo]) -> None:
    control_pragmas = {"disable", "disable-next", "enable"}
    prev_line = None
    saw_newline = True
    seen_newline = True
    for tok_type, content, start, _, _ in tokens:
        if prev_line and prev_line != start[0]:
            saw_newline = seen_newline
            seen_newline = False

        prev_line = start[0]
        if tok_type in (tokenize.NL, tokenize.NEWLINE):
            seen_newline = True

        if tok_type != tokenize.COMMENT:
            continue
        match = OPTION_PO.search(content)
        if match is None:
            continue
        try:  # pylint: disable = too-many-try-statements
            for pragma_repr in parse_pragma(match.group(2)):
                if pragma_repr.action in {"disable-all", "skip-file"}:
                    if pragma_repr.action == "disable-all":
                        self.linter.add_message(
                            "deprecated-pragma", line=start[0],
                            args=("disable-all", "skip-file"))
                    self.linter.add_message("file-ignored", line=start[0])
                    self._ignore_file: bool = True
                    return
                try:
                    meth = self._options_methods[pragma_repr.action]
                except KeyError:
                    meth = self._bw_options_methods[pragma_repr.action]
                    # found a "(dis|en)able-msg" pragma deprecated suppression
                    self.linter.add_message(
                        "deprecated-pragma", line=start[0],
                        args=(pragma_repr.action, pragma_repr.action.replace("-msg", "")))
                for msgid in pragma_repr.messages:
                    # Add the line where a control pragma was encountered.
                    if pragma_repr.action in control_pragmas:
                        self._pragma_lineno[msgid] = start[0]

                    if (pragma_repr.action, msgid) == ("disable", "all"):
                        self.linter.add_message(
                            "deprecated-pragma", line=start[0],
                            args=("disable=all", "skip-file"))
                        self.linter.add_message("file-ignored", line=start[0])
                        self._ignore_file = True
                        return
                        # If we did not see a newline between the previous line and now,
                        # we saw a backslash so treat the two lines as one.
                    l_start = start[0]
                    if not saw_newline:
                        l_start -= 1
                    try:
                        meth(msgid, "module", l_start)
                    except (exceptions.DeletedMessageError,
                            exceptions.MessageBecameExtensionError) as e:
                        self.linter.add_message(
                            "useless-option-value", args=(pragma_repr.action, e),
                            line=start[0], confidence=HIGH)
                    except exceptions.UnknownMessageError:
                        self.linter.add_message(
                            "unknown-option-value", args=(pragma_repr.action, msgid),
                            line=start[0], confidence=HIGH)

        except UnRecognizedOptionError as err:
            self.linter.add_message(
                "unrecognized-inline-option", args=err.token, line=start[0])
            continue
        except InvalidPragmaError as err:
            self.linter.add_message("bad-inline-option", args=err.token, line=start[0])
            continue
```

Point-by-point spec:

1. **Backslash-continuation tracking.** `saw_newline` is True iff a `NL`/`NEWLINE` token was seen
   between the previous physical row and the current one. A trailing pragma comment on the
   second line of a backslash-continued statement gets `l_start = start[0] - 1` (only ONE line
   back, even for multi-line continuations). Initial state `saw_newline = True`.
2. Only `tokenize.COMMENT` tokens are inspected; `OPTION_PO.search` non-match → skip silently.
3. **`skip-file` / `disable-all` (atomic keywords).** Emit `deprecated-pragma` I0022
   (`'Pragma "%s" is deprecated, use "%s" instead' % ("disable-all", "skip-file")`) only for
   `disable-all`; then `file-ignored` I0013 ("Ignoring entire file") at `line=start[0]`; set
   `self._ignore_file = True` and **return immediately** (remaining tokens/pragmas unprocessed).
   Both I-messages are default-disabled → invisible under any default config.
4. **Deprecated `disable-msg`/`enable-msg`** → mapped to disable/enable + `deprecated-pragma`
   I0022 with args `("disable-msg", "disable")` / `("enable-msg", "enable")`.
5. Per message token: control pragmas record `self._pragma_lineno[msgid] = start[0]`
   (consumed only by the format checker's line-length pragma-awareness, format.py:457; msgid here
   is the RAW token string — symbol or id).
6. **`disable=all` inline** → `deprecated-pragma` I0022 args `("disable=all", "skip-file")` +
   `file-ignored` I0013, `_ignore_file = True`, return. Note this happens mid-message-loop:
   earlier ids in the same pragma (e.g. `disable=foo,all`) were already disabled (harmless,
   module is dropped).
7. `meth(msgid, "module", l_start)` → `disable`/`enable`/`disable_next` with
   scope `"module"` (block expansion, §7) — `disable_next` overrides to `"line"`, `l_start+1`.
8. **Error messages from message resolution** (raised inside `_get_messages_to_set` →
   `get_active_msgids`):
   - Deleted/moved message → `useless-option-value` **R0022**, template
     `"Useless option value for '%s', %s"`, args `(action, exception)` (the exception's `str` is
     e.g. `'print-statement' was removed from pylint, see <url>.`), `line=start[0]`,
     `confidence=HIGH`. R-category → **disabled under -E**.
   - Unknown message → `unknown-option-value` **W0012**, template
     `"Unknown option value for '%s', expected a valid pylint message and got '%s'"`,
     args `(action, msgid_token)`, `line=start[0]`, `confidence=HIGH`. W-category →
     **disabled under -E**.
9. **Pragma-grammar errors** (from `parse_pragma`, abort the rest of THIS comment only):
   - `UnRecognizedOptionError` → **E0011 `unrecognized-inline-option`**, template
     `"Unrecognized file option %r"`, args = `err.token` (a single string, `%r` repr),
     `line=start[0]` (comment token start row; col is not passed → reported column 0),
     no confidence (UNDEFINED). **E-category → IN SCOPE and visible under -E.**
     Triggers: unknown keyword before `=` (token = the word), bare unknown word
     (token = last token seen, `""` if the payload had no tokens), `=` after an
     assignment-incapable keyword (token = keyword, e.g. `skip-file`).
   - `InvalidPragmaError` → **I0010 `bad-inline-option`**, template
     `"Unable to consider inline option %r"`, args = `err.token`, `line=start[0]`.
     I-category, default-disabled → never visible.
     Triggers: keyword without `=`/ids (`disable`, `disable=`, `disable foo`, `disable=x enable=y`),
     `=` with nothing before it (token `""`).

E0011 is itself subject to suppression (it goes through `add_message` →
`is_message_enabled("E0011", start_row)`), so `# pylint: disable=E0011` earlier in the module
suppresses later E0011s (block expansion applies; both are module-scope states).

Note on timing: pragmas are processed **file-by-file at lint time**, AFTER the AST exists and
after the per-file `FileState` is installed, and BEFORE raw checkers, token checkers and the AST
walker run (pylinter.py:1091-1106). Messages emitted by the walker/checkers therefore always see
fully-populated `_module_msgs_state`.

---

## 7. FileState block expansion — FULL

### 7.1 `set_msg_status` (`file_state.py:184-205`)

```python
def set_msg_status(self, msg, line, status, scope="package"):
    assert line > 0
    if scope != "line":
        # Expand the status to cover all relevant block lines
        self._set_state_on_block_lines(self._msgs_store, self._module, msg, {line: status})
    else:
        self._set_message_state_on_line(msg, line, status, line)

    # Store the raw value
    try:
        self._raw_module_msgs_state[msg.msgid][line] = status
    except KeyError:
        self._raw_module_msgs_state[msg.msgid] = {line: status}
```

- `scope="module"` (normal pragmas) → block expansion over the whole module AST.
- `scope="line"` (only `disable-next`) → exactly one line (`pragma_line + 1`), no expansion.
- RAW state always records the pragma's own line → used by the past-EOF fallback (§2.2) and
  useless-suppression (§9).
- `self._module` is the module node; in the lint flow it is always non-None when pragmas are
  processed. (`assert line > 0` — line 0 pragmas can't happen since token rows are ≥ 1.)

### 7.2 `_set_state_on_block_lines` (`file_state.py:56-90`) — verbatim

```python
def _set_state_on_block_lines(self, msgs_store, node, msg, msg_state):
    """Recursively walk (depth first) AST to collect block level options
    line numbers and set the state correctly.
    """
    for child in node.get_children():
        self._set_state_on_block_lines(msgs_store, child, msg, msg_state)
    # first child line number used to distinguish between disable
    # which are the first child of scoped node with those defined later.
    # For instance in the code below:
    #
    # 1.   def meth8(self):
    # 2.        """test late disabling"""
    # 3.        pylint: disable=not-callable, useless-suppression
    # 4.        print(self.blip)
    # 5.        pylint: disable=no-member, useless-suppression
    # 6.        print(self.bla)
    #
    # E1102 should be disabled from line 1 to 6 while E1101 from line 5 to 6
    #
    # this is necessary to disable locally messages applying to class /
    # function using their fromlineno
    if (isinstance(node, (nodes.Module, nodes.ClassDef, nodes.FunctionDef))
            and node.body):
        firstchildlineno = node.body[0].fromlineno
    else:
        firstchildlineno = node.tolineno
    self._set_message_state_in_block(msg, msg_state, node, firstchildlineno)
```

Children are processed BEFORE the node itself (post-order), so the **innermost** enclosing block
containing the pragma line claims it first; `_set_message_state_in_block` deletes the consumed
line from `msg_state` (`del lines[lineno]`), so ancestors then iterate an empty dict and do
nothing. `msg_state` is always a fresh single-entry `{pragma_line: status}` per `set_msg_status`
call. Note `node.body[0]` — astroid docstrings are NOT in `body` (they're `doc_node`), so the
first child is the first real statement. `AsyncFunctionDef` is a subclass of `FunctionDef` so it
takes the first branch too.

### 7.3 `_set_message_state_in_block` (`file_state.py:92-162`) — verbatim

```python
def _set_message_state_in_block(self, msg, lines, node, firstchildlineno):
    """Set the state of a message in a block of lines."""
    first = node.fromlineno
    last = node.tolineno
    for lineno, state in list(lines.items()):
        original_lineno = lineno
        if first > lineno or last < lineno:
            continue
        # Set state for all lines for this block, if the
        # warning is applied to nodes.
        if msg.scope == WarningScope.NODE:
            if lineno > firstchildlineno:
                state = True
            first_, last_ = node.block_range(lineno)
            # pylint: disable=useless-suppression
            # For block nodes first_ is their definition line. For example, we
            # set the state of line zero for a module to allow disabling
            # invalid-name for the module. For example:
            # 1. # pylint: disable=invalid-name
            # 2. ...
            # OR
            # 1. """Module docstring"""
            # 2. # pylint: disable=invalid-name
            # 3. ...
            #
            # But if we already visited line 0 we don't need to set its state again
            # 1. # pylint: disable=invalid-name
            # 2. # pylint: enable=invalid-name
            # 3. ...
            # The state should come from line 1, not from line 2
            # Therefore, if the 'fromlineno' is already in the states we just start
            # with the lineno we were originally visiting.
            # pylint: enable=useless-suppression
            if (first_ == node.fromlineno
                    and first_ >= firstchildlineno
                    and node.fromlineno in self._module_msgs_state.get(msg.msgid, ())):
                first_ = lineno

        else:
            first_ = lineno
            last_ = last
        for line in range(first_, last_ + 1):
            # Do not override existing entries. This is especially important
            # when parsing the states for a scoped node where some line-disables
            # have already been parsed.
            if ((isinstance(node, nodes.Module) and node.fromlineno <= line < lineno)
                or (not isinstance(node, nodes.Module)
                    and node.fromlineno < line < lineno)
               ) and line in self._module_msgs_state.get(msg.msgid, ()):
                continue
            if line in lines:  # state change in the same block
                state = lines[line]
                original_lineno = line

            self._set_message_state_on_line(msg, line, state, original_lineno)

        del lines[lineno]
```

Mechanics:

- **Containment check** uses `node.fromlineno`/`node.tolineno` (astroid:
  `fromlineno` = `lineno` or fixed-up, node_ng.py:399-407; `tolineno` = `end_lineno` falling back
  to last child, node_ng.py:409-424). `Module.fromlineno` is **0** (constructed with `lineno=0`,
  scoped_nodes.py:276); `FunctionDef.fromlineno` is the **`def` line computed by adding decorator
  line spans to the decorator-start lineno** (scoped_nodes.py:1386-1400; the rebuilder resets a
  decorated function's `lineno` to the first decorator's line, rebuilder.py:1130-1139).
  ClassDef has NO fromlineno override (its `lineno` is the `class` keyword line, decorators not
  included).
- **LINE-scoped messages** (`else` branch, includes ALL main-checker MSGS, all token/raw-checker
  messages): the pragma applies from its own line to the END of the enclosing block
  (`first_ = lineno; last_ = node.tolineno`). No backward extension ever.
- **NODE-scoped messages** (most E-category checker messages):
  - "Late disable" rule: if the pragma line is after the block's first statement line
    (`lineno > firstchildlineno`), state is forced to `True` (enabled) for the prefix of the
    block; the `if line in lines` switch flips it back to the pragma's state at the pragma's
    own line. Net effect: lines `first_..lineno-1` get True, `lineno..last_` get the pragma
    state.
  - "Early disable" (pragma at/before the first statement line — e.g. right under the `def`
    or among/before the docstring): state extends over the WHOLE `block_range`, including the
    definition line(s) before the pragma. This is what makes
    `def f():\n  # pylint: disable=E1102` suppress E1102 reported at the `def` line.
  - `block_range` per astroid node type:
    - default `NodeNG.block_range(lineno)` → `(lineno, self.tolineno)` (node_ng.py:445-453) —
      this is what `For` uses (no override);
    - `Module.block_range` → `(self.fromlineno, self.tolineno)` = `(0, tolineno)`
      (scoped_nodes.py:303-310);
    - `FunctionDef.block_range` → `(self.fromlineno, self.tolineno)` (scoped_nodes.py:1415-1422);
    - `ClassDef.block_range` → `(self.fromlineno, self.tolineno)` (scoped_nodes.py:1972-1979);
    - `If.block_range` (node_classes.py:3033-3045): pragma on the first body line → that line
      only `(lineno, lineno)`; inside body → `(lineno, body[-1].tolineno)`; else
      `_elsed_block_range(lineno, orelse, body[0].fromlineno - 1)`;
    - `Try.block_range` / `TryStar.block_range` (node_classes.py:3885-3907 / 3986-4008):
      `lineno == fromlineno` → `(lineno, lineno)`; inside body → till end of body; on an
      `except <type>` line → that line only; inside a handler body → till its end; on the line
      before `else:`/`finally:` body → that line; inside them → till their end; else
      `(lineno, self.tolineno)`;
    - `While.block_range` (node_classes.py:4444-4452) → `_elsed_block_range(lineno, orelse)`;
    - `_elsed_block_range` (_base_nodes.py:244-256):
      ```python
      if lineno == self.fromlineno:
          return lineno, lineno
      if orelse:
          if lineno >= orelse[0].fromlineno:
              return lineno, orelse[-1].tolineno
          return lineno, orelse[0].fromlineno - 1
      return lineno, last or self.tolineno
      ```
  - The `first_ == node.fromlineno and first_ >= firstchildlineno and node.fromlineno in
    self._module_msgs_state...` guard avoids re-overwriting the definition-line state when a
    second pragma (e.g. a subsequent `enable`) hits the same block whose start was already
    state-set; in that case the expansion starts at the pragma line instead of the block start.
- **No-override rule**: lines strictly before the pragma line (and ≥ node start; for Module the
  range includes line 0, for other nodes excludes the node's own fromlineno) that ALREADY have an
  entry in `_module_msgs_state[msgid]` are skipped — earlier pragmas win for the prefix.
- `if line in lines` ("state change in the same block") can only trigger at the pragma's own
  line in the production flow (single-entry dict), switching `state`/`original_lineno` to the
  pragma's values for the remainder of the range.

### 7.4 `_set_message_state_on_line` (`file_state.py:164-182`)

```python
def _set_message_state_on_line(self, msg, line, state, original_lineno):
    # Update suppression mapping
    if not state:
        self._suppression_mapping[(msg.msgid, line)] = original_lineno
    else:
        self._suppression_mapping.pop((msg.msgid, line), None)
    # Update message state for respective line
    try:
        self._module_msgs_state[msg.msgid][line] = state
    except KeyError:
        self._module_msgs_state[msg.msgid] = {line: state}
```

`_module_msgs_state[msgid][line]` is **overwritten** (last write wins) except where the
no-override `continue` in §7.3 skipped the write. `_suppression_mapping[(msgid, line)]` tracks
which pragma line caused a disable (for I0020 `suppressed-message` reporting, §9).

### 7.5 Module-level pragmas before any statement

For a pragma on line L < first statement line, contained only by the Module node (no inner
node spans it):
- NODE-scope msg: `firstchildlineno = body[0].fromlineno > L` → state preserved;
  `block_range(L) = (0, module.tolineno)` → **entire module including line 0** gets the state.
  (Line 0 matters for module-level NODE messages reported at `fromlineno` 0, e.g. C0103
  invalid-name for a module — none in -E scope, but E-scope module-level messages reported with
  node=Module get `line = node.fromlineno = 0`... see §8: `if not line:` then replaces 0 with
  `node.position`/`fromlineno`, still 0, and `is_message_enabled(msgid, 0)` looks up line 0.)
- LINE-scope msg: `first_ = L`, `last_ = module.tolineno` → from the pragma line down.

### 7.6 disable-next

`disable_next` → `scope="line"`, single entry `_module_msgs_state[msgid][pragma_line+1]=False`
plus `_raw_module_msgs_state[msgid][pragma_line+1]=False` and
`_suppression_mapping[(msgid, pragma_line+1)] = pragma_line+1`. NO block expansion; applies to
exactly one physical line (the line after the comment; with backslash continuation `l_start`
already shifted, so it is "the line after the logical start line minus adjustments").

---

## 8. Where a message's `line` comes from (`PyLinter._add_one_message`, pylinter.py:1195-1285)

```python
message_definition.check_message_definition(line, node)
# Look up "location" data of node if not yet supplied
if node:
    if node.position:
        if not line:           line = node.position.lineno
        if not col_offset:     col_offset = node.position.col_offset
        if not end_lineno:     end_lineno = node.position.end_lineno
        if not end_col_offset: end_col_offset = node.position.end_col_offset
    else:
        if not line:           line = node.fromlineno
        if not col_offset:     col_offset = node.col_offset
        if not end_lineno:     end_lineno = node.end_lineno
        if not end_col_offset: end_col_offset = node.end_col_offset

# should this message be displayed
if not self.is_message_enabled(message_definition.msgid, line, confidence):
    self.file_state.handle_ignored_message(
        self._get_message_state_scope(message_definition.msgid, line, confidence),
        message_definition.msgid, line)
    return
```

- `node.position` (set by astroid rebuilder `_get_position_info` for FunctionDef/ClassDef:
  the `def`/`class` keyword + name span) takes precedence; otherwise `fromlineno`/`col_offset`.
  Note `if not line` — an explicit `line=0` is REPLACED by node coordinates (0 is falsy).
- The suppression check uses this resolved `line` (so block state computed on `fromlineno`
  for NODE-scope messages lines up with the lookup line).
- After passing the check: stats updated (`stats.increase_single_message_count` etc.,
  `msg_status |= MSG_TYPES_STATUS[...]` — exit-code relevant), then
  `msg = message_definition.msg; if args is not None: msg %= args` (note: `args is not None` —
  empty tuple/string still formats), and the reporter receives
  `Message(msgid, symbol, MessageLocationTuple(abspath or "", path, module or "", obj,
  line or 1, col_offset or 0, end_lineno, end_col_offset), msg, confidence)`
  (pylinter.py:1268-1285). **`line or 1`**: messages emitted with line 0/None are REPORTED at
  line 1, column `col_offset or 0`. `end_lineno`/`end_col_offset` pass through unchanged
  (may be None).
- `add_message` (pylinter.py:1287-1319): `confidence = confidence or UNDEFINED`, resolves
  msgid/symbol through `get_message_definitions` (raises `UnknownMessageError` → crash if a
  checker emits an unregistered message), loops over the (possibly multiple) definitions.
- module/obj: `node is None` → `(current_name, "")`, abspath = `current_file`; else
  `utils.get_module_and_frameid(node)` and `node.root().file`.

Main-checker message templates in scope (pylinter.py:103-254):

| id | symbol | template | args | line/col |
|---|---|---|---|---|
| F0001 | fatal | `%s` | error string (in `_expand_files`: `str(ex)` with `os.getcwd()+os.sep` stripped, pylinter.py:926-932) | line=None → reported line 1 col 0 |
| F0002 | astroid-error | `%s: %s` | `(filepath, get_fatal_error_message(...))` (pylinter.py:748-757/786-796; text: `Fatal error while checking '{filepath}'. Please open an issue in our bug tracker so we address this. There is a pre-filled template that you can use in '{path}'.`, lint/utils.py:107-112) | line=None → 1; confidence HIGH |
| F0010 | parse-error | `error while code parsing: %s` | `ex` (AstroidBuildingError; `%s` of the exception) (pylinter.py:1027-1028) | line=None → 1 |
| F0011 | config-parse-error | `error while parsing the configuration: %s` | (config file parse failure) | line=None → 1 |
| E0001 | syntax-error | `%s` | see §10 | explicit line/col |
| E0011 | unrecognized-inline-option | `Unrecognized file option %r` | `err.token` (string) | line=pragma comment row, col 0, UNDEFINED confidence |
| E0013 | bad-plugin-value | `Plugin '%s' is impossible to load, is it installed ? ('%s')` | `(plugin_name, exception)` (load_plugin_configuration) | line 0 → 1 |
| E0014 | bad-configuration-section | `Out-of-place setting encountered in top level configuration-section '%s' : '%s'` | (toml top-level key/value) | line 0 → 1 |
| E0015 | unrecognized-option | `Unrecognized option found: %s` | comma-joined option names (config_initialization.py:108-112) | line=0 → 1 |

All MSGS have `scope: WarningScope.LINE`; the I-prefixed ones additionally
`default_enabled: False` (disabled at registration via `register_checker`, pylinter.py:500-504:
`if not message.default_enabled: self.disable(message.msgid)` → `_msgs_state[...] = False`).

---

## 9. useless-suppression machinery (side effects only under -E)

After each module is checked, `_lint_file` (pylinter.py:825-830) / `_check_file`
(pylinter.py:867-872) run:

```python
spurious_messages = self.file_state.iter_spurious_suppression_messages(self.msgs_store)
for msgid, line, args in spurious_messages:
    self.add_message(msgid, line, None, args)
```

`iter_spurious_suppression_messages` (file_state.py:225-251) — verbatim:

```python
for warning, lines in self._raw_module_msgs_state.items():
    for line, enable in lines.items():
        if (not enable
                and (warning, line) not in self._ignored_msgs
                and warning not in INCOMPATIBLE_WITH_USELESS_SUPPRESSION):
            yield "useless-suppression", line, (
                msgs_store.get_msg_display_string(warning),)
# don't use iteritems here, _ignored_msgs may be modified by add_message
for (warning, from_), ignored_lines in list(self._ignored_msgs.items()):
    for line in ignored_lines:
        yield "suppressed-message", line, (
            msgs_store.get_msg_display_string(warning), from_)
```

- `INCOMPATIBLE_WITH_USELESS_SUPPRESSION` = frozenset {R0401, W0402, W1505, W1511, W1512,
  W1513, R0801} (constants.py:88-98).
- `get_msg_display_string` (message_definition_store.py:74-79): `repr(symbol)` for a single
  definition, `repr([symbols...])` for multi-definition old ids.
- Both yielded ids are I0021/I0020 — **I-category, default-disabled**: under the pinned config
  `add_message` immediately bails at `is_message_enabled` (state scope is CONFIG → no
  `_ignored_msgs` mutation). Net effect under -E: a pure iteration, no output, no state change.
  A port may skip implementing emission but MUST still populate `_raw_module_msgs_state` (needed
  for §2.2 fallback) and may skip `_ignored_msgs`/`_suppression_mapping` entirely.
- `handle_ignored_message` (file_state.py:207-223) records
  `_ignored_msgs[(msgid, orig_pragma_line)].add(line)` only when `state_scope ==
  MSG_STATE_SCOPE_MODULE` (i.e. the suppression came from an in-file pragma) and the
  `(msgid, line)` has a `_suppression_mapping` entry.

---

## 10. skip-file and the E0001 phase interaction

Phases in the default single-job, non-stdin run (`PyLinter.check`, pylinter.py:672-727):

1. `initialize()` — py_version gating into `_msgs_state` (§1.5).
2. `_iterate_file_descrs` → FileItems.
3. **Phase A — `_get_asts`** (pylinter.py:729-759): for each file,
   `set_current_module(name, filepath)` then `self.get_ast(filepath, name, data)`.
   `get_ast` (pylinter.py:998-1038):
   ```python
   try:
       if data is None:
           return MANAGER.ast_from_file(filepath, modname, source=True)
       return astroid.builder.AstroidBuilder(MANAGER).string_build(data, modname, filepath)
   except astroid.AstroidSyntaxError as ex:
       line = getattr(ex.error, "lineno", None)
       if line is None:
           line = 0
       self.add_message("syntax-error", line=line,
                        col_offset=getattr(ex.error, "offset", None),
                        args=f"Parsing failed: '{ex.error}'", confidence=HIGH)
   except astroid.AstroidBuildingError as ex:
       self.add_message("parse-error", args=ex)
   except Exception as ex:
       traceback.print_exc()
       raise astroid.AstroidBuildingError(...) from ex
   return None
   ```
   - E0001 args: `f"Parsing failed: '{ex.error}'"` where `ex.error` is the underlying
     `SyntaxError` (its `str` includes the CPython message + `(file, line N)` suffix).
     `col_offset` = SyntaxError.offset (1-based) or None→0. `line or 1` applies at Message
     construction (a SyntaxError with lineno None reports at line 1).
   - `AstroidBuildingError` (non-syntax) → F0010 parse-error; unexpected exception → re-raised
     as AstroidBuildingError, caught by `_get_asts` → F0002 astroid-error with crash template.
4. **Phase B — `_lint_files`/`_lint_file`** (pylinter.py:771-830): files whose AST is `None`
   are **skipped entirely** (`if module is None: continue`). For others:
   `set_current_module`, `_ignore_file = False`, fresh `FileState`, then
   `check_astroid_module` → `_check_astroid_module` (pylinter.py:1062-1106):
   tokenize (TokenError → E0001 + `return None`); if `node.pure_python`: `process_tokens`
   (pragmas); **`if self._ignore_file: return False`** — skip-file aborts BEFORE raw checkers,
   token checkers, and the AST walker. Then spurious-suppression iteration (§9).

Consequences (capture these exactly):

- **skip-file cannot suppress E0001**: a file with a syntax error never reaches
  `process_tokens` (AST is None ⇒ Phase B skipped), so its `# pylint: skip-file` is never seen.
  E0001 is emitted in Phase A unconditionally (modulo global `_msgs_state` disables).
- During Phase A, `linter.file_state` is still the **base FileState** (empty module state,
  `_effective_max_line_number = None`) for every file — `is_message_enabled("E0001", line)`
  reduces to `_msgs_state.get("E0001", True)`. There is no cross-file pragma leakage in this
  flow. (In the alternative `check_single_file_item`/`_check_file` flow used by parallel jobs,
  `get_ast` runs while `file_state` belongs to the PREVIOUS file — a stale-state quirk; the
  pinned single-job invocation never hits it.)
- skip-file suppresses **everything emitted at or after `process_tokens`** for that module:
  raw/token-checker messages, all walker messages, and the spurious-suppression pass still runs
  (on whatever pragma state had accumulated before the skip-file line — all I-messages anyway).
  Messages emitted BEFORE the skip-file pragma inside `process_tokens` itself (deprecated-pragma
  I0022, locally-disabled I0011, E0011 on an earlier malformed comment, W0012/R0022) are already
  emitted and stay.
- `file-ignored` (I0013) and `deprecated-pragma` (I0022) accompany skip-file/disable-all but are
  default-disabled.
- `_ignore_file` is an instance attribute reset per file (pylinter.py:814, 857; initialized
  False at pylinter.py:364).

---

## 11. Confidence levels (`pylint/interfaces.py:26-38`)

```python
HIGH = Confidence("HIGH", "Warning that is not based on inference result.")
CONTROL_FLOW = Confidence("CONTROL_FLOW", "Warning based on assumptions about control flow.")
INFERENCE = Confidence("INFERENCE", "Warning based on inference result.")
INFERENCE_FAILURE = Confidence("INFERENCE_FAILURE", "Warning based on inference with failures.")
UNDEFINED = Confidence("UNDEFINED", "Warning without any associated confidence level.")
CONFIDENCE_LEVELS = [HIGH, CONTROL_FLOW, INFERENCE, INFERENCE_FAILURE, UNDEFINED]
```

- `config.confidence` default = all five names ⇒ **no confidence ever filtered under the pinned
  invocation** (`is_message_enabled` first line and `_get_message_state_scope`).
- `Message.confidence = confidence or UNDEFINED` (message.py:46).

---

## 12. State summary for `pylint . -E --disable=<list>` (pinned flags)

Order of state mutations before linting begins:

1. `PyLinter.__init__` → `register_checker(self)` disables I0001, I0010, I0011, I0013, I0020,
   I0021, I0022 (`default_enabled: False`).
2. `load_default_plugins` → other checkers' `default_enabled: False` messages disabled
   (W0511? no — fixme is default enabled; misc.py:31 `{"default_enabled": False}` is I0023
   use-symbolic-message-instead; implicit_booleaness C1804/C1805 disabled — all non-E anyway).
3. Config file parsing (none for `iso` variant: `--rcfile=empty.rcfile`).
4. Command-line parsing left-to-right: `-E` sets `_error_mode = True`;
   `--disable=C0301,...` calls `disable()` per CSV item → `_msgs_state[id] = False` for each
   (includes the in-scope exclusions E0110, E0401, E0611, E1101 → globally disabled).
5. `_emit_stashed_messages` (none expected), `enable_fail_on_messages` (no `--fail-on`),
   `_parse_error_mode` → disables categories I, R, W, C (in that order) + checker
   "miscellaneous"; reports/persistent/score off.
6. `check()` → `initialize()` → `_msgs_state["E0106"] = False` (py_version gate).

Effective global state: every non-E/F msgid → False; E/F msgids → absent from `_msgs_state`
(=True) except the explicitly disabled E0110/E0401/E0611/E1101/E0106 → False.
Per-file pragmas then overlay `_module_msgs_state` per §6-§7, with exact-line hits taking
precedence over the global map per §2.2.

---

## 13. Porting checklist of conservatism / bailout paths

1. `is_message_enabled`: unknown msg_descr → treated as msgid, default enabled (no crash).
2. `_is_one_message_enabled` past-EOF fallback only when `max_line_number` truthy AND
   `line > max_line_number`; fallback default from last-inserted raw pragma ≤ line; global
   `_msgs_state` still consulted first via `.get(msgid, fallback)`.
3. `_register_by_id_managed_msg`: silently ignores unknown numeric ids.
4. `_XableAction._call`: CLI disable/enable of deleted/moved/unknown ids never aborts —
   stashed as R0022/W0012 messages at line 0 (invisible under -E).
5. `process_tokens`: non-comment tokens skipped; non-matching comments skipped; grammar errors
   abort only the current comment (`continue`); resolution errors abort only the current msgid
   token; skip-file/disable-all/disable=all return immediately (rest of file's pragmas dropped).
6. `_set_message_state_in_block`: containment check skip (`first > lineno or last < lineno`);
   no-override rule for already-set prefix lines; `del lines[lineno]` prevents ancestor
   reprocessing.
7. `get_ast`: AstroidSyntaxError → E0001 (`lineno` default 0 → reported line 1);
   AstroidBuildingError → F0010; anything else → F0002 with crash-report template.
8. `_check_astroid_module`: TokenError → E0001 + skip module; `not node.pure_python` →
   I0001 raw-checker-failed (disabled) but walker still runs; `_ignore_file` → return False
   before any checker.
9. `_add_one_message`: `check_message_definition` invariants (would be assertion-level crashes);
   `if not line` (0 is falsy) node-coordinate substitution; `line or 1`, `col_offset or 0` at
   Message construction.
10. Dict-order dependencies: `MSG_TYPES` literal order (disable-all expansion);
    `_msgs_by_category` registration order `E,F,I,R,W,C` with per-checker msgid sort;
    `_msgs_state` insertion order (config.enable/disable rebuild);
    `_raw_module_msgs_state[msgid]` insertion order (= ascending pragma line order) for the
    past-EOF fallback; `_module_msgs_state` plain dicts keyed by int lines (no sorting anywhere).
11. `ALL_KEYWORDS` tie order (disable-all vs disable-msg) is hash-seed dependent in source but
    behaviorally irrelevant (equal length, `\b`-delimited alternation).
12. Long category names (`error`, `warning`, ...) do NOT expand in 4.0.5 (uppercased key vs
    lowercase `MSG_TYPES_LONG`); single letters (any case) do. Checker names must be lowercase
    in `_checkers` lookup (`msgid.lower()`).
13. `enable`/`disable` pragma with an old msgid/symbol affects ALL mapped new msgids
    (`__old_names` list).
14. E0011 emission is itself suppressible by pragma state on its line.
