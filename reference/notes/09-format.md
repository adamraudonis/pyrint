# 09 — format.py / misc.py / non_ascii_names.py / unicode (C2503) / dunder_methods / threading / lambda_expressions / nested_min_max — exact spec (pylint 4.0.5, full-pylint mode)

Sources (all paths relative to `reference/pylint/pylint/` unless noted):

- `checkers/format.py` (733 lines) — FormatChecker (token + raw + AST `visit_default`)
- `checkers/misc.py` (192) — EncodingChecker (W0511 fixme + encoding), ByIdManagedMessagesChecker (I0023)
- `checkers/non_ascii_names.py` (174) — NonAsciiNameChecker (C2401/W2402/C2403)
- `checkers/unicode.py` (537) — UnicodeChecker; E-codes already ported (notes/08 §9,
  `crates/pycheckers/src/unicode.rs`); this doc covers the full-mode delta: **C2503**
- `checkers/dunder_methods.py` (102) — DunderCallChecker (C2801)
- `checkers/threading_checker.py` (59) — ThreadingChecker (W2101)
- `checkers/lambda_expressions.py` (94) — LambdaExpressionChecker (C3001/C3002)
- `checkers/nested_min_max.py` (176) — NestedMinMaxChecker (W3301)

All behaviors below verified against the pinned venv (`.venv-pylint`, pylint 4.0.5 /
astroid 4.0.4 / CPython 3.12.12). Empirical probes are marked **[probe]** with the exact
observed output. Runtime: pylint's tokens come from CPython 3.12's `tokenize` module
(C-tokenizer based) — the Rust port must match its token stream semantics (NL vs NEWLINE,
INDENT/DEDENT, multi-line STRING `line` attribute, synthetic trailing NEWLINE with empty
string, ENDMARKER row placement, `\r`-as-line-terminator handling).

---

## 0. Pipeline mechanics shared by everything in this doc

### 0.1 Per-module check order (pylinter.py:1062-1106 `_check_astroid_module`)

```python
try:
    tokens = utils.tokenize_module(node)        # pylint/utils/utils.py:151-154:
except tokenize.TokenError as ex:               #   with node.stream() as stream:
    self.add_message("syntax-error",            #       return list(tokenize.tokenize(stream.readline))
        line=ex.args[1][0], col_offset=ex.args[1][1],
        args=ex.args[0], confidence=HIGH)
    return None                                  # NOTHING else runs for this module
...
self.process_tokens(tokens)                       # linter pragma scan (notes/03) — emits E0011/W0012/
if self._ignore_file:                             #   R0022/I0013/I0022 etc FIRST, sets _pragma_lineno
    return False                                  # disable=all / skip-file: raw+token+walk all skipped
for raw_checker in rawcheckers:                   # pylinter.py:1100-1101  ← RAW FIRST
    raw_checker.process_module(node)
for token_checker in tokencheckers:               # pylinter.py:1102-1103  ← THEN TOKEN
    token_checker.process_tokens(tokens)
walker.walk(node)                                 # pylinter.py:1105       ← THEN AST WALK
```

`tokenize_module` uses **bytes** `tokenize.tokenize` → the token list starts with an
ENCODING token at `start=(0,0)` (matters for FormatChecker's line-iteration seed, §1.3).

E0001-adjacency: if astroid parse already failed, the module never reaches
`_check_astroid_module` (two-phase pipeline, notes/02); if astroid parse succeeded but
`tokenize` raises TokenError, the tokenize-form E0001 is emitted and **no raw/token/walk
checks run**. prylint's shell already implements both paths.

### 0.2 Which checkers are in the raw/token lists, and in what order

`_astroid_module_checker` (pylinter.py:966-991): `_checkers = self.prepare_checkers()`,
then `tokencheckers = [c for c in _checkers if isinstance(c, BaseTokenChecker)]` and
`rawcheckers = [c for c in _checkers if isinstance(c, BaseRawFileChecker)]` — list order
= prepared order. PyLinter itself is **not** a BaseTokenChecker/BaseRawFileChecker
(pylinter.py:258-263, inherits plain `checkers.BaseChecker`), so it is in neither list.

`prepare_checkers` (pylinter.py:588-598): `needed_checkers = [self]` + every checker from
`get_checkers()[1:]` that has **at least one config-enabled message**:

```python
messages = {msg for msg in checker.msgs if self.is_message_enabled(msg)}
if messages or any(self.report_is_enabled(r[0]) for r in checker.reports):
    needed_checkers.append(checker)
```

`get_checkers` (pylinter.py:574-576) = `sorted(...)` over registration; sort key via
`BaseChecker.__gt__` (base_checker.py:54-69): main first, then builtin checkers
(module starts with `"pylint.checkers"`) alphabetically **by lowercased `name`**, then
extension checkers alphabetically. Equal names keep registration order (timsort stable;
within `misc.py::register` EncodingChecker is registered before ByIdManagedMessagesChecker,
misc.py:190-192). Checker `name` is lowercased in `__init__` (base_checker.py:49-50) —
NonAsciiNameChecker's declared `name = "NonASCII-Checker"` becomes `"nonascii-checker"`.

Resulting default full-mode order of the lists relevant here:

- rawcheckers: `format` (FormatChecker — `process_module` is `pass`, format.py:272-273) →
  `miscellaneous` (EncodingChecker) → [`miscellaneous` (ByIdManagedMessagesChecker) — only
  if I0023 enabled; it is default-OFF so normally absent] → `unicode_checker`.
- tokencheckers: `format` (FormatChecker) → `miscellaneous` (EncodingChecker) →
  `refactoring` (RefactoringChecker, out of scope here) → `spelling` (SpellingChecker,
  no-op without a spelling dict).

**[probe]** latin-1 file with fixme(line2)+long line(line3)+trailing ws(line4):

```
ord1.py:1:0: C2503: PEP8 recommends UTF-8 ... (bad-file-encoding)      ← raw (unicode_checker)
ord1.py:3:0: C0301: Line too long (106/100) (line-too-long)            ← token (format)
ord1.py:4:5: C0303: Trailing whitespace (trailing-whitespace)          ← token (format)
ord1.py:2:1: W0511: TODO: first (fixme)                                ← token (miscellaneous)
```

So within a module the emission (= output) order is: linter pragma messages → raw
checkers in name order → token checkers in name order (each checker's own messages in its
internal scan order) → walk messages in traversal order.

### 0.3 Four enablement gates (matters for arbitrary `--disable` lists)

1. **Checker-level** (prepare_checkers): if every message of a checker is config-disabled,
   the checker is dropped — its `process_tokens`/`process_module`/visit callbacks never
   run, and inline `# pylint: enable=` can NOT resurrect its messages.
   E.g. `--disable=fixme` drops EncodingChecker entirely.
2. **Walk-method-level** (ast_walker.py:37-40, 52-56): visit/leave methods carrying
   `checks_msgs` (set by `@only_required_for_messages(...)`, checkers/utils.py) are only
   registered if `any(is_message_enabled(m) for m in checks_msgs)` at prepare time
   (config-level, no line). EXCEPTION: `visit_default` is registered **without** this
   check (ast_walker.py:64-69) — FormatChecker's C0321 callback runs for every node even
   when `multiple-statements` is config-disabled (output still suppressed at gate 3, but
   an inline `# pylint: enable=multiple-statements` CAN resurrect C0321, unlike messages
   behind a gated specific visit method).
3. **add_message line-level** (pylinter.py:1233-1241): standard pragma/block state
   (msgstate.rs port). Suppressed messages go to `handle_ignored_message`.
4. **C0301's bespoke `checker_off`** path (§1.7) — affects only useless-suppression
   bookkeeping, not visible output.

`default_enabled: False` messages (here: I0023) are implemented as a plain config
`disable(msgid)` at registration (pylinter.py:500-504) — i.e. exactly like a CLI
`--disable=I0023` that an explicit `--enable` can override.

### 0.4 Message position computation (pylinter.py:1195-1230, 1268-1281)

For `add_message(... node=N)`: if `N.position` is set (only FunctionDef/ClassDef keyword
spans), missing line/col/end_* are taken from it, else from
`N.fromlineno/.col_offset/.end_lineno/.end_col_offset`. Note the guards are `if not line:`
etc. — falsy 0 values get overwritten too. The final MessageLocationTuple applies
`line or 1` and `col_offset or 0` (pylinter.py:1277-1278) — a Module node (fromlineno 0)
reports at `1:0`. For `add_message(line=L)` without node: col is `col_offset or 0`,
module/abspath from `current_name`/`current_file`.

Message text: `msg = message_definition.msg; if args is not None: msg %= args`
(pylinter.py:1253-1255) — plain `%` formatting (tuple or scalar).

### 0.5 Message scope LINE vs NODE

`create_message_definition_from_tuple` (base_checker.py:182-207): default scope is
`WarningScope.LINE` if the **checker** is a BaseTokenChecker/BaseRawFileChecker, else
`WarningScope.NODE`; a 4th msgs-tuple element can override (`{"scope": WarningScope.NODE}`).
Hence: all FormatChecker messages are LINE-scoped **except C0321** (explicit NODE,
format.py:96); W0511/I0023 LINE; all messages of the pure-AST checkers in this doc
(C2401/W2402/C2403, C2801, W2101, C3001/C3002, W3301) are NODE-scoped. NODE scope changes
pragma block expansion in `FileState._set_state_on_block_lines` (file_state.py:56-162,
already ported in `cli/msgstate.rs`): a `# pylint: disable=<node-scoped-msg>` inside a
block applies from the enclosing block's start line (with the firstchildlineno / re-visit
subtleties quoted in file_state.py:102-160). `node_scope` flags in
`crates/pycheckers/src/msgs.rs` already encode this and match this doc's inventory.

---

## 1. FormatChecker — checkers/format.py

Checker `name = "format"` (format.py:157), inherits both BaseTokenChecker and
BaseRawFileChecker (format.py:147); `process_module` is a no-op (format.py:272-273) — it
exists only so the checker also counts as raw (historic). All real work is in
`process_tokens` (token phase) and `visit_default` (walk phase).

### 1.1 Messages owned (format.py:54-114)

| id    | symbol                        | template                                                  | scope | conf  |
|-------|-------------------------------|-----------------------------------------------------------|-------|-------|
| C0301 | line-too-long                 | `Line too long (%s/%s)`                                   | LINE  | UNDEF |
| C0302 | too-many-lines                | `Too many lines in module (%s/%s)`                        | LINE  | UNDEF |
| C0303 | trailing-whitespace           | `Trailing whitespace`                                     | LINE  | HIGH  |
| C0304 | missing-final-newline         | `Final newline missing`                                   | LINE  | UNDEF |
| C0305 | trailing-newlines             | `Trailing newlines`                                       | LINE  | UNDEF |
| W0311 | bad-indentation               | `Bad indentation. Found %s %s, expected %s`               | LINE  | UNDEF |
| W0301 | unnecessary-semicolon         | `Unnecessary semicolon`                                   | LINE  | UNDEF |
| C0321 | multiple-statements           | `More than one statement on a single line`                | NODE  | HIGH  |
| C0325 | superfluous-parens            | `Unnecessary parens after %r keyword`                     | LINE  | UNDEF |
| C0327 | mixed-line-endings            | `Mixed line endings LF and CRLF`                          | LINE  | UNDEF |
| C0328 | unexpected-line-ending-format | `Unexpected line ending format. There is '%s' while it should be '%s'.` | LINE | UNDEF |

All default-enabled. NOTE C0325 uses `%r` → keyword rendered with repr quotes:
`Unnecessary parens after 'if' keyword`. Related deleted ids (pragma references produce
R0022 useless-option-value via the deleted-ids table, already ported): W0312
mixed-indentation, C0330 bad-continuation (_deleted_message_ids.py:99,113).

Dead code: format.py:442-443

```python
if tok_type == tokenize.NUMBER and string.endswith("l"):
    self.add_message("lowercase-l-suffix", line=line_num)
```

`"lowercase-l-suffix"` is not a registered message (raises UnknownMessageError if reached)
but the branch is unreachable on CPython 3.12: **[probe]** `0xal`, `10l`, `1_0l` all
tokenize as NUMBER followed by a separate NAME `l`. Omit from the port.

### 1.2 Options (format.py:162-254) — defaults that gate behavior

| option                        | default                       | used by |
|-------------------------------|-------------------------------|---------|
| max-line-length               | `100` (int)                   | C0301 |
| ignore-long-lines             | regex `^\s*(# )?<?https?://\S+>?$` | C0301 (this IS the "URL handling": a long line consisting solely of optional indent, optional `# `, optional `<`, `http://`/`https://` + non-space run, optional `>` is exempt). Matching uses `regex.search(line)` on the **rstripped** line — but since the pattern is `^...$`-anchored without MULTILINE, search == fullmatch of the line. |
| single-line-if-stmt           | `False` (yn)                  | C0321 exemption |
| single-line-class-stmt        | `False` (yn)                  | C0321 exemption |
| max-module-lines              | `1000` (int)                  | C0302 |
| indent-string                 | `"    "` (non_empty_string)   | W0311 |
| indent-after-paren            | `4` (int)                     | **unused** by any 4.0.5 code path (legacy; keep as accepted option only) |
| expected-line-ending-format   | `""` (choice of ``""``/`LF`/`CRLF`) | C0328 (check inactive when empty — the default) |

### 1.3 `process_tokens` line-iteration model (format.py:380-469)

State: `indents=[0]`, `check_equal=False`, `line_num=0`, `self._lines={}`,
`self._visited_lines={}`, `self._last_line_ending=None`, `last_blank_line_num=0`
(all reset per module — no leakage of C0327 state across files).

Per token `(tok_type, string, start, _, line)` at index `idx`:

```python
if start[0] != line_num:                  # token starts on a new row
    line_num = start[0]
    if tok_type == tokenize.INDENT:
        self.new_line(TokenWrapper(tokens), idx - 1, idx + 1)   # line content from NEXT token
    else:
        self.new_line(TokenWrapper(tokens), idx - 1, idx)
```

- The first token is ENCODING at row 0 → `line_num` stays 0 until the first real token at
  row 1 triggers `new_line` (so `idx-1` = ENCODING index; `_last_token_on_line_is` guards
  `line_end > 0` handle it).
- INDENT special case (comment at format.py:396-399): "if an indented line contains a
  multi-line docstring, the line member of the INDENT token does not contain the full
  line; therefore we check the next token on the line" → `line_start = idx+1`. This also
  means `tokens.type(line_start)` is the docstring's STRING type → trailing-whitespace is
  skipped inside it (§1.5).
- Rows that contain no token *start* are never visited: physical lines strictly inside a
  multi-line string never get a `new_line` call (verified: trailing whitespace on an
  interior string line is NOT flagged, §1.5 probe).
- ENDMARKER (row = last line + 1, `line=""`) triggers a final `new_line` whose
  `specific_splitlines("")` is `[]` → no checks.

Then the `match tok_type` state machine (format.py:405-440):

```python
case tokenize.NEWLINE:
    check_equal = True
    self._check_line_ending(string, line_num)      # §1.8
case tokenize.INDENT:
    check_equal = False
    self.check_indent_level(string, indents[-1] + 1, line_num)   # §1.9
    indents.append(indents[-1] + 1)
case tokenize.DEDENT:
    check_equal = True
    if len(indents) > 1:
        del indents[-1]
case tokenize.NL:
    if not line.strip("\r\n"):                     # ONLY \r\n stripped: "   \n" is NOT blank
        last_blank_line_num = line_num
case tokenize.COMMENT | tokenize.ENCODING:
    pass                                            # do not consume check_equal
case _:
    if check_equal:                                 # first concrete token of next statement
        check_equal = False
        self.check_indent_level(line, indents[-1], line_num)   # NOTE: whole raw line string
```

Then per token, regardless of row change (format.py:442-446): the dead NUMBER/`l` branch,
and `if string in _KEYWORD_TOKENS: self._check_keyword_parentheses(tokens, idx)` (§1.10).
`_KEYWORD_TOKENS` (format.py:34-50) = `{assert, del, elif, except, for, if, in, not,
raise, return, while, yield, with, "=", ":="}`.

After the loop (format.py:448-469):

```python
line_num -= 1                                       # "to be ok with wc -l" (ENDMARKER row - 1)
if line_num > self.linter.config.max_module_lines:  # C0302, see §1.11
    ...
if line_num == last_blank_line_num and line_num > 0:
    self.add_message("trailing-newlines", line=line_num)   # C0305, §1.12
```

### 1.4 `new_line` (format.py:261-270) — W0301 + per-line checks

```python
def new_line(self, tokens, line_end, line_start):
    if _last_token_on_line_is(tokens, line_end, ";"):
        self.add_message("unnecessary-semicolon", line=tokens.start_line(line_end))
    line_num = tokens.start_line(line_start)
    line = tokens.line(line_start)
    if tokens.type(line_start) not in _JUNK_TOKENS:           # {COMMENT, NL} (format.py:51)
        self._lines[line_num] = line.split("\n")[0]           # first physical line only
    self.check_lines(tokens, line_start, line, line_num)
```

`_last_token_on_line_is` (format.py:117-122): with `line_end = idx-1` (index of the
**last token of the previous row**, usually NEWLINE/NL):

```python
return (line_end > 0 and tokens.token(line_end - 1) == token) or (
    line_end > 1 and tokens.token(line_end - 2) == token
    and tokens.type(line_end - 1) == tokenize.COMMENT)
```

i.e. W0301 when the token just before the row-ending token is `;`, or `;` followed by a
COMMENT. Reported at `line=tokens.start_line(line_end)` = the row of the previous row's
last token, col 0. Only the statement-final semicolon counts (a `;` separating two
statements mid-line is not "last on line"). **[probe]**: `x = 1;` → `2:0 W0301`;
`y = 2 ;  # comment` → `3:0 W0301`; `z = 3; w = 4;` → single `4:0 W0301`.

`self._lines` (used only by visit_default's dead code, §1.13) stores the first physical
line of each row whose first token isn't COMMENT/NL.

### 1.5 `check_lines` (format.py:651-705) — C0304 + C0303 dispatch + C0301

`lines` is the raw `token.line` of the first token on the row — a single physical line
for normal tokens, the ENTIRE multi-line source for a STRING token that starts the row.
`lineno` = that token's start row.

```python
split_lines = self.specific_splitlines(lines)            # §1.5a
for offset, line in enumerate(split_lines):
    if not line.endswith("\n"):
        self.add_message("missing-final-newline", line=lineno + offset)
        continue                                          # skips C0303 for this line
    if tokens.type(line_start) != tokenize.STRING:        # no C0303 inside strings (#6936/#3822)
        self.check_trailing_whitespace_ending(line, lineno + offset)

potential = any(len(line) > max_chars for line in split_lines)   # raw len incl. newline chars
if not potential: return                                          # fast path

mobj = OPTION_PO.search(lines)                            # FIRST pylint pragma in the chunk
checker_off = False
if mobj:
    if not self.is_line_length_check_activated(mobj):     # §1.7
        checker_off = True
    lines = self.remove_pylint_option_from_lines(mobj)    # pragma text excluded from length
for offset, line in enumerate(self.specific_splitlines(lines)):
    self.check_line_length(line, lineno + offset, checker_off)
```

C0304 notes:
- Fires for any physical line in the chunk that doesn't end with `"\n"` — normally only
  the true last line of a file lacking a final newline (`5:0 C0304`-style, col 0).
- `\r\n` endswith `\n` → fine. **`\r`-only (old-Mac) endings**: CPython 3.12's tokenizer
  treats lone `\r` as a row terminator and `str.splitlines` splits on it, so EVERY
  `\r`-terminated line is flagged. **[probe]** `'"""doc"""\rx = 1\r'` →
  `1:0 C0304` and `2:0 C0304`.

C0303 `check_trailing_whitespace_ending` (format.py:581-591):

```python
stripped_line = line.rstrip("\t\n\r\v ")          # NOTE: \f (formfeed) NOT stripped
if line[len(stripped_line):] not in ("\n", "\r\n"):
    self.add_message("trailing-whitespace", line=i, col_offset=len(stripped_line),
                     confidence=HIGH)
```

- col_offset = length of the kept prefix. **[probe]** `"x = 1 \n"` → col 5;
  `"y = 2\t\n"` → col 5; `"w = 4   # c \n"` → col 11.
- `"z = 3\x0c\n"` → remainder is `"\n"` → NOT flagged (formfeed protected).
- Whitespace-only line `"   \n"` → stripped `""`, remainder `"   \n"` → flagged at col 0.
  **[probe]** `3:0 C0303`.
- Skipped when the row's first token is a STRING — but trailing spaces on the FIRST line
  of `s = """a \n…` ARE flagged (first token of the row is NAME `s`): **[probe]**
  `5:8 C0303` while the interior line `b \n` of the same string emits nothing (interior
  rows never get a `new_line`).

#### 1.5a `specific_splitlines` (format.py:626-649)

Splits like `str.splitlines(keepends=True)` but re-merges line breaks in
`unsplit_ends = {"\x0b","\x0c","\x1c","\x1d","\x1e","\x85"," "," "}` (i.e. only
`\n`, `\r`, `\r\n` are real boundaries):

```python
res, buffer = [], ""
for atomic_line in lines.splitlines(True):
    if atomic_line[-1] not in unsplit_ends:
        res.append(buffer + atomic_line); buffer = ""
    else:
        buffer += atomic_line
return res
```

**Buffer-drop quirk**: if the final atomic line ends with an unsplit char (e.g. the file's
last line ends with `\x0c` and has no newline), the buffer is never flushed — that line
silently vanishes from all checks. **[probe]** `b'"""doc"""\nx = 1\x0c'` → NO C0304 at all.
Replicate exactly.

### 1.6 C0301 line-too-long (`check_line_length`, format.py:593-602)

```python
max_chars = config.max_line_length            # 100
line = line.rstrip()                          # full Python str.rstrip (all whitespace incl. \x0c)
if len(line) > max_chars and not config.ignore_long_lines.search(line):
    if checker_off:
        self.linter.add_ignored_message("line-too-long", i)
    else:
        self.add_message("line-too-long", line=i, args=(len(line), max_chars))
```

- args = (rstripped length, max). Length is in CHARACTERS of the decoded source line.
- Lines inside multi-line strings ARE length-checked when the string starts the row
  (offset attribution): **[probe]** 105-char line inside a module docstring →
  `2:0 C0301 (105/100)`.
- `add_ignored_message` (pylinter.py:1321-1344) only records into
  `FileState._ignored_msgs` for useless-suppression — no visible output. prylint can
  treat `checker_off` as plain suppression until I0021 machinery exists.

### 1.7 C0301 ↔ pragma interplay (format.py:604-624, 694-705)

`OPTION_PO` (utils/pragma_parser.py:14-27, already regex-exact in `cli/pragma.rs`):
group(1) = `(\#.*?\bpylint:\s*([^;#\n]+))`, group(2) = the message list part. Note
group(1) ends right after group(2); the optional trailing `[;#]` and anything after it
(e.g. `; reason text`) are NOT part of group(1).

`is_line_length_check_activated` (format.py:614-624): parse `mobj.group(2)` with
`parse_pragma`; return False (→ `checker_off=True`) iff any pragma has
`action == "disable"` and `"line-too-long" in pragma.messages` (raw string match —
**`disable=C0301` does NOT set checker_off**, neither do `disable-next`/`enable`).
`PragmaParserError` → swallowed, stays activated.

`remove_pylint_option_from_lines` (format.py:604-612):

```python
lines = mobj.string
purged = lines[: mobj.start(1)].rstrip() + lines[mobj.end(1):]
```

Effects (all verified **[probe]**):

- The pragma text never counts toward line length, for ANY pragma (even
  `disable=invalid-name` or an unknown option value): 126-char line that is ≤100 after
  pragma removal → no C0301.
- `disable=line-too-long` (symbol) → checker_off → silent.
- `disable=C0301` (id) → not checker_off; message added with the now-shorter length but
  suppressed by the standard line-state machinery → also silent. Same visible output.
- Pragma + still >100 after removal → C0301 with the PURGED rstripped length:
  101-char-after-purge line → `(101/100)`.
- Only the FIRST `OPTION_PO.search` match in the chunk is processed. In a multi-line
  STRING chunk, pragma-looking text inside the string is found by this search (and, if it
  says `disable=line-too-long`, turns the whole chunk's length check off; the purge glues
  `lines[:start(1)].rstrip()` to `lines[end(1):]`, which can merge two physical lines and
  shift `lineno + offset` attribution for subsequent lines). Exotic but deterministic;
  port the string surgery literally.
- The rough pre-filter `any(len(line) > max_chars ...)` uses the UNstripped line
  (includes `\n`), so a line of exactly max_chars+newline enters the slow path but is not
  flagged after rstrip.

### 1.8 C0327 / C0328 (`_check_line_ending`, format.py:471-493)

Called only on NEWLINE tokens (logical line ends — NOT on NL), with `string` = the
NEWLINE token text (`"\n"`, `"\r\n"`, `"\r"`, or `""` for the synthetic EOF newline) and
`line_num` = current row.

```python
if self._last_line_ending is not None:
    if line_ending and line_ending != self._last_line_ending:
        self.add_message("mixed-line-endings", line=line_num)     # C0327
self._last_line_ending = line_ending
expected = config.expected_line_ending_format
if expected:                                                       # default "" → inactive
    line_ending = reduce(lambda x, y: x + y if x != y else x, line_ending, "")
    line_ending = "LF" if line_ending == "\n" else "CRLF"
    if line_ending != expected:
        self.add_message("unexpected-line-ending-format",
                         args=(line_ending, expected), line=line_num)
```

- C0327 fires on EVERY change of ending (alternating files flag lines 2,3,4,…):
  **[probe]** `\n,\r\n,\n,\r\n` → C0327 at 2,3,4. Message has no args. Only logical-line
  NEWLINEs participate (continuation/blank/comment lines' NL endings are invisible here).
- C0328: the `reduce` dedups consecutive identical chars (`"\r\n"`→`"\r\n"`, `"\n"`→`"\n"`),
  then anything ≠ `"\n"` is labeled `CRLF` — including the EMPTY synthetic newline of a
  file lacking a final newline: **[probe]** no-final-newline file with
  `--expected-line-ending-format=LF` → `2:0 C0328 ... There is 'CRLF' while it should be 'LF'.`
  Also a lone-`\r` ending → "CRLF". Args = (actual_label, expected).
- C0327 state is per-module (reset in process_tokens). C0327 and C0328 can both fire for
  the same line.

### 1.9 W0311 bad-indentation (`check_indent_level`, format.py:707-729)

Two call sites: INDENT tokens (`string` = the INDENT whitespace text, expected =
`indents[-1] + 1`), and the first concrete token after a NEWLINE-run with no
INDENT/DEDENT consumed (`check_equal` path; `string` = the WHOLE raw line, expected =
`indents[-1]`). The indents stack counts logical block depth (one entry per INDENT),
independent of actual character counts.

```python
indent = config.indent_string                  # default "    "
if indent == "\\t": indent = "\t"              # literal backslash-t from rcfile
level = 0; unit_size = len(indent)
while string[:unit_size] == indent:
    string = string[unit_size:]; level += 1
suppl = ""
while string and string[0] in " \t":
    suppl += string[0]; string = string[1:]
if level != expected or suppl:
    i_type = "tabs" if indent[0] == "\t" else "spaces"
    self.add_message("bad-indentation", line=line_num,
        args=(level * unit_size + len(suppl), i_type, expected * unit_size))
```

- arg1 = consumed indent chars **by count, not display width** (a single tab where 4
  spaces are expected → `Found 1 spaces, expected 4`). arg2 is `"spaces"`/`"tabs"` from
  the CONFIG indent string, not the file content. arg3 = expected×unit_size.
- The first `while` consumes whole units only; leftover spaces/tabs (interrupted by the
  first non-blank char) become `suppl`; `suppl` non-empty → always a message even when
  level == expected.
- **[probe]** (4-space config): 3-space body → `(3, spaces, 4)`; 8-space body at depth 1
  → `(8, spaces, 4)`; tab body → `(1, spaces, 4)`; 6-space body at depth 2 →
  `(6, spaces, 8)`; 10-space at depth 3 → `(10, spaces, 12)`.
- check_equal path: every subsequent misindented statement of the same block re-flags
  (string = full line; the suppl loop stops at the first code char). Comments/NL between
  NEWLINE and the statement do NOT consume check_equal (format.py:429-430); INDENT clears
  it; DEDENT re-arms it. Continuation rows (inside brackets) are never indent-checked
  (their preceding token is NL, which doesn't set check_equal).
- DEDENT pops with `if len(indents) > 1` underflow guard (format.py:424-425).

### 1.10 C0325 superfluous-parens (`_check_keyword_parentheses`, format.py:276-378)

Called at every token whose string ∈ _KEYWORD_TOKENS (with the FULL token list and
`start=idx`). Pseudocode (faithful):

```python
if tokens[start+1].string != "(": return
if tokens[start].string == "not" and start > 0 and tokens[start-1].string == "is":
    return                                            # `is not (x)` is binary → fine
found_and_or = False; contains_walrus_operator = False
walrus_operator_depth = 0; contains_double_parens = 0; depth = 0
keyword_token = tokens[start].string; line_num = tokens[start].start[0]
for i in range(start, len(tokens) - 1):               # last token never examined
    token = tokens[i]
    if token.type == tokenize.NL: return              # parens assumed for continuation
    if token.string == ":=" or token.string + tokens[i+1].string == ":=":
        contains_walrus_operator = True; walrus_operator_depth = depth
    if token.string == "(":
        depth += 1
        if tokens[i+1].string == "(": contains_double_parens = 1
    elif token.string == ")":
        depth -= 1
        if depth:                                     # not yet back at 0
            if contains_double_parens and tokens[i+1].string == ")":
                if keyword_token in {"in", "if", "not"}: continue
                return
            contains_double_parens -= 1
            continue
        # depth == 0: closing the outer paren
        if tokens[i+1].string in {":", ")", "]", "}", "in"} or \
           tokens[i+1].type in {tokenize.NEWLINE, tokenize.ENDMARKER, tokenize.COMMENT}:
            if contains_walrus_operator and walrus_operator_depth - 1 == depth: return
            if i == start + 2: return                 # empty tuple ()
            if found_and_or: return
            if keyword_token == "in": return          # churn-avoidance special case (PR #4948)
            self.add_message("superfluous-parens", line=line_num, args=keyword_token)
        return                                        # ALWAYS return once depth hits 0
    elif depth == 1:
        match token[1]:
            case ",":            return               # tuple
            case "and" | "or":   found_and_or = True
            case "yield":        return               # parens required
            case "for":          return               # genexp
            case "else":
                if "(" in (i.string for i in tokens[i:]):
                    self._check_keyword_parentheses(tokens[i:], 0)
                return
```

Message: line = keyword's row, col 0, args = keyword string (rendered via `%r`).
**[probe]** flagged: `if (True):`, `while (1):`, `assert (True)`, `a = (1)` (`'='`),
`b = not (a)` (`'not'`), `del (a)`. Not flagged: `for x in (1, 2):` (comma),
`print((1))` (not a keyword), `c = (yield) if ...`, `(… for …)` genexps,
`return_val = ([listcomp])` (the `for` of the comprehension is at paren-depth 1 — only
parens count toward depth, brackets don't), `if (a := 4):` (walrus), `d = b is not (True)`,
`if (b) and (a):` (first `)` at depth 0 has next token `and` → bare return). Brackets
`[`/`{` are NOT depth-tracked — only `(`/`)`.

### 1.11 C0302 too-many-lines (format.py:448-464)

```python
line_num -= 1                                    # ENDMARKER row - 1 == wc -l
if line_num > config.max_module_lines:           # strictly greater; default 1000
    message_definition = msgs_store.get_message_definitions("too-many-lines")[0]
    names = (message_definition.msgid, "too-many-lines")        # ("C0302", "too-many-lines")
    lineno = next(filter(None, (self.linter._pragma_lineno.get(name) for name in names)), 1)
    self.add_message("too-many-lines", args=(line_num, config.max_module_lines), line=lineno)
```

`_pragma_lineno` (message_state_handler.py:55, 396-399) maps **raw pragma message
strings** → the row of the last `disable`/`disable-next`/`enable` pragma that mentioned
them. It is populated during the linter's pragma scan and **NEVER cleared between
modules** — state leaks across files in a run. **[probe]**: `a_mod.py` containing
`# pylint: disable=too-many-lines` (line 2) and `# pylint: enable=too-many-lines`
(line 3); `b_mod.py` (11 lines, no pragmas) checked after with `--max-module-lines=5` →
`b_mod.py:3:0: C0302: Too many lines in module (11/5)` — line 3 comes from a_mod's last
pragma. Port: a run-global `HashMap<String,u32>` updated in file order (sequential
flush order, careful under rayon: pylint processes files sequentially, so the map state
seen by module N is "all pragmas from modules 1..=N processed so far, last writer wins").
Lookup order: key `"C0302"` first, then `"too-many-lines"`; first non-None (note
`filter(None, …)` also skips a theoretical line 0); default 1.

args = (line_num, max_module_lines). line_num counts physical lines wc-l style (a file
ending without newline still counts its last partial line because ENDMARKER sits one row
past it).

### 1.12 C0305 trailing-newlines (format.py:426-428, 466-469)

`last_blank_line_num` = row of the last NL token whose raw line, after stripping ONLY
`"\r\n"` chars, is empty (so `"   \n"` is NOT blank — it gets C0303 instead). After the
loop (and after the `line_num -= 1`):

```python
if line_num == last_blank_line_num and line_num > 0:
    self.add_message("trailing-newlines", line=line_num)
```

i.e. fires iff the LAST physical line of the file is blank; reported at that last blank
line. **[probe]** `x = 1\n\n\n` → C0305 at line 3; file `"\n\n"` → C0305 at line 2;
truly empty file → `line_num == 0` → nothing ("__init__.py markers" comment,
format.py:466-467); file ending `"   \n"` → no C0305.

### 1.13 C0321 multiple-statements (`visit_default`, format.py:495-579)

Registered via the walker's visit_default mechanism for EVERY node class (ast_walker.py:
64-69; FormatChecker defines no other visit_ methods). The
`@only_required_for_messages("multiple-statements")` decorator is INEFFECTIVE here (the
walker doesn't consult checks_msgs for visit_default) — see §0.3 gate 2 for the inline-
enable consequence.

```python
def visit_default(self, node):
    if not node.is_statement: return
    if not node.root().pure_python: return            # always true in prylint pipeline
    prev_sibl = node.previous_sibling()
    if prev_sibl is not None:
        prev_line = prev_sibl.fromlineno
    elif isinstance(node.parent, nodes.Try) and \
         self._is_first_node_in_else_finally_body(node, node.parent):
        prev_line = self._infer_else_finally_line_number(node, node.parent)
    elif isinstance(node.parent, nodes.Module):
        prev_line = 0
    else:
        prev_line = node.parent.statement().fromlineno
    line = node.fromlineno
    if prev_line == line and self._visited_lines.get(line) != 2:
        self._check_multi_statement_line(node, line)
        return
    if line in self._visited_lines: return
    try: tolineno = node.blockstart_tolineno          # block headers: line of the ':'
    except AttributeError: tolineno = node.tolineno
    lines = []
    for line in range(line, tolineno + 1):
        self._visited_lines[line] = 1
        try: lines.append(self._lines[line].rstrip())
        except KeyError: lines.append("")
    # `lines` is DEAD CODE (built and discarded) — only the _visited_lines marking matters
```

- `previous_sibling` is astroid's "previous child of parent across all child fields" —
  e.g. for the first body statement of a `with`, the previous sibling is the last
  context-manager item; of an ExceptHandler body, the exception type node. That is why
  `with open("f") as f: data = f.read()` flags `data = ...` (prev_sibl = the Call, same
  line) and `except ValueError: pass` flags `pass`.
- else/finally helpers (format.py:533-553): for the first statement of `Try.orelse`,
  prev_line = (last handler body stmt's tolineno, else Try body's last stmt) + 1; for the
  first statement of `Try.finalbody`, prev_line = orelse[-1].tolineno + 1 if orelse else
  same fallback chain; 0 if nothing found. Net effect: `else: pass` / `finally: pass` on
  one line are flagged.
- `_check_multi_statement_line` (format.py:555-579) exemptions, in match order:
  1. `node` is a With node itself → return ("multiple nested context managers").
  2. `node.parent` is an If with `orelse == []` AND `config.single_line_if_stmt` (default
     False) → return.
  3. `node.parent` is a ClassDef with exactly one body stmt AND
     `config.single_line_class_stmt` (default False) → return.
  4. `node` is `Expr(value=Const(Ellipsis))` whose parent is FunctionDef or ClassDef →
     return (stubs: `def s(): ...` / `class C: ...` exempt; `def f(): return 1` not).
  Otherwise `add_message("multiple-statements", node=node, confidence=HIGH)` and
  `self._visited_lines[line] = 2` — at most ONE C0321 per line (`y = 2` in
  `if c: x = 1; y = 2` is silent), reported at the offending statement node's position
  (line = its fromlineno, col = its col_offset; if the node is a FunctionDef/ClassDef,
  node.position i.e. the `def`/`class` keyword span wins per §0.4).
- `_visited_lines`/`_lines` come from the SAME checker instance's process_tokens for the
  current module (token phase runs before walk; both dicts reset per module).
- **[probe]** flags: `if True: pass` (2:9), `while True: break` (4:12),
  `for i in (1,): print(i)` (5:15), `with ...: data = f.read()` (6:21),
  `class B: pass` (7:9), `except ValueError: pass` (12:19), `else: pass` (13:6),
  `finally: pass` (14:9), `def two(): return 1` (15:11). Not flagged: `def s(): ...`,
  `class C: ...`, `lam = lambda: 7` (lambda body isn't a statement).

---

## 2. checkers/misc.py

### 2.1 EncodingChecker — name "miscellaneous", BaseTokenChecker + BaseRawFileChecker

Single message (misc.py:63-69): **W0511 fixme — template `"%s"`** — LINE scope,
default-enabled, confidence UNDEFINED. Options (misc.py:71-102):

| option                    | default                     |
|---------------------------|-----------------------------|
| notes                     | `("FIXME", "XXX", "TODO")` (csv) |
| notes-rgx                 | `""` (string)               |
| check-fixme-in-docstring  | `False` (yn)                |

`open()` (misc.py:104-123) compiles three case-insensitive patterns:

```python
notes = "|".join(re.escape(note) for note in config.notes)
if config.notes_rgx: notes += f"|{config.notes_rgx}"
comment:   rf"#\s*(?P<msg>({notes})(?=(:|\s|\Z)).*?$)"            re.I
docstring: rf"((\"\"\")|(\'\'\'))\s*(?P<msg>({notes})(?=(:|\s|\Z)).*?)((\"\"\")|(\'\'\'))"  re.I
multiline: rf"^\s*(?P<msg>({notes})(?=(:|\s|\Z)).*$)"             re.I
```

`process_tokens` (misc.py:150-180):

```python
if not config.notes: return        # empty notes list disables fixme ENTIRELY, even with notes-rgx!
for token_info in tokens:
    if token_info.type == tokenize.COMMENT:
        if match := self._comment_fixme_pattern.match(token_info.string):
            self.add_message("fixme", col_offset=token_info.start[1] + 1,
                args=match.group("msg"), line=token_info.start[0])
    elif config.check_fixme_in_docstring:                      # default False
        if self._is_multiline_docstring(token_info):
            for line_no, line in enumerate(token_info.string.split("\n")):
                if match := self._multiline_docstring_fixme_pattern.match(line):
                    self.add_message("fixme", col_offset=token_info.start[1] + 1,
                        args=match.group("msg"), line=token_info.start[0] + line_no)
        elif match := self._docstring_fixme_pattern.match(token_info.string):
            self.add_message("fixme", col_offset=token_info.start[1] + 1,
                args=match.group("msg"), line=token_info.start[0])
```

Comment-path semantics (the only path under default config):
- `.match` is ANCHORED: the comment token must be `#`, optional whitespace, then the note
  word immediately. `## TODO`, `# pre TODO`, `# TODOX` do NOT match; `#TODO:` does.
- Lookahead `(?=(:|\s|\Z))`: note must be followed by `:`, whitespace, or end-of-string.
- `msg` group runs to end of the token string (comment tokens never contain `\n`; `.*?$`
  with non-multiline `$`).
- Case-insensitive; the message arg preserves the ORIGINAL spelling+rest of comment:
  **[probe]** `# todo: lowercase` → `W0511: todo: lowercase`.
- Position: line = comment token row, col = comment token start col **+ 1**:
  **[probe]** comment at col 0 → reported col 1; inline comment at col 7 → col 8.
- Iterates ALL tokens in stream order → W0511s are emitted in token order, after all of
  FormatChecker's token messages (checker order §0.2).

Docstring path (default-OFF via option, not message state — W0511 itself stays enabled):
`_is_multiline_docstring` (misc.py:182-187) = STRING token AND `token.line.lstrip()`
starts with `"""` or `'''` (note: token.LINE, the raw physical line) AND
`"\n" in token.line.rstrip()`. Multiline: each `\n`-split line of token.string matched
against the `^\s*…` pattern; line = token row + index, col = token start col + 1 for ALL
hits. Single-line triple-quoted: `docstring` pattern anchored at the opening quotes.
The `elif` applies to every non-COMMENT token (NAMEs can't start with quotes → harmless).

`process_module` (raw side, misc.py:125-148): reads `node.stream()` line by line and
tries `line.decode(file_encoding or "ascii")`:
- UnicodeDecodeError → silently ignored (returns None, NO message).
- LookupError (unknown codec) → if the line starts with `b"#"` and `"coding"` and the
  encoding name appear in `str(line)`, emits **E0001 syntax-error** with
  `Cannot decode using encoding '<enc>', bad encoding` at that line.
- In the prylint pipeline both paths are DEAD: astroid's `open_source_file`
  (reference/astroid/astroid/builder.py:49-55) runs `detect_encoding` first, so an
  unknown/undecodable encoding fails the build → E0001 from get_ast, module never checked;
  `Module.file_encoding` is always the detect_encoding result for file-built modules
  (builder.py:163), and a file that fully decoded at build time re-decodes line-by-line.
  Recommended port: no-op (keep a comment).

### 2.2 ByIdManagedMessagesChecker — name "miscellaneous", BaseRawFileChecker

Message (misc.py:26-33): **I0023 use-symbolic-message-instead — `"%s"`,
`{"default_enabled": False}`** → registered then immediately config-disabled
(pylinter.py:502-504). DEFAULT-OFF: under default config the checker is dropped by
prepare_checkers (its only message disabled) and never runs.

When enabled (`--enable=use-symbolic-message-instead`): `process_module` (misc.py:42-50)
iterates `linter._by_id_managed_msgs` — tuples `(mod_name, msgid, symbol, lineno,
is_disabled)` appended by the message-state handler whenever a pragma manages a message
by NUMERIC id (e.g. `# pylint: disable=C0301`). For entries whose `mod_name == node.name`:

```python
verb = "disable" if is_disabled else "enable"
txt = f"'{msgid}' is cryptic: use '# pylint: {verb}={symbol}' instead"
self.add_message("use-symbolic-message-instead", line=lineno, args=txt)
```

then CLEARS the whole list (misc.py:50). Exit-code note: I-category contributes status
bit 0 (constants.py:43) — I0023 never affects the exit code. Port priority: low (needs
`_by_id_managed_msgs` bookkeeping in the pragma scanner; only relevant when explicitly
enabled).

---

## 3. checkers/non_ascii_names.py — NonAsciiNameChecker (name "nonascii-checker")

Plain BaseChecker (AST walk only). Messages (non_ascii_names.py:37-62), all NODE scope,
all emitted with confidence HIGH, all default-enabled:

| id    | symbol                 | template |
|-------|------------------------|----------|
| C2401 | non-ascii-name         | `%s name "%s" contains a non-ASCII character, consider renaming it.` (old_names: `[("C0144", "old-non-ascii-name")]`) |
| W2402 | non-ascii-file-name    | `%s name "%s" contains a non-ASCII character.` |
| C2403 | non-ascii-module-import| `%s name "%s" contains a non-ASCII character, use an ASCII-only alias for import.` |

Core `_check_name(node_type, name, node)` (non_ascii_names.py:66-85):

```python
if name is None: return                                  # e.g. Keyword(arg=None) for **kwargs
if not str(name).isascii():
    type_label = constants.HUMAN_READABLE_TYPES[node_type]
    args = (type_label.capitalize(), name)
    msg = {"file": "non-ascii-file-name", "module": "non-ascii-module-import"}.get(node_type,
          "non-ascii-name")
    self.add_message(msg, node=node, args=args, confidence=HIGH)
```

`str.isascii()` = every code point < 128. HUMAN_READABLE_TYPES (constants.py:64-81)
labels used here: file→"file", module→"module", const→"constant", class→"class",
function→"function", attr→"attribute", argument→"argument", variable→"variable".
`.capitalize()` gives "File", "Module", "Constant", …

Visitors (each with `@only_required_for_messages(...)` — gate-2 lists shown):

- `visit_module` [non-ascii-name, non-ascii-file-name] (:87-89):
  `_check_name("file", node.name.split(".")[-1], node)` → W2402, reported at the module
  node → `1:0` (line `0 or 1` rule, §0.4). The name checked is the last dotted component
  of the FileItem module name (for packages: the package dir basename via `__init__`
  stripping rules — astroid module name, not filename).
- `visit_functiondef` = `visit_asyncfunctiondef` [non-ascii-name] (:91-115): function
  name (label "Function", at the def node → position = keyword span), then
  `args.posonlyargs`, `args.args`, `args.kwonlyargs` each as "argument" at the AssignName
  arg node. **`vararg`/`kwarg` (`*args`/`**kwargs`) names are NOT checked.**
- `visit_global` [non-ascii-name] (:117-120): each name in `node.names` as "const"
  (label "Constant") at the Global node.
- `visit_assignname` [non-ascii-name] (:122-144):
  ```python
  match frame := node.frame():
      case nodes.FunctionDef():
          if node.parent in frame.body:        # only direct body statements
              self._check_name("variable", node.name, node)
      case nodes.ClassDef():
          self._check_name("attr", node.name, node)     # label "Attribute"
      case _:
          self._check_name("variable", node.name, node)
  ```
  Consequences: function parameters are NOT double-reported (their AssignName's parent is
  the Arguments node, not in frame.body); module-level and comprehension targets are
  "variable"; **lambda parameters fall into `case _` and ARE reported as "Variable"**
  (frame is Lambda, not FunctionDef) — **[probe]** `lambda lärg: lärg` →
  `14:22 C2401 Variable name "lärg"`. `in frame.body` is list membership via `==`
  (astroid NodeNG eq = identity).
- `visit_classdef` [non-ascii-name] (:146-151): class name ("Class") at the class node
  (position = keyword span), then for each `attr, anodes in node.instance_attrs.items()`:
  if `not any(node.instance_attr_ancestors(attr))` (no ancestor class also sets it),
  `_check_name("attr", attr, anodes[0])` at the FIRST recorded AssignAttr node.
  - `instance_attrs` iteration order = build-time insertion order (pyast locals/attrs
    maps must preserve it).
  - **Emission-order quirk**: these fire when the walk ENTERS the ClassDef — i.e. an
    instance attr assigned at line 11 is reported BEFORE a class-level AssignName at
    line 9 (which waits for its own node visit). **[probe]**: order `8:0` (class name),
    `11:8` (inst_ättr), `9:4` (ättr).
  - `instance_attr_ancestors` walks `node.ancestors()` (MRO-ish chain) checking each
    ancestor's instance_attrs — conservatism: inherited attrs are skipped entirely.
- `visit_import` / `visit_importfrom` [non-ascii-name, non-ascii-module-import]
  (:153-164): for each `(module_name, alias)` in `node.names`:
  `name = alias or module_name` → `_check_name("module", name, node)` → C2403 at the
  import statement node (col 0). Note ImportFrom's imported OBJECT names also get the
  "Module" label/C2403 (quirk). Plain `import ós` reports "ós"; `import x as ó` reports
  "ó"; `from m import x` reports "x" only if non-ASCII.
- `visit_call` [non-ascii-name] (:166-170): every `keyword.arg` of `node.keywords` as
  "argument" at the Keyword node (None arg = `**expr` → skipped by the None guard).
  **[probe]** `dict(kwärg=1)` → `13:9 C2401 Argument name "kwärg"`.

**[probe]** summary (file `nön_ascii.py`): `1:0 W2402 File name "nön_ascii"`,
`2:0/3:0 C2403 Module name "ós"/"päth"`, `4:0 Variable schön`, `5:0 Function fünc`,
`5:10 Argument ärg` (värgs/kwärgs silent), `6:4 Variable löcal`, `7:4 Constant glöb`,
`8:0 Class Klässe`, `11:8 Attribute inst_ättr`, `9:4 Attribute ättr`,
`13:9 Argument kwärg`, `14:22 Variable lärg`.

---

## 4. checkers/unicode.py — full-mode delta (C2503); E-codes already ported

The raw checker (name "unicode_checker") and its E2501/E2502/E2510-E2515 logic are fully
specified in notes/08 §9 and implemented bug-for-bug in `pycheckers::unicode`. Full-pylint
mode adds exactly one visible change: **C2503 bad-file-encoding** is now displayable.

`_check_codec(codec, codec_definition_line)` (unicode.py:460-475) — runs unconditionally
inside `process_module` after `_determine_codec`:

```python
if codec != "utf-8":
    msg = "bad-file-encoding"
    if self._is_invalid_codec(codec):        # codec.startswith(("utf-16","utf-32"))
        msg = "invalid-unicode-codec"        # E2501
    self.add_message(msg, line=codec_definition_line, end_lineno=codec_definition_line,
                     confidence=HIGH, col_offset=None, end_col_offset=None)
```

- C2503 template: `PEP8 recommends UTF-8 as encoding for Python files` (no args), LINE
  scope, col rendered 0 (`col_offset or 0`).
- `codec_definition_line` = `len(lines) or 1` from `detect_encoding` (1 if the codec came
  from a BOM/default, 2 if the coding cookie is on line 2), or 1 for the UTF-16/32 BOM
  fallback path (unicode.py:437-458; already ported).
- Codec name normalization (`_normalize_codec_name`, :208-215) means `latin-1`,
  `iso-8859-15`, `ascii` declarations all → C2503; only exact normalized `"utf-8"`
  (which includes `utf-8-sig`? NO — `utf 8 sig` normalizes to `utf-8sig`?? see below) is
  exempt.
  - Careful: `UTF_NAME_REGEX_COMPILED.sub(r"utf-\1\2", codec).lower()` maps
    `UTF-8-SIG`→`utf-8` (the `(sig)?` group is matched but not re-emitted) and
    `utf-8`→`utf-8`. So BOM'd UTF-8 files ("utf-8-sig" from detect_encoding) are exempt.
    This is existing ported behavior — just verify the C2503 branch reuses it.
- Since this is the raw phase, C2503 is emitted before any token-checker message of the
  same module but after format/miscellaneous raw checkers (which emit nothing) —
  **[probe]** in §0.2 shows C2503 first.
- The checker is prepared if ANY of its 9 messages is enabled, so under default full mode
  it always runs; with `--disable=all --enable=bad-file-encoding` it still runs and only
  C2503 survives the line gate.

No other W/C codes exist in unicode.py (E2501/E2502/E2510-15 are E; C2503 is the lone C).

---

## 5. checkers/dunder_methods.py — DunderCallChecker (name "unnecessary-dunder-call")

One message (dunder_methods.py:37-44): **C2801 unnecessary-dunder-call —
`Unnecessarily calls dunder method %s. %s.`** — NODE scope, HIGH confidence,
default-enabled. No options.

`open()` (:47-51): build `self._dunder_methods` from `constants.DUNDER_METHODS`
(constants.py:123-221) merging every version bucket with
`since_vers <= self.linter.config.py_version`; py-version defaults to the running
interpreter (base_options.py:356-364) = (3,12) in the pinned env → both buckets:
`(0,0)` (≈95 methods `__init__`…`__fspath__`, each mapped to a replacement-hint string)
and `(3,10)` (`__aiter__`/`__anext__`). Copy the full table verbatim from constants.py.
EXTRA_DUNDER_METHODS (constants.py:222-246) is NOT consulted here (those names are simply
absent from DUNDER_METHODS).

`visit_call` (:74-98) — no only_required_for_messages (always registered when prepared):

```python
if (isinstance(node.func, nodes.Attribute)
    and node.func.attrname in self._dunder_methods
    and not self.within_dunder_or_lambda_def(node)
    and not (isinstance(node.func.expr, nodes.Call)
             and isinstance(node.func.expr.func, nodes.Name)
             and node.func.expr.func.name == "super")):
    inf_expr = safe_infer(node.func.expr)
    if not (inf_expr is None or isinstance(inf_expr, (Instance, UninferableBase))):
        return                                   # dunder call on a non-instantiated class etc.
    self.add_message("unnecessary-dunder-call", node=node,
        args=(node.func.attrname, self._dunder_methods[node.func.attrname]),
        confidence=HIGH)
```

Trigger conditions, in order:
1. `node.func` is an Attribute whose attrname is in the merged dunder map.
2. `within_dunder_or_lambda_def` (:53-65): walk ALL parents up to module; return True
   (→ exempt) if any ancestor is a FunctionDef named `__*__`
   (`name.startswith("__") and name.endswith("__")`) OR
   (`isinstance(ancestor, nodes.Lambda)` AND `node.func.attrname` ∈
   UNNECESSARY_DUNDER_CALL_LAMBDA_EXCEPTIONS (constants.py:261-282: `__init__`,
   `__del__`, `__delattr__`, `__set__`, `__delete__`, `__setitem__`, `__delitem__`, and
   the 13 in-place ops `__iadd__`…`__ior__`)). Note FunctionDef check matches ANY dunder
   def (e.g. all dunder calls inside `__len__` are exempt). Lambda check: the attrname
   must be in the exception list, otherwise lambdas don't exempt.
3. Not a direct `super().X()` call (textual check on `node.func.expr` being
   `Call(func=Name("super"))` — `super(C, self).__init__()` is ALSO matched since func
   is still Name "super").
4. Inference gate: `safe_infer(node.func.expr)` (notes/08 §0.1). The message is emitted
   when the result is **None (inference failed/ambiguous), Uninferable, or an Instance**
   (astroid Const/List/etc count as Instance ⇒ `[1].__len__()`, `"abc".__str__()`,
   `(3).__add__(4)` all flagged). It is SKIPPED when the expr infers to a non-Instance
   node — ClassDef (`K.__len__(K())`), Module, FunctionDef — the "not instantiated
   class" conservatism.

args = (attrname, replacement hint); template appends a final `.` →
`Unnecessarily calls dunder method __len__. Use len built-in function.`. Reported at the
Call node. **[probe]**: flags at `5:4`, `6:0`, `7:4`, `14:0` (`K().__len__()`),
`16:23` (`lambda s: s.__len__()` — __len__ not in lambda exceptions); silent for
`K.__len__(K())`, dunder calls inside `def __len__`, `lambda s: s.__iadd__(1)`.

---

## 6. checkers/threading_checker.py — ThreadingChecker (name "threading")

One message (:36-43): **W2101 useless-with-lock —
`'%s()' directly created in 'with' has no effect`** — NODE scope, confidence UNDEFINED
(none passed), default-enabled.

`visit_with` [@only_required_for_messages("useless-with-lock")] (:45-55):

```python
LOCKS = frozenset(("threading.Lock", "threading.RLock", "threading.Condition",
                   "threading.Semaphore", "threading.BoundedSemaphore"))
context_managers = (c for c, _ in node.items if isinstance(c, nodes.Call))
for context_manager in context_managers:
    if isinstance(context_manager, nodes.Call):          # redundant double-check
        infered_function = safe_infer(context_manager.func)
        if infered_function is None: continue
        qname = infered_function.qname()
        if qname in self.LOCKS:
            self.add_message("useless-with-lock", node=node, args=qname)
```

- Only `with EXPR():` where the item expression is a Call; the presence of `as var` does
  NOT exempt (**[probe]** `with Lock() as lk:` flagged).
- `safe_infer(func)`; needs `.qname()` — the inferred value must be a scoped node
  (ClassDef/FunctionDef). With astroid's `brain_threading` the names `threading.Lock`
  etc. infer to a synthetic ClassDef with qname `threading.Lock` (the snapshot generator
  runs after brains, so the pinned snapshot already contains it — verify `lock` class
  qnames in `crates/pyinfer/snapshot/threading.json`). Uninferable has a `qname`? No —
  `safe_infer` returning Uninferable would crash on `.qname()`… actually
  `Uninferable.qname()` returns Uninferable (UninferableBase swallows attribute access
  and calls), so `qname in LOCKS` is False → no message. None → continue.
- Message at the **With node** (line/col of `with` keyword), args = the qname string
  (template adds `()`): **[probe]** `19:4 W2101: 'threading.Lock()' directly created in
  'with' has no effect`. One message per matching item (a `with Lock(), RLock():` yields
  two messages at the same With node).
- AsyncWith is NOT visited (no visit_asyncwith).

---

## 7. checkers/lambda_expressions.py — LambdaExpressionChecker (name "lambda-expressions")

Messages (:23-37), NODE scope, HIGH confidence, default-enabled, no options:

| id | symbol | template |
|----|--------|----------|
| C3001 | unnecessary-lambda-assignment | `Lambda expression assigned to a variable. Define a function using the "def" keyword instead.` |
| C3002 | unnecessary-direct-lambda-call | `Lambda expression called directly. Execute the expression inline instead.` |

No only_required_for_messages — all three visitors run whenever the checker is prepared
(i.e. when at least one of the two messages is config-enabled).

`visit_assign` (:40-70):

```python
match node:
    case nodes.Assign(targets=[nodes.AssignName(), *_], value=nodes.Lambda() as value):
        self.add_message("unnecessary-lambda-assignment", node=value, confidence=HIGH)
    case nodes.Assign(targets=[nodes.Tuple() as target, *_],
                      value=nodes.Tuple() | nodes.List() as value):
        for lhs_elem, rhs_elem in zip_longest(target.elts, value.elts):
            if lhs_elem is None or rhs_elem is None:
                break                                    # unbalanced unpacking: stop
            if isinstance(lhs_elem, nodes.AssignName) and isinstance(rhs_elem, nodes.Lambda):
                self.add_message("unnecessary-lambda-assignment", node=rhs_elem, confidence=HIGH)
```

- Pattern 1: only the FIRST target must be AssignName (`x = y = lambda: 1` matches via
  `[AssignName(), *_]`). The message node is the **Lambda** (its position):
  **[probe]** `func_assigned = lambda: 1` → `34:16`.
- Pattern 2: first target a Tuple, value Tuple or List → element-wise; non-AssignName lhs
  (e.g. Starred, subscript) elements just don't match; unbalanced lengths stop the scan
  at the first None. **[probe]** `a, b = lambda: 1, lambda: 2` → `35:7` and `35:18`.
- NOT matched: AugAssign, AnnAssign (`x: T = lambda: 1` is an AnnAssign — silent!),
  first-target Tuple with non-Tuple/List rhs (e.g. `a, b = make()`), List as TARGET
  (`[a, b] = ...` — target pattern is Tuple only).

`visit_namedexpr` (:72-81): `(x := lambda: 3)` → C3001 at the Lambda. **[probe]** `37:6`.

`visit_call` (:83-90): `isinstance(node.func, nodes.Lambda)` → C3002 at the **Call** node
(includes the wrapping paren: **[probe]** `(lambda z: z)(9)` → `36:0`).

---

## 8. checkers/nested_min_max.py — NestedMinMaxChecker (name "nested_min_max")

One message (:40-46): **W3301 nested-min-max —
`Do not use nested call of '%s'; it's possible to do '%s' instead`** — NODE scope,
confidence INFERENCE, default-enabled.

`visit_call` [@only_required_for_messages("nested-min-max")] (:78-139):

```python
inferred = self.maybe_get_inferred_min_max_call(node)     # safe_infer(node.func) must be a
if inferred is None: return                               # FunctionDef with qname in
redundant_calls = self.get_redundant_calls(node, inferred)#   {"builtins.min","builtins.max"}
if not redundant_calls: return
fixed_node = copy.copy(node)                              # SHALLOW copy
while len(redundant_calls) > 0:
    for i, arg in enumerate(fixed_node.args):
        if isinstance(arg, nodes.Call) and any(isinstance(a, nodes.GeneratorExp)
                                               for a in arg.args):
            return                                         # genexp anywhere → bail, no message
        if arg in redundant_calls:
            fixed_node.args = fixed_node.args[:i] + arg.args + fixed_node.args[i+1:]
            break                                          # splice inner args in place
    redundant_calls = self.get_redundant_calls(fixed_node, inferred)
# splat pass:
for idx, arg in enumerate(fixed_node.args):                # iterates the list object as of HERE
    if not isinstance(arg, nodes.Const):
        if self._is_splattable_expression(arg):
            splat_node = nodes.Starred(... synthetic ...); splat_node.value = arg
            fixed_node.args = [*fixed_node.args[:idx], splat_node,
                               *fixed_node.args[idx+1:idx]]     # ← EMPTY tail slice (bug)
func_name = node.func.attrname if isinstance(node.func, nodes.Attribute) else node.func.name
self.add_message("nested-min-max", node=node, args=(func_name, fixed_node.as_string()),
                 confidence=INFERENCE)
```

`get_redundant_calls` (:60-76): args of `node` that are Calls, infer to min/max, AND

```python
inferred.qname == inferred_call.qname        # BOUND METHOD comparison, no call!
and len(arg.parent.args) > 1                 # parent in the ORIGINAL tree
```

- `qname == qname` on bound methods is True iff same underlying function AND same `self`
  ⇒ effectively `inferred is inferred_call`: only min-inside-min / max-inside-max (the
  builtins module FunctionDef nodes are singletons per run). `min(1, max(2,3))` silent.
- `arg.parent` is the arg's REAL parent Call in the original tree (shallow copy doesn't
  reparent), so "matrix" single-arg nesting `max(max([[…]]))` is exempt
  (`len(parent.args) == 1`), including on later flatten iterations.

Splat-pass details (all **[probe]**-verified):

- `_is_splattable_expression` (:141-172): recursively true for `BinOp` `+` or `|` with
  both sides splattable; else `safe_infer(arg)` with
  `inferred.pytype() in {"builtins.list","builtins.tuple"}` → true; else
  `isinstance(inferred or arg, (List, Tuple, Set, ListComp, DictComp, DictValues,
  DictKeys, DictItems, Dict))` → true (note `inferred or arg`: falls back to the SYNTAX
  node when inference returned None/Uninferable… careful: `Uninferable` is falsy so
  `inferred or arg` picks `arg`).
- The enumerate-over-stale-list + empty `args[idx+1:idx]` tail slice produce the
  **tail-drop bug**: when a splattable non-Const arg at index i is wrapped, all args
  AFTER i (as of that assignment) are dropped — but if every following arg is also
  splattable, each later iteration re-appends its own splat (using the stale enumerate
  source), accidentally reconstructing the list. Net effect:
  - all non-Const args splattable, in order → correct-looking result:
    `min([1,2], min([3],[4]))` → `min(*[1, 2], *[3], *[4])`.
  - leading Consts then one splattable → fine: `min(min(1,2), [3,4])` →
    `min(1, 2, *[3, 4])`.
  - splattable arg FOLLOWED by Consts → Consts dropped:
    **[probe]** `min([1, 2], 3, min(4, 5))` → suggestion `min(*[1, 2])` (!).
  - dict-merge BinOp + DictValues: **[probe]**
    `max(max(d1 | d2), max(d1.values()))` → `max(*d1 | d2, *d1.values())`.
- GeneratorExp bail: checked on CALL args inside the while loop (so a genexp inside the
  nested call kills the whole message): **[probe]** `min(1, min(x for x in [1,2]))` →
  silent. NOTE the check happens before the `arg in redundant_calls` test each
  iteration — a genexp in ANY Call arg of fixed_node (even a non-redundant one) bails.
- Multiple nestings on one line: each Call node is visited separately → inner nested
  calls also produce their own messages: **[probe]** `max(1, max(2, max(3, 4)))` → two
  messages: `28:5 'max(1, 2, 3, 4)'` and `28:12 'max(2, 3, 4)'`.
- args = (`min`/`max` — from `func.attrname` if Attribute else `func.name`,
  `fixed_node.as_string()`). **PORT DEPENDENCY: requires an exact astroid `as_string()`
  for Call/Starred/BinOp/etc.** Starred renders as `*` + value.as_string() (no parens:
  `*d1 | d2`). The synthetic Starred's bogus positions don't affect as_string.
- Reported at the ORIGINAL Call node, confidence INFERENCE.
- `keywords` are ignored entirely (only `node.args` examined); `min(min(1,2), key=f)` —
  outer has 1 positional arg... `len(arg.parent.args)` counts positionals only → 1 → no
  message.

---

## 9. Message inventory cross-check (vs crates/pycheckers/src/msgs.rs)

All ids owned by this doc's checkers, with msgs.rs presence/flags verified:

- format: C0301 C0302 C0303 C0304 C0305 W0301 W0311 C0321(node_scope:true) C0325 C0327
  C0328 — present, `enabled:false` (msgs.rs `enabled` = -E set; in full mode all are
  default-ON).
- misc: W0511 (LINE, default-ON), I0023 (`%s`, default-OFF via default_enabled:False —
  the only default-OFF message in this doc; msgs.rs has it `enabled:false` which under
  full mode must ALSO stay disabled unless explicitly enabled — make sure full-mode
  enablement uses default_enabled, not just category).
- non_ascii_names: C2401 (old_names C0144 ✓ in msgs.rs), W2402, C2403 — node_scope:true ✓.
- unicode: E2501 E2502 E2510-E2515 (ported), C2503 (this doc) — all LINE scope ✓.
- dunder: C2801 node ✓. threading: W2101 node ✓. lambda: C3001 C3002 node ✓.
  nested_min_max: W3301 node ✓.
- Not registered anywhere: "lowercase-l-suffix" (dead reference, format.py:443). Deleted
  ids W0312/C0330 handled by the deleted-ids table.

Exit-code bits (constants.py:43): C→16, W→4 — confirmed by probes (C-only run exits 16,
W-only 4, mixed 20).

---

## 10. Iteration-order dependencies & conservatism bail-outs (summary for the port)

Order-sensitive:
1. raw checkers before token checkers; both in sorted-checker order (§0.2) — fixes
   relative order of C2503 vs C0301 vs W0511 within a module.
2. FormatChecker process_tokens emits strictly in token-scan order (W0301 of line N's
   semicolon fires when the first token of line N+1 is seen; C0301/C0303/C0304 per
   new_line; C0327/C0328 at NEWLINE tokens; W0311 at INDENT or first-token; C0325 at the
   keyword token; C0302/C0305 at the very end).
3. visit_classdef instance-attr C2401s precede body-line C2401s (walk-entry vs
   child-visit, §3 probe).
4. `_pragma_lineno` is RUN-GLOBAL and file-order dependent (C0302 line attribution leaks
   across modules, §1.11) — port as shared map updated in sequential flush order.
5. `instance_attrs` dict insertion order (build order) → C2401 attr order.
6. nested_min_max flatten loop: leftmost redundant call first per while-iteration;
   message arg text depends on that order plus the tail-drop bug (§8).

Conservatism bail-outs:
- C0301: ignore-long-lines regex; pragma excision; the rough `len > max` pre-filter.
- C0321: With nodes; If/Class single-line opt-ins; Ellipsis stubs; one-per-line via
  `_visited_lines[line] = 2`.
- C0325: NL (continuation) bail; tuple-comma; yield/for/else at depth 1; walrus; `is not`;
  `in` special-case; empty-tuple; found_and_or.
- C2801: dunder-def ancestors; lambda exceptions; super(); non-Instance inference.
- W2101: only Call items; inference must produce a LOCKS qname.
- W3301: same-builtin only; parent-args>1; genexp bail; Const args never splatted.
- C2401 class attrs: skipped when any ancestor defines the same instance attr.
- W0511: empty `notes` config kills the check entirely (even with notes-rgx).

Dead/no-op paths to keep as comments: FormatChecker.process_module, EncodingChecker
process_module encoding scan, lowercase-l-suffix branch, visit_default's `lines` list.
