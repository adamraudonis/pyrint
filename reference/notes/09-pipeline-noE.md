# 09 — Full-pylint mode (no `-E`): pipeline deltas, score, exit codes, config files, `-j`

Spec extracted from pylint 4.0.5 source (`reference/pylint/pylint`, cited as
`<file>:<line>`), verified empirically against the pinned venv
(`.venv-pylint`: pylint 4.0.5 / astroid 4.0.4 / CPython 3.12.12, macOS).
All probe transcripts in this doc were produced with `PYLINTHOME` pointed at a
scratch dir so the persistent-stats cache is reproducible.

Scope: everything that changes when `-E` is dropped — the score footer and its
persistent-stats "previous run" suffix, the reports machinery, the full exit
bitmask + `--fail-under`/`--fail-on`, config-file discovery/parsing/precedence,
`check_parallel` (`-j`), and the default-enabled message set. Message-control
internals (pragmas, `_msgs_state`, block expansion) are in notes/03; two-phase
serial pipeline and output formatting are in notes/02. This doc only covers
what is NEW or DIFFERENT relative to those.

Quick contents:

1. What `-E` actually does (so we know what to undo)
2. Option defaults that gate the no-E pipeline
3. Default-enabled message set in full mode
4. Score: evaluation expression, footer bytes, suppression rules
5. Persistent stats: PYLINT_HOME, pickle file, filename derivation
6. Reports system (default off) and section rendering machinery
7. Exit codes: full bitmask, fail-under, fail-on, exit ladder (VERIFIED)
8. Config file discovery, parsing, precedence, disable merging
9. `check_parallel` (`-j`): protocol, merge, divergences vs `-j1` (PROBED)
10. Pipeline-level messages owned by the main checker (enumerated)
11. Port plan notes + msgs.rs cross-check
12. Open questions

---------------------------------------------------------------------------
## 1. What `-E` actually does (so we know what to undo)

`--errors-only` / `-E` is a `_CallableArgument` (base_options.py:551-561,
action `_ErrorsOnlyModeAction` callback_actions.py:266-284). The action ONLY
sets `self.run.linter._error_mode = True`. The actual mode switch happens at
the very END of `_config_initialization` (config_initialization.py:144
`linter._parse_error_mode()`), i.e. AFTER both the config file and the whole
command line have been parsed:

```python
# pylinter.py:558-570
def _parse_error_mode(self) -> None:
    if not self._error_mode:
        return
    self.disable_noerror_messages()      # disable every non-E/F category
    self.disable("miscellaneous")        # the fixme/encoding checker
    self.set_option("reports", False)
    self.set_option("persistent", False)
    self.set_option("score", False)
```

`disable_noerror_messages` (message_state_handler.py:234-239) iterates
`msgs_store._msgs_by_category` and calls `self.disable(msgcat)` for every
category except `{"E", "F"}` — i.e. it disables `W`, `C`, `R`, `I` wholesale
(category-level `_get_messages_to_set` expansion).

Consequences (all PROBED):
- `-E` prints NO footer even with explicit `--score=y` in either order
  (`pylint -E --score=y` and `--score=y -E` both footer-less) because
  `_parse_error_mode` runs after CLI parsing and unconditionally
  `set_option("score", False)`.
- `-E` never reads or writes the PYLINT_HOME stats pickle
  (`persistent=False`). Verified: no `$PYLINTHOME` dir created.
- `-E` disables reports (default off anyway).
- Inline `# pylint: enable=` pragmas can still resurrect W/R/C per line/block
  (notes/03; that's why pylfunc under `-E` exits 14).

Full mode = simply never calling `_parse_error_mode`'s body. Everything below
is the behavior with `_error_mode == False`.

---------------------------------------------------------------------------
## 2. Option defaults that gate the no-E pipeline

From `_make_linter_options` (base_options.py:38+). Exact defaults:

| option           | default                                   | cite (base_options.py) |
|------------------|-------------------------------------------|------|
| `persistent`     | `True` (type `yn`)                        | :77  |
| `output-format`  | `"text"`                                  | :97  |
| `reports` (`-r`) | `False` (type `yn`)                       | :114 |
| `evaluation`     | `"max(0, 0 if fatal else 10.0 - ((float(5 * error + warning + refactor + convention) / statement) * 10))"` | :126 |
| `score` (`-s`)   | `True` (type `yn`)                        | :143 |
| `fail-under`     | `10` (type `float` → parsed to `10.0`)    | :154 |
| `fail-on`        | `""` (type `csv` → `()`)                  | :163 |
| `confidence`     | `["HIGH","CONTROL_FLOW","INFERENCE","INFERENCE_FAILURE","UNDEFINED"]` (all levels ⇒ no filtering) | :174 |
| `enable`         | `()` (action `_EnableAction`)             | :185 |
| `disable`        | `()` (action `_DisableAction`)            | :203 |
| `msg-template`   | `""`                                      | :227 |
| `jobs` (`-j`)    | `1` (type `int`)                          | :242 |
| `exit-zero`      | `False` (store_true)                      | :310 |
| `py-version`     | `sys.version_info[:2]` → `(3, 12)` on the pinned runtime | :356-358 |

Note: the *evaluation* expression has NO `info` term — I messages never
affect the score (PROBED, §4.6). `fail-under` is a float; the comparison is
`score_value >= linter.config.fail_under` (run.py:253).

---------------------------------------------------------------------------
## 3. Default-enabled message set in full mode

### 3.1 Mechanism

Three layers decide the config-level (`_msgs_state`) default:

1. **Default = enabled.** `_is_one_message_enabled` falls back to
   `self._msgs_state.get(msgid, True)` (message_state_handler.py:283) — a
   message absent from `_msgs_state` is enabled.

2. **`default_enabled: False` in the msgs dict** → disabled at checker
   registration:
   ```python
   # pylinter.py:495-506 (register_checker)
   if hasattr(checker, "msgs"):
       self.msgs_store.register_messages_from_checker(checker)
       for message in checker.messages:
           if not message.default_enabled:
               self.disable(message.msgid)
   # Register the checker, but disable all of its messages.
   if not getattr(checker, "enabled", True):
       self.disable(checker.name)
   ```
   (`checker.enabled` is True for every default checker; only some
   extensions set it False.) These `disable()` calls also populate
   `config.disable` with the symbols via the `_set_msg_status` sync
   (message_state_handler.py:157-167).

3. **py-version gating** at `PyLinter.initialize()` (pylinter.py:624-636),
   run at the start of `check()`:
   ```python
   for msg in self.msgs_store.messages:
       if not msg.may_be_emitted(self.config.py_version):
           self._msgs_state[msg.msgid] = False
   ```
   `may_be_emitted` (message_definition.py:75-81):
   `minversion > py_version` → False; `maxversion <= py_version` → False.

### 3.2 The exact sets (pinned runtime, py-version (3,12)) — PROBED

Total registered messages: **389** (matches msgs.rs).

`default_enabled=False` (disabled at registration → appear in
`config.disable` even with empty config) — **10 messages**:

```
C1804 use-implicit-booleaness-not-comparison-to-string   (implicit_booleaness_checker.py:90; old_names C1901 compare-to-empty-string)
C1805 use-implicit-booleaness-not-comparison-to-zero     (implicit_booleaness_checker.py:102; old_names C2001 compare-to-zero)
I0001 raw-checker-failed              (pylinter.py:139)
I0010 bad-inline-option               (pylinter.py:149)
I0011 locally-disabled                (pylinter.py:158)
I0013 file-ignored                    (pylinter.py:167)
I0020 suppressed-message              (pylinter.py:179)
I0021 useless-suppression             (pylinter.py:189)
I0022 deprecated-pragma               (pylinter.py:201; old_names I0014 deprecated-disable-all)
I0023 use-symbolic-message-instead    (checkers/misc.py:31)
```

py-version-gated OFF at (3,12) (via `initialize()`, `_msgs_state` only —
NOT in `config.disable`) — **2 messages**:

```
E0106 return-arg-in-generator   maxversion (3,3)   (basic_error_checker.py:202)
W1502 boolean-datetime          maxversion (3,5)   (stdlib.py:508)
```

(For reference: stdlib.py:583 `maxversion (3,15)` and
basic_error_checker.py:261 / async 3.5 `minversion`s stay emittable at 3.12.)

⇒ **Full-mode default-enabled set = 389 − 10 − 2 = 377 messages.**

`harness/gen_msgs_rs.py` must grow new fields on `MessageDef`: the current
`enabled` flag means "enabled under `-E --disable=<harness list>`"
(msgs.rs:7). Full mode needs `default_enabled: bool` (the 10 above false) and
`minversion`/`maxversion` (or a precomputed `emittable_at_3_12`) so the
config-state seeding can be reproduced for arbitrary `--py-version`.

### 3.3 `default_enabled_messages` and the `disable=all` filter

`_MessageStateHandler.__init__` (message_state_handler.py:40-44) captures the
MAIN checker's own default-enabled message tuples:

```python
self.default_enabled_messages = {
    k: v for k, v in self.linter.msgs.items()
    if len(v) == 3 or v[3].get("default_enabled", True)
}
```

For PyLinter.msgs (MSGS, pylinter.py:103-252) that is exactly these **11**:
`F0001 fatal`, `F0002 astroid-error`, `F0010 parse-error`,
`F0011 config-parse-error`, `E0001 syntax-error`,
`E0011 unrecognized-inline-option`, `W0012 unknown-option-value`,
`R0022 useless-option-value`, `E0013 bad-plugin-value`,
`E0014 bad-configuration-section`, `E0015 unrecognized-option`.
(The I00xx in MSGS all carry `default_enabled: False` and are excluded.)

`disable=all` / `enable=all` expansion (message_state_handler.py:87-100):
"all" expands to every category; when DISABLING, messages whose msgid is in
`default_enabled_messages` are filtered OUT — i.e. `--disable=all` does NOT
disable the 11 pipeline messages above. (Enable=all enables everything.)

`run.py:202-211` uses the same set for the "nothing to do" bail:

```python
disable_all_msg_set = set(
    msg.symbol for msg in linter.msgs_store.messages
) - set(msg[1] for msg in linter.default_enabled_messages.values())
if not args or (
    len(linter.config.enable) == 0
    and set(linter.config.disable) == disable_all_msg_set
):
    print("No files to lint: exiting.")
    sys.exit(32)
```

PROBED: `pylint --disable=all pkg/d.py` prints `No files to lint: exiting.`
and exits **32** even though a file argument was given (the equality holds
because the 10 registration-time-disabled symbols are members of
`disable_all_msg_set` anyway). `--disable=all --enable=X` runs normally
(enable list non-empty). Replicate this check verbatim — including that it
compares SYMBOL sets reconstructed from `_msgs_state` (see §8.6).

---------------------------------------------------------------------------
## 4. Score

### 4.1 Where it happens

`Run.__init__` (run.py:229-243): after `linter.check(args)` it always calls
`score_value = linter.generate_reports(verbose=self.verbose)` (with
`reporter.out` re-pointed at the `--output` file if given; OSError opening
that file → print to stderr, exit 32).

```python
# pylinter.py:1121-1146 (generate_reports)
self.reporter.display_messages(report_nodes.Section())   # no-op for TextReporter
if not self.file_state._is_base_filestate:
    previous_stats = load_results(self.file_state.base_name)
    self.reporter.on_close(self.stats, previous_stats)    # no-op for TextReporter
    if self.config.reports:
        sect = self.make_reports(self.stats, previous_stats)
    else:
        sect = report_nodes.Section()
    if self.config.reports:
        self.reporter.display_reports(sect)
    score_value = self._report_evaluation(verbose)
    if self.config.persistent:
        save_results(self.stats, self.file_state.base_name)
else:
    self.reporter.on_close(self.stats, LinterStats())
    score_value = None
return score_value
```

`file_state._is_base_filestate` is True only for the placeholder FileState
created in `PyLinter.__init__` (pylinter.py:361,
`FileState("", self.msgs_store, is_base_filestate=True)`). It is replaced by
a real FileState the first time a module is actually linted
(`_lint_file` pylinter.py:816 / `_check_file` pylinter.py:858:
`self.file_state = FileState(file.modpath, self.msgs_store, module)`), and in
parallel mode by `check_parallel`'s merge loop (parallel.py:163-164). So:
**if no module was ever linted (all args fatal/ignored, or zero FileItems),
there is no footer, no stats save, and `score_value is None`.**

### 4.2 `_report_evaluation` — exact algorithm

```python
# pylinter.py:1149-1192
note = None
previous_stats = load_results(self.file_state.base_name)   # 2nd load; same file as generate_reports'
if self.stats.statement == 0:
    return note                                             # no footer AT ALL, even with score=y
evaluation = self.config.evaluation
try:
    stats_dict = {"fatal": ..., "error": ..., "warning": ..., "refactor": ...,
                  "convention": ..., "statement": ..., "info": ...}   # from self.stats counters
    note = eval(evaluation, {}, stats_dict)
except Exception as ex:
    msg = f"An exception occurred while rating: {ex}"
else:
    self.stats.global_note = note
    msg = f"Your code has been rated at {note:.2f}/10"
    if previous_stats:
        pnote = previous_stats.global_note
        if pnote is not None:
            msg += f" (previous run: {pnote:.2f}/10, {note - pnote:+.2f})"
    if verbose:
        checked_files_count = self.stats.node_count["module"]
        unchecked_files_count = self.stats.undocumented["module"]   # (!) "skipped" is the undocumented-module count
        checked_files = ", ".join(self.stats.modules_names)         # (!) set join — hash order
        msg += (f"\nChecked {checked_files_count} files/modules ({checked_files}),"
                f" skipped {unchecked_files_count} files/modules")
if self.config.score:
    sect = report_nodes.EvaluationSection(msg)
    self.reporter.display_reports(sect)
return note
```

Details that matter for byte parity:

- `stats.statement` is assigned ONCE, at `_astroid_module_checker` context
  exit (pylinter.py:994 `self.stats.statement = walker.nbstatements`); the
  walker increments `nbstatements` for every visited node with
  `is_statement` (ast_walker.py:83-84). Zero statements ⇒ early return:
  **no footer line whatsoever** (not even with `--score=y`). PROBED: a run
  whose only output was an E0001 printed messages only, exit 2. A run on a
  module containing only a docstring/comment also has 0 statements (module
  docstrings are `doc_node`, not statements).
- The default expression is evaluated with Python semantics:
  `float` division, `max(0, x)` returns the int `0` when clamped (also when
  `fatal>0`: `0 if fatal` → int 0 → `max(0,0)` → `0`), otherwise a float.
  `{note:.2f}` formats either. Any `fatal` ⇒ score `0.00`. There is **no
  upper clamp**: PROBED `--evaluation='11.5'` prints
  `Your code has been rated at 11.50/10`.
- The exception arm: PROBED `--evaluation='1/0'` prints (as the whole footer
  body) `An exception occurred while rating: division by zero`, and returns
  `note=None` ⇒ exit falls through to `sys.exit(msg_status)` (run.py:260).
  Note `stats.global_note` is NOT updated in this arm, but `save_results`
  still runs ⇒ the pickle keeps the stale/initial `global_note` (initial is
  the int `0` from `LinterStats.__init__`, linterstats.py:137).
- `previous_stats` truthiness: `load_results` returns a `LinterStats` or
  `None`; the object is always truthy. `pnote = previous_stats.global_note`
  is never `None` in practice (init 0) so the suffix is printed whenever the
  stats file loaded. PROBED: after a zero-statement run saved stats, the next
  run printed `(previous run: 0.00/10, +10.00)`.
- Float formatting: `f"{x:.2f}"` and `f"{x:+.2f}"` — correctly-rounded
  fixed-point of the exact double, ties-to-even; Rust `format!("{:.2}")` /
  `format!("{:+.2}")` match (including `+0.00`, and a possible `-0.00` when
  `-0.005 < delta < 0`). Delta is computed in double precision
  (`note - pnote`).
- `verbose` extra line: `modules_names` is a `set[str]` of FILEPATHS (added
  at pylinter.py:786 `self.stats.modules_names.add(fileitem.filepath)` — only
  in the serial `_lint_files` path!). The join order is set-iteration order ⇒
  PYTHONHASHSEED-dependent; harness pins `PYTHONHASHSEED=0`.
  `skipped N files/modules` actually prints `stats.undocumented["module"]`
  (the undocumented-modules counter), not `stats.skipped` — replicate the
  bug. PROBED output:
  `Checked 3 files/modules (pkg/a.py, pkg/__init__.py, pkg/b.py), skipped 0 files/modules`.

### 4.3 Footer rendering — exact bytes

`EvaluationSection(msg)` (ureports/nodes.py:139-147) builds:
`Paragraph[Text("-"*len(msg))]`, `Paragraph[Text(msg)]`.

`BaseReporter.display_reports` (base_reporter.py:54-62): `report_id` is empty
for an EvaluationSection ⇒ no title mutation; calls `self._display(layout)`.

`TextReporter._display` (text.py:163-166):

```python
print(file=self.out)                  # leading "\n"
TextWriter().format(layout, self.out)
```

`TextWriter.visit_evaluationsection` (text_writer.py:45-50): `section += 1`,
`format_children` (each Paragraph: children text then `writeln()` —
text_writer.py:60-62), `section -= 1`, `writeln()`.

So the footer byte sequence appended after the last message line is exactly:

```
"\n" + ("-" * len(msg)) + "\n" + msg + "\n" + "\n"
```

`len(msg)` is the Python `str` length (chars, not bytes) of the FULL message
including the previous-run suffix and, in verbose mode, the appended
`"\nChecked ..."` line (the `\n` counts as 1 toward the dash count, and the
dashes are computed AFTER the verbose append — verified: verbose dash line is
much longer than the rating line). Verified with `od -c`:

```
...unused-variable)\n
\n
------------------------------------------------------------------\n
Your code has been rated at 7.50/10 (previous run: 7.50/10, +0.00)\n
\n
```

First-run form (no stats file): `Your code has been rated at 7.50/10`
(35 dashes). With suffix: `... (previous run: 7.50/10, +0.00)` (67 dashes).
Negative delta probe: `(previous run: 7.50/10, -7.50)`.

### 4.4 When the footer is printed vs suppressed — summary (all PROBED)

| condition | footer? | score_value returned |
|---|---|---|
| normal run, statements > 0 | yes | float (or int 0) |
| `--score=n` | **no** (display only is gated) | still computed & returned |
| `-E` (any `--score` value) | no (score forced off) | still computed & returned |
| `stats.statement == 0` (e.g. only E0001s) | no | `None` |
| nothing linted (all fatal / no FileItems) | no (`_is_base_filestate`) | `None` |
| eval raises | prints the "An exception occurred while rating: ..." section | `None` |
| errors present (E/F) but statements > 0 | **yes** (errors do NOT suppress) | float |

### 4.5 Score and reports interleaving

With `--reports=y` the report section is displayed first (own `_display` call
⇒ own leading `"\n"`), then the EvaluationSection (another `_display` ⇒
another leading `"\n"`). See §6 for the junction bytes.

### 4.6 Info messages and the score

The default evaluation has no `info` term ⇒ I messages don't lower the score.
PROBED: a run emitting only `I0021 useless-suppression` (with `--enable=…`)
printed `rated at 10.00/10` and exited **0** (`MSG_TYPES_STATUS["I"] == 0`
and `10.0 >= fail_under`).

---------------------------------------------------------------------------
## 5. Persistent stats (PYLINT_HOME pickle)

### 5.1 Location

`PYLINT_HOME` (constants.py:100-108): `os.environ["PYLINTHOME"]` if set, else
`DEFAULT_PYLINT_HOME = platformdirs.user_cache_dir("pylint")` (constants.py:50)
— on macOS `~/Library/Caches/pylint` (PROBED), on Linux `~/.cache/pylint`.

### 5.2 Filename derivation — `_get_pdata_path` (caching.py:18-26)

```python
underscored_name = "_".join(
    str(p.replace(":", "_").replace("/", "_").replace("\\", "_"))
    for p in base_name.parts          # pathlib parts of Path(base_name)
)
return pylint_home / f"{underscored_name}_{recurs}.stats"   # recurs is ALWAYS 1
```

**No python version in the filename** (the per-version split existed in older
pylints; 4.0.5 is exactly the above). PROBED mappings:

```
"pkg"              → pkg_1.stats
"."                → _1.stats          (Path(".").parts == ())
"pkg/sub/mod.py"   → pkg_sub_mod.py_1.stats
"/abs/path/pkg"    → __abs_path_pkg_1.stats   (parts ('/','abs',...) ; '/' → '_')
"C:\\x"            → C__x_1.stats
```

`base_name` = `linter.file_state.base_name` = the `modpath` (3rd member,
`basename` from `expand_modules`) of the **last linted FileItem**
(FileState ctor file_state.py:30-38 stores `modname` arg as `base_name`;
`_lint_file`/`_check_file` pass `file.modpath`). `expand_modules`
(expand_modules.py:85-186) sets `basename` = the top-level argument's
`modname` for the argument itself AND all submodules discovered under it
(expand_modules.py:144,182). So:
- `pylint mypkg` → every FileItem has basename `mypkg` → `mypkg_1.stats`.
- `pylint pkg/a.py pkg/b.py` → basenames `pkg.a`, `pkg.b`; the LAST linted
  file wins → `pkg.b_1.stats` (PROBED).
- `pylint .` → modname for "." resolves to `"."` → `_1.stats` (PROBED).

In parallel mode the merge loop overwrites `linter.file_state.base_name` with
each result's `base_name` (parallel.py:163), so the last input file likewise
wins.

### 5.3 Load/save semantics (caching.py:30-71)

- `load_results(base)`: path = `_get_pdata_path(base, 1, pylint_home)`; if
  missing → `None`. `pickle.load`; if the object is not a `LinterStats`,
  warns + treats as corrupt; **any Exception** → `None` (silent tolerance).
- `save_results(stats, base)`: `mkdir -p` PYLINT_HOME (failure → stderr note,
  continue), `pickle.dump(results, stream)` (default protocol; CPython 3.12
  default protocol = 4 — irrelevant for parity as long as WE can round-trip
  our own files, but interop with real pylint caches requires reading
  arbitrary-protocol pickles of `pylint.utils.linterstats.LinterStats`).
- Note `save_results` computes `data_file = _get_pdata_path(base, 1)` WITHOUT
  forwarding the `pylint_home` parameter — only the module-level default
  (frozen at import from `PYLINT_HOME`, i.e. PYLINTHOME env at process
  start). Harmless for the CLI; relevant only for embedding.

### 5.4 What is pickled

A plain `LinterStats` instance (linterstats.py:79-141); attribute dict:
`bad_names` (15-key dict), `by_module: dict[str, ModuleStats]`
(7-key dicts: convention/error/fatal/info/refactor/statement/warning),
`by_msg: dict[symbol, int]`, `code_type_count` (code/comment/docstring/empty/
total), `modules_names: set[str]`, `dependencies: dict[str, set[str]]`,
`duplicated_lines`, `node_count` (function/klass/method/module),
`undocumented`, plus scalars `convention,error,fatal,info,refactor,statement,
warning,skipped,global_note,nb_duplicated_lines,percent_duplicated_lines`.

Only `global_note` matters for the footer suffix; the full object matters for
`--reports=y` "previous/old number" columns (§6). PROBED pickle content after
a normal run: `global_note 7.5, statement 4, by_msg {'unused-variable': 1}`,
and `by_module` contains pseudo-modules `"Command line"` and
`"Command line or configuration file"` (and `""` keys when applicable) from
`set_current_module` calls during config init — replicate if you write
real pickles.

### 5.5 When saved

`generate_reports` (pylinter.py:1141-1142): only if `config.persistent` AND
something was linted. Saved AFTER `_report_evaluation` ran (so `global_note`
is current — except the eval-exception/zero-statement paths, §4.2). PROBED:
`--persistent=n` and `-E` never create the dir; `--score=n` still saves.

For the Rust port: writing a CPython-compatible pickle is required for
byte-identical `(previous run: ...)` output across interleaved
pylint/prylint runs; for self-consistency a Rust serde format would change
observable behavior only when real pylint reads our cache. Minimum viable:
implement pickle protocol-4 read/write of this one class shape
(GLOBAL `pylint.utils.linterstats LinterStats` + `__dict__` of
builtins — dicts/sets/strs/ints/floats only). `recurs` is always 1.

---------------------------------------------------------------------------
## 6. Reports system (default OFF) and its residual effects

### 6.1 Default-off, and what still runs

`reports=False` by default; `-E` also forces it off. Two touch points exist
even when off:

1. `prepare_checkers` (pylinter.py:588-598):
   ```python
   if not self.config.reports:
       self.disable_reporters()       # sets _reports_state[rid]=False for ALL
   needed_checkers = [self]
   for checker in self.get_checkers()[1:]:
       messages = {msg for msg in checker.msgs if self.is_message_enabled(msg)}
       if messages or any(self.report_is_enabled(r[0]) for r in checker.reports):
           needed_checkers.append(checker)
   ```
   With reports off, a checker is prepared iff it has ≥1 enabled message. In
   full mode every default checker has enabled messages, so the prepared-
   checker list (and hence callback order) equals "all checkers" — the
   walk-order extraction in notes/02 must be re-derived for the full set
   (the -E harness order omitted checkers that had no enabled E messages).
2. `generate_reports` constructs an empty `Section()` and skips
   `display_reports` for it; only the EvaluationSection is displayed. So in
   default full mode the output is exactly: messages + footer. **No other
   section machinery leaks into the output.**

`disable_report`/`enable_report` are also reachable via
`--disable=RP0001`-style ids (`_get_messages_to_set`,
message_state_handler.py:127-132: any msgid starting with `"rp"`
case-insensitively is routed to report state, returning no message defs).

### 6.2 Registered reports (full default set, PROBED via `report_order()`)

```
RP0101 basic        Statistics by type            (checkers/base/...)
RP0401 imports      External dependencies
RP0402 imports      Modules dependencies graph
RP0701 metrics      Raw metrics
RP0801 similarities Duplication
RP0001 main         Messages by category          (pylinter.py:344-353 self.reports)
RP0002 main         % errors / warnings by module
RP0003 main         Messages
```

`report_order` (pylinter.py:481-490): `sorted(self._reports, key=name)` with
the PyLinter itself ("main") moved to the END (pop+append). The dict
`_reports` is keyed by checker object in registration order; sorting is by
checker `name`.

### 6.3 `make_reports` (reports_handler_mix_in.py:65-83)

`Section("Report", f"{self.stats.statement} statements analysed.")`, then for
each enabled report id, `Section(r_title)`; callback fills it; on
`EmptyReportError` the section is skipped; `report_sect.report_id = reportid`.

Known EmptyReportError sources: RP0002 raises when `len(by_module) == 1`
(report_functions.py:53-55: "don't print this report when we are analysing a
single module" — note pseudo-modules like "Command line" count toward this
len!) and when no module row is nonzero (report_functions.py:84-85); RP0401/
RP0402 when no dependencies; RP0801 never (always prints).

**The `(RP000X)` id suffix never appears in text output**:
`display_reports` (base_reporter.py:54-62) appends ` ({report_id})` only on
the TOP-LEVEL layout it is handed, and `generate_reports` hands it the outer
"Report" section whose `report_id` is `""`. Sub-section ids are never
stamped. Verified in the probe output (titles plain: "Statistics by type",
"Raw metrics", ...).

### 6.4 Text rendering of report sections

`TextWriter.visit_section` (text_writer.py:37-43): leading `writeln()`,
children, trailing `writeln()`. `visit_title` underlines with
`TITLE_UNDERLINES[self.section]` = `["", "=", "-", "`", ".", "~", "^"]`
(text_writer.py:24): the outer "Report" gets `=`, each report sub-section
gets `-`. `visit_table` (text_writer.py:64-97): column widths =
max(cell)+1, rows rendered `|cell |…|` with `+---+` separators and `+===+`
after the header row when `rheaders`. `visit_paragraph`: text + `writeln()`.

Sample (PROBED, `-ry -j1`, abridged):

```
Report
======
45 statements analysed.

Statistics by type
------------------

+---------+-------+-----------+-----------+------------+---------+
|type     |number |old number |difference |%documented |%badname |
+=========+=======+===========+===========+============+=========+
|module   |9      |9          |=          |100.00      |0.00     |
...
```

Junction to the footer: the last table is followed by THREE blank lines
(table's trailing `writeln`, paragraph/section trailing `writeln`s, outer
section trailing `writeln`) and then the EvaluationSection's `_display`
leading newline + dashes.

"previous"/"old number"/"difference" columns use the loaded `previous_stats`
(same pickle as the score suffix); `diff_string` (utils/utils.py:84-88)
renders `=`, `+N.NN`, `-N.NN`. RP0003 ("Messages") sorts
`(value, msg_id)` tuples `reverse=True` and EXCLUDES symbols starting with
`"I"` (report_functions.py:31-43). RP0001 table columns:
type/number/previous/difference.

Reports are NOT a parity target initially (default off) — but note §9.4:
`-j` inflates `by_msg`, which is visible in RP0003.

---------------------------------------------------------------------------
## 7. Exit codes — full bitmask + fail-under/fail-on (VERIFIED)

### 7.1 `msg_status`

`MSG_TYPES_STATUS = {"I": 0, "C": 16, "R": 8, "W": 4, "E": 2, "F": 1}`
(constants.py:43). OR-ed in `_add_one_message` (pylinter.py:1245) only AFTER
the `is_message_enabled` displayed-check — suppressed messages contribute
nothing. I messages contribute 0.

### 7.2 The exit ladder (run.py:245-260) — VERBATIM

```python
if exit:
    if linter.config.exit_zero:
        sys.exit(0)
    elif linter.any_fail_on_issues():
        # We need to make sure we return a failing exit code in this case.
        # So we use self.linter.msg_status if that is non-zero, otherwise we just return 1.
        sys.exit(self.linter.msg_status or 1)
    elif score_value is not None:
        if score_value >= linter.config.fail_under:
            sys.exit(0)
        else:
            sys.exit(self.linter.msg_status or 1)
    else:
        sys.exit(self.linter.msg_status)
```

**VERIFIED rule: when a score exists, `score >= fail-under` exits 0 EVEN WITH
displayed messages.** Probe matrix (default flags unless noted):

| scenario | footer | exit |
|---|---|---|
| W present, fail-under 10 (default) | 7.50/10 | 4 (`msg_status or 1` → 4) |
| W present, `--fail-under=5` | 7.50/10 | **0** (messages displayed!) |
| W present, `--fail-under=5 --fail-on=W` | 7.50/10 | 4 (fail-on short-circuits) |
| `--exit-zero`, W present | printed | 0 |
| only I0021 displayed (statements>0) | 10.00/10 | 0 |
| only E0001 (0 statements ⇒ score None) | none | 2 |
| only F0001 (nothing linted ⇒ score None) | none | 1 |
| F0001 + lintable code | 0.00/10 | 5 (=F1|W4; 0.00 < 10) |
| C only (e.g. C0116) | 6.67/10 | 16 |
| R only (cyclic-import) | 8.33/10 | 8 |
| F0011 config-parse-error + C | printed | 17 (=16|1) |
| W0012 from bad `--disable` + C | printed | 20 (=16|4) |
| R0022 deleted-msg disable + C | printed | 24 (=16|8) |
| E0015 unrecognized option in rcfile + C | printed | 18 (=16|2) |

Since the default evaluation subtracts ≥ `1/statement*10` for ANY C/R/W and
`5/statement*10` for E (and forces 0 on fatal), any displayed C/R/W/E/F with
default `--fail-under=10` ⇒ score < 10 ⇒ nonzero exit = `msg_status`. The
`or 1` arm only matters when messages were counted but all carried status 0
(I-only) AND score < fail_under (possible only with custom
evaluation/fail-under) — then exit is 1.

### 7.3 `--fail-on`

`enable_fail_on_messages` (pylinter.py:509-538), called from
`_config_initialization` (config_initialization.py:139): values that are a
category letter (`val in MSG_TYPES`, i.e. exactly "I","C","R","W","E","F")
are category matches; everything else matches msgid OR symbol. msgid/symbol
matches are **`self.enable()`d** (even if disabled!) and appended to
`fail_on_symbols`; category matches are flagged only (every message of that
category gets its symbol appended; enablement unchanged).
`any_fail_on_issues` (pylinter.py:540-541):
`any(x in self.fail_on_symbols for x in self.stats.by_msg.keys())` — i.e.
keyed on EMITTED+DISPLAYED symbols (by_msg is only incremented for displayed
messages, pylinter.py:1248-1251).

### 7.4 Exit 32 and other special exits

- `--version`: print `full_version` (constants.py:60-62: three lines
  `pylint 4.0.5` / `astroid 4.0.4` / `Python <sys.version>`), exit 0
  (run.py:151-153).
- `ArgumentPreprocessingError` from `_preprocess_options` → stderr, exit 32
  (run.py:160-165).
- `--rcfile` pointing at a missing file: `OSError` from
  `_RawConfParser.parse_config_file` (config_file_parser.py:104-105) is NOT
  caught by `_ConfigurationFileParser` (only configparser/TOML errors are) →
  caught in `_config_initialization` (config_initialization.py:45-50) →
  printed, exit 32.
- argparse usage errors / unrecognized CLI option → `_arg_parser.error` →
  SystemExit caught → exit 32 (config_initialization.py:100-104); message
  `pylint: error: Unrecognized option found: not-an-option` on stderr
  (PROBED).
- "No files to lint: exiting." → 32 (§3.3).
- `jobs < 0` → stderr `Jobs number (-1) should be greater than or equal to
  0`, exit 32 (run.py:213-218; PROBED).
- `--output` open failure → exit 32 (run.py:236-238).
- `--enable=all --disable=all` together (either source, same list):
  `_order_all_first` raises `ArgumentPreprocessingError` which is NOT caught
  on this path → **Python traceback, exit 1** (PROBED; replicate exit code
  1 + the message `--enable=all and --disable=all are incompatible.` —
  byte-matching a traceback is out of scope, note as accepted divergence).

---------------------------------------------------------------------------
## 8. Config file discovery, parsing, precedence

### 8.1 Run order (run.py:144-244)

1. `--version` short-circuit.
2. `_preprocess_options` (config/utils.py:215-260) — scans raw argv for the
   table below, EXECUTES side effects, and REMOVES matched args. Matching is
   prefix-based to mimic argparse abbreviation (config/utils.py:188-211):

   | option | takes arg | match rule | effect |
   |---|---|---|---|
   | `--init-hook` | yes | prefix ≥ 8 chars (`--init-h`) | `exec(value)` immediately |
   | `--rcfile` | yes | prefix ≥ 4 (`--rc`) | `run._rcfile = value` |
   | `--output` | yes | exact only | `run._output = value` |
   | `--load-plugins` | yes | prefix ≥ 5 (`--loa`) | extend `run._plugins` |
   | `--verbose` | no | prefix ≥ 4 (`--ve`) | `run.verbose = True` |
   | `-v` | no | prefix ≥ 2 | verbose |
   | `--enable-all-extensions` | no | prefix ≥ 9 (`--enable-a`) | append every `pylint.extensions.*` module to plugins |

   `--opt=value` and `--opt value` both supported; missing value or value on
   a no-arg option → `ArgumentPreprocessingError` (exit 32).
3. If `_rcfile` unset: `self._rcfile = str(next(find_default_config_files(), None) or …)`
   (run.py:167-170) — FIRST yielded file only; **no merging of multiple
   config files**.
4. `PyLinter(...)` constructed; option defaults loaded via
   `parse_args([], self.config)` (`_load_default_argument_values`,
   arguments_manager.py:205-207); `load_default_plugins()` registers all
   checkers (registration-time disables, §3.1); CLI plugins loaded.
5. `_config_initialization(linter, args, reporter, config_file, verbose)`.

### 8.2 `find_default_config_files` (find_default_config_files.py:125-150) — exact yield order

```
1. _yield_default_files()  — cwd-relative names, in CONFIG_NAMES order:
     pylintrc, pylintrc.toml, .pylintrc, .pylintrc.toml,
     pyproject.toml, setup.cfg, tox.ini
   • a .toml candidate is skipped unless tomllib parse succeeds AND
     "pylint" in content.get("tool", []) (_toml_has_config:48-56; a TOML
     decode error PRINTS "Failed to load '<path>': <err>" to stdout and skips)
   • setup.cfg / tox.ini (.cfg/.ini suffixes) are skipped unless some section
     is "pylint" or startswith "pylint." (_cfg_or_ini_has_config:59-68)
   • plain `pylintrc`/`.pylintrc` need no content check
   • yielded paths are .resolve()d; OSError per-candidate swallowed
2. _find_project_config() (:88-100)  — only if cwd has __init__.py:
     curdir = resolved cwd
     while (curdir/"__init__.py").is_file():
         curdir = curdir.parent
         for rc_name in RC_NAMES:        # pylintrc, pylintrc.toml, .pylintrc, .pylintrc.toml
             yield if file                # NOTE: no [tool.pylint] content check here!
   i.e. ascend OUT of the package; check each ancestor dir that is itself
   "above" a package level. Loop continues while the *new* curdir still has
   __init__.py.
3. _find_pyproject() (:28-45): from resolved cwd ascend until a directory
   containing pyproject.toml is found, stopping the SEARCH at a dir with
   .git/.hg or filesystem root (the root dir itself is the last checked);
   yield it only if it is_file() and _toml_has_config().
4. _find_config_in_home_or_environment() (:103-122):
   if PYLINTRC env set AND exists AND is_file → yield it (and SKIP home);
   else: yield ~/.pylintrc, then ~/.config/pylintrc (skipped entirely when
   home is "~" or "/root" or unobtainable).
5. /etc/pylintrc if os.path.isfile.
```

PROBED: cwd `pylintrc` (with an arbitrary section name) beats
`pyproject.toml`; with only `pyproject.toml` (containing `[tool.pylint.*]`)
it is used.

### 8.3 File parsing (config_file_parser.py)

`parse_config_file` (:90-112): `os.path.expandvars` + `expanduser` on the
path; missing → OSError (→ exit 32 path, §7.4). Suffix `.toml` → TOML parser,
EVERYTHING else → INI parser.

INI (`parse_ini_file` :31-59):
- `configparser.ConfigParser(inline_comment_prefixes=("#", ";"))`, read with
  encoding `utf_8_sig` (strips BOM). configparser semantics apply:
  `key = value` or `key: value`, multi-line continuations, `%` interpolation
  (BasicInterpolation — `%%` escapes needed; an invalid `%` raises
  configparser.Error → F0011!), DEFAULT section merging.
- If the file is `setup.cfg` or `tox.ini` (matched via
  `"setup.cfg" in file_path.parts` — any path COMPONENT equal to it,
  :53-59), only sections named `pylint` or starting with `pylint.` are read;
  otherwise (pylintrc & friends) **every section is read regardless of
  name** (PROBED: `[WHATEVER SECTION NAME]` works).
- Output: `config_content[option] = value` (later duplicate option names
  across sections overwrite) and `options += [f"--{option}", value]`
  (duplicates PRESERVED in the arg list — for `disable`, every occurrence
  fires the action ⇒ accumulates).

TOML (`parse_toml_file` :62-87): `content["tool"]["pylint"]`; for each key:
if the value is a dict (i.e. `[tool.pylint.section]`), iterate ITS items;
else treat as a top-level option. Values flattened by
`_parse_rich_type_value` (config/utils.py:134-142): lists/tuples →
comma-join of recursive flattening; `re.Pattern` → pattern; dict →
`"k:v"` comma-joined; else `str(value)` (booleans become `"True"`/`"False"`
— accepted by the `yn`/store_true… actually `yn` accepts y/n/True/False via
`_yn_validator`). Each becomes `["--{name}", flattened]`.
Section names under `[tool.pylint.*]` are arbitrary (only used for grouping).
There is no E0014 emission anywhere in 4.0.5 — `bad-configuration-section`
is registered (pylinter.py:240-246) but DEAD (grep: no `add_message` site).
A stray nested dict value inside `[tool.pylint]` ends up flattened and then
rejected as an unrecognized option (PROBED: produced `E0015 ... found: x`).

Errors: `configparser.Error` / `TOMLDecodeError` → caught by
`_ConfigurationFileParser.parse_config_file` (:120-128) →
`add_message("config-parse-error", line=0, args=str(e))` → **F0011** under
module header `************* Module <str(config_file)>` with location
`<path>:1:0` (line 0 → rendered 1 via `line or 1`, pylinter.py:1277), and the
run continues WITHOUT config (PROBED, exit had F bit set).

### 8.4 `_config_initialization` (config_initialization.py:26-161) — exact sequence

```
linter.set_current_module(str(config_file) if config_file else "")
config_data, config_args = parse_config_file(...)
config_args = _order_all_first(config_args, joined=False)
if "init-hook" in config_data: exec(utils._unquote(config_data["init-hook"]))
if "load-plugins" in config_data: linter.load_plugin_modules(_splitstrip(...))
linter._parse_configuration_file(config_args)      # argparse parse_known_args into namespace
  → leftover "--xxx" tokens collected; raises _UnrecognizedOptionError
if reporter: linter.set_reporter(reporter)
linter.set_current_module("Command line")
args_list = _order_all_first(args_list, joined=True)
parsed_args_list = linter._parse_command_line_configuration(args_list)
parsed_args_list.remove("--") if present
any leftover -/-- tokens → _arg_parser.error(...) → exit 32
if unrecognized_options_message (from the CONFIG FILE):
    set_current_module(str(config_file)); add_message("unrecognized-option", line=0)   # E0015
overgeneral_exceptions sanity warnings (stderr UserWarning, not a message)
linter._emit_stashed_messages()                    # W0012 / R0022, see below
linter.set_current_module("Command line or configuration file")
linter.load_plugin_configuration()                 # may emit E0013 bad-plugin-value, line=0
linter.enable_fail_on_messages()
linter.pass_fail_on_config_to_color_reporter()
linter._parse_error_mode()                         # -E switch (§1)
linter._directory_namespaces[Path().resolve()] = (linter.config, {})
return [glob(arg, recursive=True) or [arg] for arg in parsed_args_list]  # flattened
```

Notes:
- Positional args are **recursive-globbed** (`glob(arg, recursive=True)`,
  :150-160); non-matching args pass through verbatim so the later F0001 can
  fire.
- `_order_all_first` (:164-206): within ONE arg list, any
  `--enable(=…)`/`--disable(=…)` whose csv contains a bare `all` is moved to
  the FRONT (stable otherwise). Applied separately to config-file args
  (`joined=False`, option and value are separate tokens) and to CLI args
  (`joined=True`, `--disable=all,...` form; NOTE the joined matcher only
  recognizes `--enable=`/`--disable=` prefixes — a CLI `--disable all`
  split-form is NOT reordered). Mixing `--enable=all` and `--disable=all`
  in the same list raises (§7.4). PROBED: `--enable=X --disable=all` on the
  CLI still emits X; same inside an rcfile in either textual order.

### 8.5 Stashed messages (W0012 / R0022)

`_DisableAction`/`_EnableAction` (callback_actions.py:349-409) call
`linter.disable/enable(msgid)` per csv item; `DeletedMessageError`/
`MessageBecameExtensionError` → stash `("useless-option-value", option_string, str(e))`,
`UnknownMessageError` → stash `("unknown-option-value", option_string, msgid)`,
keyed by `linter.current_name` at parse time (config file → `str(config_file)`
or `""`; CLI → `"Command line"`). `_emit_stashed_messages`
(pylinter.py:1346-1357) replays them: `set_current_module(modname)` then
`add_message(symbol, args=(option_string, value), line=0, confidence=HIGH)`.

PROBED renderings (note `line or 1` and `path = current_file = modname`):

```
************* Module Command line
Command line:1:0: W0012: Unknown option value for '--disable', expected a valid pylint message and got 'notamessage' (unknown-option-value)
Command line:1:0: R0022: Useless option value for '--disable', 'buffer-builtin' was removed from pylint, see https://github.com/pylint-dev/pylint/pull/4942. (useless-option-value)
```

These count fully toward stats/score/exit bits (exit 20 / 24 in probes).
Equivalent pragma-side handling already exists in notes/03; the CLI/rcfile
side must route through the same deleted/moved tables in
`pycheckers::msgstore`.

### 8.6 Precedence and the disable/enable merge — MECHANISM

There is no "merge policy" object; precedence is purely **parse order over a
single argparse namespace**:

1. defaults (`parse_args([])`),
2. config file args (`parse_known_args(config_args, self.config)`,
   arguments_manager.py:209-222),
3. CLI args (same, :224-234).

For ordinary store-options, later parses overwrite (CLI wins over file, file
wins over default). For `enable`/`disable` the argparse "value" is irrelevant
— the ACTIONS run `linter.enable/disable()` imperatively per occurrence, so
states ACCUMULATE across file and CLI in parse order, and
`_set_msg_status` (message_state_handler.py:142-167) afterwards REWRITES
`config.enable`/`config.disable` from scratch out of `_msgs_state`:

```python
# message_state_handler.py:158-167
self.linter.config.enable = []
self.linter.config.disable = []
for msgid_or_symbol, is_enabled in self._msgs_state.items():
    symbols = [m.symbol for m in self.linter.msgs_store.get_message_definitions(msgid_or_symbol)]
    if is_enabled: self.linter.config.enable += symbols
    else:          self.linter.config.disable += symbols
```

⇒ `config.disable` ends up as SYMBOLS in `_msgs_state` insertion order
(dict order: the 10 registration-time disables first, then file disables,
then CLI). PROBED semantics:
- file `disable=[A]` + CLI `--disable=B` → both disabled (merge/union);
- file `disable=[A]` + CLI `--enable=A` → A enabled (CLI later wins);
- file `disable=all` + CLI `--enable=X` → only X (+ the 11 main-checker
  messages, §3.3) enabled.

`disable`/`enable` value expansion (`_get_messages_to_set`,
message_state_handler.py:82-140) accepts: `all`, category letter or long
name (`W`/`warning`), checker name (`self.linter._checkers` keys), report id
(`rp...`), symbol, msgid, old msgid/old symbol (via
`get_message_definitions`). Unknown → exceptions per §8.5.

### 8.7 Per-directory namespaces (dormant)

`_directory_namespaces` gets exactly one entry (resolved cwd → base config).
`set_current_module` (pylinter.py:935-950) looks up the namespace for each
file path; files under cwd map to the base config; others leave `self.config`
unchanged. Net effect in 4.0.5: none. Port as a no-op.

---------------------------------------------------------------------------
## 9. `check_parallel` (`-j`)

### 9.1 Job-count handling (run.py:213-227, 103-124)

- `jobs < 0` → exit 32 (§7.4).
- `jobs == 0` → `_cpu_count()`: cgroup v2 `/sys/fs/cgroup/cpu.max` else
  cgroup v1 quota/shares (run.py:40-101), k8s 0→1; then
  `len(os.sched_getaffinity(0))` if available (not on macOS) else
  `multiprocessing.cpu_count()`; win32 cap 56; final = `min(share, count)`.
- `jobs > 1 or jobs == 0` with no `concurrent.futures` → fall back to 1
  (stderr note).
- `check()` (pylinter.py:695-705): parallel only when
  `not config.from_stdin and config.jobs > 1`; **no minimum file count** —
  `-j2` with one file still forks. `sys.path` snapshot/restore around it.

### 9.2 Protocol (parallel.py:124-173)

```python
with ProcessPoolExecutor(max_workers=jobs, initializer=initializer,
                         initargs=(dill.dumps(linter),)) as executor:
    linter.open()
    for (worker_idx, module, file_path, base_name, messages, stats,
         msg_status, mapreduce_data) in executor.map(_worker_check_single_file, files):
        linter.file_state.base_name = base_name
        linter.file_state._is_base_filestate = False
        linter.set_current_module(module, file_path)
        for msg in messages:
            linter.reporter.handle_message(msg)
        all_stats.append(stats)
        all_mapreduce_data[worker_idx].append(mapreduce_data)
        linter.msg_status |= msg_status
_merge_mapreduce_data(linter, all_mapreduce_data)
linter.stats = merge_stats([linter.stats, *all_stats])
```

- **Partition**: there is NO static partition. `executor.map(..., files)`
  with default `chunksize=1` feeds FileItems one-by-one to whichever worker
  is free (dynamic; worker assignment is timing-dependent and
  NON-deterministic).
- **Result order**: `Executor.map` yields results in INPUT order regardless
  of completion order ⇒ messages are handled (and module headers printed)
  in FileItem order — deterministic.
- `files` is the lazy `linter._iterate_file_descrs(...)` generator: the
  parent runs `expand_modules` (emitting F0001 "fatal" messages itself,
  before any worker output since the descriptor dict is built eagerly on
  first next()) and counts `stats.skipped`.
- Workers (`_worker_initialize` :38-61): linter deserialized via dill (one
  per worker process; macOS/Windows spawn ⇒ fresh interpreter), reporter
  replaced by `CollectingReporter`, `linter.open()`, dynamic plugins
  re-loaded with `force=True`, `_augment_sys_path(extra_packages_paths)`.
- Per file (`_worker_check_single_file` :64-98): `_worker_linter.open()`
  (resets the 7 per-category counters via `stats.reset_message_count()`,
  linterstats.py:328-336 — but NOT `by_msg`/`by_module`/`statement`), then
  `check_single_file_item(file_item)` = fresh
  `_astroid_module_checker` context per file (checkers `open()`ed,
  AST built IN the worker via `get_ast`, checked, checkers `close()`d,
  `stats.statement = walker.nbstatements` for that file). Returns
  `(id(current_process()), current_name, filepath, file_state.base_name,
  collected msgs, the worker's WHOLE stats object (pickled snapshot),
  msg_status, {checker_name: [map_data]})`, then resets the collecting
  reporter (but NOT msg_status or by_msg!).

### 9.3 Stats merging — quirks to replicate

`merge_stats` (linterstats.py:338-408) sums everything elementwise;
`by_module` entries are OVERWRITTEN per key (last snapshot wins — fine since
keys are per-module); `dependencies` set-unioned per key;
`merged.global_note += stat.global_note` (sums! workers never set it ⇒ 0).

**`by_msg` double-counting bug (VERIFIED):** worker `open()` does not reset
`by_msg`, and each per-file return pickles the worker's CUMULATIVE stats, so
a worker that checked files A then B returns A's counts twice. With
`-ry -j2` on an 11-file tree, RP0003 reported `missing-function-docstring
24` vs the true `6`, `unused-import 10` vs `3`, `syntax-error 3` vs `2`,
etc., and the inflation is timing-dependent (depends on which worker got
which files). The 7 category counters/score/`statement` are NOT affected
(reset per file; PROBED identical score 3.11 for `-j1`/`-j2`).
`any_fail_on_issues` only reads `by_msg` KEYS → unaffected.
Also: workers' `msg_status` snowballs across files within one worker (never
reset) — harmless because the parent ORs everything anyway.

`stats.modules_names` stays EMPTY in parallel mode (only `_lint_files` adds
to it; workers use `_check_file`) ⇒ `--verbose -jN` footer prints
`Checked 0 files/modules (), skipped ...` — wait: `checked_files_count` is
`node_count["module"]` (merged, correct); only the parenthesized list is
empty. (Not byte-relevant unless verbose.)

### 9.4 Map/reduce checkers

Only two checkers override `get_map_data`/`reduce_map_data`
(base_checker.py:222-227 defaults: None / no-op):

- **imports** (imports.py:492-512): map = `(import_graph, _excluded_edges)`
  per file (graph reset in `open()`, imports.py:461-476); reduce =
  rebuild graphs via `dict.update` per snapshot (NOTE: `update` REPLACES the
  value set per key — since each per-file graph keys mostly its own module
  this usually unions correctly, but two files importing the SAME module
  produce snapshots both keyed by their own names... edges keyed by importer
  so distinct; collisions only if the same importer appears in two snapshots
  — impossible per-file) **then calls `self.close()`** (imports.py:512) ⇒
  R0401 cyclic-import IS emitted in parallel mode, in the parent, after all
  per-file messages.
- **similarities** (symilar.py:862-874): map = linesets; reduce =
  `combine_mapreduce_data` + `self.close()` ⇒ R0801 duplicate-code emitted
  in the parent at reduce time.

Both are gated on `is_message_enabled` at map AND reduce time.

### 9.5 `-j1` vs `-jN` output differences — PROBED diff

Probe: package with 4 clean modules, 2 modules with messages, 2 syntax-error
files, 2 similar files, cyclic imports.

1. **Two-phase vs streaming E0001/F0002**: serial mode emits ALL parse-phase
   messages first (during `_get_asts`), then per-file messages. Parallel
   workers build the AST inside `check_single_file_item`, so E0001 appears
   at the file's position in file order. Diff observed:
   ```
   -j1: Module par.broken E0001 / Module par.bad E0001 / ...messages...
   -j2: ...d1 messages... / Module par.broken E0001 / ...d2 messages... / Module par.bad E0001
   ```
2. **R0801/R0401 attribution**: emitted via `add_message` with no node →
   `module = current_name`, `path = current_file`, line `0→1`. Serial:
   current module = last successfully LINTED module (broken files never call
   `set_current_module` in `_lint_files`). Parallel: parent's
   `set_current_module` is called for EVERY result INCLUDING broken files ⇒
   attribution lands on the last FileItem. Observed: R0801 under
   `par/m4.py:1:0` (`-j1`) vs `par/bad.py:1:0` (`-j2`).
   R0401 probe (cycle between two clean modules, last file clean in both
   orders): identical output `cyc/x.py:1:0: R0401: Cyclic import
   (cyc.x -> cyc.y)` for `-j1` and `-j2`.
3. **by_msg inflation** (reports only, §9.3) — and nondeterministic.
4. **Per-worker astroid caches**: each worker builds its own astroid module
   cache; messages depending on cross-module inference state accumulated
   from OTHER target files seen earlier in the run can differ. (Not observed
   in the small probe; expect divergence potential on big corpora — pylint
   itself documents `-j` as changing some results.)
5. Exit code and score: identical in probes (msg_status OR-ed; statement
   summed correctly).

**Port guidance**: prylint's rayon pipeline with ordered flush already
matches `-j1` semantics (two-phase E0001 ordering). DO NOT model prylint's
parallelism on `check_parallel` — pylint's own `-jN` output is NOT
`-j1`-identical, so the parity target stays `-j1` ground truth; treat
`-jN` reproduction as out of scope unless explicitly requested (then the
items above are the spec).

---------------------------------------------------------------------------
## 10. Pipeline-level messages owned by the main checker (full-mode census)

For each MSGS entry (pylinter.py:103-252): template, default state, emission
sites in full mode. (Scope is `WarningScope.LINE` for all of them.)

| id | symbol | default | emission sites (full mode) |
|---|---|---|---|
| F0001 | fatal | on | expand_modules errors (pylinter.py:929-934: `message = str(error["ex"]).replace(os.getcwd() + os.sep, "")` when key=="fatal"); non-Astroid exception in `_lint_files` (pylinter.py:797-800, args=crash msg) |
| F0002 | astroid-error | on | `_get_asts` AstroidBuildingError fallback (pylinter.py:748-757); AstroidError during lint (pylinter.py:792-796). Template `"%s: %s"` args (filepath, crash-template msg). Crash-file path is wall-clock (`pylint-crash-%Y-...`) |
| F0010 | parse-error | on | `get_ast` AstroidSyntaxError-with-non-SyntaxError-cause path (notes/02) |
| F0011 | config-parse-error | on | `_ConfigurationFileParser.parse_config_file` (config_file_parser.py:125-128), line=0, module=str(config_file) (§8.3) |
| I0001 | raw-checker-failed | OFF | `check_astroid_module` when `not node.pure_python` (pylinter.py:1090-1091) — unreachable for .py targets |
| I0010 | bad-inline-option | OFF | `process_tokens` InvalidPragmaError (message_state_handler.py:432-436) |
| I0011 | locally-disabled | OFF | `_set_one_msg_status` on every module/line-scope disable (message_state_handler.py:74-78) |
| I0013 | file-ignored | OFF | `process_tokens` skip-file pragma (message_state_handler.py:357-366, 392-400) |
| I0020 | suppressed-message | OFF | `iter_spurious_suppression_messages` (file_state, notes/03) after each module (pylinter.py:826-830) |
| I0021 | useless-suppression | OFF | same iterator (needs checkers' `_ignored_msgs` bookkeeping + INCOMPATIBLE_WITH_USELESS_SUPPRESSION constants.py:88-99) |
| I0022 | deprecated-pragma | OFF | `process_tokens` for `disable-all`/`disable=all`/`disable-msg`/`enable-msg` (message_state_handler.py:359-364, 372-381, 396-400) |
| I0023 | use-symbolic-message-instead | OFF | ByIdManagedMessagesChecker.process_module (misc.py:41-49): for every `_by_id_managed_msgs` entry with `mod_name == node.name`, args=`f"'{msgid}' is cryptic: use '# pylint: {verb}={symbol}' instead"`; list CLEARED after each module. Entries are registered by `_register_by_id_managed_msg` (message_state_handler.py:169-185) whenever enable/disable is called with a NUMERIC id (CLI, rcfile, or pragma) |
| E0001 | syntax-error | on | get_ast (notes/02 forms) |
| E0011 | unrecognized-inline-option | on | process_tokens UnRecognizedOptionError (message_state_handler.py:425-429) |
| W0012 | unknown-option-value | on | pragmas (message_state_handler.py:417-422) AND stashed CLI/rcfile values (§8.5); old_names E0012 |
| R0022 | useless-option-value | on | pragmas (message_state_handler.py:404-415) AND stashed (§8.5); old_names E0012 |
| E0013 | bad-plugin-value | on | load_plugin_configuration (pylinter.py:654-657), line=0, module="Command line or configuration file" |
| E0014 | bad-configuration-section | on | **DEAD — no emission site in 4.0.5** |
| E0015 | unrecognized-option | on | _config_initialization for config-FILE unknowns only (config_initialization.py:107-112), line=0, module=str(config_file); CLI unknowns exit 32 instead |

These are the messages prylint's pipeline shell owns directly (cross-check
msgs.rs: all 19 present). The remaining 370 belong to the ported checkers
(notes/05/06/08/09-*).

---------------------------------------------------------------------------
## 11. Port plan notes + msgs.rs cross-check

- **msgs.rs**: 389 messages — count matches the live store. Required
  additions for full mode: per-message `default_enabled` (10 false, §3.2)
  and min/max version (E0106 max (3,3); W1502 max (3,5); plus the
  non-gating ones if we want `--py-version` generality: stdlib.py:583 max
  (3,15) message, basic_error_checker.py:261 min (3,6), async_checker.py:33/40
  min (3,5)). Regenerate via `harness/gen_msgs_rs.py` reading
  `MessageDefinition.default_enabled/minversion/maxversion`.
- **MsgState seeding** (crates/cli/msgstate.rs): full mode = seed
  `_msgs_state` with the 10 registration-time disables (in registration
  order) + py-gates, then apply rcfile args then CLI args through the SAME
  enable/disable expansion used for pragmas (already in
  pycheckers::msgstore), including deleted/moved → stashed W0012/R0022
  replay (§8.5) and the `all` filter + `_order_all_first` reordering.
- **config.disable/enable mirrors** must be maintained (symbol lists in
  `_msgs_state` insertion order) because run.py's exit-32 check and
  `--fail-on`'s use of `config.fail_on` read them.
- **Reporter**: footer = `"\n" + dashes + "\n" + msg + "\n\n"` appended after
  the message stream; gate on (score && !error_mode && statements>0 &&
  something_linted). Exit ladder verbatim (§7.2).
- **Stats**: track the 7 category counters + `statement` (walker statement
  count — count nodes with `is_statement` during OUR walk; astroid
  `is_statement` = node is a Statement subclass; verify equivalence on
  corpora) + `by_msg` + `global_note`; persist per §5 (pickle codec needed
  for cross-tool parity; PYLINTHOME override is how the harness isolates).
- **Walk order**: re-extract prepared-checker callback order with ALL
  checkers enabled (the -E harness dump excluded checkers that had no
  enabled messages; `prepare_checkers` includes every checker in full mode).
- **Ground truth**: regenerate corpora outputs without `-E` (and with
  `PYLINTHOME` pointed at an empty dir per run so the footer has no
  `(previous run: ...)` suffix — or two-run protocol to ALSO pin the suffix
  path).

---------------------------------------------------------------------------
## 12. Open questions

1. `statement` parity: pylint counts statements during the checker walk
   (`ASTWalker.nbstatements`); prylint must count the same node set in its
   walk — needs a corpus diff of the final score denominator (any
   discrepancy shifts every score).
2. Pickle interop: do we need to READ caches written by real pylint
   (arbitrary pickle protocol / memoization) or only round-trip our own?
   Current plan assumes self-round-trip + best-effort read; corrupted/foreign
   files harmlessly behave as "no previous run" (matches pylint's
   swallow-everything `load_results`).
3. `-jN` byte parity is declared out of scope (pylint's own `-jN` ≠ `-j1`,
   with nondeterministic report tables); confirm with the user that the
   prylint CLI should accept `-j` and keep producing `-j1`-identical output.
4. Verbose footer's `modules_names` set-iteration order under
   `PYTHONHASHSEED=0` — only matters if `--verbose` becomes a parity target.
5. `--enable=all --disable=all` traceback (exit 1) — replicate exit code +
   stderr message only, not the Python traceback bytes?
6. configparser edge semantics for exotic rcfiles (interpolation `%`,
   DEFAULT section inheritance, multi-line continuation) — port via a
   configparser-faithful INI reader; needs differential tests on corpus
   rcfiles actually encountered.
