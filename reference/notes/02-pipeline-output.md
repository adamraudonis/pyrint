# 02 — Lint pipeline, message emission order, text output format, exit codes

Pinned sources:

- pylint 4.0.5: `/Users/adamraudonis/Desktop/Projects/prylint/reference/pylint/pylint`
- astroid 4.0.4: `/Users/adamraudonis/Desktop/Projects/prylint/reference/astroid/astroid`
- Ground-truth runtime: CPython 3.12.12 (`.venv-pylint`)

All file:line citations below refer to those trees. All "empirically confirmed" outputs in this
document were produced with the pinned venv
(`/Users/adamraudonis/Desktop/Projects/prylint/.venv-pylint/bin/pylint`, pylint 4.0.5 /
astroid 4.0.4 / Python 3.12.12).

Harness invocation that the port must reproduce:

```
pylint . -E --disable=C0301,...,E0110,...,E0401,E0611,E1101,...  [--rcfile=empty.rcfile]
```

i.e. serial mode (`jobs=1`), `from_stdin=False`, output format `text` (default), no
`--msg-template`, no `--output`, no `--recursive`, `--exit-zero` off, `fail-on` empty.

---

## Table of contents

1. [Top-level `Run` flow](#1-top-level-run-flow)
2. [`-E` / errors-only mode](#2--e--errors-only-mode)
3. [Config-phase messages (before any file is linted)](#3-config-phase-messages)
4. [`check()` — the 3-step pipeline](#4-check--the-3-step-pipeline)
5. [File expansion and FileItem ordering (determines all inter-module ordering)](#5-file-expansion-and-fileitem-ordering)
6. [Phase 1: `_get_asts` — building every AST first](#6-phase-1-_get_asts)
7. [`get_ast` error taxonomy: E0001 / F0010 / F0002 exact behavior](#7-get_ast-error-taxonomy)
8. [Checker preparation and `_astroid_module_checker`](#8-checker-preparation-and-_astroid_module_checker)
9. [`ASTWalker` — registration and dispatch order](#9-astwalker--registration-and-dispatch-order)
10. [Phase 2: `_lint_files` / `_lint_file` / `_check_astroid_module`](#10-phase-2-_lint_files)
11. [Message emission: `add_message` → `_add_one_message` → reporter](#11-message-emission)
12. [`TextReporter` — exact output format](#12-textreporter--exact-output-format)
13. [`generate_reports`, score, and end-of-run output under `-E`](#13-generate_reports-score-end-of-run)
14. [Exit codes](#14-exit-codes)
15. [`set_current_module` and stats](#15-set_current_module-and-stats)
16. [Ordering/iteration-dependency summary](#16-ordering-dependency-summary)
17. [Main-checker message formats in scope (E0001, E0011, F0001, F0002, F0010, F0011)](#17-main-checker-message-formats)
18. [Empirical transcripts (ground truth)](#18-empirical-transcripts)

---

## 1. Top-level `Run` flow

`pylint/lint/run.py`, class `Run`, `__init__` (run.py:143-260). Relevant steps in order:

1. `--version` short-circuit (run.py:150-152).
2. `_preprocess_options(self, args)` (run.py:160-164) — extracts `--rcfile`, `--output`,
   `--load-plugins`, `--verbose`, `--enable-all-extensions` from the raw argv.
   `ArgumentPreprocessingError` → print to stderr, **exit 32**.
3. rcfile discovery if none given (run.py:166-170): `config.find_default_config_files()`
   (first hit). The harness passes `--rcfile` explicitly, so discovery is bypassed.
4. `PyLinter` constructed (run.py:172-175); default reporter is `TextReporter()`
   (pylinter.py:311-315). **`BaseReporter.__init__` captures
   `self.path_strip_prefix = os.getcwd() + os.sep` at construction time**
   (reporters/base_reporter.py:37).
5. `linter.load_default_plugins()` (run.py:177) → `checkers.initialize(linter)` +
   `reporters.initialize(linter)` (pylinter.py:370-372). Both call
   `register_plugins(linter, dir)` (pylint/utils/utils.py:157-183) which iterates
   **`os.listdir(directory)`** (utils.py:162) — *unsorted, filesystem order* — imports each
   module/package and calls its `register(linter)` function. (See §8 for why this order
   ends up not mattering for the final checker order, except through a Timsort quirk.)
6. `_config_initialization(...)` (run.py:186-188) — see §3.
7. "No files to lint" check (run.py:202-211): if `args` is empty after option parsing, or
   `--disable=all` with no enables, print `No files to lint: exiting.` to **stdout** and
   **exit 32**.
8. jobs validation (run.py:213-227). Harness: jobs=1, nothing happens.
9. `linter.check(args)` then `score_value = linter.generate_reports(verbose=self.verbose)`
   (run.py:239-240, or the `--output` variant 229-237).
10. Exit-code logic (run.py:245-260) — see §14.

---

## 2. `-E` / errors-only mode

`-E`/`--errors-only` is an argparse action `_ErrorsOnlyModeAction`
(pylint/config/callback_actions.py:266-284) whose only effect at parse time is:

```python
self.run.linter._error_mode = True
```

The actual mode application is `PyLinter._parse_error_mode` (pylinter.py:558-570), called at
the **end** of `_config_initialization` (config_initialization.py:145), i.e. *after* all
`--disable=...` command-line options have already been processed:

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

`disable_noerror_messages` (message_state_handler.py:234-239) disables every message
category except `E` and `F`:

```python
for msgcat in self.linter.msgs_store._msgs_by_category:
    if msgcat in {"E", "F"}:
        continue
    self.disable(msgcat)
```

Net result for the harness: enabled = all E + all F messages, minus the explicit
`--disable` list (E0110, E0401, E0611, E1101, …). `reports=False`, `persistent=False`,
`score=False`.

Note the **timing**: messages emitted *during* config parsing (see §3) are checked for
enablement at emission time, **before** `_parse_error_mode` runs, so e.g. a `W0012` from an
unknown `--disable` value still prints under `-E` (empirically confirmed, §18.5).

---

## 3. Config-phase messages

`_config_initialization` (pylint/config/config_initialization.py:26-161) can emit messages
before any file is linted. Order of operations:

1. `linter.set_current_module(str(config_file) if config_file else "")`
   (config_initialization.py:41).
2. Config file parsed; OSError → print to stderr, exit 32 (lines 45-51).
3. Config-file options parsed; unrecognized ones recorded (lines 66-69).
4. `linter.set_current_module("Command line")` (line 79); command-line options parsed.
   `--disable`/`--enable` actions call `linter.disable/enable` **immediately during argparse**
   (callback_actions.py:385-408). Unknown values inside the list raise
   `UnknownMessageError` deep inside; `_XableAction` stashes them into
   `linter._stashed_messages[(current_name, "unknown-option-value")]` for later.
5. Leftover argv entries starting with `-` → argparse error → **exit 32**
   (lines 93-104).
6. If the *config file* had unrecognized options:
   `set_current_module(config_file or "")` then
   `add_message("unrecognized-option", args=", ".join(...), line=0)` — **E0015**, in scope
   (lines 108-112).
7. `linter._emit_stashed_messages()` (line 128; pylinter.py:1346-1357): iterates
   `self._stashed_messages` (a `defaultdict(list)`, **insertion order**), keyed by
   `(modname, symbol)`; calls `set_current_module(modname)` then
   `add_message(symbol, args=args, line=0, confidence=HIGH)`. This produces e.g.
   `W0012 unknown-option-value` under module `Command line` (see §18.5).
8. `load_plugin_configuration()` (line 136) — may emit `E0013 bad-plugin-value`
   (pylinter.py:410-414: `add_message("bad-plugin-value", args=(modname, error), line=0)`)
   for plugins that failed to import. (Harness loads no plugins.)
9. `enable_fail_on_messages()` (line 140) — no-op with empty `fail-on`.
10. `linter._parse_error_mode()` (line 145) — §2.
11. Positional args are glob-expanded: `glob(arg, recursive=True) or [arg]`
    (lines 152-161). `.` has no glob metacharacters → passed through unchanged.

Because these messages go through the same `add_message` machinery, they print
immediately, with a module header such as `************* Module Command line`, and they
set `msg_status` bits (§14) — e.g. a stray `W0012` contributes bit 4 to the exit code.

`F0011 config-parse-error` (`"error while parsing the configuration: %s"`, pylinter.py:126-131)
is registered but in pylint 4.0.5 the config-file OSError path exits 32 instead; F0011 has no
emission site reachable from this harness invocation (verified: `grep -rn "config-parse-error"`
only matches the definition).

---

## 4. `check()` — the 3-step pipeline

`PyLinter.check` (pylinter.py:672-727):

```python
def check(self, files_or_modules):
    self.initialize()                                  # 677
    if self.config.recursive: ...                      # 678-679  (off in harness)
    if self.config.from_stdin: ...                     # 680-684  (off)
    extra_packages_paths = list(dict.fromkeys(
        [discover_package_path(f, self.config.source_roots) for f in files_or_modules]
    ).keys())                                          # 686-693 (dedup, order-preserving)
    if not from_stdin and jobs > 1: ... return         # 696-705  (not taken, jobs=1)
    progress_reporter = ProgressReporter(self.verbose) # 707 — prints ONLY when verbose
    with augmented_sys_path(extra_packages_paths):     # 710
        fileitems = self._iterate_file_descrs(files_or_modules)   # 715 (LAZY generator)
        data = None
    with augmented_sys_path(extra_packages_paths):     # 719
        with self._astroid_module_checker() as check_astroid_module:   # 720
            ast_per_fileitem = self._get_asts(fileitems, data, progress_reporter)  # 722
            self._lint_files(ast_per_fileitem, check_astroid_module, progress_reporter)  # 725
```

- `initialize()` (pylinter.py:624-634): sets `self._ignore_paths` from config and disables
  every message whose `may_be_emitted(py_version)` is false (version-gated messages).
- `augmented_sys_path` (lint/utils.py:128-136) prepends the deduplicated package paths to
  `sys.path` for the duration.
- `ProgressReporter` (reporters/progress_reporters.py): all its methods print **only if
  verbose** (line 30-31). Harness: silent.
- **Phase 1 (`_get_asts`) fully completes before phase 2 (`_lint_files`) starts.**
  Consequence: every `E0001`/`F0010`/`F0002`(/`F0001` from expansion) produced while
  *building* ASTs is printed **before** any per-module lint message.

---

## 5. File expansion and FileItem ordering

### 5.1 `_iterate_file_descrs` / `_expand_files`

pylinter.py:900-933:

```python
def _iterate_file_descrs(self, files_or_modules):
    for descr in self._expand_files(files_or_modules).values():
        name, filepath, is_arg = descr["name"], descr["path"], descr["isarg"]
        if descr["isignored"]:
            self.stats.skipped += 1
        elif self.should_analyze_file(name, filepath, is_argument=is_arg):
            yield FileItem(name, filepath, descr["basename"])
```

- It is a **generator**; `_expand_files` runs at the first `next()`, i.e. inside
  `_get_asts`'s loop. All `F0001 fatal` messages from expansion errors are emitted at that
  moment — before any AST is built.
- `should_analyze_file` (pylinter.py:601-620): `True` if `is_argument` else
  `path.endswith((".py", ".pyi"))`.
- `FileItem` is `NamedTuple(name, filepath, modpath)` (pylint/typing.py:31-42); `modpath`
  receives `descr["basename"]` (top-level module name of the argument that produced it).

`_expand_files` (pylinter.py:915-933):

```python
result, errors = expand_modules(files_or_modules, self.config.source_roots,
                                self.config.ignore, self.config.ignore_patterns,
                                self._ignore_paths)
for error in errors:
    message = modname = error["mod"]
    key = error["key"]
    self.set_current_module(modname)
    if key == "fatal":
        message = str(error["ex"]).replace(os.getcwd() + os.sep, "")
    self.add_message(key, args=message)
return result
```

`F0001` ("fatal", template `"%s"`, pylinter.py:104-110) details:

- Only error key ever produced by `expand_modules` is `"fatal"`
  (expand_modules.py:123: `errors.append({"key": "fatal", "mod": modname, "ex": ex})`),
  raised when the argument does not exist on disk and
  `modutils.file_from_modpath` raises `ImportError`.
- args = `str(ImportError)` with the literal substring `os.getcwd() + os.sep` removed
  (a plain `str.replace`, all occurrences).
- `set_current_module(modname)` with `filepath=None` → `current_file = modname`
  (pylinter.py:943). `add_message` with `node=None, line=None` → location
  module=modname, abspath=modname, path=modname-with-cwd-strip, line `None or 1` → **1**,
  column **0**.
- Empirically (§18.4): `nonexistent_module_xyz:1:0: F0001: No module named nonexistent_module_xyz (fatal)`,
  exit code 1.

### 5.2 `expand_modules` ordering (expand_modules.py:71-185)

`result` is a `dict[str(filepath) → ModuleDescriptionDict]` filled in this order:

```
for something in files_or_modules:            # CLI argument order
    if _is_ignored_file(...): result[something] = {... isignored: True}; continue
    if os.path.exists(something):
        modname = ".".join(modutils.modpath_from_file(something, path=additional_search_path))
                  except ImportError -> os.path.splitext(basename)[0]
        filepath = something + "/__init__.py" if isdir else something
    else:
        modname = something
        filepath = modutils.file_from_modpath(...)   # ImportError -> errors += fatal
    filepath = os.path.normpath(filepath)
    ... spec lookup; is_namespace / is_directory ...
    if not is_namespace:
        result.setdefault(filepath, default)["isarg"] = True     # arg file itself first
    if has_init or is_namespace or is_directory:
        for subfilepath in modutils.get_module_files(dirname(filepath) or ".",
                                                     ignore_list, list_all=is_namespace):
            subfilepath = os.path.normpath(subfilepath)
            if filepath == subfilepath: continue
            if _is_ignored_file(...): result[subfilepath] = {... isignored ...}; continue
            modpath = _modpath_from_file(subfilepath, is_namespace, path=additional_search_path)
            submodname = ".".join(modpath)
            isarg = subfilepath in result and result[subfilepath]["isarg"]
            result[subfilepath] = {...}
```

Key ordering facts:

- The dict is keyed by normalized file path; **insertion order = argument order, then for
  each package/namespace argument, `modutils.get_module_files` order**.
- `get_module_files` (astroid/modutils.py:445-477) uses **`os.walk(src_directory)`** with no
  sorting — i.e. **raw `os.scandir` order**, filesystem-dependent. Subdirectories are walked
  top-down after the files of a directory (os.walk default). For a bug-for-bug port the file
  order must be obtained from the same readdir order (do not sort).
  Empirically (§18.1) the order matched neither creation order nor strict alphabetical —
  it is whatever APFS returned.
- For `pylint .` on a directory **without** `__init__.py`: `modname` resolution for `.`
  falls into the `ImportError` fallback, `filepath = ./__init__.py` (normpath
  `__init__.py`), spec lookup fails → `is_namespace = not os.path.exists("__init__.py") = True`
  → the arg itself yields **no** result entry; all files come from
  `get_module_files(".", ignore_list, list_all=True)`, with module names from
  `_modpath_from_file(..., is_namespace=True)` — e.g. `a.py → "a"`, `sub/c.py → "sub.c"`
  (empirically confirmed, §18.3).
- For `pylint .` on a directory **with** `__init__.py`: the directory becomes a real
  package; `result[normpath("./__init__.py")]` is inserted first with the package's dotted
  name, then submodules.
- `_is_ignored_file` (expand_modules.py:55-67): basename in `ignore` list (default
  `("CVS",)`), basename matches `ignore-patterns` (default `^\.#`), or normpath matches
  `ignore-paths` (default empty). Ignored files produce `isignored: True` entries which
  increment `stats.skipped` and are *not* yielded.

---

## 6. Phase 1: `_get_asts`

pylinter.py:729-759, verbatim core:

```python
def _get_asts(self, fileitems, data, progress_reporter):
    ast_per_fileitem: dict[FileItem, nodes.Module | None] = {}
    progress_reporter.start_get_asts()
    for fileitem in fileitems:
        progress_reporter.get_ast_for_file(fileitem.filepath)
        self.set_current_module(fileitem.name, fileitem.filepath)
        try:
            ast_per_fileitem[fileitem] = self.get_ast(
                fileitem.filepath, fileitem.name, data
            )
        except astroid.AstroidBuildingError as ex:
            template_path = prepare_crash_report(
                ex, fileitem.filepath, self.crash_file_path
            )
            msg = get_fatal_error_message(fileitem.filepath, template_path)
            self.add_message(
                "astroid-error",
                args=(fileitem.filepath, msg),
                confidence=HIGH,
            )
    return ast_per_fileitem
```

- `set_current_module(fileitem.name, fileitem.filepath)` is called **before** building, so
  any message emitted during AST building (E0001/F0010/F0002) carries
  `module = FileItem.name` and (for node-less messages) `abspath = FileItem.filepath`
  *as given on the command line / from expansion* (normally a relative, normpath'ed path).
- If `get_ast` **returns None** (handled syntax/parse error), the fileitem **is** stored
  with value `None` (skipped later). If `get_ast` **raises** `AstroidBuildingError`
  (the unexpected-crash wrapper), the fileitem is **not inserted at all** — same skip
  effect, but note the dict difference.
- The result dict's insertion order = fileitem order = §5 order. This dict order drives
  phase 2.

---

## 7. `get_ast` error taxonomy

`PyLinter.get_ast` (pylinter.py:998-1038), verbatim:

```python
try:
    if data is None:
        return MANAGER.ast_from_file(filepath, modname, source=True)
    return astroid.builder.AstroidBuilder(MANAGER).string_build(
        data, modname, filepath
    )
except astroid.AstroidSyntaxError as ex:
    line = getattr(ex.error, "lineno", None)
    if line is None:
        line = 0
    self.add_message(
        "syntax-error",
        line=line,
        col_offset=getattr(ex.error, "offset", None),
        args=f"Parsing failed: '{ex.error}'",
        confidence=HIGH,
    )
except astroid.AstroidBuildingError as ex:
    self.add_message("parse-error", args=ex)
except Exception as ex:
    traceback.print_exc()
    raise astroid.AstroidBuildingError(
        "Building error when trying to create ast representation of module '{modname}'",
        modname=modname,
    ) from ex
return None
```

Harness uses `data is None` → `MANAGER.ast_from_file(filepath, modname, source=True)`.

### 7.1 `ast_from_file` (astroid/manager.py:131-168)

```python
if modname in self.astroid_cache and self.astroid_cache[modname].file == filepath:
    return self.astroid_cache[modname]
try:
    filepath = get_source_file(filepath, include_no_ext=True, prefer_stubs=self.prefer_stubs)
    source = True
except NoSourceFile:
    pass
if modname in self.astroid_cache and self.astroid_cache[modname].file == filepath:
    return self.astroid_cache[modname]
if source:
    return AstroidBuilder(self).file_build(filepath, modname)
if fallback and modname:
    return self.ast_from_module_name(modname)
raise AstroidBuildingError("Unable to build an AST for {path}.", path=filepath)
```

- `get_source_file` (astroid/modutils.py:480-504) **absolutizes the path**
  (`os.path.abspath`, line 493) and resolves to a `.py`/`.pyw`/`.pyi` source. From here on
  every astroid-internal path (and thus error text mentioning the file) is **absolute**.
- The cache (`astroid_cache`, keyed by modname) means two FileItems with the same module
  name but different files are each built (file mismatch defeats cache), with the second
  build overwriting the cache entry.

### 7.2 `file_build` (astroid/builder.py:113-149), verbatim error handling

```python
try:
    stream, encoding, data = open_source_file(path)
except OSError as exc:
    raise AstroidBuildingError(
        "Unable to load file {path}:\n{error}",
        modname=modname, path=path, error=exc,
    ) from exc
except (SyntaxError, LookupError) as exc:
    raise AstroidSyntaxError(
        "Python 3 encoding specification error or unknown encoding:\n"
        "{error}",
        modname=modname, path=path, error=exc,
    ) from exc
except UnicodeError as exc:  # wrong encoding
    # detect_encoding returns utf-8 if no encoding specified
    raise AstroidBuildingError(
        "Wrong or no encoding specified for {filename}.", filename=path
    ) from exc
```

`open_source_file` (builder.py:49-55): `detect_encoding` on the raw bytes, then
`open(filename, newline=None, encoding=encoding)` (universal newlines: `\r\n` → `\n`)
and `stream.read()`.

Then `_data_build(data, modname, path)` (builder.py:180-211):

```python
try:
    node, parser_module = _parse_string(data, type_comments=True, modname=modname)
except (TypeError, ValueError, SyntaxError, MemoryError) as exc:
    raise AstroidSyntaxError(
        "Parsing Python code failed:\n{error}",
        source=data, modname=modname, path=path, error=exc,
    ) from exc
```

and `_parse_string` (builder.py:486-505):

```python
parser_module = get_parser_module(type_comments=True)
try:
    parsed = parser_module.parse(data + "\n", type_comments=True, filename=modname)
except SyntaxError as exc:
    type_annot_related = re.search(r"#\s+type:", exc.text or "")
    if not (type_annot_related and type_comments):
        raise
    parser_module = get_parser_module(type_comments=False)
    parsed = parser_module.parse(data + "\n", type_comments=False)
```

`ParserModule.parse` (astroid/_ast.py:25-30): `ast.parse(string, filename=filename,
type_comments=...)` when `filename` is truthy, else `ast.parse(string, type_comments=...)`
(default CPython filename `"<unknown>"`).

**Crucially the `filename` passed to `ast.parse` is the dotted `modname`, not the file
path.** CPython's `SyntaxError.__str__` appends ` ({basename(filename)}, line {lineno})`
when both filename and lineno are set (and it takes the *basename* of the filename — a
dotted module name has no `/` so it appears verbatim).

### 7.3 E0001 `syntax-error` — exact spec

Message definition: `"E0001": ("%s", "syntax-error", ..., {"scope": WarningScope.LINE})`
(pylinter.py:204-209).

Emission (get_ast path):

- `line = getattr(ex.error, "lineno", None); if line is None: line = 0`
- `col_offset = getattr(ex.error, "offset", None)` — **SyntaxError `.offset` is 1-based**
  and is reported *as-is* (node-based messages use 0-based `col_offset`; E0001 columns are
  therefore 1-based — bug-for-bug requirement).
- `args = f"Parsing failed: '{ex.error}'"` — note: `ex.error` (the wrapped exception),
  **not** `str(ex)`; single quotes around it; no escaping if the message itself contains
  quotes.
- `confidence=HIGH`, `node=None`, no end positions.
- In `_add_one_message`, `line or 1` converts `line=0` to **1**, and `col_offset or 0`
  converts `None` to **0** (pylinter.py:1277-1278). So a SyntaxError without `lineno`
  prints as `:1:0:`.

Sub-cases of `ex.error` (all empirically confirmed, §18.2):

| Cause | wrapped exception | `str(ex.error)` | line:col printed |
|---|---|---|---|
| normal syntax error | `SyntaxError` from `ast.parse(data+"\n", filename=modname)` | `invalid syntax (pkg.bad_syntax, line 1)` — message + ` ({modname}, line {lineno})` | `lineno`:`offset` (1-based offset) |
| unclosed bracket etc. | `SyntaxError` | `'(' was never closed (pkg.mod, line 1)` | same |
| indentation | `IndentationError`/`TabError` (SyntaxError subclasses) | e.g. `expected an indented block after 'if' statement on line 1 (pkg.mod, line 2)` | same |
| type-comment retry also fails | `SyntaxError` from the **second** `ast.parse` (no filename) | `'(' was never closed (<unknown>, line 1)` — filename is `<unknown>` | same |
| null byte in source | `ValueError` from `ast.parse` | `source code string cannot contain null bytes` (no lineno/offset) | `1:0` |
| unknown encoding declaration | `SyntaxError` from `tokenize.detect_encoding` (no lineno) | `unknown encoding for '/abs/path/file.py': bad-codec` — **absolute** path (from `get_source_file`) | `1:0` |
| BOM/declared-encoding conflict | `SyntaxError` from `detect_encoding` | `encoding problem for '/abs/path/file.py': utf-8` | `1:0` |
| `LookupError` (defensive; practically unreachable) | `LookupError` | `str(LookupError)` | `1:0` |

The type-comment retry trigger condition: the *first* SyntaxError's `.text` (offending
source line) matches `re.search(r"#\s+type:", exc.text or "")` — i.e. requires at least
one whitespace between `#` and `type:`.

### 7.4 F0010 `parse-error` — exact spec

Definition: `"F0010": ("error while code parsing: %s", "parse-error", ...,
{"scope": WarningScope.LINE})` (pylinter.py:119-125).

Emitted at pylinter.py:1027-1028 with `args=ex` (the `AstroidBuildingError` instance,
non-syntax subclass). `%s` of the exception calls `AstroidError.__str__`
(astroid/exceptions.py:66-70):

```python
def __str__(self) -> str:
    try:
        return self.message.format(**vars(self))
    except ValueError:
        return self.message
```

Reachable variants (data=None path):

- decode failure (declared/default encoding can't decode the bytes; `UnicodeDecodeError`
  ⊂ `UnicodeError`): message `Wrong or no encoding specified for {filename}.` →
  `error while code parsing: Wrong or no encoding specified for /abs/path/file.py.`
  (absolute path; empirically confirmed §18.2).
- `OSError` opening the file (deleted between expansion and build): message
  `Unable to load file {path}:\n{error}` — **contains a newline**, which is printed
  verbatim inside the message line (the text reporter does not escape it).
- node-less: `line=None` → printed line **1**, column **0**. F is in `_SCOPE_EXEMPT = "FR"`
  (constants.py:31) so `check_message_definition` (message_definition.py:109-131) does not
  require a line for it. `confidence` is `None` → `UNDEFINED`.

### 7.5 F0002 `astroid-error` — exact spec + crash-report side effect

Definition: `"F0002": ("%s: %s", "astroid-error", ...)` (pylinter.py:111-118).

Trigger A (phase 1): any non-Astroid exception inside AST building. `get_ast` catches
`Exception`, prints **the original traceback to stderr** (`traceback.print_exc()`,
pylinter.py:1030), and re-raises `AstroidBuildingError(...)`. `_get_asts` catches it
(pylinter.py:748-757):

```python
template_path = prepare_crash_report(ex, fileitem.filepath, self.crash_file_path)
msg = get_fatal_error_message(fileitem.filepath, template_path)
self.add_message("astroid-error", args=(fileitem.filepath, msg), confidence=HIGH)
```

Trigger B (phase 2): see §10 — exceptions while linting a module.

`prepare_crash_report` (lint/utils.py:18-104):

- `issue_template_path = (Path(PYLINT_HOME) / datetime.now().strftime("pylint-crash-%Y-%m-%d-%H-%M-%S.txt")).resolve()`
  — `PYLINT_HOME` = `$PYLINTHOME` if set else `platformdirs.user_cache_dir("pylint")`
  (constants.py:50, 101-108; on macOS `~/Library/Caches/pylint`, on Linux
  `~/.cache/pylint`). `crash_file_path` is the class attribute
  `"pylint-crash-%Y-%m-%d-%H-%M-%S.txt"` (pylinter.py:283).
- **Side effects**: reads the analyzed file's full content (`open(filepath, encoding="utf8")`
  — may itself raise!), then **appends** a GitHub issue template containing the source and
  `traceback.format_exc()` to the crash file. If writing fails, prints a fallback message to
  stderr (lines 98-103). Returns the path.

`get_fatal_error_message` (lint/utils.py:107-112):

```python
return (
    f"Fatal error while checking '{filepath}'. "
    f"Please open an issue in our bug tracker so we address this. "
    f"There is a pre-filled template that you can use in '{issue_template_path}'."
)
```

So the final F0002 text is:

```
{filepath}: Fatal error while checking '{filepath}'. Please open an issue in our bug tracker so we address this. There is a pre-filled template that you can use in '{abs_resolved_crash_path}'.
```

with `{filepath}` = `FileItem.filepath` exactly as given, and the crash path
`.resolve()`d (symlinks resolved — on macOS `/tmp/...` becomes `/private/tmp/...`).
Reported at line **1** col **0** (node-less, line None), `confidence=HIGH`.
Empirically confirmed (§18.6).

**Port note:** the message argument content depends on the current wall-clock time
(crash filename) and on PYLINT_HOME — a harness comparing outputs must mask it or set
`PYLINTHOME`.

---

## 8. Checker preparation and `_astroid_module_checker`

pylinter.py:966-996, verbatim:

```python
@contextlib.contextmanager
def _astroid_module_checker(self):
    walker = ASTWalker(self)
    _checkers = self.prepare_checkers()
    tokencheckers = [c for c in _checkers if isinstance(c, checkers.BaseTokenChecker)]
    rawcheckers = [c for c in _checkers if isinstance(c, checkers.BaseRawFileChecker)]
    for checker in _checkers:
        checker.open()
        walker.add_checker(checker)

    yield functools.partial(self.check_astroid_module, walker=walker,
                            tokencheckers=tokencheckers, rawcheckers=rawcheckers)

    # notify global end
    self.stats.statement = walker.nbstatements
    for checker in reversed(_checkers):
        checker.close()
```

- The context opens **before phase 1** and closes **after phase 2** (check() lines
  719-727). `open()` is called once per run per prepared checker, in prepared order;
  `close()` in **reversed** prepared order, after all modules. Any message a checker emits
  in `close()` is attributed to the **last** `current_name`/`current_file`.
- `PyLinter.open` (pylinter.py:1108-1119) configures the astroid MANAGER
  (`max_inferable_values = limit_inference_results` (100), `module_denylist`,
  extension whitelist, `prefer_stubs`) and calls `stats.reset_message_count()` —
  **this zeroes the category counters but not `msg_status` and not `by_msg`** (linterstats.py:328-335).
  Consequence: config-phase messages do not count toward the score (§13/§14).

### 8.1 `prepare_checkers` (pylinter.py:588-598)

```python
def prepare_checkers(self):
    if not self.config.reports:
        self.disable_reporters()
    needed_checkers: list[BaseChecker] = [self]
    for checker in self.get_checkers()[1:]:
        messages = {msg for msg in checker.msgs if self.is_message_enabled(msg)}
        if messages or any(self.report_is_enabled(r[0]) for r in checker.reports):
            needed_checkers.append(checker)
    return needed_checkers
```

- A checker is kept iff **any** of its message ids is enabled (package-level check,
  `line=None` → `self._msgs_state.get(msgid, True)`), or one of its reports is enabled
  (none under `-E`, since `disable_reporters` ran).
- The PyLinter itself (`main`) is always first.

### 8.2 `get_checkers` ordering — the `total_ordering` quirk

`get_checkers` (pylinter.py:574-576):

```python
return sorted(c for _checkers in self._checkers.values() for c in _checkers)
```

- `self._checkers` is a `defaultdict(list)` keyed by checker **name**; key order =
  first-registration order (os.listdir-dependent); list order per name = registration
  order within that plugin's `register()` function.
- `BaseChecker` is decorated with **`@functools.total_ordering`** (base_checker.py:34) and
  defines `__gt__` and `__eq__` (base_checker.py:54-75):

```python
def __gt__(self, other):
    if not isinstance(other, BaseChecker): return False
    if self.name == MAIN_CHECKER_NAME: return False
    if other.name == MAIN_CHECKER_NAME: return True
    self_is_builtin = type(self).__module__.startswith("pylint.checkers")
    if self_is_builtin ^ type(other).__module__.startswith("pylint.checkers"):
        return not self_is_builtin
    return self.name > other.name

def __eq__(self, other):
    if not isinstance(other, BaseChecker): return False
    return f"{self.name}{self.msgs}" == f"{other.name}{other.msgs}"
```

  `total_ordering` synthesizes `__lt__ = not self.__gt__(other) and self != other`.
  For two *different classes with the same name* (e.g. `BasicChecker` vs
  `BasicErrorChecker`, both `"basic"`): `__gt__` is False both ways and `__eq__` is False
  (msgs differ) → **`a < b` and `b < a` are both True** — an inconsistent comparator.
  CPython's binary-insertion sort (lists < 64 elements) then inserts each later-seen
  same-name checker **to the left of** earlier ones. Net effect, empirically verified:
  **same-name groups appear in reverse registration order**; different names sort
  ascending; `main` sorts first.

- Final prepared order under the harness flags (empirical, pinned venv):

```
main              pylint.lint.pylinter.PyLinter                                (raw=no, token=no)
async             pylint.checkers.async_checker.AsyncChecker
basic             pylint.checkers.base.basic_checker.BasicChecker
basic             pylint.checkers.base.basic_error_checker.BasicErrorChecker
classes           pylint.checkers.classes.special_methods_checker.SpecialMethodsChecker
classes           pylint.checkers.classes.class_checker.ClassChecker
dataclass         pylint.checkers.dataclass_checker.DataclassChecker
exceptions        pylint.checkers.exceptions.ExceptionsChecker
imports           pylint.checkers.imports.ImportsChecker
logging           pylint.checkers.logging.LoggingChecker
match_statements  pylint.checkers.match_statements_checker.MatchStatementChecker
method_args       pylint.checkers.method_args.MethodArgsChecker
modified_iteration pylint.checkers.modified_iterating_checker.ModifiedIterationChecker
newstyle          pylint.checkers.newstyle.NewStyleConflictChecker
stdlib            pylint.checkers.stdlib.StdlibChecker
string            pylint.checkers.strings.StringFormatChecker
typecheck         pylint.checkers.typecheck.IterableChecker
typecheck         pylint.checkers.typecheck.TypeChecker
unicode_checker   pylint.checkers.unicode.UnicodeChecker                       (raw checker)
variables         pylint.checkers.variables.VariablesChecker
```

  - **`unicode_checker` is the only `BaseRawFileChecker`** in the prepared set; there are
    **no `BaseTokenChecker`s** (format checker is excluded — none of its messages are E/F
    and in-scope).
  - Registration orders that produce the reversed pairs: `base/__init__.py:43-50` registers
    `BasicErrorChecker` then `BasicChecker` (→ sorted: BasicChecker, BasicErrorChecker);
    `classes/__init__.py:16-18` registers `ClassChecker` then `SpecialMethodsChecker`
    (→ SpecialMethodsChecker, ClassChecker); `typecheck.py:2353-2355` registers
    `TypeChecker` then `IterableChecker` (→ IterableChecker, TypeChecker).
  - A port can hard-code this order; it only changes if checker names/classes change.

---

## 9. `ASTWalker` — registration and dispatch order

pylint/utils/ast_walker.py (entire file is authoritative; key parts):

### 9.1 `add_checker` (ast_walker.py:42-70)

```python
def add_checker(self, checker):
    vcids: set[str] = set()
    lcids: set[str] = set()
    visits = self.visit_events      # defaultdict(list)
    leaves = self.leave_events      # defaultdict(list)
    for member in dir(checker):     # dir() => alphabetically sorted member names
        cid = member[6:]
        if cid == "default":
            continue
        if member.startswith("visit_"):
            v_meth = getattr(checker, member)
            if self._is_method_enabled(v_meth):
                visits[cid].append(v_meth)
                vcids.add(cid)
        elif member.startswith("leave_"):
            l_meth = getattr(checker, member)
            if self._is_method_enabled(l_meth):
                leaves[cid].append(l_meth)
                lcids.add(cid)
    visit_default = getattr(checker, "visit_default", None)
    if visit_default:
        for cls in nodes.ALL_NODE_CLASSES:
            cid = cls.__name__.lower()
            if cid not in vcids:
                visits[cid].append(visit_default)
    # For now, we have no "leave_default" method in Pylint
```

- `_is_method_enabled` (ast_walker.py:37-40): a method is registered iff it has no
  `checks_msgs` attribute, or **any** of its `checks_msgs` is enabled.
  `checks_msgs` is set by the `@only_required_for_messages(...)` decorator
  (checkers/utils.py:480-501). This is a *registration-time* (per-run) gate — a method
  whose messages are all disabled never runs at all (a conservatism/perf path).
- Because `add_checker` is called once per prepared checker in prepared order (§8), the
  callback list per node-class cid is: **[checker1's method, checker2's method, …] in
  prepared-checker order**. (Each checker contributes at most one `visit_<cid>` per cid
  since `dir()` deduplicates names.)
- `visit_default`: under the harness flag set, **no prepared checker defines
  `visit_default`** (only format.py:496 and design_analysis.py:640 define it on checkers,
  both excluded; exceptions.py's `visit_default`s at 198/274 are on internal helper visitor
  classes, not checkers).

### 9.2 `walk` (ast_walker.py:72-102)

```python
def walk(self, astroid):
    cid = astroid.__class__.__name__.lower()
    visit_events = self.visit_events[cid]
    leave_events = self.leave_events[cid]
    try:
        if astroid.is_statement:
            self.nbstatements += 1
        for callback in visit_events:      # registration order
            callback(astroid)
        for child in astroid.get_children():
            self.walk(child)               # depth-first, child order
        for callback in leave_events:
            callback(astroid)
    except Exception:
        if self.exception_msg is False:
            file = getattr(astroid.root(), "file", None)
            print(f"Exception on node {astroid!r} in file '{file}'", file=sys.stderr)
            traceback.print_exc()
            self.exception_msg = True
        raise
```

- Dispatch key = `type(node).__name__.lower()` (e.g. `functiondef`, `asyncfunctiondef`,
  `call`, `joinedstr`).
- **Per-module message order from AST checkers is fully determined**: pre-order visit
  callbacks (checker order per node), recursing into `get_children()` order (astroid field
  order), post-order leave callbacks.
- `nbstatements` accumulates across **all modules** (one walker per run);
  `check_astroid_module` snapshots it per module (§10).
- On a checker exception: one-time stderr diagnostic
  (`Exception on node <repr> in file '<abs file>'` + traceback) — the `exception_msg`
  flag is **per-run**, so only the first crashing module gets the stderr print; then the
  exception propagates (→ F0002 path in `_lint_files`).

---

## 10. Phase 2: `_lint_files`

pylinter.py:771-796, verbatim:

```python
def _lint_files(self, ast_mapping, check_astroid_module, progress_reporter):
    progress_reporter.start_linting()
    for fileitem, module in ast_mapping.items():     # dict insertion order (= §5 order)
        progress_reporter.lint_file(fileitem.filepath)
        if module is None:
            continue                                  # parse failed in phase 1
        try:
            self._lint_file(fileitem, module, check_astroid_module)
            self.stats.modules_names.add(fileitem.filepath)
        except Exception as ex:
            template_path = prepare_crash_report(ex, fileitem.filepath, self.crash_file_path)
            msg = get_fatal_error_message(fileitem.filepath, template_path)
            if isinstance(ex, astroid.AstroidError):
                self.add_message("astroid-error", args=(fileitem.filepath, msg), confidence=HIGH)
            else:
                self.add_message("fatal", args=msg, confidence=HIGH)
```

- Any exception during a module's lint produces F0002 (astroid-error) — because
  `_lint_file` wraps **all** exceptions from `check_astroid_module` into
  `astroid.AstroidError` (pylinter.py:820-823) — or F0001 (`fatal`, args = message only)
  for exceptions raised outside that wrapper (very rare). Crash report side effects as §7.5.
  Linting then **continues with the next module**.
- `KeyboardInterrupt`/`SystemExit` are *not* caught (`except Exception`).

`_lint_file` (pylinter.py:798-830):

```python
self.set_current_module(file.name, file.filepath)
self._ignore_file = False
self.file_state = FileState(file.modpath, self.msgs_store, module)
self.current_file = module.file      # ABSOLUTE path from astroid
try:
    check_astroid_module(module)
except Exception as e:
    raise astroid.AstroidError from e
spurious_messages = self.file_state.iter_spurious_suppression_messages(self.msgs_store)
for msgid, line, args in spurious_messages:
    self.add_message(msgid, line, None, args)
```

- A fresh `FileState` per module (file_state.py:30-54): `base_name = file.modpath`,
  `_effective_max_line_number = module.tolineno`.
- `iter_spurious_suppression_messages` (file_state.py:225-251) yields
  `useless-suppression` (I0021) and `suppressed-message` (I0020) — both
  `default_enabled: False` **and** category I disabled under `-E`, so they never print in
  the harness; they are still *iterated* (dict insertion order of
  `_raw_module_msgs_state` / `_ignored_msgs`).

`check_astroid_module` (pylinter.py:1040-1060) snapshots statement counts:

```python
before_check_statements = walker.nbstatements
retval = self._check_astroid_module(ast_node, walker, rawcheckers, tokencheckers)
self.stats.by_module[self.current_name]["statement"] = walker.nbstatements - before_check_statements
return retval
```

`_check_astroid_module` (pylinter.py:1062-1106), the per-module sequence — this fixes the
**intra-module emission order**:

```python
try:
    tokens = utils.tokenize_module(node)
except tokenize.TokenError as ex:
    self.add_message(
        "syntax-error",
        line=ex.args[1][0],
        col_offset=ex.args[1][1],
        args=ex.args[0],
        confidence=HIGH,
    )
    return None

if not node.pure_python:
    self.add_message("raw-checker-failed", args=node.name)   # I0001; built-in modules only
else:
    self.process_tokens(tokens)         # pragma handling; may emit E0011 etc.
    if self._ignore_file:               # pylint: skip-file
        return False
    for raw_checker in rawcheckers:     # prepared order; harness: unicode_checker only
        raw_checker.process_module(node)
    for token_checker in tokencheckers: # harness: none
        token_checker.process_tokens(tokens)
walker.walk(node)                       # AST checkers, §9.2
return True
```

1. **Tokenization** — `tokenize_module` (pylint/utils/utils.py:151-154):
   `node.stream()` (re-opens `module.file` in binary since `file_bytes` is None on the
   file_build path; astroid scoped_nodes.py:287-301) + `list(tokenize.tokenize(readline))`.
   A `tokenize.TokenError` here yields **E0001** with
   `args = ex.args[0]` (e.g. `unexpected EOF in multi-line statement` — *no*
   `Parsing failed:` prefix!), `line = ex.args[1][0]`, `col = ex.args[1][1]`, and the
   module is otherwise skipped (`return None`). Rare (parse already succeeded).
2. **Pragma processing** — `PyLinter.process_tokens`
   (message_state_handler.py:347-444). Of its emissions, only
   **E0011 `unrecognized-inline-option`** (`"Unrecognized file option %r"`,
   pylinter.py:210-215) is enabled under the harness:
   for each COMMENT token matching `OPTION_PO` (`pragma_parser.py:14-27` — comment
   containing `pylint:`), `parse_pragma(match.group(2))` raising
   `UnRecognizedOptionError` → `add_message("unrecognized-inline-option",
   args=err.token, line=start[0])` (message_state_handler.py:435-439). `%r` ⇒ token in
   Python repr quotes. `UnRecognizedOptionError` is raised when an `=` follows a known
   keyword that doesn't take assignment / an unknown word, or when the pragma contains no
   keyword at all (pragma_parser.py:99-135; `err.token` = offending token, possibly `""`).
   Other pragma messages (I0010/I0011/I0013/I0022, W0012, R0022) are disabled but the
   `disable`/`enable` state mutations still happen here, *before* any checker messages —
   which is what makes inline suppression of same-line checker messages work.
   `skip-file` sets `_ignore_file` → module produces nothing further (returns False).
3. **Raw checkers** then **token checkers** in prepared order.
4. **AST walk** last.

So within one module the message stream order is: E0001-from-tokenize (alone) | E0011s in
token order → unicode-checker raw messages → walker messages in DFS order. **No sorting of
messages happens anywhere in pylint** — output order is exactly emission order.

---

## 11. Message emission

`add_message` (pylinter.py:1287-1319) resolves msgid/symbol via
`msgs_store.get_message_definitions` (handles old_names; may map one symbol to multiple
definitions) and calls `_add_one_message` per definition.

`_add_one_message` (pylinter.py:1195-1285), verbatim with annotations:

```python
message_definition.check_message_definition(line, node)   # sanity (raises InvalidMessageError)

# Look up "location" data of node if not yet supplied
if node:
    if node.position:                       # ClassDef/FunctionDef name position
        if not line:           line = node.position.lineno
        if not col_offset:     col_offset = node.position.col_offset
        if not end_lineno:     end_lineno = node.position.end_lineno
        if not end_col_offset: end_col_offset = node.position.end_col_offset
    else:
        if not line:           line = node.fromlineno
        if not col_offset:     col_offset = node.col_offset
        if not end_lineno:     end_lineno = node.end_lineno
        if not end_col_offset: end_col_offset = node.end_col_offset
```

- NB: the guards are truthiness (`if not line`) — an explicit `line=0` or `col_offset=0`
  is *overwritten* by node data. `node.position` is non-None essentially only for
  ClassDef/FunctionDef (the `class`/`def` name token span).

```python
if not self.is_message_enabled(message_definition.msgid, line, confidence):
    self.file_state.handle_ignored_message(
        self._get_message_state_scope(message_definition.msgid, line, confidence),
        message_definition.msgid, line)
    return                                   # suppressed: NO stats, NO msg_status
```

- Enablement (message_state_handler.py:315-345): confidence filter
  (`config.confidence` defaults to all five names → no-op), then per-line module state
  (`file_state._module_msgs_state[msgid][line]`, set by pragmas) with fallbacks; package
  state `self._msgs_state.get(msgid, True)` when `line is None` or no module override.
  Lines beyond `module.tolineno` use the closest preceding raw pragma state
  (lines 290-313).

```python
msg_cat = MSG_TYPES[message_definition.msgid[0]]        # "E"->"error", "F"->"fatal"...
self.msg_status |= MSG_TYPES_STATUS[message_definition.msgid[0]]   # F=1 E=2 W=4 R=8 C=16 I=0
self.stats.increase_single_message_count(msg_cat, 1)    # stats.error += 1 etc.
self.stats.increase_single_module_message_count(self.current_name, msg_cat, 1)
try:    self.stats.by_msg[message_definition.symbol] += 1
except KeyError: self.stats.by_msg[message_definition.symbol] = 1

msg = message_definition.msg
if args is not None:
    msg %= args                                          # printf-style interpolation

if node is None:
    module, obj = self.current_name, ""
    abspath = self.current_file
else:
    module, obj = utils.get_module_and_frameid(node)
    abspath = node.root().file                           # absolute (os.path.abspath in astroid)

if abspath is not None:
    path = abspath.replace(self.reporter.path_strip_prefix, "", 1)
else:
    path = "configuration"

self.reporter.handle_message(
    Message(
        message_definition.msgid,
        message_definition.symbol,
        MessageLocationTuple(
            abspath or "", path, module or "", obj,
            line or 1,             # None and 0 both become 1
            col_offset or 0,       # None becomes 0
            end_lineno, end_col_offset,
        ),
        msg,
        confidence,
    )
)
```

Notes:

- `msg %= args`: `args` can be a tuple or a single object (`"%s" % exception` works).
- `get_module_and_frameid` (pylint/utils/utils.py:91-104): walks `node.frame()` up to the
  root: `module` = root Module's `.name`; `obj` = dotted reverse-joined frame names
  (functions/classes; lambda frames contribute `"<lambda>"`). For module-level nodes
  `obj == ""`.
- `path` computation is a **plain `str.replace(prefix, "", 1)`** — it removes the *first
  occurrence anywhere*, not strictly a prefix; `path_strip_prefix` is the cwd at reporter
  construction + `os.sep`. On macOS `os.getcwd()` is symlink-resolved
  (`/private/tmp/...`), so a CLI path spelled `/tmp/...` is *not* stripped (observed in
  §18.6).
- Messages are **streamed**: `reporter.handle_message` is called synchronously inside
  `_add_one_message`; `TextReporter` prints immediately (§12). There is no buffering,
  sorting, or dedup of messages.
- `Message` (pylint/message/message.py:14-62) fields available to templates:
  `msg_id, symbol, msg, C (=msgid[0]), category (MSG_TYPES[C]), confidence, abspath, path,
  module, obj, line, column, end_line, end_column`.
- `check_message_definition` (message_definition.py:109-131): F/R ids are scope-exempt
  (`_SCOPE_EXEMPT = "FR"`, constants.py:31); LINE-scoped messages require `line is not None`
  and `node is None`; NODE-scoped require `node is not None`.

---

## 12. `TextReporter` — exact output format

pylint/reporters/text.py.

```python
class TextReporter(BaseReporter):
    name = "text"
    extension = "txt"
    line_format = "{path}:{line}:{column}: {msg_id}: {msg} ({symbol})"     # text.py:114

    def __init__(self, output=None):
        super().__init__(output)          # out = output or sys.stdout
        self._modules: set[str] = set()
        self._template = self.line_format
        self._fixed_template = self.line_format
```

`on_set_current_module` (text.py:123-144) — called from every `set_current_module`:

```python
template = str(self.linter.config.msg_template or self._template)
if template == self._template:
    return                       # harness: msg_template == "" -> falsy -> early return always
...
```

(Only relevant with `--msg-template`; it validates `{field}` names against `Message`
fields, warns + strips unknown ones into `_fixed_template`.)

`handle_message` (text.py:156-161):

```python
def handle_message(self, msg: Message) -> None:
    if msg.module not in self._modules:
        self.writeln(make_header(msg))       # "************* Module {msg.module}"
        self._modules.add(msg.module)
    self.write_message(msg)
```

- **Module header**: printed lazily, immediately before the *first message whose
  `msg.module` has not been seen yet* — exactly
  `************* Module {module}` (13 asterisks; `make_header`, text.py:105-106).
  The dedup set is **global for the run and keyed by the module name string** — two
  different files that map to the same module name share one header (empirically
  confirmed §18.7); a module with zero messages gets **no header**; phase-1 and phase-2
  messages for different modules naturally interleave headers in emission order.
- `msg.module` is `FileItem.name` for node-less messages (current_name) or the AST root
  module name for node messages (these are equal in practice since pylint passes
  `FileItem.name` as the astroid modname; for `pkg/__init__.py` astroid strips a trailing
  `.__init__`, builder.py:201-208).

`write_message` (text.py:146-154):

```python
self_dict = asdict(msg)
for key in ("end_line", "end_column"):
    self_dict[key] = self_dict[key] or ""        # None/0 -> ""
self.writeln(self._fixed_template.format(**self_dict))
```

With the default template the printed line is exactly:

```
{path}:{line}:{column}: {msg_id}: {msg} ({symbol})
```

`writeln` (base_reporter.py:43-48): `print(string, file=self.out)` → always appends `\n`;
on `UnicodeEncodeError` re-encodes with `errors="replace"` (base_reporter.py:50-52).

- Output stream: `sys.stdout` (default reporter built with no output;
  base_reporter.py:34).
- **Zero messages ⇒ zero bytes on stdout** (no headers, no summary, no trailing newline;
  empirically confirmed §18.8).
- `TextReporter.handle_message` does **not** call `super().handle_message`, so messages are
  not accumulated; `display_messages` (called once from `generate_reports`) is the no-op
  `BaseReporter.display_messages` (base_reporter.py:68-77 — docstring-only body);
  `on_close` is also a no-op hook (base_reporter.py:84-89).
  `TextReporter._display` (text.py:163-166) — which would print an empty line then the
  report sections — is only reached via `display_reports`, which under `-E` is never
  called (§13).

---

## 13. `generate_reports`, score, end-of-run

`Run` calls `linter.generate_reports(verbose=False)` after `check()` (run.py:240).
pylinter.py:1121-1147:

```python
def generate_reports(self, verbose=False):
    self.reporter.display_messages(report_nodes.Section())   # no-op for TextReporter
    if not self.file_state._is_base_filestate:
        # at least one module was actually linted (FileState replaced in _lint_file)
        previous_stats = load_results(self.file_state.base_name)   # reads PYLINT_HOME cache
        self.reporter.on_close(self.stats, previous_stats)         # no-op
        if self.config.reports:    sect = self.make_reports(...)   # False under -E
        else:                      sect = report_nodes.Section()
        if self.config.reports:    self.reporter.display_reports(sect)   # skipped
        score_value = self._report_evaluation(verbose)
        if self.config.persistent: save_results(...)               # False under -E
    else:
        self.reporter.on_close(self.stats, LinterStats())
        score_value = None
    return score_value
```

- `_is_base_filestate` is True only if **no module reached `_lint_file`** (all files
  failed to parse, or there were no files). In that case `score_value = None`.
- `load_results` (lint/caching.py:30-54): unpickles
  `{PYLINT_HOME}/{mangled_base_name}_1.stats` if present; returns None on any problem.
  Read-only side effect; affects only the optional "previous run" suffix of the score
  message — which is never displayed under `-E`.

`_report_evaluation` (pylinter.py:1149-1193):

```python
note = None
previous_stats = load_results(self.file_state.base_name)
if self.stats.statement == 0:
    return note                       # -> None
evaluation = self.config.evaluation   # default:
# "max(0, 0 if fatal else 10.0 - ((float(5 * error + warning + refactor + convention) / statement) * 10))"
try:
    stats_dict = {"fatal": ..., "error": ..., "warning": ..., "refactor": ...,
                  "convention": ..., "statement": ..., "info": ...}
    note = eval(evaluation, {}, stats_dict)
except Exception as ex:
    msg = f"An exception occurred while rating: {ex}"
else:
    self.stats.global_note = note
    msg = f"Your code has been rated at {note:.2f}/10"
    if previous_stats: ...append previous run...
    if verbose: ...append checked files...
if self.config.score:                  # False under -E
    sect = report_nodes.EvaluationSection(msg)
    self.reporter.display_reports(sect)
return note
```

**Under `-E` (`score=False`) the "Your code has been rated …" line is NEVER printed —
neither with zero messages nor with messages** (empirically confirmed). But the *score
value is still computed and returned*, and it drives the exit code (§14). The counters
feeding it were reset by `PyLinter.open()` at the start of `check()`, so only messages
emitted during checking count (config-phase messages do not).

End-of-run output summary under `-E`:

- messages only, streamed, each `\n`-terminated; no separator/summary/score; zero
  messages → empty stdout; stderr gets content only for crash paths (tracebacks,
  `Exception on node …`) and warnings.

---

## 14. Exit codes

`MSG_TYPES_STATUS = {"I": 0, "C": 16, "R": 8, "W": 4, "E": 2, "F": 1}` (constants.py:43).
`msg_status` starts 0 (pylinter.py:357) and is OR-ed in `_add_one_message`
(pylinter.py:1245) **for every displayed message, including config-phase ones**; it is
never reset.

`Run.__init__` exit block (run.py:245-260), verbatim:

```python
if exit:
    if linter.config.exit_zero:
        sys.exit(0)
    elif linter.any_fail_on_issues():
        sys.exit(self.linter.msg_status or 1)
    elif score_value is not None:
        if score_value >= linter.config.fail_under:    # fail_under default 10 (int)
            sys.exit(0)
        else:
            sys.exit(self.linter.msg_status or 1)
    else:
        sys.exit(self.linter.msg_status)
```

Harness facts: `exit_zero=False`, `fail_on` empty → `any_fail_on_issues()` False
(pylinter.py:540-541, checks `fail_on_symbols` ∩ `stats.by_msg`).

Decision table for the harness invocation:

| Situation | score_value | exit code |
|---|---|---|
| ≥1 module linted, no E/F messages during checking | 10.0 (≥10) | **0** |
| ≥1 module linted, ≥1 E (no F) | <10 | `msg_status` = **2** |
| ≥1 module linted, ≥1 F (e.g. F0010/F0002 in one file, others clean) | 0 (`0 if fatal`) | `msg_status` = **1** (F only) / **3** (F+E) |
| no module linted (all parse failures / fatals) | None | `msg_status` (1, 2, 3, …) |
| no files at all, no messages | None | **0** |
| config-phase W/R/C message only (e.g. W0012), modules clean | 10.0 | **0** (score branch wins despite msg_status=4!) |
| config-phase W + lint E | <10 | msg_status = **6** (4|2) |
| usage/config errors, `No files to lint` | — | **32** |

Empirical confirmations (§18): clean→0; E-only→2; F-only→1; E+F→3; W(config)+E→6;
W(config)-only→0.

Subtle: because `score_value >= fail_under` short-circuits to `sys.exit(0)`, non-E/F bits
in `msg_status` are reported **only when** an E/F (or fatal) message also dragged the score
below 10, or when `score_value is None`.

---

## 15. `set_current_module` and stats

pylinter.py:935-952:

```python
def set_current_module(self, modname, filepath=None):
    if not modname and filepath is None:
        return
    self.reporter.on_set_current_module(modname or "", filepath)
    self.current_name = modname
    self.current_file = filepath or modname
    self.stats.init_single_module(modname or "")
    if filepath:
        namespace = self._get_namespace_for_file(Path(filepath), self._directory_namespaces)
        if namespace:
            self.config = namespace or self._base_config
```

- `init_single_module` (linterstats.py:165-171) **overwrites**
  `by_module[modname] = {convention:0, error:0, fatal:0, info:0, refactor:0, statement:0, warning:0}`.
  Called in phase 1 *and* again in phase 2 for each module (so phase-1 counts for a module
  are zeroed before lint; messages from phase 1 still counted globally and in `by_msg`).
  Keys accumulate over the run: `""`, `"Command line"`, etc., then each module name.
- `current_file` falls back to `modname` when no filepath (this is how F0001 for a missing
  module gets `path == modname`).
- Directory-namespace config switching only matters with per-directory rcfiles — the
  harness registers only the base namespace (config_initialization.py:148).
- Called: config init (3×, §3), `_expand_files` errors, phase-1 per file, phase-2 per file,
  `_emit_stashed_messages`.

---

## 16. Ordering-dependency summary

Everything that determines byte-exact output order:

1. **CLI argument order** → expansion order (§5).
2. **`os.walk`/`os.scandir` order** inside `modutils.get_module_files` for
   package/namespace expansion — *filesystem order, unsorted*. The only
   platform-/filesystem-dependent ordering in the pipeline.
3. `expand_modules` result dict — insertion order (Python 3.7+ dict semantics).
4. Phase 1 messages strictly precede phase 2 messages.
5. `ast_per_fileitem` dict insertion order drives phase-2 module order.
6. Within a module: tokenize-E0001 | pragma-E0011 (token order) → raw checkers
   (prepared order) → token checkers (prepared order) → AST walk: per node, visit
   callbacks in prepared-checker order, children in astroid `get_children()` order,
   then leave callbacks.
7. Prepared-checker order: `main` first, then builtin checkers by name ascending;
   same-name groups in **reverse** registration order (Timsort + inconsistent
   `total_ordering` comparator, §8.2). Registration order itself comes from
   `os.listdir(pylint/checkers)` + each module's `register()`, but only same-name groups
   (registered together) are affected, so the final order is stable in practice.
8. `dir(checker)` (sorted) during walker registration — irrelevant to output order
   (one method per cid per checker).
9. `_msgs_by_category` / `_msgs_state` / `_stashed_messages` dicts — insertion order;
   only `_stashed_messages` order is observable (config-phase message order).
10. **No sorting and no deduplication of messages anywhere**; reporter prints in
    `handle_message` call order. Module headers dedup by module-name string only.

---

## 17. Main-checker message formats

(Defined in `MSGS`, pylinter.py:103-254. Scope: all LINE; F-ids scope-exempt.)

| id | symbol | template | args | location |
|---|---|---|---|---|
| E0001 | syntax-error | `%s` | `f"Parsing failed: '{ex.error}'"` (get_ast) or `ex.args[0]` (tokenize) | line = `ex.error.lineno` or 0→1 (get_ast) / `ex.args[1][0]` (tokenize); col = `ex.error.offset` (1-based) or None→0 / `ex.args[1][1]`; node=None; confidence HIGH |
| E0011 | unrecognized-inline-option | `Unrecognized file option %r` | `err.token` | line = COMMENT token start line; col 0; node=None |
| E0013 | bad-plugin-value | `Plugin '%s' is impossible to load, is it installed ? ('%s')` | `(modname, ModuleNotFoundError)` | line=0→1; module = "Command line or configuration file" |
| E0014 | bad-configuration-section | `Out-of-place setting encountered in top level configuration-section '%s' : '%s'` | (toml config path only) | line=0→1 |
| E0015 | unrecognized-option | `Unrecognized option found: %s` | comma-joined option names | line=0→1; module = rcfile path or "" |
| F0001 | fatal | `%s` | expansion: `str(ImportError)` minus cwd+sep; lint crash: `get_fatal_error_message(...)` | line None→1, col 0; module = modname / current |
| F0002 | astroid-error | `%s: %s` | `(fileitem.filepath, get_fatal_error_message(filepath, crash_template_path))` | line None→1, col 0; confidence HIGH |
| F0010 | parse-error | `error while code parsing: %s` | the `AstroidBuildingError` instance (`str()` = `message.format(**vars)`) | line None→1, col 0 |
| F0011 | config-parse-error | `error while parsing the configuration: %s` | — (no reachable emission site in 4.0.5) | — |
| F0202 | method-check-failed | (classes checker — covered in the classes doc) | | |

---

## 18. Empirical transcripts

All runs: pinned venv, `--rcfile=harness/empty.rcfile`, `-E`, cwd `/tmp/prylint_exp`
(macOS, so `os.getcwd()` resolves to `/private/tmp/prylint_exp`).

### 18.1 Mixed parse failures in a package (`pylint pkg -E`)

Files created in order: `pkg/__init__.py`, `bad_syntax.py` (`def f(:`),
`bad_encoding.py` (`# -*- coding: unknown-codec-xyz -*-`), `null_byte.py` (NUL byte),
`bad_decode.py` (`# -*- coding: ascii -*-` + UTF-8 bytes), `errors.py` (undefined names).

```
************* Module pkg.bad_syntax
pkg/bad_syntax.py:1:7: E0001: Parsing failed: 'invalid syntax (pkg.bad_syntax, line 1)' (syntax-error)
************* Module pkg.null_byte
pkg/null_byte.py:1:0: E0001: Parsing failed: 'source code string cannot contain null bytes' (syntax-error)
************* Module pkg.bad_decode
pkg/bad_decode.py:1:0: F0010: error while code parsing: Wrong or no encoding specified for /private/tmp/prylint_exp/pkg/bad_decode.py. (parse-error)
************* Module pkg.bad_encoding
pkg/bad_encoding.py:1:0: E0001: Parsing failed: 'unknown encoding for '/private/tmp/prylint_exp/pkg/bad_encoding.py': unknown-codec-xyz' (syntax-error)
************* Module pkg.errors
pkg/errors.py:2:0: E0602: Undefined variable 'undefined_var_use' (undefined-variable)
pkg/errors.py:3:6: E0602: Undefined variable 'undefined_name' (undefined-variable)
```

Exit code **3** (E|F). Note: phase-1 messages (4 modules) precede phase-2 (`errors.py`);
file order = os.walk order (≠ creation order, ≠ alphabetical); E0001 columns are the raw
1-based SyntaxError offsets; paths inside encoding-related messages are absolute+resolved.

### 18.2 Type-comment retry → `<unknown>` filename

`x = ( # type: int` →

```
typecomment_err.py:1:5: E0001: Parsing failed: ''(' was never closed (<unknown>, line 1)' (syntax-error)
```

(exit 2). Doubled quote `''('` is just the f-string quote plus the message's own quote —
no escaping.

### 18.3 `pylint .` on a non-package directory

`a.py`, `b.py`, `sub/__init__.py`, `sub/c.py`:

```
************* Module a
a.py:1:6: E0602: Undefined variable 'undefined_one' (undefined-variable)
************* Module b
b.py:1:6: E0602: Undefined variable 'undefined_two' (undefined-variable)
************* Module sub.c
sub/c.py:1:6: E0602: Undefined variable 'undefined_three' (undefined-variable)
```

exit 2. Namespace expansion (`list_all=True`), module names from
`_modpath_from_file(..., is_namespace=True)`.

### 18.4 F0001 for a missing argument

```
************* Module nonexistent_module_xyz
nonexistent_module_xyz:1:0: F0001: No module named nonexistent_module_xyz (fatal)
```

exit **1**.

### 18.5 Unknown `--disable` value under `-E`

`pylint clean/good.py -E --disable=notarealmsg123`:

```
************* Module Command line
Command line:1:0: W0012: Unknown option value for '--disable', expected a valid pylint message and got 'notarealmsg123' (unknown-option-value)
```

exit **0** (!) — msg_status=4 but score=10.0 ≥ fail_under → exit 0.
With an additional real E0602 in the linted file: exit **6** (4|2).

### 18.6 F0002 (simulated internal crash), `PYLINTHOME=/tmp/prylint_exp/pylinthome`

```
************* Module crashme_mod
/tmp/prylint_exp/crashme_mod.py:1:0: F0002: /tmp/prylint_exp/crashme_mod.py: Fatal error while checking '/tmp/prylint_exp/crashme_mod.py'. Please open an issue in our bug tracker so we address this. There is a pre-filled template that you can use in '/private/tmp/prylint_exp/pylinthome/pylint-crash-2026-06-09-20-41-53.txt'. (astroid-error)
```

msg_status 1; crash file created; original traceback printed to stderr by
`traceback.print_exc()`. Note `/tmp/...` arg path *not* cwd-stripped (path_strip_prefix is
`/private/tmp/prylint_exp/`), crash path `.resolve()`d to `/private/tmp/...`.

### 18.7 Header dedup by module name

`pylint dup1/x.py dup2/x.py -E` (both resolve to module `x`):

```
************* Module x
dup1/x.py:1:6: E0602: Undefined variable 'undef_a' (undefined-variable)
dup2/x.py:1:6: E0602: Undefined variable 'undef_b' (undefined-variable)
```

exit 2 — one header for two files.

### 18.8 Clean run

`pylint clean/good.py -E` → stdout **0 bytes**, stderr empty, exit **0**.
