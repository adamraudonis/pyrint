# 04 — astroid AST construction pipeline (file bytes → tree pylint walks)

Pinned sources:
- astroid 4.0.4: `/Users/adamraudonis/Desktop/Projects/prylint/reference/astroid/astroid`
- pylint 4.0.5: `/Users/adamraudonis/Desktop/Projects/prylint/reference/pylint/pylint`
- Ground-truth runtime: CPython 3.12.12 (`PY311_PLUS=True, PY312_PLUS=True, PY313_PLUS=False, PY314_PLUS=False` — astroid/const.py:8-12)

All runtime behaviors below were verified empirically against astroid 4.0.4 running on CPython 3.12.12
(uv-managed `cpython-3.12.12-macos-aarch64-none`).

Scope: everything between raw file bytes on disk and the `nodes.Module` tree that pylint's ASTWalker
visits — encoding detection, parse-error mapping, the TreeRebuilder (positions for every node type),
Module metadata, line-number properties (`fromlineno`/`tolineno`/`blockstart_tolineno`/`block_range`),
and the brain transforms that run on every built module.

---

## Table of contents

1. [Entry points: how pylint asks for an AST](#1-entry-points)
2. [open_source_file & encoding detection (exact)](#2-encoding)
3. [file_build / string_build error mapping](#3-error-mapping)
4. [_data_build and _parse_string (exact ast.parse parameters)](#4-parse-string)
5. [Module node: construction & metadata](#5-module-node)
6. [_post_build: future imports, from-import locals, delayed assattr, transforms](#6-post-build)
7. [TreeRebuilder: dispatch and general position passthrough](#7-rebuilder-dispatch)
8. [Docstring extraction (doc_node)](#8-doc-node)
9. [FunctionDef / AsyncFunctionDef / ClassDef positions, decorators, `position` attribute](#9-defs)
10. [Synthesized AssignName positions (arg, except-as, match, type params)](#10-assignname)
11. [Nodes with missing/special positions (Arguments, Comprehension, MatchCase, Keyword, Slice)](#11-positionless)
12. [Remaining node-by-node construction notes](#12-node-notes)
13. [fromlineno / tolineno / _fixed_source_line (base implementation + ALL overrides)](#13-linenos)
14. [blockstart_tolineno and block_range — exact, per class](#14-block-range)
15. [How pylint converts a node into a message location](#15-pylint-location)
16. [Scope/locals bookkeeping during the rebuild (set_local, globals)](#16-locals)
17. [TransformVisitor: exact algorithm and ordering](#17-transforms)
18. [inference_tip mechanism](#18-inference-tip)
19. [Brain modules: full table and in-scope impact](#19-brains)
20. [CPython 3.12 position quirks passed through verbatim](#20-cpython-quirks)
21. [Order-dependency summary (dicts, lists, sorting)](#21-ordering)

---

<a name="1-entry-points"></a>
## 1. Entry points: how pylint asks for an AST

pylint/lint/pylinter.py:998-1038:

```python
def get_ast(
    self, filepath: str, modname: str, data: str | None = None
) -> nodes.Module | None:
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

- For `pylint . -E` (files on disk, not stdin), `data is None`, so the path is
  **`MANAGER.ast_from_file(filepath, modname, source=True)` → `AstroidBuilder(self).file_build(filepath, modname)`**
  (astroid/manager.py:131-168; with `source=True` it goes straight to `file_build` after a cache check;
  `get_source_file()` is still attempted first and may swap `filepath` for the `.py` source —
  manager.py:151-157 — but for plain `.py` inputs it returns the same path).
- Cache check (manager.py:144-148): if `modname in self.astroid_cache and self.astroid_cache[modname].file == filepath`,
  the cached Module is returned without rebuilding.
- **E0001 `syntax-error`** is produced from `AstroidSyntaxError`: message arg is
  `f"Parsing failed: '{ex.error}'"` (the E0001 template is just `%s`), line is `ex.error.lineno`
  (or `0` if the wrapped exception has no `lineno`, e.g. `MemoryError`), col_offset is `ex.error.offset` (may be None).
- **F0010 `parse-error`** is produced from any other `AstroidBuildingError`; arg is the exception itself
  (template `error while code parsing: %s`; `str(ex)` via `AstroidError.__str__` below).
- **F0002 `astroid-error`** is raised by the caller `_get_asts` when `get_ast` re-raises
  `AstroidBuildingError` from an unexpected exception (pylinter.py:745-757).

A second E0001 source lives just past the build: `_check_astroid_module` re-tokenizes the module
(pylinter.py:1079-1089) via `utils.tokenize_module(node)` (pylint/utils/utils.py:151-154 — 
`tokenize.tokenize(node.stream().readline)`); a `tokenize.TokenError` yields
`syntax-error` with `line=ex.args[1][0]`, `col_offset=ex.args[1][1]`, `args=ex.args[0]` (NO
"Parsing failed:" prefix). Since `ast.parse` already succeeded by then, this only triggers in
exotic cases, but it reads the file **again from disk** through `Module.stream()` (see §5).

---

<a name="2-encoding"></a>
## 2. open_source_file & encoding detection (exact)

astroid/builder.py:49-55:

```python
def open_source_file(filename: str) -> tuple[TextIOWrapper, str, str]:
    # pylint: disable=consider-using-with
    with open(filename, "rb") as byte_stream:
        encoding = detect_encoding(byte_stream.readline)[0]
    stream = open(filename, newline=None, encoding=encoding)
    data = stream.read()
    return stream, encoding, data
```

Key facts for the port:

1. `detect_encoding` is **`tokenize.detect_encoding`** (stdlib; imported at builder.py:21).
2. The file is then re-opened in **text mode with `newline=None` (universal newlines)**: every
   `\r\n` and lone `\r` in the decoded text is translated to `\n` before parsing. Your Rust port
   must apply the same translation before computing line/col positions.
3. If the detected encoding is `utf-8-sig` (BOM present), the decode strips the BOM, so column 0 of
   line 1 is the first real character.

### tokenize.detect_encoding, CPython 3.12.12 (verbatim, doc-comment elided)

```python
def detect_encoding(readline):
    try:
        filename = readline.__self__.name
    except AttributeError:
        filename = None
    bom_found = False
    encoding = None
    default = 'utf-8'
    def read_or_stop():
        try:
            return readline()
        except StopIteration:
            return b''

    def find_cookie(line):
        try:
            # Decode as UTF-8. Either the line is an encoding declaration,
            # in which case it should be pure ASCII, or it must be UTF-8
            # per default encoding.
            line_string = line.decode('utf-8')
        except UnicodeDecodeError:
            msg = "invalid or missing encoding declaration"
            if filename is not None:
                msg = '{} for {!r}'.format(msg, filename)
            raise SyntaxError(msg)

        match = cookie_re.match(line_string)
        if not match:
            return None
        encoding = _get_normal_name(match.group(1))
        try:
            codec = lookup(encoding)
        except LookupError:
            # This behaviour mimics the Python interpreter
            if filename is None:
                msg = "unknown encoding: " + encoding
            else:
                msg = "unknown encoding for {!r}: {}".format(filename,
                        encoding)
            raise SyntaxError(msg)

        if bom_found:
            if encoding != 'utf-8':
                # This behaviour mimics the Python interpreter
                if filename is None:
                    msg = 'encoding problem: utf-8'
                else:
                    msg = 'encoding problem for {!r}: utf-8'.format(filename)
                raise SyntaxError(msg)
            encoding += '-sig'
        return encoding

    first = read_or_stop()
    if first.startswith(BOM_UTF8):
        bom_found = True
        first = first[3:]
        default = 'utf-8-sig'
    if not first:
        return default, []

    encoding = find_cookie(first)
    if encoding:
        return encoding, [first]
    if not blank_re.match(first):
        return default, [first]

    second = read_or_stop()
    if not second:
        return default, [first]

    encoding = find_cookie(second)
    if encoding:
        return encoding, [first, second]

    return default, [first, second]
```

With:

```
cookie_re = re.compile(r'^[ \t\f]*#.*?coding[:=][ \t]*([-\w.]+)')   # matched on str
blank_re  = re.compile(rb'^[ \t\f]*(?:[#\r\n]|$)')                  # matched on bytes

def _get_normal_name(orig_enc):
    enc = orig_enc[:12].lower().replace("_", "-")
    if enc == "utf-8" or enc.startswith("utf-8-"):
        return "utf-8"
    if enc in ("latin-1", "iso-8859-1", "iso-latin-1") or \
       enc.startswith(("latin-1-", "iso-8859-1-", "iso-latin-1-")):
        return "iso-8859-1"
    return orig_enc
```

Semantics to mirror:

- Only the **first two physical lines** (split on the bytes-level `readline` of the binary stream)
  are inspected. The cookie on line 2 only counts if line 1 matches `blank_re` (blank or
  comment-only line).
- `filename` IS available here (`readline.__self__.name` of the binary file object), so the
  SyntaxError messages include the file path, e.g.
  `unknown encoding for '/path/f.py': bogus`.
- UTF-8 BOM ⇒ returned encoding string is `"utf-8-sig"`; BOM + cookie naming a non-utf-8 codec ⇒
  `SyntaxError('encoding problem for {!r}: utf-8')`.
- Empty file ⇒ `('utf-8', [])` (no error).
- Note 3.12 nuance: `find_cookie` decodes the line **as UTF-8** (not as the declared encoding) just
  to run the regex; an undecodable-in-utf8 first line *without* a valid cookie raises
  `SyntaxError("invalid or missing encoding declaration for '...'")`. (3.13/3.14 restructured this;
  do NOT copy newer behavior such as the in-`detect_encoding` null-byte check — that is 3.14.)

---

<a name="3-error-mapping"></a>
## 3. file_build / string_build error mapping

astroid/builder.py:113-157:

```python
def file_build(self, path: str, modname: str | None = None) -> nodes.Module:
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
    with stream:
        # get module name if necessary
        if modname is None:
            try:
                modname = ".".join(modutils.modpath_from_file(path))
            except ImportError:
                modname = os.path.splitext(os.path.basename(path))[0]
        # build astroid representation
        module, builder = self._data_build(data, modname, path)
        return self._post_build(module, builder, encoding)

def string_build(self, data: str, modname: str = "", path: str | None = None) -> nodes.Module:
    module, builder = self._data_build(data, modname, path)
    module.file_bytes = data.encode("utf-8")
    return self._post_build(module, builder, "utf-8")
```

Mapping table (what pylint message each failure becomes):

| Failure | Exception raised by builder | str() of exception (AstroidError.__str__ formats message with vars) | pylint message |
|---|---|---|---|
| `open()`/read OSError | `AstroidBuildingError("Unable to load file {path}:\n{error}")` | `Unable to load file <path>:\n<oserror>` | F0010 parse-error |
| detect_encoding SyntaxError (bad/unknown cookie, BOM conflict, undecodable line 1) or LookupError | `AstroidSyntaxError("Python 3 encoding specification error or unknown encoding:\n{error}")` | that text + the SyntaxError str | E0001, line = `ex.error.lineno` → usually None → **0** |
| UnicodeDecodeError while reading body (`stream.read()`) | `AstroidBuildingError("Wrong or no encoding specified for {filename}.")` | `Wrong or no encoding specified for <path>.` | F0010 parse-error |
| ast.parse failure (see §4) | `AstroidSyntaxError("Parsing Python code failed:\n{error}")` | `Parsing Python code failed:\n<error str>` | E0001 |

`AstroidError.__str__` (astroid/exceptions.py:66-70):

```python
def __str__(self) -> str:
    try:
        return self.message.format(**vars(self))
    except ValueError:
        return self.message  # Return raw message if formatting fails
```

(`vars(self)` includes `modname`, `error`, `source`, `path`, `cls`, `class_repr` set in
`AstroidBuildingError.__init__`, exceptions.py:81-98. `AstroidSyntaxError.__init__` signature is
`(message, modname, error, path, source=None)` — exceptions.py:129-137.)

`string_build` vs `file_build` differences that persist on the Module:

- `string_build` sets `module.file_bytes = data.encode("utf-8")` and `file_encoding="utf-8"`.
- `file_build` leaves `file_bytes = None` (class default) and sets `file_encoding` to the detected
  encoding. Consequently `Module.stream()` **re-opens the file from disk in binary mode**
  (scoped_nodes.py:287-301) — pylint's token/raw checkers re-read the file.
- For `pylint .` the modname is always supplied by pylint (expand_modules), so the
  `modpath_from_file` fallback inside `file_build` is normally dead code.

---

<a name="4-parse-string"></a>
## 4. _data_build and _parse_string (exact ast.parse parameters)

astroid/builder.py:180-211:

```python
def _data_build(self, data, modname, path):
    """Build tree node from data and add some informations."""
    try:
        node, parser_module = _parse_string(
            data, type_comments=True, modname=modname
        )
    except (TypeError, ValueError, SyntaxError, MemoryError) as exc:
        raise AstroidSyntaxError(
            "Parsing Python code failed:\n{error}",
            source=data, modname=modname, path=path, error=exc,
        ) from exc

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
    builder = rebuilder.TreeRebuilder(self._manager, parser_module, data)
    module = builder.visit_module(node, modname, node_file, package)
    return module, builder
```

astroid/builder.py:486-505:

```python
def _parse_string(data, type_comments=True, modname=None):
    parser_module = get_parser_module(type_comments=type_comments)
    try:
        parsed = parser_module.parse(
            data + "\n", type_comments=type_comments, filename=modname
        )
    except SyntaxError as exc:
        # If the type annotations are misplaced for some reason, we do not want
        # to fail the entire parsing of the file, so we need to retry the
        # parsing without type comment support. We use a heuristic for
        # determining if the error is due to type annotations.
        type_annot_related = re.search(r"#\s+type:", exc.text or "")
        if not (type_annot_related and type_comments):
            raise

        parser_module = get_parser_module(type_comments=False)
        parsed = parser_module.parse(data + "\n", type_comments=False)
    return parsed, parser_module
```

and astroid/_ast.py:25-30:

```python
def parse(self, string, type_comments=True, filename=None) -> ast.Module:
    if filename:
        return ast.parse(string, filename=filename, type_comments=type_comments)
    return ast.parse(string, type_comments=type_comments)
```

Exact facts:

1. **`ast.parse(data + "\n", filename=modname, type_comments=True)`** — a single `"\n"` is ALWAYS
   appended to the source. `mode` defaults to `"exec"`, **`feature_version` is NOT passed**
   (current-interpreter grammar, i.e. 3.12).
2. `filename=modname` — only when `modname` is truthy (empty string ⇒ `ast.parse` without
   `filename`, whose default is `"<unknown>"`). This is **why SyntaxError text shows the module
   name, not the file path**: `str(SyntaxError)` is rendered by CPython as
   `"<msg> (<filename>, line <n>)"`. Verified on 3.12.12:
   - `string_build("def f(:\n", "mymod", "/tmp/x.py")` ⇒
     `AstroidSyntaxError` with `str() == "Parsing Python code failed:\ninvalid syntax (mymod, line 1)"`;
     `error.filename == "mymod"`, `error.lineno == 1`, `error.offset == 7`.
   - empty modname ⇒ `"Parsing Python code failed:\ninvalid syntax (<unknown>, line 1)"`.
   So pylint's E0001 arg becomes e.g. `Parsing failed: 'invalid syntax (mymod, line 1)'` at
   line 1, col_offset 7.
3. **Null bytes**: on CPython 3.12 a NUL in the source raises
   `SyntaxError("source code string cannot contain null bytes")` (NOT ValueError; lineno/offset are
   `None`). Verified: E0001 arg
   `Parsing failed: 'source code string cannot contain null bytes'`, line `0` (because lineno is
   None → `get_ast` substitutes 0), col_offset None. The `ValueError` in the catch tuple is legacy
   (pre-3.12) / defensive; `TypeError` likewise; `MemoryError` is caught and also becomes
   `AstroidSyntaxError` (and E0001 at line 0, since MemoryError has no `lineno`).
4. **Type-comment retry heuristic**: if the SyntaxError's `.text` contains regex `#\s+type:`
   (note: requires at least one whitespace char between `#` and `type:` — `#type:` does NOT match
   and the error propagates), parsing is retried with `type_comments=False` and **without
   `filename`** (so a SyntaxError raised by the retry would show `<unknown>`). Verified: source
   `"if x: # type: int\n    pass\n"` fails with type_comments=True (`invalid syntax`) but builds
   fine through astroid via the retry.
5. `get_parser_module(type_comments=...)` ignores its parameter and always returns the same
   operator/context mapping tables (astroid/_ast.py:39-103). Operator string tables (used for
   `BinOp.op`, `BoolOp.op`, `UnaryOp.op`, `Compare.ops`, `AugAssign.op`):
   `+ & | ^ / // @ % * ** - << >>`, `and or`, `+ - not ~`,
   `== > >= in is "is not" < <= != "not in"`; contexts `Load/Store/Del`, `Param→Store`.
6. Module/package determination:
   - `node_file = os.path.abspath(path)` (or `"<?>"` if path is None).
   - `modname` ending in `.__init__` is stripped (9 chars) and `package=True`.
   - else `package = path is not None and os.path.splitext(os.path.basename(path))[0] == "__init__"`
     (so a file literally named `__init__.py` — or `__init__.anything` — is a package).
7. At import of `astroid.builder` on 3.12+:
   `warnings.filterwarnings("ignore", ".*invalid escape sequence", SyntaxWarning)`
   (builder.py:41-42) — invalid escapes never surface as warnings during parse.

---

<a name="5-module-node"></a>
## 5. Module node: construction & metadata

`TreeRebuilder.visit_module` (rebuilder.py:158-176):

```python
def visit_module(self, node, modname, modpath, package) -> nodes.Module:
    node, doc_ast_node = self._get_doc(node)
    newnode = nodes.Module(
        name=modname,
        file=modpath,
        path=[modpath],
        package=package,
    )
    newnode.postinit(
        [self.visit(child, newnode) for child in node.body],
        doc_node=self.visit(doc_ast_node, newnode),
    )
    return newnode
```

`nodes.Module.__init__` (scoped_nodes/scoped_nodes.py:237-277):

| Attribute | Value |
|---|---|
| `name` | modname (with `.__init__` stripped) |
| `file` | `os.path.abspath(path)` or `"<?>"` |
| `path` | `[file]` (single-element list) |
| `package` | bool per §4.6 |
| `pure_python` | `True` (default) — only False for inspect-built C modules |
| `locals` / `globals` | the **same dict object** (`self.locals = self.globals = {}`) |
| `body` | `[]` then set by postinit |
| `future_imports` | `set()` filled in `_post_build` |
| `lineno` | **0** |
| `col_offset` | **0** |
| `end_lineno`, `end_col_offset` | **None** |
| `parent` | None |
| `file_bytes` | None (class default; set to `data.encode("utf-8")` only by `string_build`) |
| `file_encoding` | set in `_post_build` (detected encoding, or `"utf-8"` for string_build) |
| `doc_node` | first-statement string Const or None (§8) |

`Module._astroid_fields = ("doc_node", "body")` (scoped_nodes.py:197) — note **doc_node comes first**
in `get_children()`, and `last_child()` scans the reversed field tuple so it returns `body[-1]` if
body is non-empty, else `doc_node`.

`Module.stream()` (scoped_nodes.py:287-301): `io.BytesIO(file_bytes)` if `file_bytes is not None`,
else `open(self.file, "rb")`.

Other notable Module API used by pylint: `block_range` (§14), `scope_attrs`
(`{"__name__", "__doc__", "__file__", "__path__", "__package__"}`, scoped_nodes.py:218-224).

Verified on 3.12.12: an empty module (`string_build("", "empty", None)`) has `body == []`,
`fromlineno == 0`, `tolineno == 0`, `package is False`.

---

<a name="6-post-build"></a>
## 6. _post_build: future imports, from-import locals, delayed assattr, transforms

astroid/builder.py:159-178 (exact order matters):

```python
def _post_build(self, module, builder, encoding) -> nodes.Module:
    module.file_encoding = encoding
    self._manager.cache_module(module)          # (1) cache FIRST (recursion guard)
    for from_node, global_names in builder._import_from_nodes:   # (2)
        if from_node.modname == "__future__":
            for symbol, _ in from_node.names:
                module.future_imports.add(symbol)
        self.add_from_names_to_locals(from_node, global_names)
    for delayed in builder._delayed_assattr:    # (3)
        self.delayed_assattr(delayed)
    if self._apply_transforms:                  # (4)
        module = self._manager.visit_transforms(module)
    return module
```

1. `cache_module` = `self.astroid_cache.setdefault(module.name, module)` (manager.py:420-422) —
   if a module with the same name is already cached, the **old one stays cached**, but the freshly
   built module object is still the one returned to pylint.
2. `builder._import_from_nodes` is a list in **visit order** of every `ast.ImportFrom` anywhere in
   the module (also inside functions/classes — rebuilder.py:1101-1104). For `__future__` imports the
   raw symbol names (not asnames) go into `module.future_imports`.
3. `add_from_names_to_locals` (builder.py:213-246):

```python
def add_local(parent_or_root, name):
    parent_or_root.set_local(name, node)
    my_list = parent_or_root.scope().locals[name]
    my_list.sort(key=lambda n: n.fromlineno or 0)

for name, asname in node.names:
    if name == "*":
        try:
            imported = node.do_import_module()
        except AstroidBuildingError:
            continue                       # <- conservatism: star-import of an
                                           #    unresolvable module adds NOTHING
        for name in imported.public_names():
            if name in global_name: add_local(module, name)
            else:                  add_local(node.parent, name)
    else:
        name = asname or name
        if name in global_name: add_local(module, name)
        else:                   add_local(node.parent, name)
```

   - Names from `from x import y` thus land in `locals` only AFTER the whole module is built, and
     the affected `locals[name]` list is **re-sorted stably by `fromlineno or 0`** (which can
     reorder e.g. a def and a from-import of the same name).
   - `global_name` is the *live* `dict_keys` view of the enclosing function's `global` names
     captured at visit time (rebuilder.py:1102-1103: `self._global_names[-1].keys() if self._global_names else ()`).
   - `imported.public_names()` honors `__all__` if resolvable.
4. `delayed_assattr` (builder.py:248-284) — runs **inference at build time** for every
   `AssignAttr` in Store context (collected at rebuilder.py:1227-1232 — but NOT those whose direct
   parent is an `ExceptHandler`). Bailouts, in order: `InferenceError` ⇒ whole node skipped;
   inferred `Uninferable` ⇒ continue; `type(inferred) in {bases.Instance, objects.ExceptionInstance}`
   ⇒ use `_proxied.instance_attrs` and skip if `_can_assign_attr` fails (slots check + not
   `builtins.object`, builder.py:58-66); other `Instance` subclasses (Const/Tuple/…) ⇒ continue;
   `Proxy`/`UninferableBase` ⇒ continue; `is_function` ⇒ `instance_attrs`; else `locals`;
   `AttributeError` anywhere ⇒ continue; duplicate node in list ⇒ not appended. This populates
   `ClassDef.instance_attrs` used by many checks.
5. Transforms last (§17).

---

<a name="7-rebuilder-dispatch"></a>
## 7. TreeRebuilder: dispatch and general position passthrough

`TreeRebuilder.__init__` (rebuilder.py:55-73): keeps `self._data = data.split("\n") if data else None`
(NB: empty source string ⇒ `_data is None` ⇒ all `position` attributes None), a `_global_names`
stack, `_import_from_nodes`, `_delayed_assattr`, and a memo dict `_visit_meths`.

Dispatch (rebuilder.py:481-492): `visit(node, parent)` returns None for None; otherwise method
`visit_` + `REDIRECT.get(cls_name, cls_name).lower()` where

```python
REDIRECT = {"arguments": "Arguments", "comprehension": "Comprehension",
            "ListCompFor": "Comprehension", "GenExprFor": "Comprehension",
            "excepthandler": "ExceptHandler", "keyword": "Keyword",
            "match_case": "MatchCase"}
```

**Default position rule** — the overwhelming majority of visit methods copy CPython positions
verbatim into the astroid node constructor:

```python
lineno=node.lineno, col_offset=node.col_offset,
end_lineno=node.end_lineno, end_col_offset=node.end_col_offset, parent=parent
```

This applies to: Assert, Await, Assign, AnnAssign, AugAssign, BinOp, BoolOp, Break, Call, Continue,
Compare, Delete, Dict, DictComp, Expr, ExceptHandler, For/AsyncFor, ImportFrom, GeneratorExp,
Attribute/AssignAttr/DelAttr, Global, If, IfExp, Import, JoinedStr, FormattedValue, NamedExpr,
Lambda, List, ListComp, Name/AssignName/DelName, Nonlocal, Const(ant), ParamSpec, Pass, Raise,
Return, Set, SetComp, Subscript, Starred, Try, TryStar, Tuple, TypeAlias, TypeVar, TypeVarTuple,
UnaryOp, While, With/AsyncWith, Yield, YieldFrom, Match, MatchValue, MatchSingleton, MatchSequence,
MatchMapping, MatchClass, MatchStar, MatchAs, MatchOr.

So for a Rust port: for those nodes the positions are EXACTLY what CPython's parser produces.
`lineno`/`end_lineno` are 1-based; `col_offset`/`end_col_offset` are 0-based **UTF-8 byte offsets**
within the (universal-newline-translated) line, and `end_col_offset` points one past the last
byte of the node's last token.

Exceptions and synthesized positions are documented in §§8-12.

---

<a name="8-doc-node"></a>
## 8. Docstring extraction (doc_node)

rebuilder.py:75-88:

```python
def _get_doc(self, node):
    try:
        if node.body and isinstance(node.body[0], ast.Expr):
            first_value = node.body[0].value
            if isinstance(first_value, ast.Constant) and isinstance(
                first_value.value, str
            ):
                doc_ast_node = first_value
                node.body = node.body[1:]
                return node, doc_ast_node
    except IndexError:
        pass  # ast built from scratch
    return node, None
```

Applied to `ast.Module`, `ast.ClassDef`, `ast.FunctionDef/AsyncFunctionDef` only.

- The docstring statement is **REMOVED from `body`** (the astroid `body` does not contain it);
  the Const value node (not the Expr) is visited separately and stored as `doc_node`
  (a child via `_astroid_fields`, listed BEFORE `body` for Module, and at index 4 of
  `("decorators","args","returns","type_params","doc_node","body")` for FunctionDef /
  `("decorators","bases","keywords","doc_node","body","type_params")` for ClassDef).
- Conditions: first statement is `Expr` wrapping `Constant` whose value `isinstance(..., str)` —
  bytes docstrings, f-strings, implicitly-concatenated strings are fine **only** if CPython folded
  them into a single `Constant` (implicit concatenation IS a single Constant; f-strings are
  JoinedStr ⇒ NOT a doc node, stays in body).
- The doc Const keeps its real CPython position.
- A module whose entire source is a docstring has `body == []`, `doc_node` set, and
  `tolineno == doc_node.tolineno` (via `last_child`).

---

<a name="9-defs"></a>
## 9. FunctionDef / AsyncFunctionDef / ClassDef positions, decorators, `position`

### 9.1 FunctionDef lineno is moved to the first decorator

rebuilder.py:1120-1148 (`_visit_functiondef`):

```python
self._global_names.append({})
node, doc_ast_node = self._get_doc(node)

lineno = node.lineno
if node.decorator_list:
    # Python 3.8 sets the line number of a decorated function
    # to be the actual line number of the function, but the
    # previous versions expected the decorator's line number instead.
    # We reset the function's line number to that of the
    # first decorator to maintain backward compatibility.
    # It's not ideal but this discrepancy was baked into
    # the framework for *years*.
    lineno = node.decorator_list[0].lineno

newnode = cls(name=node.name, lineno=lineno, col_offset=node.col_offset,
              end_lineno=node.end_lineno, end_col_offset=node.end_col_offset,
              parent=parent)
```

So a decorated function's `.lineno` = first decorator's line, while `.col_offset` stays the
column of the `def` keyword and `.end_lineno/end_col_offset` cover the whole function.
The "real" def line is recovered by the `fromlineno` property (§13) and by `position` (below).

postinit order (rebuilder.py:1149-1177): decorators are visited first, then returns, type
comment, then `postinit(args=visit(node.args), body=[...], decorators, returns,
type_comment_returns, type_comment_args, position=self._get_position_info(node, newnode),
doc_node, type_params=[...] if PY312_PLUS else [])`; finally `self._global_names.pop()` and
`parent.set_local(newnode.name, newnode)`.

### 9.2 ClassDef does NOT move lineno

rebuilder.py:834-873 (`visit_classdef`): `lineno=node.lineno` directly — CPython 3.8+ already puts
ClassDef.lineno at the `class` keyword (decorators excluded), and astroid does **not** apply the
decorator-compat shift here. Verified: `@cdeco` line 9 + `class K(Base):` line 10 ⇒
`K.lineno == K.fromlineno == 10`.

Also in visit_classdef: the `metaclass` keyword is extracted (first keyword whose `.arg ==
"metaclass"`, visited and its `.value` stored as `_metaclass`); remaining keywords (`kwd.arg !=
"metaclass"`) become `newnode.keywords`. `newstyle=True`. `parent.set_local(name, newnode)` at the
end.

### 9.3 Decorators node

rebuilder.py:929-956:

```python
if not node.decorator_list:
    return None
lineno = node.decorator_list[0].lineno
end_lineno = node.decorator_list[-1].end_lineno
end_col_offset = node.decorator_list[-1].end_col_offset

newnode = nodes.Decorators(
    lineno=lineno,
    col_offset=node.col_offset,     # <- the *function/class* col_offset, NOT the '@'
    end_lineno=end_lineno,
    end_col_offset=end_col_offset,
    parent=parent,
)
newnode.postinit([self.visit(child, newnode) for child in node.decorator_list])
```

The child decorator expressions have plain CPython positions (i.e. they start AFTER the `@`).
`Decorators._astroid_fields = ("nodes",)`; `Decorators.scope()` skips one level
(`self.parent.parent.scope()`, node_classes.py:2219-2230).

### 9.4 `position` (Position(lineno, col_offset, end_lineno, end_col_offset))

`_get_position_info` (rebuilder.py:103-156), VERBATIM:

```python
if not self._data:
    return None
end_lineno = node.end_lineno
if node.body:
    end_lineno = node.body[0].lineno
# pylint: disable-next=unsubscriptable-object
data = "\n".join(self._data[node.lineno - 1 : end_lineno])

start_token: TokenInfo | None = None
keyword_tokens: tuple[int, ...] = (token.NAME,)
if isinstance(parent, nodes.AsyncFunctionDef):
    search_token = "async"
elif isinstance(parent, nodes.FunctionDef):
    search_token = "def"
else:
    search_token = "class"

for t in generate_tokens(StringIO(data).readline):
    if (
        start_token is not None
        and t.type == token.NAME
        and t.string == node.name
    ):
        break
    if t.type in keyword_tokens:
        if t.string == search_token:
            start_token = t
            continue
        if t.string in {"def"}:
            continue
    start_token = None
else:
    return None

return Position(
    lineno=node.lineno + start_token.start[0] - 1,
    col_offset=start_token.start[1],
    end_lineno=node.lineno + t.end[0] - 1,
    end_col_offset=t.end[1],
)
```

Semantics:

- Operates on the **raw CPython node's** `lineno` (the def/class line, decorators excluded), so the
  source slice starts at the def/class line — decorator lines and comments above never appear in it.
- Slice end: line of the first body statement (1-based; the Python slice
  `self._data[node.lineno-1 : end_lineno]` includes that line), or `node.end_lineno` for an
  ast-with-no-body (impossible from parse).
- Tokenizes the slice with `tokenize.generate_tokens`. Finds `search_token`
  (`"async"`/`"def"`/`"class"` — all NAME tokens in 3.12), remembers it; then the FIRST NAME token
  equal to `node.name` ends the scan. Between them only the literal NAME `"def"` is tolerated
  (the async case); ANY other token (including COMMENT/NL) resets `start_token = None`.
- Result spans **keyword(s) through the name**, e.g. `async def some_func` ⇒
  `Position(l, 0, l, len("async def some_func"))`. Verified:
  `def f(a, b)` at line 5 ⇒ `Position(5, 0, 5, 5)`; `async def af(x)` at line 2 ⇒
  `Position(2, 0, 2, 12)`; `class K(Base)` at line 10 ⇒ `Position(10, 0, 10, 7)`.
- Returns None if `self._data` is falsy (e.g. module built from `""`) or the loop exhausts without
  the break (also note: a `tokenize.TokenError` from the slice is NOT caught and would propagate —
  in practice the header slice is always tokenizable up to the body's first line).
- `position` is `None` for every other node type (NodeNG.__init__ default, node_ng.py:114).

### 9.5 Empirically verified example

```
1  @deco1
2  @deco2(
3      arg=1,
4  )
5  def f(a, b):
6      """doc"""
7      return a
```
⇒ `f.lineno == 1`, `f.col_offset == 0`, `f.end_lineno == 7`, `f.end_col_offset == 12`,
`f.fromlineno == 5`, `f.position == Position(5, 0, 5, 5)`,
`f.decorators.lineno == 1`, `f.decorators.end_lineno == 4`, `f.decorators.end_col_offset == 1`,
`f.doc_node.value == 'doc'`, `len(f.body) == 1`, `f.tolineno == 7`, `f.blockstart_tolineno == 5`.

With a comment between decorator and def:

```
17  @deco1
18  # comment between
19  def h():
20      pass
```
⇒ `h.lineno == 17` (first decorator), **`h.fromlineno == 18`** (WRONG line — see §13 formula),
`h.position == Position(19, 0, 19, 5)` (correct). Because pylint prefers `position` (§15), messages
on `h` are reported at line 19 anyway.

---

<a name="10-assignname"></a>
## 10. Synthesized AssignName positions

`visit_assignname` (rebuilder.py:743-761) builds an `AssignName` from an arbitrary donor AST node:

```python
if node_name is None:
    return None
newnode = nodes.AssignName(
    name=node_name,
    lineno=node.lineno, col_offset=node.col_offset,
    end_lineno=node.end_lineno, end_col_offset=node.end_col_offset,
    parent=parent,
)
self._save_assignment(newnode)
return newnode
```

Donors and the resulting (sometimes surprising) positions — all verified:

| Construct | Donor node | Effect |
|---|---|---|
| function parameter | `ast.arg` (visit_arg, rebuilder.py:503-505) | exact arg position |
| `except ValueError as err:` | the whole `ast.ExceptHandler` (rebuilder.py:1047) | AssignName `err` spans the ENTIRE handler incl. body: lineno=`except` line, col 0, end = handler end. e.g. handler lines 3-4 ⇒ err position (3,0)-(4,8) |
| `case {…, **rest}` | `ast.MatchMapping` (rebuilder.py:1895) | `rest` spans the whole mapping pattern |
| `case [1, *more]` | `ast.MatchStar` (rebuilder.py:1931) | `more` spans the star pattern (e.g. col of `*`) |
| `case P() as pt` | `ast.MatchAs` (rebuilder.py:1946) | `pt` spans the whole as-pattern |
| PEP 695 `type X[T]`, `def f[T]` | `ast.TypeVar` / `ast.ParamSpec` / `ast.TypeVarTuple` (rebuilder.py:1492/1690/1712) | AssignName name spans the param |
| `*args` / `**kwargs` | the `ast.arg` vararg/kwarg node (rebuilder.py:520-543, built inline as `vararg_node`/`kwarg_node` on the Arguments ctor) | exact position |

`_save_assignment` (rebuilder.py:494-501): if the name is in the current function's `global`
set ⇒ `node.root().set_local(name, node)`; else `node.parent.set_local(name, node)`
(NodeNG.set_local delegates up to the nearest scope — node_ng.py:455-467).

Also: `visit_name` / `visit_attribute` split by ctx (rebuilder.py:1414-1451 / 1201-1243):
Store ⇒ AssignName/AssignAttr, Del ⇒ DelName/DelAttr, else Name/Attribute — all with verbatim
positions. AssignName/DelName trigger `_save_assignment`; AssignAttr is appended to
`_delayed_assattr` **unless its parent node is an ExceptHandler** (rebuilder.py:1227-1232).

---

<a name="11-positionless"></a>
## 11. Nodes with missing/special positions

| Node | lineno/col_offset/end_* | Source |
|---|---|---|
| `Arguments` | all `None` (`Arguments.__init__` passes None for all four) | node_classes.py:709-726 |
| `Comprehension` | all `None` explicitly | rebuilder.py:908-920 |
| `MatchCase` | constructor takes only `parent`; positions all None | rebuilder.py:1829-1838 (`nodes.MatchCase(parent=parent)`) |
| `Keyword` | `getattr(node, "lineno", None)` etc. (present on 3.9+, so real values on 3.12; `arg` is `None` for `**kw`) | rebuilder.py:1357-1369 |
| `Slice` | `getattr(node, ...)` fallback (real values on 3.12) | rebuilder.py:1564-1573 |
| `Module` | lineno 0, col 0, end None | §5 |
| `Decorators` | §9.3 mixed synthesis | rebuilder.py:944-954 |
| `DictUnpack` (dict `**` key placeholder) | copies the **value** node's full position | rebuilder.py:976-984 |

For positionless nodes, `fromlineno` falls back to `_fixed_source_line` (§13) and `tolineno`
recurses into the last child. Empirically: a `MatchCase` for `case {...}:` at line 9 has
`lineno is None`, `fromlineno == 9` (first child's line); a `lambda a, b=1: a` at line 5 has
`args.lineno is None`, `args.fromlineno == 5`.

`Arguments.fromlineno` override (node_classes.py:784-791):

```python
@cached_property
def fromlineno(self) -> int:
    lineno = super().fromlineno
    return max(lineno, self.parent.fromlineno or 0)
```

(`super().fromlineno` walks children via `_fixed_source_line`; the `max` with the parent function's
`fromlineno` matters for argument-less functions where the child walk would escape to the parent.)

---

<a name="12-node-notes"></a>
## 12. Remaining node-by-node construction notes

- **visit_arguments** (rebuilder.py:507-598): builds the `Arguments` node with
  `vararg`/`kwarg` *strings*, plus `vararg_node`/`kwarg_node` AssignNames (note these AssignNames
  get `parent=parent` — the FunctionDef/Lambda — not the Arguments node). Child lists, in postinit
  order: `args, defaults, kwonlyargs, posonlyargs, kw_defaults, annotations,
  kwonlyargs_annotations, posonlyargs_annotations, varargannotation, kwargannotation,
  type_comment_args, type_comment_kwonlyargs, type_comment_posonlyargs`. After postinit, the
  vararg/kwarg NAMES are registered in the parent's locals: `newnode.parent.set_local(vararg, newnode)`
  (the Arguments node itself is the assignment statement). `_astroid_fields` order (used by
  get_children / last_child / tolineno): node_classes.py:628-642 —
  `("args","defaults","kwonlyargs","posonlyargs","posonlyargs_annotations","kw_defaults",
  "annotations","varargannotation","kwargannotation","kwonlyargs_annotations",
  "type_comment_args","type_comment_kwonlyargs","type_comment_posonlyargs")`.
- **check_type_comment** (rebuilder.py:615-645): parses `node.type_comment` with the same parser;
  bailouts: falsy type_comment ⇒ None; SyntaxError ⇒ None; empty body (`# type: # comment`) ⇒
  None; result not an `Expr` ⇒ None; else returns `expr.value` (positions are from the
  mini-parse: line 1 of the comment string!). Attached as `type_annotation` on
  Assign/For/With and per-arg `type_comment_args`.
- **check_function_type_comment** (rebuilder.py:647-669): `parse_function_type_comment` via
  `ast.parse(s, "<type_comment>", "func_type")`; SyntaxError ⇒ None; returns
  `(returns, argtypes)` visited with `parent` = the FunctionDef.
- **visit_assign** (697-712): `targets`, `value`, plus `type_annotation` from type comment.
- **visit_augassign** (763-778): `op = bin_op_classes[type(node.op)] + "="` (e.g. `"+="`).
- **visit_compare** (887-906): `ops` is a list of `(op_string, expr_node)` pairs from
  `zip(node.ops, node.comparators)`; `left` separate.
- **visit_dict** (970-1002): items list of `(key, value)` node pairs; `None` key (a `**spread`)
  becomes a `DictUnpack` node carrying the value's position. NOTE the generator visits the
  **value first, then the key** (`rebuilt_value = self.visit(value, newnode)` before key) — child
  parent links identical either way, but mirrors evaluation order of nothing; just replicate.
- **visit_excepthandler** (1034-1050): `type` (expression or None), `name` (AssignName per §10,
  or None), `body`. ExceptHandler positions: CPython gives lineno at the `except` keyword,
  end spanning the handler's body.
- **visit_for/_visit_for** (1052-1084): target, iter, body, orelse, type_annotation.
- **visit_importfrom** (1086-1105): `fromname = node.module or ""`, `level = node.level or None`
  (level 0 stored as **None**!), `names = [(alias.name, alias.asname), ...]`. No locals are set at
  visit time (deferred, §6).
- **visit_import** (1292-1310): locals set immediately for each alias:
  `name = (asname or name).split(".")[0]`, honoring the `global` declaration stack.
- **visit_global** (1245-1258): records names in `self._global_names[-1]` only when inside a
  function (`if self._global_names:` — module-level `global` is a no-op).
- **visit_lambda** (1371-1381): plain positions, `args` + `body` (no doc node, no decorators).
- **visit_constant** (1466-1476): `nodes.Const(value=node.value, kind=node.kind, ...)` — value is
  the live Python object (str/bytes/int/float/complex/bool/None/Ellipsis); `kind` is `"u"` for
  `u"..."` literals, else None. **No constant folding** is performed by astroid or ast.parse;
  implicit string concatenation arrives pre-merged from CPython as one Constant (verified
  `("one"\n "two")` ⇒ one Const at (2,5)-(3,10), value `'onetwo'`).
- **visit_joinedstr / visit_formattedvalue** (1312-1340): plain passthrough; in 3.12 (PEP 701)
  the inner Const/FormattedValue/JoinedStr(format_spec) children carry REAL positions inside the
  f-string (verified `f"a{b!r:>{w}}c"`: Const "a" at cols 6-7, FormattedValue cols 7-17, inner
  Name `b` at col 8, format_spec JoinedStr at col 11, Const "c" cols 17-18). `conversion` is the
  int char code (-1 ⇒ none, 114=`!r`, 115=`!s`, 97=`!a`); `format_spec` is a JoinedStr or None.
- **visit_if** (1260-1274): test/body/orelse. `elif` chains arrive from CPython as a nested `If`
  inside `orelse` whose lineno/col are at the `elif` keyword (verified: elif at line 6 col 0).
- **visit_try / visit_trystar** (1613-1644): body/handlers/orelse/finalbody. CPython puts
  `Try.lineno` at the `try` keyword and `end_lineno` at the end of the last block (finally body).
- **_visit_with** (1758-1785): `items = [(context_expr_node, optional_vars_node_or_None), ...]`
  (a list of 2-tuples, not nodes), then body, then type_annotation.
- **visit_match*** (1815-1961): all plain passthrough except the AssignName synthesis (§10) and
  MatchCase positionlessness (§11). `MatchClass.kwd_attrs` is the raw string list.
- **TypeAlias/TypeVar/TypeVarTuple/ParamSpec** (1660-1717): `default_value` only visited on
  3.13+ (None on 3.12).
- **visit_slice** within **visit_subscript** (1564-1597): Subscript gets ctx-dependent class? No —
  Subscript always `nodes.Subscript` with `ctx`; Slice child holds lower/upper/step.

---

<a name="13-linenos"></a>
## 13. fromlineno / tolineno / _fixed_source_line — base + ALL overrides

Base (node_ng.py:399-443), all three `cached_property`/method:

```python
@cached_property
def fromlineno(self) -> int:
    if self.lineno is None:
        return self._fixed_source_line()
    return self.lineno

@cached_property
def tolineno(self) -> int:
    if self.end_lineno is not None:
        return self.end_lineno
    if not self._astroid_fields:
        last_child = None
    else:
        last_child = self.last_child()
    if last_child is None:
        return self.fromlineno
    return last_child.tolineno

def _fixed_source_line(self) -> int:
    line = self.lineno
    _node = self
    try:
        while line is None:
            _node = next(_node.get_children())   # FIRST child, repeatedly
            line = _node.lineno
    except StopIteration:
        parent = self.parent
        while parent and line is None:
            line = parent.lineno
            parent = parent.parent
    return line or 0
```

`last_child` (node_ng.py:248-257) scans `_astroid_fields` in REVERSE; empty lists/None are
skipped; returns the last element of the first non-empty field.

Since every node built from source on 3.12 has `end_lineno` set (except the positionless ones in
§11 and Module), `tolineno` is normally just `end_lineno`. Module: `end_lineno is None` ⇒
`tolineno = body[-1].tolineno` (or `doc_node.tolineno` if body empty; or `fromlineno`=0 if both
empty).

**Complete list of position-property overrides in astroid 4.0.4** (grep:
`def fromlineno|def tolineno|def block_range|def blockstart_tolineno` over astroid/nodes):

1. `Arguments.fromlineno` — node_classes.py:784-791 (§11).
2. `FunctionDef.fromlineno` — scoped_nodes.py:1386-1400:

```python
@cached_property
def fromlineno(self) -> int:
    # lineno is the line number of the first decorator, we want the def
    # statement lineno. Similar to 'ClassDef.fromlineno'
    lineno = self.lineno or 0
    if self.decorators is not None:
        lineno += sum(
            node.tolineno - (node.lineno or 0) + 1 for node in self.decorators.nodes
        )
    return lineno or 0
```

   I.e. first-decorator line + Σ(line-span of each decorator expression). EXACT only when
   decorators are contiguous and the `def` immediately follows; comments/blank lines between
   decorators and `def` make it **undershoot** (verified: decorator line 17, comment line 18, def
   line 19 ⇒ fromlineno 18). The decorator expression's own span excludes the `@`, but since `@`
   is on the same line it doesn't matter.
   (AsyncFunctionDef inherits.) **ClassDef has NO fromlineno override** in 4.0.4 — its
   lineno is already the `class` line.
3. `Module` — no override, but `lineno=0` ⇒ `fromlineno` 0.

Caveat: all three are `cached_property`, so any transform mutating `lineno` afterwards does not
update them (brains that synthesize nodes set lineno before first access).

---

<a name="14-block-range"></a>
## 14. blockstart_tolineno and block_range — exact, per class

These drive pylint's `FileState` block-level pragma scoping (`# pylint: disable=...` inside a
block) — they must match exactly.

Default `NodeNG.block_range` (node_ng.py:445-453):

```python
def block_range(self, lineno: int) -> tuple[int, int]:
    return lineno, self.tolineno
```

Default `blockstart_tolineno` for `MultiLineWithElseBlockNode` (_base_nodes.py:240-242):
`return self.lineno`.

Shared helper `_elsed_block_range` (_base_nodes.py:244-256):

```python
def _elsed_block_range(self, lineno, orelse, last=None):
    """Handle block line numbers range for try/finally, for, if and while statements."""
    if lineno == self.fromlineno:
        return lineno, lineno
    if orelse:
        if lineno >= orelse[0].fromlineno:
            return lineno, orelse[-1].tolineno
        return lineno, orelse[0].fromlineno - 1
    return lineno, last or self.tolineno
```

### Per-class table

| Class | blockstart_tolineno | block_range(lineno) |
|---|---|---|
| `Module` (scoped_nodes.py:303-310) | n/a | `(self.fromlineno, self.tolineno)` — **ignores argument**, i.e. (0, last line) |
| `FunctionDef` (scoped_nodes.py:1402-1422) | `returns.tolineno` if returns else `args.tolineno` | `(self.fromlineno, self.tolineno)` — ignores argument |
| `ClassDef` (scoped_nodes.py:1961-1979) | `bases[-1].tolineno` if bases else `fromlineno` | `(self.fromlineno, self.tolineno)` — ignores argument |
| `If` (node_classes.py:3025-3045) | `test.tolineno` | see code below |
| `While` (node_classes.py:4436-4452) | `test.tolineno` | `self._elsed_block_range(lineno, self.orelse)` |
| `For`/`AsyncFor` (node_classes.py:2719-2725) | `iter.tolineno` | **NO override — default** `(lineno, self.tolineno)` (verified) |
| `With`/`AsyncWith` (node_classes.py:4554-4560) | `items[-1][0].tolineno` | **NO override — default** |
| `Try` (node_classes.py:3885-3907) | inherited `self.lineno` | see code below |
| `TryStar` (node_classes.py:3986-4008) | inherited `self.lineno` | identical to Try |
| `ExceptHandler` (node_classes.py:2640-2650) | `name.tolineno` if name else `type.tolineno` if type else `lineno` (NB: `name.tolineno` is the handler's end line because of the §10 quirk!) | default |
| `Match` | n/a (`_multi_line_block_fields = ("cases",)`) | default |
| everything else | n/a | default |

`If.block_range` (node_classes.py:3033-3045):

```python
def block_range(self, lineno: int) -> tuple[int, int]:
    if lineno == self.body[0].fromlineno:
        return lineno, lineno
    if lineno <= self.body[-1].tolineno:
        return lineno, self.body[-1].tolineno
    return self._elsed_block_range(lineno, self.orelse, self.body[0].fromlineno - 1)
```

Verified for `if a:(4) pass(5) elif b:(6) pass(7) else:(8) pass(9)`:
`if.block_range(4) == (4,5)`, `(5) == (5,5)`; nested-elif-If: `block_range(6) == (6,7)`,
`(7) == (7,7)`, `(8) == (8,8)`, `(9) == (9,9)`.

`Try.block_range` (node_classes.py:3885-3907):

```python
def block_range(self, lineno: int) -> tuple[int, int]:
    if lineno == self.fromlineno:
        return lineno, lineno
    if self.body and self.body[0].fromlineno <= lineno <= self.body[-1].tolineno:
        # Inside try body - return from lineno till end of try body
        return lineno, self.body[-1].tolineno
    for exhandler in self.handlers:
        if exhandler.type and lineno == exhandler.type.fromlineno:
            return lineno, lineno
        if exhandler.body[0].fromlineno <= lineno <= exhandler.body[-1].tolineno:
            return lineno, exhandler.body[-1].tolineno
    if self.orelse:
        if self.orelse[0].fromlineno - 1 == lineno:
            return lineno, lineno
        if self.orelse[0].fromlineno <= lineno <= self.orelse[-1].tolineno:
            return lineno, self.orelse[-1].tolineno
    if self.finalbody:
        if self.finalbody[0].fromlineno - 1 == lineno:
            return lineno, lineno
        if self.finalbody[0].fromlineno <= lineno <= self.finalbody[-1].tolineno:
            return lineno, self.finalbody[-1].tolineno
    return lineno, self.tolineno
```

(Note the `except` line itself only matches `exhandler.type.fromlineno` when the handler HAS a
type; a bare `except:` line falls through — typically to `(lineno, self.tolineno)`.)

`_multi_line_block_fields` (used by return/yield/assign collectors, not positions):
ExceptHandler `("body",)`; For/While/If `("body","orelse")`; Try/TryStar
`("body","handlers","orelse","finalbody")`; With `("body",)`; Match `("cases",)`;
MatchCase `("body",)`; FunctionDef `("body",)`.

---

<a name="15-pylint-location"></a>
## 15. How pylint converts a node into a message location

pylint/lint/pylinter.py:1211-1230 (`_add_one_message`):

```python
if node:
    if node.position:
        if not line:
            line = node.position.lineno
        if not col_offset:
            col_offset = node.position.col_offset
        if not end_lineno:
            end_lineno = node.position.end_lineno
        if not end_col_offset:
            end_col_offset = node.position.end_col_offset
    else:
        if not line:
            line = node.fromlineno
        if not col_offset:
            col_offset = node.col_offset
        if not end_lineno:
            end_lineno = node.end_lineno
        if not end_col_offset:
            end_col_offset = node.end_col_offset
```

Consequences:

- For ClassDef/FunctionDef/AsyncFunctionDef with a non-None `position`, the message
  line/col/end-span is the **keyword+name span** (`def f` / `class K`), NOT
  fromlineno/end_lineno of the full node. When `position is None` (built from empty data / token
  scan failure), it falls back to `fromlineno` (decorator-arithmetic line, §13).
- For every other node: `fromlineno`, `col_offset`, `end_lineno`, `end_col_offset` verbatim.
- Note `if not line:` — a passed-in `line=0` is also replaced (falsy), and a node `col_offset` of
  0 cannot be overridden by explicit `col_offset=0` (both falsy — same output either way).

---

<a name="16-locals"></a>
## 16. Scope/locals bookkeeping during the rebuild

- `set_local(name, stmt)` on scope nodes appends to `scope.locals[name]` list
  (`locals.setdefault(name, []).append(node)` — scoped_nodes/mixin or LocalsDictNodeNG).
  Insertion order = visit order = source order, EXCEPT from-import names which are appended in
  `_post_build` and then that one list is sorted by `fromlineno or 0` (stable sort).
- Registrations during visiting: ClassDef/FunctionDef register **themselves** in their parent's
  locals at the END of their visit (after children); `Import` registers immediately; AssignName /
  DelName register at creation; Arguments registers vararg/kwarg names; ImportFrom defers.
- `_global_names` is a stack of dicts pushed/popped per function
  (`_visit_functiondef`). `visit_global` adds `{name: [Global nodes]}` to the top. Affects
  `_save_assignment`, `visit_import`, and the global-name set captured by `visit_importfrom`.
- `Module.locals is Module.globals` (same dict object).

---

<a name="17-transforms"></a>
## 17. TransformVisitor: exact algorithm and ordering

astroid/transforms.py:36-163. Applied as the LAST step of `_post_build` via
`self._manager.visit_transforms(module)` (manager.py:127-129); the single
`TransformVisitor` instance lives in `AstroidManager.brain["_transform"]`.

```python
def _transform(self, node):
    cls = node.__class__
    for transform_func, predicate in self.transforms[cls]:
        if predicate is None or predicate(node):
            ret = transform_func(node)
            if ret is not None:
                _invalidate_cache()
                node = ret
            if ret.__class__ != cls:
                # Can no longer apply the rest of the transforms.
                break
    return node

def _visit(self, node):
    for name in node._astroid_fields:
        value = getattr(node, name)
        visited = self._visit_generic(value)
        if visited != value:
            setattr(node, name, visited)
    return self._transform(node)

def _visit_generic(self, node):
    if not node:
        return node
    if isinstance(node, list):
        return [self._visit_generic(child) for child in node]
    if isinstance(node, tuple):
        return tuple(self._visit_generic(child) for child in node)
    if isinstance(node, str):
        return node
    try:
        return self._visit(node)
    except RecursionError:
        warnings.warn(...)
        return node           # untransformed on RecursionError
```

Facts:

- **Post-order traversal** following `_astroid_fields` in declared order: children are transformed
  before their parent.
- `self.transforms` is `defaultdict(type → list[(transform, predicate)])`; lookup is by EXACT
  class (`node.__class__`), no subclass dispatch. List order = registration order =
  `register_all_brains` order (astroid/brain/helpers.py:33-137; executed once at
  `import astroid` via astroid/astroid_manager.py:15-20).
- A transform returning non-None REPLACES the node (and invalidates the inference cache); if the
  replacement is a different class, remaining transforms for the old class are skipped (`break`).
- **Subtle and load-bearing**: `if ret.__class__ != cls: break` is evaluated even when the
  transform returned `None` (in-place mutators like `attr_attributes_transform`,
  `_patch_uuid_class`, `_transform_lru_cache`, the brain_io transforms all return None).
  `None.__class__` (`NoneType`) ≠ cls, so **a matched transform that returns None terminates the
  transform chain for that node** — later registered transforms whose predicates would also match
  never run. Verified empirically: two transforms registered on `Pass`, the first returning None
  ⇒ only the first runs. Transforms whose PREDICATE does not match are skipped without breaking.
  Example consequence: a user class that is both `@attr.s`-decorated and an Enum subclass gets
  only the attrs transform (attrs registers before namedtuple_enum and returns None).
- `RecursionError` during the walk leaves the subtree untransformed (warning only).
- Note `if visited != value:` uses `__eq__` (identity for NodeNG; list compare element-wise).

Transforms also run on inspect-built (C extension) modules via `module_build`
(builder.py:102-109), and `AstroidBuilder(manager)` construction triggers
`manager.bootstrap()` once (builder.py:81-82) which builds the `builtins` module by introspection
(`raw_building._astroid_bootstrapping`).

---

<a name="18-inference-tip"></a>
## 18. inference_tip mechanism

astroid/inference_tip.py:87-130. `register_transform(NodeClass, inference_tip(fn), predicate)`
registers a *transform* whose action at **build time** is only:

```python
node._explicit_inference = _inference_tip_cached(infer_function)
return node
```

- The PREDICATE runs at build time on every node of that class in every module (cost + ordering
  effects); the inference function runs lazily when `node.infer()` is called
  (node_ng.py:140-152, `UseInferenceDefault` ⇒ fall back to standard `_infer`).
- `_inference_tip_cached` caches per `(func, node, context-or-None)` in a 64-entry OrderedDict
  with a recursion guard that raises `UseInferenceDefault` on re-entrance.
- `raise_on_overwrite=True` (only the dataclass tips) raises `InferenceOverwriteError` if a
  different `_explicit_inference` was already set.
- Because the wrapped transform returns the SAME node, the `ret.__class__ != cls` break never
  triggers for inference tips, and multiple tips on one node simply overwrite
  `_explicit_inference` — the LAST matching registration wins.

---

<a name="19-brains"></a>
## 19. Brain modules — full table

Registered once at `import astroid` in EXACTLY this order (brain/helpers.py:87-137):
argparse, attrs, boto3, builtin_inference, collections, crypt, ctypes, curses, dataclasses,
datetime, dateutil, functools, gi, hashlib, http, hypothesis, io, mechanize, multiprocessing,
namedtuple_enum, numpy_core_einsumfunc, numpy_core_fromnumeric, numpy_core_function_base,
numpy_core_multiarray, numpy_core_numerictypes, numpy_core_umath, numpy_random_mtrand, numpy_ma,
numpy_ndarray, numpy_core_numeric, pathlib, pkg_resources, pytest, qt, random, re, regex,
responses, scipy_signal, signal, six, sqlalchemy, ssl, statistics, subprocess, threading, type,
typing, unittest, uuid.

Registration kinds:
- **TREE** = `register_transform(NodeClass, fn, pred)` with a real mutating transform — runs
  during the build of EVERY module whose nodes match.
- **TIP** = `register_transform(NodeClass, inference_tip(fn), pred)` — predicate runs at build
  time, inference deferred (§18).
- **EXT** = `register_module_extender(manager, "modname", factory)` (brain/helpers.py:18-29) — a
  Module TREE transform with predicate `n.name == module_name`; merges the factory module's
  locals into that stdlib/3rd-party module when *it* is built (never fires on user modules unless
  a user module is literally named e.g. `re` at top level).
- **HOOK** = `register_failed_import_hook` — only on import failure.

| brain | registrations | what it changes | in-scope checks plausibly affected |
|---|---|---|---|
| brain_argparse | TIP Call `infer_namespace` pred `_looks_like_namespace` (func name/attrname == "Namespace") | calls named `Namespace(...)` infer to a synthetic class instance | E1120/E1123 family on Namespace() |
| brain_attrs | TREE ClassDef `attr_attributes_transform` pred `is_decorated_with_attrs` (decorator `as_string()` in attr/attrs names, or inferred root `attr._next_gen`) | injects `locals[name]`/`instance_attrs[name] = [Unknown rhs]` for attr.ib/field/annotated attributes | E1120/E1125 on attrs classes; attribute checks |
| brain_boto3 | TREE ClassDef pred `qname() == "boto3.resources.factory.ResourceFactory"` | adds `__getattr__` to that one class | none for user code |
| brain_builtin_inference | TIP Call ×19 for names `bool super callable property getattr hasattr tuple set list dict frozenset type slice isinstance issubclass len str int dict.fromkeys` (pred `_builtin_filter_predicate`: `Name` func matching name, or `dict.fromkeys` Attribute); TIP ClassDef `__new__`-decorator; TIP Call `.copy`; TIP Call str-format pred `_is_str_format_call` | inference results for builtin calls (e.g. `len(...)` → Const int) | E11xx arg checking on these calls; unbalanced-tuple E0632 via inferred containers |
| brain_collections | EXT "collections"; TREE ClassDef `easy_class_getitem_inference` pred `_looks_like_subscriptable` (**qname startswith "collections"/"_collections" AND has `__class_getitem__`**) | deque etc. methods; `__class_getitem__` mock | E1136 unsubscriptable on collections.abc generics |
| brain_crypt / curses / datetime / dateutil / hashlib / http / mechanize / multiprocessing / numpy_* (most) / pkg_resources / pytest / responses / scipy_signal / signal / sqlalchemy / ssl / subprocess / threading / unittest | EXT only (module extenders for those exact module names; numpy function_base/multiarray/ndarray also TIP Attribute/Name/Call) | enrich stdlib/3rd-party modules at *their* build | indirectly all inference-based E checks when user imports them |
| brain_ctypes | EXT "ctypes" | adds value/restype etc. | — |
| brain_dataclasses | TREE ClassDef `dataclass_transform` pred `is_decorated_with_dataclass` (**inference-based**: each decorator inferred; match ClassDef/FunctionDef named `dataclass` whose root module ∈ {dataclasses, marshmallow_dataclass, pydantic.dataclasses}); TIP Call `infer_dataclass_field_call` (field() in dataclass body); TIP Unknown `infer_dataclass_attribute`. (A Module TREE transform exists only on PY313+ — NOT active on 3.12.) | sets `is_dataclass=True`; replaces field-call RHS view with `Unknown` in `instance_attrs`; **synthesizes `__init__`** from generated source (`init_node.lineno = init_node.col_offset = None`; stored in `node.locals["__init__"]`), plus module-level `_HAS_DEFAULT_FACTORY` local | E1120/E1123/E1125 on dataclass instantiation; E1101-adjacent attribute resolution (E1101 excluded though) |
| brain_functools | TREE FunctionDef `_transform_lru_cache` pred `_looks_like_lru_cache` (decorated with Attribute/Call matching functools lru_cache); TIP Call partial | replaces `special_attributes` with LruWrappedModel (adds `cache_clear` etc.); partial() inference | E1120 on partial calls |
| brain_gi | HOOK gi modules; TREE Call `_register_require_version` pred `_looks_like_require_version` | gi introspection | — |
| brain_hypothesis | TREE FunctionDef `remove_draw_parameter_from_composite_strategy` pred `is_decorated_with_st_composite` (first arg literally named `draw` + decorator `as_string()` ∈ {"composite","st.composite","strategies.composite","hypothesis.strategies.composite"}) | **deletes args[0]/annotations[0]/type_comment_args[0]** of the function | E1120 no-value-for-parameter on calls to @st.composite functions |
| brain_io | TREE ClassDef pred `node.name in {"BufferedWriter","BufferedReader"}` → adds `locals["raw"]`; TREE ClassDef pred `node.name == "TextIOWrapper"` → adds `locals["buffer"]` | **name-only predicate**: fires on ANY class with these names in ANY module (instantiates `_io` classes) | attribute inference on stdout/stderr `.buffer` |
| brain_namedtuple_enum | TIP Call namedtuple pred `_looks_like_namedtuple`; TIP Call enum-functional pred `_looks_like_enum`; **TREE ClassDef `infer_enum_class` pred `_is_enum_subclass`** (`cls.is_subtype_of("enum.Enum")` — inference-based); TIP ClassDef NamedTuple-base; TIP FunctionDef typing.NamedTuple; TIP Call typing.NamedTuple | for Enum subclasses, rewrites class body: each member becomes a synthetic instance with `name`/`value` locals (mutates `node.locals`); namedtuple calls produce synthetic classes | E1101 excluded, but E1120/E1133/E1136 on enums/namedtuples; invalid-enum etc. |
| brain_numpy_core_function_base/multiarray/numeric/random_mtrand | EXT + TIP Attribute/Name for selected numpy members | numpy call inference | — unless numpy imported |
| brain_numpy_ndarray | TIP Attribute `infer_numpy_ndarray` | ndarray attrs | — |
| brain_pathlib | TIP Subscript pred `_looks_like_parents_subscript` | `Path.parents[i]` → Path | E1136 |
| brain_qt | EXT "PyQt4.QtCore"; TREE FunctionDef `transform_pyqt_signal` pred `_looks_like_signal`; TREE ClassDef `transform_pyside_signal` pred qname ∈ {PySide.QtCore.Signal, PySide2.QtCore.Signal} | adds emit/connect/disconnect to signal classes | E1101 excluded; E1120 on .emit |
| brain_random | TIP Call `infer_random_sample` pred `_looks_like_random_sample` | random.sample → List | E1133/E1136 |
| brain_re | EXT "re"; TIP Call pred `_looks_like_pattern_or_match` | re.Pattern/Match `__class_getitem__` etc. | E1136 |
| brain_regex | EXT "regex"; TIP Call (same idea) | — | — |
| brain_six | EXT "six" + EXT "requests.packages.urllib3.packages.six"; HOOK `_six_fail_hook` (six.moves); TREE ClassDef `transform_six_add_metaclass` pred `_looks_like_decorated_with_six_add_metaclass`; TREE ClassDef `transform_six_with_metaclass` pred `_looks_like_nested_from_six_with_metaclass` | builds a fake `six` module (six.moves mapped to real stdlib); sets `_metaclass` from `@six.add_metaclass(M)` / `with_metaclass(M, …)` bases | E0240/E0239 metaclass checks (E02xx in scope!), MRO-related E0241 |
| brain_statistics | TIP Call quantiles | — | — |
| brain_type | TIP Name pred `_looks_like_type_subscript` | `type[...]` subscript inference | E1136 |
| brain_typing | TIP Call TypeVar/NewType; TIP Subscript `infer_typing_attr`; TIP Call typing.cast; TIP FunctionDef TypedDict; TIP Call typing alias / special alias; PY312: EXT "typing" + TIP ClassDef pep695 generics | typing constructs infer to usable classes | E1136 unsubscriptable, E1120 on cast etc. |
| brain_uuid | TREE ClassDef pred `qname() == "uuid.UUID"` | adds `locals["int"]` | — |

Summary for the port: the brains that can mutate a **plain user module's tree** at build time are
exactly: **attrs, dataclasses, functools(lru_cache), hypothesis, io (name-collision quirk),
namedtuple_enum (Enum subclasses), qt (signal-named funcs), six (add_metaclass/with_metaclass),
collections/boto3/uuid (qname-restricted, effectively never user code)**. Everything else either
extends a specific named module or only plants inference tips.

Note also: several TREE predicates perform **inference at build time** (dataclasses, six,
namedtuple `_is_enum_subclass`) — and inference can trigger building other modules (imports),
recursively. The build cache insert in `_post_build` happens before transforms partly for this
reason.

---

<a name="20-cpython-quirks"></a>
## 20. CPython 3.12 position quirks passed through verbatim (verified)

- Parenthesized expressions: parentheses are NOT part of most expression spans
  (`x = (a + b)` ⇒ BinOp cols 5-10), EXCEPT: parenthesized tuples include their parens
  (`(1, 2)` ⇒ cols 0-6; bare `1, 2` ⇒ 0-4) and GeneratorExp includes its enclosing parens —
  even when they belong to a call: `f(i for i in x)` ⇒ GeneratorExp cols 1-15.
- `elif`: nested `If` node at the `elif` keyword position.
- f-strings (PEP 701): all sub-nodes have real in-string positions; nested
  format_spec JoinedStr too.
- Implicit string concatenation: a single `Constant` spanning first to last fragment (across
  lines).
- `ast.Slice` has real positions (3.9+); `ast.keyword` too.
- Decorated FunctionDef/ClassDef raw nodes: lineno at `def`/`class`, NOT the decorators
  (astroid re-adds the decorator shift only for functions, §9.1).
- `withitem` has no positions (astroid stores plain tuples for items anyway).
- `ExceptHandler.end_lineno/end_col_offset` cover the handler body.
- `Module` has no positions in CPython; astroid pins (0, 0, None, None).

---

<a name="21-ordering"></a>
## 21. Order-dependency summary

1. `transforms` registry: `defaultdict(list)`; per-class transform list runs in brain
   registration order (§19 list). The `break`-on-class-change/None-return rule makes order
   observable.
2. Transform traversal: post-order over `_astroid_fields` in declared per-class order.
3. `locals[name]` lists: visit order; from-import names appended at `_post_build` then that
   list sorted (stable) by `fromlineno or 0` (builder.py:221-226).
4. `builder._import_from_nodes`, `builder._delayed_assattr`: visit order, processed FIFO.
5. `module.future_imports`: a `set` — unordered; consumers only test membership.
6. `astroid_cache`: plain dict keyed by modname; `setdefault` semantics on cache_module.
7. `_visit_meths` memo: per-TreeRebuilder method cache (no behavioral effect).
8. `_get_doc` mutates the raw CPython node (`node.body = node.body[1:]`) BEFORE children are
   visited — body indices in the astroid tree are shifted by one relative to raw source
   statements when a docstring exists.
9. `_global_names` dict-of-lists: only key membership is consulted.
10. inference_tip `_cache`: OrderedDict bounded at 64 entries (FIFO eviction) — only affects
    inference performance/results under heavy recursion, not build output.
