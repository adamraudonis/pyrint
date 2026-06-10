# 01 — File discovery and module naming for `pylint .`

Spec extracted from:

- pylint 4.0.5 — `/Users/adamraudonis/Desktop/Projects/prylint/reference/pylint/pylint`
- astroid 4.0.4 — `/Users/adamraudonis/Desktop/Projects/prylint/reference/astroid/astroid`
- Ground-truth runtime: CPython 3.12.12 (POSIX/macOS; `os.path.normcase` is identity, `os.sep == '/'`).

Scope: the exact invocation `pylint . -E --disable=...`. `config.recursive` defaults to
`False` (`pylint/lint/base_options.py:347-354`), so **`PyLinter._discover_files`
(`pylint/lint/pylinter.py:636-670`) is NEVER executed** for this invocation. All
discovery goes through `expand_modules()`.

All behaviors below were verified empirically against the pinned versions in
`/Users/adamraudonis/Desktop/Projects/prylint/.venv-pylint` (Python 3.12.12,
pylint 4.0.5, astroid 4.0.4); empirical transcripts are in §14.

---

## Table of contents

1. [CLI argument handling before `check()`](#1-cli-argument-handling-before-check)
2. [sys.path composition](#2-syspath-composition)
3. [`PyLinter.check()` flow](#3-pylintercheck-flow)
4. [`_iterate_file_descrs`, `_expand_files`, `should_analyze_file`, `FileItem`](#4-_iterate_file_descrs-_expand_files-should_analyze_file-fileitem)
5. [`expand_modules()` — full walkthrough](#5-expand_modules--full-walkthrough)
6. [`_is_ignored_file` and default ignore settings](#6-_is_ignored_file-and-default-ignore-settings)
7. [`discover_package_path`](#7-discover_package_path)
8. [astroid `modutils`: module-path computation](#8-astroid-modutils-module-path-computation)
9. [astroid `modutils.get_module_files` and `os.walk` semantics](#9-astroid-modutilsget_module_files-and-oswalk-semantics)
10. [`file_info_from_modpath` / `spec.find_spec` — what happens for `.` and namespace dirs](#10-file_info_from_modpath--specfind_spec)
11. [Module naming in messages and output headers](#11-module-naming-in-messages-and-output-headers)
12. [Ordering, dedup, collisions, caching](#12-ordering-dedup-collisions-caching)
13. [The two top-level scenarios for `pylint .`, end-to-end](#13-the-two-top-level-scenarios-for-pylint--end-to-end)
14. [Empirical verification transcripts](#14-empirical-verification-transcripts)
15. [Porting checklist: every bailout / conservatism path](#15-porting-checklist-every-bailout--conservatism-path)

---

## 1. CLI argument handling before `check()`

### 1.1 Entry points and `modify_sys_path`

Two ways pylint starts:

- **Console script** (`<venv>/bin/pylint`): generated wrapper calls
  `pylint.run_pylint()` directly. `modify_sys_path()` is **NOT** called.
  `sys.path[0]` is the (symlink-resolved, absolute) directory containing the
  script — i.e. the venv `bin` dir (verified empirically on 3.12.12). The cwd is
  therefore *not* on `sys.path` at startup.
- **`python -m pylint`** (`pylint/__main__.py:8-10`): calls
  `pylint.modify_sys_path()` *then* `pylint.run_pylint()`.

`modify_sys_path` (`pylint/__init__.py:69-95`), verbatim:

```python
def modify_sys_path() -> None:
    cwd = os.getcwd()
    if sys.path[0] in ("", ".", cwd):
        sys.path.pop(0)
    env_pythonpath = os.environ.get("PYTHONPATH", "")
    if env_pythonpath.startswith(":") and env_pythonpath not in (f":{cwd}", ":."):
        sys.path.pop(0)
    elif env_pythonpath.endswith(":") and env_pythonpath not in (f"{cwd}:", ".:"):
        sys.path.pop(1)
```

Net effect for both entry points: **cwd is not on `sys.path` when `check()` is
reached** (console script: never was; `-m`: popped). It is re-added in resolved
form by `augmented_sys_path` — see §2.

### 1.2 Glob expansion of positional args

`Run.__init__` (`pylint/lint/run.py:186-187`) calls `_config_initialization`,
which returns the residual positional args after option parsing, **glob-expanded**
(`pylint/config/config_initialization.py:150-161`):

```python
    return list(
        chain.from_iterable(
            # NOTE: 'or [arg]' is needed in the case the input file or directory does
            # not exist and 'glob(arg)' cannot find anything. Without this we would
            # not be able to output the fatal import error for this module later on,
            # as it would get silently ignored.
            glob(arg, recursive=True) or [arg]
            for arg in parsed_args_list
        )
    )
```

For the literal argument `.` (no glob magic chars): `glob('.', recursive=True)`
returns `['.']`, so `files_or_modules == ['.']`. (A nonexistent arg passes
through unchanged thanks to `or [arg]`, later producing F0001.)

### 1.3 `Run` invokes `check`

`pylint/lint/run.py:206-239`:

- If `not args` (or `--disable=all`-equivalent config): prints
  `"No files to lint: exiting."` and `sys.exit(32)` (run.py:206-211).
- `jobs < 0` → exit 32; `jobs == 0` → `_cpu_count()`; default `jobs` is 1, so
  the **single-process path** is taken.
- Finally `linter.check(args)` (run.py:233 with `--output`, run.py:239 without).

---

## 2. sys.path composition

### 2.1 `extra_packages_paths`

`PyLinter.check` (`pylint/lint/pylinter.py:686-693`):

```python
        extra_packages_paths = list(
            dict.fromkeys(
                [
                    discover_package_path(file_or_module, self.config.source_roots)
                    for file_or_module in files_or_modules
                ]
            ).keys()
        )
```

- One `discover_package_path` per CLI arg (§7), **deduplicated preserving first
  occurrence order** via `dict.fromkeys`.
- For `pylint .`: a single entry.
  - cwd has **no** `__init__.py` → `[os.path.realpath(cwd)]`.
  - cwd **has** `__init__.py` → `[realpath of the first ancestor directory that
    does not contain __init__.py]` (e.g. the parent dir).

### 2.2 `augmented_sys_path`

`pylint/lint/utils.py:115-135`, verbatim:

```python
def _augment_sys_path(additional_paths: Sequence[str]) -> list[str]:
    original = list(sys.path)
    changes = []
    seen = set()
    for additional_path in additional_paths:
        if additional_path not in seen:
            changes.append(additional_path)
            seen.add(additional_path)

    sys.path[:] = changes + sys.path
    return original


@contextlib.contextmanager
def augmented_sys_path(additional_paths: Sequence[str]) -> Iterator[None]:
    """Augment 'sys.path' by adding non-existent entries from additional_paths."""
    original = _augment_sys_path(additional_paths)
    try:
        yield
    finally:
        sys.path[:] = original
```

Note: dedup is only *within* `additional_paths`; an entry already present in
`sys.path` is still prepended (duplicate). The paths are **prepended** in order.

### 2.3 sys.path during discovery

`check()` enters `augmented_sys_path(extra_packages_paths)` twice
(pylinter.py:710 and :719). `_iterate_file_descrs` is a **lazy generator**
created at pylinter.py:715 but only *consumed* inside `_get_asts`
(pylinter.py:740), which runs inside the second `augmented_sys_path` block
(pylinter.py:719-722). Therefore when `expand_modules` actually executes:

```
sys.path = [ package_path,            # realpath(cwd) or ancestor; prepended
             <script-dir or remains>, # venv bin dir for console script
             ...stdlib...,
             ...site-packages... ]
```

This matters for:

- `expand_modules` line 83: `path = sys.path.copy()` — the copy contains
  `package_path` at the front.
- `modutils.modpath_from_file*` — its candidate prefix list includes the live
  `sys.path` (modutils.py:282).
- `astroid.interpreter._import.util.is_namespace` — uses the **runtime import
  system** (`_find_spec_from_path`), which searches the live `sys.path`; with
  `package_path` prepended, sub-directories of cwd lacking `__init__.py` are
  resolvable as PEP 420 namespace packages.
- `spec.find_spec`'s `ImportlibFinder` when it falls back to `sys.path`
  (spec.py:149) — though for our flows an explicit path is always passed.

### 2.4 `additional_search_path` inside `expand_modules`

`expand_modules` (`pylint/lint/expand_modules.py:99-100`):

```python
        module_package_path = discover_package_path(something, source_roots)
        additional_search_path = [".", module_package_path, *path]
```

where `path = sys.path.copy()` was taken once at function entry (line 83). So
the search list for every modpath/spec lookup is:

```
['.', package_path, package_path(again, via augmented sys.path), bin_dir, ...stdlib..., ...site-packages...]
```

The literal `'.'` entry **never matches anything** in
`_get_relative_base_path` (because `os.path.abspath(file)` starts with `/` and
cannot start with the prefix `'.'` — see §8.2), so the effective first match for
files under cwd is `package_path`.

---

## 3. `PyLinter.check()` flow

`pylint/lint/pylinter.py:672-727`, verbatim (relevant portion):

```python
    def check(self, files_or_modules: Sequence[str]) -> None:
        self.initialize()
        if self.config.recursive:
            files_or_modules = tuple(self._discover_files(files_or_modules))
        if self.config.from_stdin:
            if len(files_or_modules) != 1:
                raise exceptions.InvalidArgsError(
                    "Missing filename required for --from-stdin"
                )

        extra_packages_paths = list(
            dict.fromkeys(
                [
                    discover_package_path(file_or_module, self.config.source_roots)
                    for file_or_module in files_or_modules
                ]
            ).keys()
        )

        # TODO: Move the parallel invocation into step 3 of the checking process
        if not self.config.from_stdin and self.config.jobs > 1:
            original_sys_path = sys.path[:]
            check_parallel(...)
            sys.path = original_sys_path
            return

        progress_reporter = ProgressReporter(self.verbose)

        # 1) Get all FileItems
        with augmented_sys_path(extra_packages_paths):
            if self.config.from_stdin:
                fileitems = self._get_file_descr_from_stdin(files_or_modules[0])
                data: str | None = _read_stdin()
            else:
                fileitems = self._iterate_file_descrs(files_or_modules)
                data = None

        # The contextmanager also opens all checkers and sets up the PyLinter class
        with augmented_sys_path(extra_packages_paths):
            with self._astroid_module_checker() as check_astroid_module:
                # 2) Get the AST for each FileItem
                ast_per_fileitem = self._get_asts(fileitems, data, progress_reporter)

                # 3) Lint each ast
                self._lint_files(
                    ast_per_fileitem, check_astroid_module, progress_reporter
                )
```

For our invocation: `recursive=False`, `from_stdin=False`, `jobs=1` →
single-process path; `fileitems` generator → `_get_asts` builds a
`dict[FileItem, Module|None]` in **generator yield order**
(pylinter.py:729-759); `_lint_files` iterates `ast_mapping.items()` in that same
insertion order (pylinter.py:779).

`_get_asts` per item (pylinter.py:740-757): `set_current_module(name, filepath)`
then `self.get_ast(filepath, name, data=None)`; on `AstroidBuildingError` it
emits **F0002 `astroid-error`** with `args=(fileitem.filepath, msg)` and stores
nothing (the FileItem is *absent* from the dict, not None — note: actually the
exception path skips the assignment, so the key is missing entirely).
`get_ast` itself (pylinter.py:998-1038) emits **E0001 `syntax-error`** (line =
`ex.error.lineno` or 0, col_offset = `ex.error.offset`, args
`f"Parsing failed: '{ex.error}'"`) or **F0010 `parse-error`** (`args=ex`) and
returns `None`; `None` entries are skipped by `_lint_files`
(pylinter.py:781-782).

---

## 4. `_iterate_file_descrs`, `_expand_files`, `should_analyze_file`, `FileItem`

### 4.1 `FileItem`

`pylint/typing.py:31-42`:

```python
class FileItem(NamedTuple):
    name: str       # full dotted module name        <- descr["name"]
    filepath: str   # path of the file (normpath'd)  <- descr["path"]
    modpath: str    # NOTE: receives descr["basename"], i.e. the dotted module
                    # name of the *base* CLI argument, NOT a path
```

`ModuleDescriptionDict` (`pylint/typing.py:45-53`): keys `path`, `name`,
`isarg`, `basepath`, `basename`, `isignored`.
`ErrorDescriptionDict` (`pylint/typing.py:56-61`): `key: Literal["fatal"]`,
`mod: str`, `ex: ImportError | SyntaxError`.

`FileItem.modpath` (= `basename`) is what gets passed to
`FileState(file.modpath, ...)` in `_lint_file` (pylinter.py:815) — for `pylint .`
in a no-`__init__.py` cwd, every FileItem has `modpath == "."`.

### 4.2 `_iterate_file_descrs`

`pylint/lint/pylinter.py:900-913`, verbatim:

```python
    def _iterate_file_descrs(
        self, files_or_modules: Sequence[str]
    ) -> Iterator[FileItem]:
        for descr in self._expand_files(files_or_modules).values():
            name, filepath, is_arg = descr["name"], descr["path"], descr["isarg"]
            if descr["isignored"]:
                self.stats.skipped += 1
            elif self.should_analyze_file(name, filepath, is_argument=is_arg):
                yield FileItem(name, filepath, descr["basename"])
```

- Iterates the result **dict in insertion order** — this defines lint order and
  output order.
- `isignored` entries: counted in `stats.skipped`, never linted.
- Everything else passes through `should_analyze_file`.

### 4.3 `should_analyze_file`

`pylint/lint/pylinter.py:600-620` (staticmethod):

```python
    @staticmethod
    def should_analyze_file(modname: str, path: str, is_argument: bool = False) -> bool:
        if is_argument:
            return True
        return path.endswith((".py", ".pyi"))
```

Consequence: files discovered by the walk with extensions `.so`, `.pyd`, `.pyw`
(which `_is_python_file` accepts, §9.3) are silently dropped here unless they
were the CLI argument (`isarg=True` bypasses the extension check). For
`pylint .`, the only possible `isarg` entry is `__init__.py`, which ends with
`.py` anyway.

### 4.4 `_expand_files` — error to F0001

`pylint/lint/pylinter.py:915-933`, verbatim:

```python
    def _expand_files(
        self, files_or_modules: Sequence[str]
    ) -> dict[str, ModuleDescriptionDict]:
        """Get modules and errors from a list of modules and handle errors."""
        result, errors = expand_modules(
            files_or_modules,
            self.config.source_roots,
            self.config.ignore,
            self.config.ignore_patterns,
            self._ignore_paths,
        )
        for error in errors:
            message = modname = error["mod"]
            key = error["key"]
            self.set_current_module(modname)
            if key == "fatal":
                message = str(error["ex"]).replace(os.getcwd() + os.sep, "")
            self.add_message(key, args=message)
        return result
```

- `self._ignore_paths` was set from `self.config.ignore_paths` in
  `initialize()` (pylinter.py:629).
- **F0001 `fatal`** message template is `"%s"` (pylinter.py:104-110, scope
  LINE). Args = `str(ImportError).replace(cwd + os.sep, "")` (only the *first*
  occurrence? No — `str.replace` with no count replaces ALL occurrences; note
  pylinter.py:931 passes no count). Reported with `node=None`, `line=None` →
  rendered at line 1, col 0; `module` = the failed modname
  (`set_current_module(modname)` set `current_name`), `abspath` =
  `current_file` = modname (because `filepath=None` →
  `self.current_file = filepath or modname`, pylinter.py:943).
- For `pylint .` with an existing cwd, `errors` is always empty (the arg exists
  on disk, so the ImportError branch that appends errors is unreachable —
  expand_modules.py:113-124 only runs for nonexistent args).

Other F-messages for context (all in pylinter.py:103-131): F0002
`astroid-error` template `"%s: %s"`; F0010 `parse-error` template
`"error while code parsing: %s"`; F0011 `config-parse-error` template
`"error while parsing the configuration: %s"` (raised during config init, not
discovery).

---

## 5. `expand_modules()` — full walkthrough

Source: `pylint/lint/expand_modules.py:71-185`. Signature:

```python
def expand_modules(
    files_or_modules: Sequence[str],   # ['.']
    source_roots: Sequence[str],       # () by default
    ignore_list: list[str],            # config.ignore         = ["CVS"]
    ignore_list_re: list[Pattern],     # config.ignore_patterns = [re.compile(r"^\.#")]
    ignore_list_paths_re: list[Pattern]# config.ignore_paths    = []
) -> tuple[dict[str, ModuleDescriptionDict], list[ErrorDescriptionDict]]:
```

Verbatim body with annotations:

```python
    result: dict[str, ModuleDescriptionDict] = {}
    errors: list[ErrorDescriptionDict] = []
    path = sys.path.copy()                      # line 83 — copied ONCE, includes
                                                # augmented package_path (see §2.3)

    for something in files_or_modules:          # line 85
        basename = os.path.basename(something)  # '.' -> '.'
        if _is_ignored_file(
            something, ignore_list, ignore_list_re, ignore_list_paths_re
        ):                                      # lines 87-89  [BAILOUT #1]
            result[something] = {               # key = RAW arg string, not normpath
                "path": something,
                "name": "",
                "isarg": False,
                "basepath": something,
                "basename": "",
                "isignored": True,
            }
            continue
        module_package_path = discover_package_path(something, source_roots)  # line 99
        additional_search_path = [".", module_package_path, *path]            # line 100
        if os.path.exists(something):           # line 101 — '.' always exists
            # this is a file or a directory
            try:
                modname = ".".join(
                    modutils.modpath_from_file(something, path=additional_search_path)
                )                               # lines 103-106
            except ImportError:                 # [BAILOUT #2]
                modname = os.path.splitext(basename)[0]
                # for '.': os.path.splitext('.') == ('.', '') -> modname = '.'
            if os.path.isdir(something):        # line 109
                filepath = os.path.join(something, "__init__.py")   # './__init__.py'
            else:
                filepath = something
        else:
            # suppose it's a module or package  (NOT taken for '.')
            modname = something
            try:
                filepath = modutils.file_from_modpath(
                    modname.split("."), path=additional_search_path
                )
                if filepath is None:            # builtin module  [BAILOUT #3]
                    continue
            except ImportError as ex:           # [BAILOUT #4] -> F0001
                errors.append({"key": "fatal", "mod": modname, "ex": ex})
                continue
        filepath = os.path.normpath(filepath)   # line 125  './__init__.py' -> '__init__.py'
        modparts = (modname or something).split(".")   # line 126
                                                # modname '.'  -> ['', '']  (!!)
                                                # modname 'pkg'-> ['pkg']
        try:
            spec = modutils.file_info_from_modpath(
                modparts, path=additional_search_path
            )                                   # lines 127-130
        except ImportError:                     # [BAILOUT #5]
            # Might not be acceptable, don't crash.
            is_namespace = not os.path.exists(filepath)
            is_directory = os.path.isdir(something)
        else:
            is_namespace = modutils.is_namespace(spec)     # type == PY_NAMESPACE
            is_directory = modutils.is_directory(spec)     # type == PKG_DIRECTORY
        if not is_namespace:                    # line 138
            default: ModuleDescriptionDict = {
                "path": filepath,
                "name": modname,
                "isarg": True,
                "basepath": filepath,
                "basename": modname,
                "isignored": False,
            }
            result.setdefault(filepath, default)["isarg"] = True   # line 147
            # setdefault: if a previous arg's walk already inserted this filepath,
            # the existing entry's name/basename are KEPT, only isarg is forced True.
        has_init = (
            modparts[-1] != "__init__" and os.path.basename(filepath) == "__init__.py"
        )                                       # lines 148-150
        if has_init or is_namespace or is_directory:       # line 151
            for subfilepath in modutils.get_module_files(
                os.path.dirname(filepath) or ".", ignore_list, list_all=is_namespace
            ):                                  # lines 152-154
                subfilepath = os.path.normpath(subfilepath)
                if filepath == subfilepath:     # [BAILOUT #6] skip the arg itself
                    continue
                if _is_ignored_file(
                    subfilepath, ignore_list, ignore_list_re, ignore_list_paths_re
                ):                              # [BAILOUT #7]
                    result[subfilepath] = {     # key = normpath'd subfile path
                        "path": subfilepath,
                        "name": "",
                        "isarg": False,
                        "basepath": subfilepath,
                        "basename": "",
                        "isignored": True,
                    }
                    continue

                modpath = _modpath_from_file(
                    subfilepath, is_namespace, path=additional_search_path
                )                               # lines 171-173 — NOTE: may raise
                                                # ImportError (uncaught -> crash);
                                                # in practice package_path always matches
                submodname = ".".join(modpath)
                # Preserve arg flag if module is also explicitly given.
                isarg = subfilepath in result and result[subfilepath]["isarg"]
                result[subfilepath] = {         # OVERWRITES any earlier entry,
                    "path": subfilepath,        # but preserves isarg
                    "name": submodname,
                    "isarg": isarg,
                    "basepath": filepath,       # the base arg's filepath
                    "basename": modname,        # the base arg's modname
                    "isignored": False,
                }
    return result, errors
```

Helper at the top of the file (`expand_modules.py:18-24`):

```python
def _modpath_from_file(filename: str, is_namespace: bool, path: list[str]) -> list[str]:
    def _is_package_cb(inner_path: str, parts: list[str]) -> bool:
        return modutils.check_modpath_has_init(inner_path, parts) or is_namespace

    return modutils.modpath_from_file_with_callback(
        filename, path=path, is_package_cb=_is_package_cb
    )
```

When `is_namespace=True`, the callback is **always true**, so the first
path-prefix match wins unconditionally. When `is_namespace=False`, the prefix
must additionally satisfy `check_modpath_has_init` (§8.4).

Key dict facts:

- `result` is keyed by **`os.path.normpath(filepath)`** (raw arg string for
  ignored args). Two distinct paths are never merged; **dedup happens only when
  the same normalized path is seen twice** (e.g. arg listed twice, or arg also
  found by a walk).
- **Insertion order of `result` is the output order** of the whole lint run
  (via `_iterate_file_descrs` → `_get_asts` dict → `_lint_files`).

---

## 6. `_is_ignored_file` and default ignore settings

`pylint/lint/expand_modules.py:50-67`, verbatim:

```python
def _is_in_ignore_list_re(element: str, ignore_list_re: list[Pattern[str]]) -> bool:
    """Determines if the element is matched in a regex ignore-list."""
    return any(file_pattern.match(element) for file_pattern in ignore_list_re)


def _is_ignored_file(
    element: str,
    ignore_list: list[str],
    ignore_list_re: list[Pattern[str]],
    ignore_list_paths_re: list[Pattern[str]],
) -> bool:
    element = os.path.normpath(element)
    basename = Path(element).absolute().name
    return (
        basename in ignore_list
        or _is_in_ignore_list_re(basename, ignore_list_re)
        or _is_in_ignore_list_re(element, ignore_list_paths_re)
    )
```

Exact semantics:

- `element` is first `normpath`'d (`./x.py` → `x.py`, `.` → `.`).
- `basename = Path(element).absolute().name`: `Path.absolute()` prepends cwd
  **without resolving symlinks**. For `element='.'` this yields the **cwd's own
  basename** — so running `pylint .` inside a directory literally named `CVS`
  ignores everything (verified empirically: returns True). For `sub/x.py` it
  yields `x.py`.
- `ignore_list` (`--ignore`, dest `black_list`): exact basename membership.
  Default `("CVS",)` — `pylint/constants.py:52` `DEFAULT_IGNORE_LIST = ("CVS",)`,
  wired in `pylint/lint/base_options.py:41-51`.
- `ignore_list_re` (`--ignore-patterns`, dest `black_list_re`): `re.match`
  (anchored at start, not fullmatch) against the **basename**. Default
  `(re.compile(r"^\.#"),)` — base_options.py:53-63 (Emacs lock files).
- `ignore_list_paths_re` (`--ignore-paths`): `re.match` against the
  **normpath'd element** (relative path as passed). Default `[]` —
  base_options.py:65-75.

The same function (same defaults) is also used by `_discover_files`
(pylinter.py:651-656) and `_get_file_descr_from_stdin` (pylinter.py:881-886),
neither of which runs for our invocation.

---

## 7. `discover_package_path`

`pylint/lint/expand_modules.py:27-47`, verbatim:

```python
def discover_package_path(modulepath: str, source_roots: Sequence[str]) -> str:
    """Discover package path from one its modules and source roots."""
    dirname = os.path.realpath(os.path.expanduser(modulepath))
    if not os.path.isdir(dirname):
        dirname = os.path.dirname(dirname)

    # Look for a source root that contains the module directory
    for source_root in source_roots:
        source_root = os.path.realpath(os.path.expanduser(source_root))
        if os.path.commonpath([source_root, dirname]) in [dirname, source_root]:
            return source_root

    # Fall back to legacy discovery by looking for __init__.py upwards as
    # it's the only way given that source root was not found or was not provided
    while True:
        if not os.path.exists(os.path.join(dirname, "__init__.py")):
            return dirname
        old_dirname = dirname
        dirname = os.path.dirname(dirname)
        if old_dirname == dirname:
            return os.getcwd()
```

- Input is `realpath(expanduser(...))` — **symlinks resolved**. For `'.'` this
  is `realpath(cwd)` (on macOS `/tmp/...` becomes `/private/tmp/...`).
- `source_roots` default is `()` (base_options.py:335-345) → loop skipped.
- Walk-up: returns the **first ancestor (starting at the dir itself) whose
  `__init__.py` does not exist**. Only `__init__.py` exactly — `__init__.pyi`
  does **not** count here. If the walk reaches the filesystem root and it has
  `__init__.py` (degenerate), returns `os.getcwd()`.
- Called twice per arg: once in `check()` for `extra_packages_paths`
  (pylinter.py:689) and once inside `expand_modules` (expand_modules.py:99).
  Both calls return the same value.

---

## 8. astroid `modutils`: module-path computation

All citations `astroid/modutils.py`.

### 8.1 Normalization helpers

```python
def _normalize_path(path: str) -> str:                 # lines 107-115
    return os.path.normcase(os.path.realpath(path))

@lru_cache
def _cache_normalize_path_(path: str) -> str:          # lines 141-143
    return _normalize_path(path)

def _cache_normalize_path(path: str) -> str:           # lines 146-153
    if not path:  # don't cache result for ''
        return _normalize_path(path)
    return _cache_normalize_path_(path)
```

On POSIX `normcase` is identity; `realpath` resolves symlinks and makes
absolute. `_normalize_path('')` = `realpath('')` = cwd.

`_path_from_filename` (lines 118-124) is identity on CPython (Jython-only
transformation).

### 8.2 `_is_subpath` and `_get_relative_base_path`

Lines 235-273, verbatim:

```python
def _is_subpath(path: str, base: str) -> bool:
    path = os.path.normcase(os.path.normpath(path))
    base = os.path.normcase(os.path.normpath(base))
    if not path.startswith(base):
        return False
    return (len(path) == len(base)) or (path[len(base)] == os.path.sep)


def _get_relative_base_path(filename: str, path_to_check: str) -> list[str] | None:
    path_to_check = os.path.normcase(os.path.normpath(path_to_check))

    abs_filename = os.path.abspath(filename)
    if _is_subpath(abs_filename, path_to_check):
        base_path = os.path.splitext(abs_filename)[0]
        relative_base_path = base_path[len(path_to_check) :].lstrip(os.path.sep)
        return [pkg for pkg in relative_base_path.split(os.sep) if pkg]

    real_filename = os.path.realpath(filename)
    if _is_subpath(real_filename, path_to_check):
        base_path = os.path.splitext(real_filename)[0]
        relative_base_path = base_path[len(path_to_check) :].lstrip(os.path.sep)
        return [pkg for pkg in relative_base_path.split(os.sep) if pkg]

    return None
```

Exact behaviors to replicate:

- Tries `os.path.abspath(filename)` (cwd-joined, **symlinks not resolved**)
  first; if that's not under the prefix, retries with
  `os.path.realpath(filename)` (symlinks resolved). This makes names correct
  when cwd is reached through a symlink, because `path_to_check`
  (= `package_path`) is realpath'd by `discover_package_path`.
- `os.path.splitext` strips only the **last** extension:
  `pkg/sub/__init__.py` → `pkg/sub/__init__` → parts
  `['pkg', 'sub', '__init__']`. **`__init__` is NOT stripped here** — the
  dotted name for a walked `pkg/sub/__init__.py` is `pkg.sub.__init__`
  (verified; the `.__init__` suffix is stripped later by astroid's builder, §11.1).
- Dots in directory/file names survive into parts: `my.dir/x.py` →
  `['my.dir', 'x']` → joined `my.dir.x`; `.hidden/h.py` → `'.hidden.h'`
  (verified). A file `a.tar.py` → part `a.tar`.
- Returns `[]` (falsy!) when filename normalizes to the prefix itself —
  relevant for the arg `'.'` matched against `package_path` when cwd has no
  `__init__.py`: relative base path is empty → the caller treats it as no
  match (see next).
- For `path_to_check='.'`: normpath stays `'.'`; an absolute filename can never
  start with `'.'` → always `None`. Hence the literal `'.'` entry in
  `additional_search_path` is inert.

### 8.3 `modpath_from_file_with_callback` / `modpath_from_file`

Lines 276-322, verbatim:

```python
def modpath_from_file_with_callback(
    filename: str,
    path: list[str] | None = None,
    is_package_cb: Callable[[str, list[str]], bool] | None = None,
) -> list[str]:
    filename = os.path.expanduser(_path_from_filename(filename))
    paths_to_check = sys.path.copy()
    if path:
        paths_to_check = path + paths_to_check
    for pathname in itertools.chain(
        paths_to_check, map(_cache_normalize_path, paths_to_check)
    ):
        if not pathname:
            continue
        modpath = _get_relative_base_path(filename, pathname)
        if not modpath:
            continue
        assert is_package_cb is not None
        if is_package_cb(pathname, modpath[:-1]):
            return modpath

    raise ImportError(
        "Unable to find module for {} in {}".format(
            filename, ", \n".join(paths_to_check)
        )
    )


def modpath_from_file(filename: str, path: list[str] | None = None) -> list[str]:
    return modpath_from_file(...)  # = modpath_from_file_with_callback(filename, path, check_modpath_has_init)
```

(line 322 is literally
`return modpath_from_file_with_callback(filename, path, check_modpath_has_init)`.)

Exact semantics:

- Candidate prefixes = `path + sys.path.copy()` — note `sys.path` is appended
  **again** even though pylint's `additional_search_path` already embedded a
  copy; duplicates are harmless, first match wins.
- The iteration is `chain(raw_paths, normalized_paths)`: all raw entries are
  tried first, then `normcase(realpath(...))` of each in the same order.
- Empty-string entries skipped (`if not pathname: continue`).
- `if not modpath: continue` — **an empty list result (filename == prefix) is
  treated as no-match**. This is why `modpath_from_file('.')` fails with
  ImportError when cwd has no `__init__.py` parent package: the only matching
  prefix (`package_path` == realpath(cwd)) yields `[]`.
- Match found → `is_package_cb(pathname, modpath[:-1])` must hold;
  `modpath[:-1]` excludes the final (module) component, so only the *package
  chain* is validated. A top-level module (`['top']`) validates the empty list
  → trivially true for `check_modpath_has_init`.
- No match at all → `ImportError("Unable to find module for <file> in <paths>")`.

`ImportError` propagation:

- For the **CLI arg**: caught at expand_modules.py:107 → fallback
  `modname = os.path.splitext(basename)[0]`.
- For **walked subfiles** (`_modpath_from_file`): **NOT caught** — would
  propagate out of `expand_modules` and crash pylint. It cannot fire in the
  `pylint .` flow because `package_path` always prefixes every walked file and,
  in namespace mode, the callback is always true; in non-namespace mode every
  walked dir chain has `__init__` by construction of the walk (`list_all=False`
  prunes init-less dirs) — but note the subtle case where the walk admits a dir
  via `__init__.pyi` only: `check_modpath_has_init` then consults
  `util.is_namespace` (runtime, succeeds because `package_path` is on
  `sys.path`) — see §8.4.

### 8.4 `check_modpath_has_init` and `_has_init`

Lines 222-232 and 671-680, verbatim:

```python
def check_modpath_has_init(path: str, mod_path: list[str]) -> bool:
    """Check there are some __init__.py all along the way."""
    modpath: list[str] = []
    for part in mod_path:
        modpath.append(part)
        path = os.path.join(path, part)
        if not _has_init(path):
            old_namespace = util.is_namespace(".".join(modpath))
            if not old_namespace:
                return False
    return True
```

```python
@lru_cache(maxsize=1024)
def _has_init(directory: str) -> str | None:
    mod_or_pack = os.path.join(directory, "__init__")
    for ext in (*PY_SOURCE_EXTS, "pyc", "pyo"):
        if os.path.exists(mod_or_pack + "." + ext):
            return mod_or_pack + "." + ext
    return None
```

- `PY_SOURCE_EXTS` on POSIX = `("py", "pyi")` (modutils.py:46), so `_has_init`
  accepts `__init__.py`, `__init__.pyi`, `__init__.pyc`, `__init__.pyo` — in
  that probe order, returning the first existing.
- If a chain component lacks `__init__.*`, the **runtime import system** is
  consulted: `util.is_namespace(dotted_prefix)`
  (`astroid/interpreter/_import/util.py:21-114`). That function:
  - returns False for builtins (util.py:30-31);
  - resolves each dotted component left-to-right with
    `importlib.util._find_spec_from_path`, threading
    `submodule_search_locations` (util.py:39-105);
  - returns **False immediately** if any found location is under
    `STD_LIB_DIRS ∪ EXT_LIB_DIRS` (util.py:94-104);
  - final verdict (util.py:107-114): spec exists, has
    `submodule_search_locations`, `origin is None`, and loader is `None` or a
    `NamespaceLoader`;
  - swallows `AttributeError`→False, `ValueError`→ sys.modules-based heuristic
    or False, `KeyError`→ repairs the search path and continues (util.py:50-91);
  - `@lru_cache(maxsize=4096)` — **cached per modname string for the process
    lifetime**, independent of `sys.path` changes (a stale cache across
    `augmented_sys_path` boundaries is theoretically observable).
  - Because `augmented_sys_path` put `package_path` on `sys.path`, a cwd
    subdirectory without `__init__.py` IS resolvable as a PEP 420 namespace →
    `check_modpath_has_init` can return True through this fallback.
- `_has_init` is `lru_cache`d: creating an `__init__.py` mid-run is invisible.

### 8.5 `file_from_modpath` (nonexistent-arg path only)

Lines 325-330: `file_from_modpath(modpath, path, context_file)` =
`file_info_from_modpath(...).location`. Used by expand_modules only when the
arg does not exist on disk (expand_modules.py:117) — not reachable for `'.'`.
Returns `None` for C builtins (spec location None) → expand_modules `continue`
(bailout #3); raises ImportError → F0001 (bailout #4).

---

## 9. astroid `modutils.get_module_files` and `os.walk` semantics

Lines 445-477, verbatim:

```python
def get_module_files(
    src_directory: str, blacklist: Sequence[str], list_all: bool = False
) -> list[str]:
    files: list[str] = []
    for directory, dirnames, filenames in os.walk(src_directory):
        if directory in blacklist:
            continue
        _handle_blacklist(blacklist, dirnames, filenames)
        # check for __init__.py
        if not list_all and {"__init__.py", "__init__.pyi"}.isdisjoint(filenames):
            dirnames[:] = ()
            continue
        for filename in filenames:
            if _is_python_file(filename):
                src = os.path.join(directory, filename)
                files.append(src)
    return files
```

Call site (expand_modules.py:152-154):
`get_module_files(os.path.dirname(filepath) or ".", ignore_list, list_all=is_namespace)`.
For `pylint .`, `filepath` is `__init__.py` (after normpath), so
`os.path.dirname('__init__.py') == ''` → `or "."` → `src_directory = "."`,
yielding paths like `./pkg/a.py` which the caller normpaths to `pkg/a.py`.

### 9.1 `os.walk` exact semantics (bare call, all defaults)

- `topdown=True`: pruning via `dirnames` mutation is honored.
- `onerror=None`: **unreadable directories are silently skipped** (scandir
  errors swallowed).
- `followlinks=False`: **symlinks to directories appear in `dirnames` but are
  never descended into** (verified: a `symdir -> realdir` symlink contributes
  no files; `realdir`'s files appear once, under their real path). Symlinks to
  *files* are listed in `filenames` normally.
- Entry order within each directory = `os.scandir` order = raw readdir order —
  **arbitrary and filesystem-dependent; NOT sorted, NOT insertion order**.
  pylint performs **no sorting anywhere** in this pipeline, so the final
  message output order is filesystem-dependent. (Empirically on APFS the order
  looked stable per directory state but changed when files were added.)
  A bug-for-bug port must read directory entries in OS order without sorting.
- Traversal is depth-first preorder: a directory's own `filenames` are
  processed before recursing; subdirectories are visited in `dirnames` order.
- **Hidden directories and files are walked normally** — `.hidden/h.py` is
  discovered and linted with module name `.hidden.h` (verified). Only the
  default `ignore_patterns` `^\.#` (basename) filters dot-hash files, applied
  later in expand_modules, not in the walk.

### 9.2 Blacklist handling

`_handle_blacklist` (modutils.py:127-138), verbatim:

```python
def _handle_blacklist(
    blacklist: Sequence[str], dirnames: list[str], filenames: list[str]
) -> None:
    for norecurs in blacklist:
        if norecurs in dirnames:
            dirnames.remove(norecurs)
        elif norecurs in filenames:
            filenames.remove(norecurs)
```

- Operates on **basenames** (the walk's `dirnames`/`filenames`). `CVS` dirs are
  removed from `dirnames` → never descended, never reported (not even as
  `isignored`; they simply don't exist in the result — verified).
- `elif`: if a name is both a dir and a file in the same parent (impossible) or
  if it's a dir, the file branch is skipped. `list.remove` removes the first
  occurrence only (lists from os.walk have unique entries anyway).
- The preceding `if directory in blacklist: continue` (line 466) compares the
  **full walk path** (e.g. `./CVS`) against blacklist **basenames** (`CVS`) —
  it essentially never matches for `src_directory='.'`; and when it does match
  (e.g. `get_module_files('CVS', ...)` where `directory == 'CVS'` at the top
  level), `continue` skips `_handle_blacklist` and the `__init__` check for
  that directory, **but `os.walk` still descends into its children** because
  `dirnames` was not cleared. Quirk to replicate exactly.

### 9.3 `_is_python_file`

modutils.py:663-668:

```python
def _is_python_file(filename: str) -> bool:
    return filename.endswith((".py", ".pyi", ".so", ".pyd", ".pyw"))
```

`.so`/`.pyd`/`.pyw` files are collected here but dropped later by
`should_analyze_file` (§4.3) since they're never `isarg` in this flow. `.pyi`
files **are** linted (verified: `stub.pyi` produced E0602 at
`stub.pyi:2:0` with module name `stub`).

### 9.4 `__init__` package gate (`list_all=False` only)

`{"__init__.py", "__init__.pyi"}.isdisjoint(filenames)` — a directory counts as
a package if it directly contains `__init__.py` **or `__init__.pyi`** (exact
basenames; `__init__.pyc` does NOT count here, unlike `_has_init`). On failure:
`dirnames[:] = ()` (prune entire subtree) and `continue` (skip its files).
Note this check also applies to `src_directory` itself: `get_module_files('.')`
with `list_all=False` yields nothing at all if `.` lacks `__init__.py(i)` —
irrelevant for pylint because `list_all=False` is only used when the arg
resolved as a real package (§13.2).

---

## 10. `file_info_from_modpath` / `spec.find_spec`

### 10.1 `file_info_from_modpath`

modutils.py:333-381 (doc trimmed):

```python
def file_info_from_modpath(modpath, path=None, context_file=None) -> spec.ModuleSpec:
    if context_file is not None:
        context: str | None = os.path.dirname(context_file)
    else:
        context = context_file           # None in expand_modules' call
    if modpath[0] == "xml":
        try:
            return _spec_from_modpath(["_xmlplus", *modpath[1:]], path, context)
        except ImportError:
            return _spec_from_modpath(modpath, path, context)
    elif modpath == ["os", "path"]:
        return spec.ModuleSpec(name="os.path", location=os.path.__file__,
                               type=spec.ModuleType.PY_SOURCE)
    return _spec_from_modpath(modpath, path, context)
```

`_spec_from_modpath` (modutils.py:622-660), verbatim core:

```python
    assert modpath
    location = None
    if context is not None:
        ...
    else:
        found_spec = spec.find_spec(modpath, path)
    if found_spec.type == spec.ModuleType.PY_COMPILED:
        try:
            location = get_source_file(found_spec.location)
            return found_spec._replace(location=location, type=spec.ModuleType.PY_SOURCE)
        except NoSourceFile:
            return found_spec._replace(location=location)
    elif found_spec.type == spec.ModuleType.C_BUILTIN:
        return found_spec._replace(location=None)
    elif found_spec.type == spec.ModuleType.PKG_DIRECTORY:
        location = _has_init(found_spec.location)
        return found_spec._replace(location=location, type=spec.ModuleType.PY_SOURCE)
    return found_spec
```

**Critical:** a package directory result (`PKG_DIRECTORY`) is rewritten to
`PY_SOURCE` with `location = <dir>/__init__.py` — so back in expand_modules,
`is_directory(spec)` is **False** for a real package arg (the
`has_init` condition at expand_modules.py:148-150 is what triggers the walk
instead).

### 10.2 `spec.find_spec` machinery

`astroid/interpreter/_import/spec.py`. `find_spec(modpath, path)`
(spec.py:441-458) delegates to the cached `_find_spec(tuple, tuple|None)`
(spec.py:461-496, `@lru_cache(maxsize=1024)`):

```python
def _find_spec(module_path, path):
    _path = path or sys.path
    modpath = list(module_path)
    search_paths = None
    processed: list[str] = []
    while modpath:
        modname = modpath.pop(0)
        submodule_path = search_paths or path
        if submodule_path is not None:
            submodule_path = tuple(submodule_path)
        finder, spec = _find_spec_with_path(_path, modname, module_path,
                                            tuple(processed), submodule_path)
        processed.append(modname)
        if modpath:
            if isinstance(finder, Finder):
                search_paths = finder.contribute_to_path(spec, processed)
            elif finder.__name__ in _EditableFinderClasses:
                search_paths = spec.submodule_search_locations
        if spec.type == ModuleType.PKG_DIRECTORY:
            spec = spec._replace(submodule_search_locations=search_paths)
    return spec
```

`_find_spec_with_path` (spec.py:387-438) tries, in order
(`_SPEC_FINDERS`, spec.py:339-344): `ImportlibFinder`, `ZipFinder`,
`PathSpecFinder`, `ExplicitNamespacePackageFinder`; then custom
`sys.meta_path` finders whose class/`__name__` is in
`_MetaPathFinderModuleTypes`; otherwise:

```python
    raise ImportError(f"No module named {'.'.join(module_parts)}")
```

`ImportlibFinder.find_module` (spec.py:126-194), the only finder that matters
locally:

```python
        if submodule_path is None and modname in sys.builtin_module_names:
            return ModuleSpec(name=modname, location=None, type=ModuleType.C_BUILTIN)
        if submodule_path is not None:
            search_paths = list(submodule_path)
        else:
            search_paths = sys.path
        suffixes = (".py", ".pyi", importlib.machinery.BYTECODE_SUFFIXES[0])
        for entry in search_paths:
            package_directory = os.path.join(entry, modname)
            for suffix in suffixes:
                package_file_name = "__init__" + suffix
                file_path = os.path.join(package_directory, package_file_name)
                if cached_os_path_isfile(file_path):
                    return ModuleSpec(name=modname, location=package_directory,
                                      type=ModuleType.PKG_DIRECTORY)
            for suffix, type_ in ImportlibFinder._SUFFIXES:
                file_name = modname + suffix
                file_path = os.path.join(entry, file_name)
                if cached_os_path_isfile(file_path):
                    return ModuleSpec(name=modname, location=file_path, type=type_)
        # frozen-stdlib fallback (only when modname/processed[0] in sys.stdlib_module_names)
        ...
        return None
```

Notes:

- Package check (`<entry>/<modname>/__init__{.py,.pyi,.pyc}`) takes
  **precedence over a same-named module file** within each path entry; entries
  are scanned in `additional_search_path` order.
- `cached_os_path_isfile` is `lru_cache(1024)` (modutils.py:613-616).
- `ImportlibFinder.find_module` itself is `lru_cache(1024)` keyed on
  `(modname, module_parts, processed, submodule_path)`.

### 10.3 What happens for the arg `'.'` (cwd without `__init__.py`)

`modname == '.'` → `modparts = '.'.split('.') == ['', '']`. Then
`file_info_from_modpath(['', ''], path=additional_search_path)`:

- `_find_spec(('',''), path)`: first iteration `modname=''`,
  `submodule_path = path`:
  - `ImportlibFinder`: builtin check skipped (`submodule_path is not None`);
    package probe checks `os.path.join(entry, '') + '/__init__.py'` ==
    `<entry>/__init__.py` — for entry `'.'` and `package_path` this is cwd's
    `__init__.py`, **which does not exist in this scenario**; module probe
    checks `<entry>/.py`, `<entry>/.pyi`, etc. — never exist; frozen check:
    `'' not in sys.stdlib_module_names` → returns `None`.
  - `ZipFinder`: no zipimporter match → `None`.
  - `PathSpecFinder`: `importlib.machinery.PathFinder.find_spec('', path)`
    returns `None` on CPython 3.12.12 (verified empirically).
  - `ExplicitNamespacePackageFinder`: `'' in sys.modules` is False → `None`.
  - meta_path finders: stock finders' names not in `_MetaPathFinderModuleTypes`
    → skipped.
  - → `raise ImportError("No module named .")` (`'.'.join(('',''))` == `'.'`).
- So expand_modules takes the `except ImportError` branch
  (expand_modules.py:131-134):
  `is_namespace = not os.path.exists('__init__.py')` → **True**;
  `is_directory = os.path.isdir('.')` → True.

(Empirically confirmed: `file_info_from_modpath(['',''])` raises
`ImportError: No module named .`.)

Quirk: if cwd contained `<cwd>/__init__.py`, we'd never be here (modname would
have resolved); but if any *other* entry of `additional_search_path` contains
an `__init__.py` directly (e.g. a sys.path entry that is itself a package
directory), `ImportlibFinder` would return a PKG_DIRECTORY for `''` — never the
case in practice.

### 10.4 Namespace dirs as args (context)

For an arg like `nspkg` (directory without `__init__.py`, resolvable on the
search path), `PathSpecFinder.find_module` (spec.py:309-329) is the one that
classifies it: `PathFinder.find_spec` returns a spec with `origin is None` →
`ModuleType.PY_NAMESPACE`, `location=None`. Then `modutils.is_namespace(spec)`
(modutils.py:683-684: `specobj.type == spec.ModuleType.PY_NAMESPACE`) → True,
`is_directory` (modutils.py:687-688: `type == PKG_DIRECTORY`) → False. Not the
flow for `'.'` (which fails spec lookup entirely, §10.3), but documented because
`is_namespace=True` then drives `list_all=True` identically.

---

## 11. Module naming in messages and output headers

### 11.1 astroid strips trailing `.__init__`

pylint passes `FileItem.name` (e.g. `pkg.sub.__init__`) to
`MANAGER.ast_from_file(filepath, modname, source=True)` (pylinter.py:1012).
`ast_from_file` (astroid/manager.py:131-167): with modname given and not
cached, calls `get_source_file(filepath, include_no_ext=True, prefer_stubs=...)`
then `AstroidBuilder(self).file_build(filepath, modname)`.

`AstroidBuilder._data_build` (astroid/builder.py:197-211):

```python
        if path is not None:
            node_file = os.path.abspath(path)
        else:
            node_file = "<?>"
        if modname.endswith(".__init__"):
            modname = modname[:-9]
            package = True
        else:
            package = (
                path is not None
                and os.path.splitext(os.path.basename(path))[0] == "__init__"
            )
```

So the **Module node's `.name`** for a walked `pkg/__init__.py` (FileItem name
`pkg.__init__`) is `pkg`, and `.file` is `os.path.abspath(path)`. A bare
top-level `__init__.py` arg whose modname is just `__init__` does NOT end with
`.__init__` → name stays `__init__` but `package=True` via the basename check.

### 11.2 Message `module` / `path` / header

`PyLinter._add_one_message` (pylinter.py:1256-1266):

```python
        if node is None:
            module, obj = self.current_name, ""
            abspath = self.current_file
        else:
            module, obj = utils.get_module_and_frameid(node)
            abspath = node.root().file
        if abspath is not None:
            path = abspath.replace(self.reporter.path_strip_prefix, "", 1)
        else:
            path = "configuration"
```

- `get_module_and_frameid` (`pylint/utils/utils.py:91-107`) walks frames up to
  the Module node and returns `module = frame.name` — i.e. the **astroid
  Module name** (post-`.__init__`-strip), not `FileItem.name`.
- `path_strip_prefix = os.getcwd() + os.sep`
  (`pylint/reporters/base_reporter.py:37`); `abspath` is
  `os.path.abspath(filepath)` from the builder, so for `pylint .` the displayed
  path is the **cwd-relative path** (`pkg/a.py`). `str.replace(..., 1)` —
  first occurrence only.
- For node-less messages (F0001/F0002/F0010/E0001 from `get_ast`):
  `module = self.current_name` = `FileItem.name` (NOT stripped — a syntax error
  in `pkg/__init__.py` is headed `Module pkg.__init__`), and
  `abspath = self.current_file` = the filepath passed to
  `set_current_module(name, filepath)` (pylinter.py:935-943) — relative, so the
  `replace` is a no-op and `path` equals the relative filepath.
- Text header (`pylint/reporters/text.py:105-106, 156-161`):

```python
def make_header(msg: Message) -> str:
    return f"************* Module {msg.module}"

    def handle_message(self, msg: Message) -> None:
        if msg.module not in self._modules:
            self.writeln(make_header(msg))
            self._modules.add(msg.module)
        self.write_message(msg)
```

  `self._modules` is a **set accumulated for the whole run**: the header is
  printed only on the first message of a given module *name*. If two distinct
  files yield the same module name, the second file's messages appear under the
  first header (no new header). Default line format
  (text.py:114): `"{path}:{line}:{column}: {msg_id}: {msg} ({symbol})"`.

### 11.3 Verified naming examples

cwd **without** `__init__.py` (namespace mode):

| file (result key)     | FileItem.name        | header module       |
|-----------------------|----------------------|---------------------|
| `top.py`              | `top`                | `top`               |
| `.hidden/h.py`        | `.hidden.h`          | `.hidden.h`         |
| `nopkg/c.py`          | `nopkg.c`            | `nopkg.c`           |
| `pkg/a.py`            | `pkg.a`              | `pkg.a`             |
| `pkg/__init__.py`     | `pkg.__init__`       | `pkg`               |
| `pkg/sub/__init__.py` | `pkg.sub.__init__`   | `pkg.sub`           |
| `pkg/sub/b.py`        | `pkg.sub.b`          | `pkg.sub.b`         |
| `stub.pyi`            | `stub`               | `stub`              |

cwd **with** `__init__.py` (cwd dir named `prylint_probe2`):

| file (result key)  | FileItem.name                  | isarg |
|--------------------|--------------------------------|-------|
| `__init__.py`      | `prylint_probe2`               | True  |
| `top.py`           | `prylint_probe2.top`           | False |
| `sub/__init__.py`  | `prylint_probe2.sub.__init__`  | False |
| `sub/b.py`         | `prylint_probe2.sub.b`         | False |

(`nopkg/c.py` — no `__init__.py` — was **not discovered** in the second case.)

---

## 12. Ordering, dedup, collisions, caching

### 12.1 Output order

Total order = insertion order of the `result` dict in `expand_modules`:

1. Per CLI arg, in CLI order.
2. For a non-namespace arg: the arg's own entry first
   (`result.setdefault(filepath, ...)`, expand_modules.py:147), then walked
   subfiles in `get_module_files` order.
3. For a namespace arg ('.' without `__init__.py`): **no arg entry** (the
   `if not is_namespace` block is skipped); walked subfiles only.
4. `get_module_files` order = `os.walk` order = depth-first preorder, with
   per-directory entries in **raw readdir order (unsorted,
   filesystem-dependent)**. Files of a directory come before files of its
   subdirectories.

There is **no sorting** of files or of emitted messages anywhere in this
pipeline (messages are emitted to the reporter in checker-visit order per
file).

### 12.2 Dedup

- Result dict keyed by `os.path.normpath(filepath)` (raw arg string for
  ignored-arg entries). The same file reached twice (e.g. listed as arg and
  found by a walk) yields **one** entry:
  - arg processed first, walk second: walk overwrite at expand_modules.py:177
    replaces `name`/`basepath`/`basename` but **preserves `isarg`**
    (line 176).
  - walk first, arg second: `setdefault(...)["isarg"] = True` keeps the walked
    entry's name and just forces `isarg`.
- `if filepath == subfilepath: continue` (line 156-157) prevents the base
  `__init__.py` from being re-added by its own walk.

### 12.3 Module-name collisions

Two different files can map to the same dotted name (e.g. `x.py` plus a
`PYTHONPATH` entry making `sub/x.py` also resolve as `x`; or two args from
sibling roots). Behavior:

- Both files lint (result keyed by path, not name).
- `MANAGER.ast_from_file` cache check (manager.py:144-148) is
  `modname in self.astroid_cache and self.astroid_cache[modname].file == filepath`
  — different file ⇒ cache miss ⇒ rebuild; `cache_module` then **overwrites**
  the cache slot keyed by the **post-strip module name**
  (builder.py:164; cache key `module.name`). Inference performed while linting
  the second file resolves the shared name to the second file's AST.
- Text reporter prints the shared header only once (§11.2).

### 12.4 Caches that affect discovery (process-lifetime, never invalidated)

- `modutils._cache_normalize_path_` — lru_cache, path → normcase(realpath).
- `modutils._has_init` — lru_cache(1024).
- `modutils.cached_os_path_isfile` — lru_cache(1024).
- `spec._find_spec` — lru_cache(1024) keyed `(modpath_tuple, path_tuple|None)`.
- `ImportlibFinder.find_module` / other finders' `find_module` — lru_cache(1024).
- `util.is_namespace` — lru_cache(4096) keyed by modname **only** (ignores
  sys.path state at call time).

A port that re-stats the filesystem on every query will differ only in
pathological mid-run-mutation scenarios.

---

## 13. The two top-level scenarios for `pylint .`, end-to-end

### 13.1 cwd has NO `__init__.py` (the common repo-root case)

Pseudocode of the exact flow (all confirmed empirically, §14):

```
args = ['.']                                   # glob('.') -> ['.']
package_path = realpath(cwd)                   # discover_package_path('.', ())
sys.path = [package_path, *old_sys_path]       # augmented_sys_path
expand_modules(['.']):
  _is_ignored_file('.')                        # basename = cwd's basename;
                                               # True only if cwd is named CVS
                                               # or matches ^\.#  -> all skipped
  additional_search_path = ['.', package_path, package_path, bin_dir, ...]
  os.path.exists('.') -> True
  modpath_from_file('.', path=...)             # every prefix yields None or []
    -> ImportError
  modname = splitext(basename('.'))[0] = '.'   # NOTE: splitext('.') == ('.','')
  isdir('.') -> filepath = './__init__.py' -> normpath -> '__init__.py'
  modparts = ['', '']
  file_info_from_modpath(['', ''], ...)        # §10.3
    -> ImportError("No module named .")
  is_namespace = not exists('__init__.py') = True
  is_directory = isdir('.') = True
  (is_namespace) -> NO arg entry in result
  has_init = ('' != '__init__') and ('__init__.py' == '__init__.py') = True
  walk: get_module_files('.', ['CVS'], list_all=True)
    os.walk('.'):  readdir order, topdown, no symlink-follow, errors ignored
      every dir: remove 'CVS' from dirnames/filenames
      list_all=True  -> NO __init__ gate; ALL dirs incl. hidden are entered
      collect files ending .py/.pyi/.so/.pyd/.pyw as './<rel>'
  for each subfile (normpath'd):
    '__init__.py' == subfilepath? never (doesn't exist) -> no skip
    _is_ignored_file(subfile)?                 # basename in ['CVS'] or ^\.#
       -> result[subfile] = isignored entry; continue
    modpath = modpath_from_file_with_callback(subfile, path=...,
                cb = check_modpath_has_init OR True)   # always True
      first matching prefix = package_path (the '.' entry never matches)
      -> parts of path relative to cwd, last extension stripped
    result[subfile] = {path: subfile, name: '.'-joined parts, isarg: False,
                       basepath: '__init__.py', basename: '.', isignored: False}
_iterate_file_descrs:
  skip isignored (stats.skipped++)
  should_analyze_file: keep only *.py / *.pyi   (drops .so/.pyd/.pyw)
  yield FileItem(name, path, '.')
```

Resulting FileItems: every `.py`/`.pyi` under cwd (recursively, including
hidden dirs and dirs without `__init__.py`, excluding `CVS` subtrees, excluding
`^\.#*` basenames, excluding symlinked dirs' contents), with dotted names
derived from the cwd-relative path, `modpath` (FileState name) `'.'` for all.

### 13.2 cwd HAS `__init__.py`

```
package_path = realpath(first ancestor without __init__.py)   # e.g. parent dir
modpath_from_file('.', path=...):
  prefix package_path matches abspath(cwd) (or realpath fallback)
  relative parts = path components of cwd below package_path  # e.g. ['myrepo']
  check_modpath_has_init(package_path, parts[:-1]) = True (empty chain)
  -> modname = 'myrepo'  (dots in dir names split into more parts!)
filepath = normpath('./__init__.py') = '__init__.py'
modparts = ['myrepo']
file_info_from_modpath(['myrepo'], ...):
  ImportlibFinder finds package dir <package_path>/myrepo/__init__.py
  -> PKG_DIRECTORY -> _spec_from_modpath rewrites to
     PY_SOURCE, location=<...>/myrepo/__init__.py
is_namespace = False, is_directory = False
-> result['__init__.py'] = {name: 'myrepo', isarg: True, ...}   # FIRST entry
has_init = ('myrepo' != '__init__') and basename=='__init__.py' -> True
walk: get_module_files('.', ['CVS'], list_all=False)
  '.' contains __init__.py -> not pruned
  subdirs WITHOUT __init__.py or __init__.pyi: dirnames[:]=(); files skipped
for each subfile:
  subfilepath == '__init__.py' (the arg itself) -> skipped
  _modpath_from_file(subfile, is_namespace=False, ...):
    prefix package_path; callback = check_modpath_has_init
    name = 'myrepo.' + relative parts   ('myrepo.sub.__init__' for sub/__init__.py)
  result[subfile] = {..., basepath: '__init__.py', basename: 'myrepo'}
```

Differences vs 13.1: an `isarg=True` entry for `__init__.py` comes first; the
walk **excludes** any subtree lacking `__init__.py(i)`; all names are prefixed
with the cwd package's dotted name; FileItem.modpath = `'myrepo'`.

Half-breed case: cwd has only `__init__.pyi` (no `.py`):
`discover_package_path` ignores `.pyi` → package_path = cwd;
`modpath_from_file('.')` fails (`[]` result) → modname `'.'`;
`filepath='__init__.py'` doesn't exist → spec ImportError →
`is_namespace=True` → behaves exactly like 13.1 (namespace walk).

---

## 14. Empirical verification transcripts

Setup A (`/tmp/prylint_probe`, **no** root `__init__.py`):

```
pkg/__init__.py  pkg/a.py  pkg/sub/__init__.py  pkg/sub/b.py
nopkg/c.py  .hidden/h.py  top.py  stub.pyi  native.so  script.pyw
CVS/ignored.py  .#lockfile.py  realdir/s.py  symdir -> realdir
```

`expand_modules(['.'], [], ["CVS"], [re.compile(r"^\.#")], [])` returned
`errors=[]` and (insertion order, before the symlink/stub additions):

```
'top.py'              name='top'              isarg=False basepath='__init__.py' basename='.'
'.#lockfile.py'       name=''   isignored=True (key=normpath, base entries '')
'.hidden/h.py'        name='.hidden.h'        ...
'nopkg/c.py'          name='nopkg.c'          ...
'pkg/a.py'            name='pkg.a'            ...
'pkg/__init__.py'     name='pkg.__init__'     ...
'pkg/sub/__init__.py' name='pkg.sub.__init__' ...
'pkg/sub/b.py'        name='pkg.sub.b'        ...
```

`CVS/ignored.py`: absent entirely. `discover_package_path('.')` =
`/private/tmp/prylint_probe`. `modpath_from_file('.')` raised ImportError.
`'.'.split('.') == ['', '']`. `file_info_from_modpath(['',''])` raised
`ImportError: No module named .`.

Real run `pylint . -E` (after symlink + stub added; E0602 planted everywhere):

```
************* Module stub
stub.pyi:2:0: E0602: Undefined variable 'bad_pyi_undefined' (undefined-variable)
************* Module top
top.py:1:0: E0602: ...
************* Module .hidden.h
.hidden/h.py:1:0: E0602: ...
************* Module realdir.s
realdir/s.py:1:0: E0602: ...        # via realdir only; symdir NOT walked
************* Module nopkg.c
nopkg/c.py:1:0: E0602: ...
************* Module pkg.a
pkg/a.py:1:0: E0602: ...
************* Module pkg                      # header stripped of .__init__
pkg/__init__.py:1:0: E0602: ...
************* Module pkg.sub.b
pkg/sub/b.py:1:0: E0602: ...
```

(`native.so`, `script.pyw` discovered by the walk, dropped by
`should_analyze_file`; no fatal/astroid errors. Note `stub.pyi` jumped to the
front after creation — readdir order is not insertion-stable on APFS.)

Setup B (`/tmp/prylint_probe2`, **with** root `__init__.py`):
`discover_package_path('.')` = `/private/tmp`;
`modpath_from_file('.')` = `['prylint_probe2']`; spec =
`ModuleSpec(name='prylint_probe2', type=PY_SOURCE,
location='/private/tmp/prylint_probe2/__init__.py')`; result order:
`__init__.py` (isarg=True, name `prylint_probe2`), `top.py`,
`sub/__init__.py`, `sub/b.py`; `nopkg/c.py` (no `__init__.py`) absent.
Real run printed exactly those modules; headers `prylint_probe2`,
`prylint_probe2.top`, `prylint_probe2.sub.b`.

Other empirical confirmations: `PathFinder.find_spec('', path)` → `None`;
`_is_ignored_file('.')` in a dir named `CVS` → `True`;
`sys.path[0]` for script execution = script's directory (realpath'd);
`os.walk` defaults `topdown=True, onerror=None, followlinks=False`.

---

## 15. Porting checklist: every bailout / conservatism path

Discovery-phase bailouts (in execution order):

1. **No args / disable-all** → print `"No files to lint: exiting."`, exit 32
   (run.py:206-211).
2. **`glob(arg) or [arg]`** — nonexistent args pass through for later F0001
   (config_initialization.py:152-161).
3. **Arg ignored** (`_is_ignored_file`) → `isignored` result entry keyed by raw
   arg string; `stats.skipped += 1`; nothing linted; no message
   (expand_modules.py:87-98; pylinter.py:910-911). Includes the
   cwd-basename-equals-`CVS` trap for `'.'`.
4. **`modpath_from_file` ImportError** → fallback
   `modname = splitext(basename)[0]` (`'.'` for `'.'`)
   (expand_modules.py:107-108). Inside it: empty path entries skipped; empty
   relative path (`[]`) treated as no-match (modutils.py:288-291).
5. **Nonexistent arg** (not `'.'`): `file_from_modpath` returns `None`
   (C builtin) → silent `continue`; ImportError → `errors` → **F0001 `fatal`**
   `"%s"` with `str(ex).replace(cwd+sep, "")`, reported at module=modname,
   line 1, col 0 (expand_modules.py:113-124; pylinter.py:926-932, 104-110).
6. **`file_info_from_modpath` ImportError** → don't crash:
   `is_namespace = not exists(filepath)`, `is_directory = isdir(something)`
   (expand_modules.py:131-134). This is the live path for `'.'` without
   `__init__.py`.
7. **`is_namespace`** → no result entry for the arg itself (the nonexistent
   `__init__.py` is never linted) (expand_modules.py:138-147).
8. **Walk only when** `has_init or is_namespace or is_directory`
   (expand_modules.py:151) — a plain file arg never walks.
9. **Walk gates** (modutils.py:465-476): blacklist basename removal (`CVS`
   subtree never entered); `list_all=False` → subtree pruned unless dir
   directly contains `__init__.py` or `__init__.pyi`; symlinked dirs never
   followed; unreadable dirs silently skipped; only
   `.py/.pyi/.so/.pyd/.pyw` collected.
10. **`filepath == subfilepath`** → skip (dedup of base `__init__.py`)
    (expand_modules.py:156-157).
11. **Subfile ignored** (`_is_ignored_file`) → `isignored` entry,
    `stats.skipped += 1` (expand_modules.py:158-169).
12. **`should_analyze_file`**: non-arg files must end `.py`/`.pyi`
    (pylinter.py:600-620) — silently drops walked `.so/.pyd/.pyw`.
13. **`check_modpath_has_init` fallback**: missing `__init__.*` in the chain →
    consult runtime `util.is_namespace` (which itself returns False on any
    error: AttributeError, missing sys.modules entry, stdlib/site-packages
    locations) (modutils.py:222-232; util.py:21-114).
14. **AST failures** per file: `AstroidSyntaxError` → E0001 with
    `line = ex.error.lineno or 0`, `col_offset = ex.error.offset`, args
    `Parsing failed: '<err>'`; other `AstroidBuildingError` → F0010
    `error while code parsing: %s`; unexpected exception → F0002 `%s: %s`
    with `(filepath, crash-report msg)` (pylinter.py:744-757, 1010-1038).

Order-dependence summary:

- `result` dict insertion order ⇒ lint & output order (pylinter.py:908, 740,
  779).
- `dict.fromkeys` order-preserving dedup for `extra_packages_paths`
  (pylinter.py:686-693); `_augment_sys_path` order-preserving prepend
  (utils.py:115-125).
- `additional_search_path` order ⇒ which prefix names a module
  (expand_modules.py:100; modutils.py:282-295: raw entries first, then
  normalized, first match wins).
- `os.walk` readdir order ⇒ unsorted, FS-dependent file order; **no sorting
  anywhere**.
- `TextReporter._modules` set ⇒ one header per module *name* per run
  (text.py:156-161).
