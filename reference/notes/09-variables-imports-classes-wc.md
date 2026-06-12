# 09 — Remaining W/C/R messages: variables.py, imports.py, classes/, exceptions.py, method_args (exact spec, pylint 4.0.5 / astroid 4.0.4)

Scope: every W/C/R (+ a couple of shared F/W4xxx) message owned by
`pylint/checkers/variables.py`, `pylint/checkers/imports.py`,
`pylint/checkers/classes/class_checker.py`, `pylint/checkers/exceptions.py`,
`pylint/checkers/method_args.py` that is NOT already specified for `-E` mode in
`reference/notes/05-variables.md` (E06xx variables machinery) and
`reference/notes/08-other-checkers.md` (E-category for classes/exceptions/
imports-E0402, F0202, E3102). The E-machinery (NamesConsumer, scope walking,
`safe_infer`, `are_exclusive`, `in_type_checking_block`, `is_sys_guard`,
inference protocols) is shared — this doc only specs the *additional* trigger
logic, message templates, arg computation, report node, config gates and
ordering quirks for the W/C/R family needed for full-pylint mode.

All file:line cites refer to `reference/pylint/pylint/...` at tag v4.0.5 and
`reference/astroid/astroid/...` at v4.0.4.

Conventions:
- "HIGH/INFERENCE/INFERENCE_FAILURE/CONTROL_FLOW" = pylint.interfaces
  confidence objects. Confidence affects nothing in default output (no
  `--confidence` filtering by default) but `is_message_enabled(msgid, line,
  confidence)` checks `confidence.name not in self.config.confidence` only
  when `self.config.confidence` is non-empty (default empty → no filtering).
- Default-enabled: EVERY message in this document is enabled by default in
  full-pylint mode (none carry `"default_enabled": False`). The `enabled`
  flag in `crates/pycheckers/src/msgs.rs` refers to `-E` mode only; all of
  these have `enabled: false` there except the E/F ones already done.
- Report position: unless stated, message position comes from the `node=`
  argument via `PyLinter._add_one_message` (pylinter.py:1195-1280): if
  `node.position` is set (FunctionDef/ClassDef keyword anchoring) use it,
  else `node.fromlineno`/`node.col_offset`/`node.end_lineno`/
  `node.end_col_offset`.
- `%`-interpolation: `msg = template % args` (pylinter.py:1252-1254). `%r` of
  a str produces `'name'`; `%r` of a Python list produces `['a', 'b']`
  (used by W0244). W0213 uses a dict args with named `%(overlap)s` fields.

================================================================================
# 0. Message inventory (cross-checked against crates/pycheckers/src/msgs.rs)
================================================================================

variables.py (MSGS at variables.py:351-501):
| id | symbol | template | report node |
|----|--------|----------|-------------|
| W0601 | global-variable-undefined | `Global variable %r undefined at the module level` | Global stmt |
| W0602 | global-variable-not-assigned | `Using global for %r but no assignment is done` | Global stmt |
| W0603 | global-statement | `Using the global statement` | Global stmt |
| W0604 | global-at-module-level | `Using the global statement at the module level` | Global stmt |
| W0611 | unused-import | `Unused %s` | Import/ImportFrom stmt |
| W0612 | unused-variable | `Unused variable %r` | defining stmt |
| W0613 | unused-argument | `Unused argument %r` | AssignName in Arguments |
| W0614 | unused-wildcard-import | `Unused import(s) %s from wildcard import of %s` | ImportFrom stmt |
| W0621 | redefined-outer-name | `Redefining name %r from outer scope (line %s)` | local def / inner ExceptHandler |
| W0622 | redefined-builtin | `Redefining built-in %r` | defining stmt / Global stmt |
| W0631 | undefined-loop-variable | `Using possibly undefined loop variable %r` | Name |
| W0632 | unbalanced-tuple-unpacking | `Possible unbalanced tuple unpacking with sequence %s: left side has %d label%s, right side has %d value%s` | Assign (old_names: E0632) |
| W0640 | cell-var-from-loop | `Cell variable %s defined in loop` | Name |
| W0641 | possibly-unused-variable | `Possibly unused variable %r` | defining stmt |
| W0642 | self-cls-assignment | `Invalid assignment to %s in method` | Assign |
| W0644 | unbalanced-dict-unpacking | `Possible unbalanced dict unpacking with %s: left side has %d label%s, right side has %d value%s` | Assign or For |

(E0601-E0606, E0611, E0633, E0643 → notes/05.)

imports.py (MSGS at imports.py:227-317, plus shared W4901 from
DeprecatedMixin, deprecated.py:46-53):
| id | symbol | template | report node |
|----|--------|----------|-------------|
| R0401 | cyclic-import | `Cyclic import (%s)` | **no node** (last module, line 1) |
| R0402 | consider-using-from-import | `Use 'from %s import %s' instead` | Import |
| W0401 | wildcard-import | `Wildcard import %s` | ImportFrom |
| W0404 | reimported | `Reimport %r (imported line %s)` | import stmt |
| W0406 | import-self | `Module import itself` | import stmt |
| W0407 | preferred-module | `Prefer importing %r instead of %r` | import stmt |
| W0410 | misplaced-future | `__future__ import is not the first non docstring statement` | ImportFrom |
| W0416 | shadowed-import | `Shadowed %r (imported line %s)` | import stmt |
| C0410 | multiple-imports | `Multiple imports on one line (%s)` | Import |
| C0411 | wrong-import-order | `%s should be placed before %s` | import stmt |
| C0412 | ungrouped-imports | `Imports from package %s are not grouped` | import stmt |
| C0413 | wrong-import-position | `Import "%s" should be placed at the top of the module` | import stmt |
| C0414 | useless-import-alias | `Import alias does not rename original package` | import stmt |
| C0415 | import-outside-toplevel | `Import outside toplevel (%s)` | import stmt |
| W4901 | deprecated-module | `Deprecated module %r` | import stmt (old_names: W0402; `shared: True` — also registered by stdlib.py's DeprecatedChecker; pylint allows the duplicate registration because of the shared flag) |

(E0401 import-error, E0402 → notes/08 §16.)

classes/class_checker.py (MSGS at class_checker.py:493-734):
| id | symbol | template | report node |
|----|--------|----------|-------------|
| W0201 | attribute-defined-outside-init | `Attribute %r defined outside __init__` | AssignAttr |
| W0211 | bad-staticmethod-argument | `Static method with %r as first argument` | FunctionDef |
| W0212 | protected-access | `Access to a protected member %s of a client class` | Attribute/AssignAttr |
| W0213 | implicit-flag-alias | `Flag member %(overlap)s shares bit positions with %(sources)s` | AssignName |
| W0221 | arguments-differ | `%s %s %r method` | FunctionDef |
| W0222 | signature-differs | `Signature differs from %s %r method` | FunctionDef |
| W0223 | abstract-method | `Method %r is abstract in class %r but is not overridden in child class %r` | ClassDef |
| W0231 | super-init-not-called | `__init__ method from base class %r is not called` | FunctionDef(__init__) |
| W0233 | non-parent-init-called | `__init__ method from a non direct base class %r is called` | Attribute (call func) |
| W0236 | invalid-overridden-method | `Method %r was expected to be %r, found it instead as %r` | FunctionDef |
| W0237 | arguments-renamed | `%s %s %r method` | FunctionDef |
| W0238 | unused-private-member | ``Unused private member `%s.%s` `` | FunctionDef/AssignName/AssignAttr |
| W0239 | overridden-final-method | `Method %r overrides a method decorated with typing.final which is defined in class %r` | FunctionDef |
| W0240 | subclassed-final-class | `Class %r is a subclass of a class decorated with typing.final: %r` | ClassDef |
| W0244 | redefined-slots-in-subclass | `Redefined slots %r in subclass` | slots value node |
| W0245 | super-without-brackets | `Super call without brackets` | Name("super") |
| W0246 | useless-parent-delegation | `Useless parent or super() delegation in method %r` | FunctionDef (old_names: W0235 useless-super-delegation) |
| C0202 | bad-classmethod-argument | `Class method %s should have %s as first argument` | FunctionDef |
| C0203 | bad-mcs-method-argument | `Metaclass method %s should have %s as first argument` | FunctionDef |
| C0204 | bad-mcs-classmethod-argument | `Metaclass class method %s should have %s as first argument` | FunctionDef |
| C0205 | single-string-used-for-slots | `Class __slots__ should be a non-string iterable` | ClassDef |
| R0202 | no-classmethod-decorator | `Consider using a decorator instead of calling classmethod` | Assign target |
| R0203 | no-staticmethod-decorator | `Consider using a decorator instead of calling staticmethod` | Assign target |
| R0205 | useless-object-inheritance | `Class %r inherits from object, can be safely removed from bases in python3` | ClassDef |
| R0206 | property-with-parameters | `Cannot have defined parameters for properties` | FunctionDef |

(F0202, E0202/E0203/E0211/E0213, E0236-E0245 → notes/08 §3. R0201
no-self-use was DELETED from core pylint — `message/_deleted_message_ids.py:129`
— and lives only in the optional extension `pylint/extensions/no_self_use.py`
(R0903→W6301? no: R0201 old name of the extension's message). The extension is
NOT registered by default → R0201 never emitted, and `--disable=no-self-use`
is accepted because deleted ids are silently ignored.)

exceptions.py (MSGS at exceptions.py:62-180):
| id | symbol | template | report node |
|----|--------|----------|-------------|
| W0702 | bare-except | `No exception type(s) specified` | ExceptHandler |
| W0705 | duplicate-except | `Catching previously caught exception type %s` | handler.type |
| W0706 | try-except-raise | `The except handler raises immediately` | ExceptHandler |
| W0707 | raise-missing-from | `Consider explicitly re-raising using %s'%s from %s'` | Raise |
| W0711 | binary-op-exception | `Exception to catch is the result of a binary "%s" operation` | ExceptHandler |
| W0715 | raising-format-tuple | `Exception arguments suggest string formatting might be intended` | Raise |
| W0716 | wrong-exception-operation | `Invalid exception operation. %s` | BinOp/Compare |
| W0718 | broad-exception-caught | `Catching too general exception %s` | handler.type (old_names: W0703 broad-except) |
| W0719 | broad-exception-raised | `Raising too general exception: %s` | Raise |

(E0701/E0702/E0704/E0705/E0710/E0711/E0712 → notes/08 §5. **R0701 does not
exist** in pylint 4.0.5 — no such msgid anywhere in the tree.)

method_args.py:
| id | symbol | template | report node |
|----|--------|----------|-------------|
| W3101 | missing-timeout | `Missing timeout argument for method '%s' can cause your program to hang indefinitely` | Call |

(E3102 positional-only-arguments-expected → spec'd in §5.2 below anyway since
the file is tiny; cross-check with notes/08 if already present.)

Also covered, because the task asked: R1704 redefined-argument-from-local
(owned by RefactoringChecker, refactoring_checker.py:292-299) — §6. NOTE from
msgs.rs: R1704 is the only message in this doc with `node_scope: false`
(WarningScope.LINE) — block-level pragma expansion does NOT apply to it.
W0125 (using-constant-test) is owned by `base/basic_checker.py:200` and is NOT
in this doc's checkers — it needs its own spec with the basic-checker W/C/R
batch.

================================================================================
# 1. variables.py — VariablesChecker W messages
================================================================================

## 1.0 Checker options (variables.py:1238-1324) — defaults that gate behavior

| option | default | used by |
|--------|---------|---------|
| init-import | `False` (yn) | leave_module: skip `_check_imports` for packages (`__init__` files) |
| dummy-variables-rgx | regexp `_+$|(_[a-zA-Z0-9_]*[a-zA-Z0-9]+?$)|dummy|^ignored_|^unused_` | `_is_name_ignored` (W0612/W0641/W0621), dummy-import filter (W0611) |
| additional-builtins | `()` (csv) | `_is_builtin`, undefined-variable suppression, metaclass check |
| callbacks | `("cb_", "_cb")` (csv) | W0613: skip functions whose NAME starts/ends with these |
| redefining-builtins-modules | `("six.moves", "past.builtins", "future.builtins", "builtins", "io")` (csv) | `_should_ignore_redefined_builtin` (W0622) |
| ignored-argument-names | regexp `_.*|^ignored_|^unused_` (IGNORED_ARGUMENT_NAMES, variables.py:50) | `_is_name_ignored` for Arguments-parented stmts (W0613) |
| allow-global-unused-variables | `True` (yn) | `_check_globals`: **default True → module-level W0612 never fires** |
| allowed-redefined-builtins | `()` (csv) | `_allowed_redefined_builtin` (W0622, function path only) |

Cross-checker config read: `analyse_fallback_blocks` (main, default False),
`ignored_modules` (main, default ()) — both via cached_property
(variables.py:2193-2199).

`open()` (variables.py:1339-1341): `_py314_plus = config.py_version >= (3,14)`.
Default py-version = interpreter version → (3,12) → False. It feeds
`_postponed_evaluation_enabled` per module
(`_py314_plus or is_postponed_evaluation_enabled(node)`, variables.py:1406-1408).

## 1.1 W0622 redefined-builtin

Three independent emission sites:

(a) **visit_module** (variables.py:1401-1414) — NOT decorated (always runs;
also rebuilds `self._to_consume`):
```python
for name, stmts in node.locals.items():            # module locals, insertion order
    if utils.is_builtin(name):
        if self._should_ignore_redefined_builtin(stmts[0]) or name == "__doc__":
            continue
        self.add_message("redefined-builtin", args=name, node=stmts[0])
```
- `utils.is_builtin(name)` (utils.py:291-293): `name in builtins.__dict__ or
  name in ("__builtins__",)`.
- `_should_ignore_redefined_builtin(stmt)` (variables.py:3002-3005): stmt is
  ImportFrom AND `stmt.modname in config.redefining_builtins_modules`.
- **QUIRK**: the module-level path does NOT consult
  `allowed-redefined-builtins` — only the function path does. Bug-for-bug.
- node = `stmts[0]` — the FIRST defining node for that name.

(b) **visit_functiondef** (variables.py:1499-1544; also asyncfunctiondef via
alias :1592) — gate: runs the redefinition block only when
`is_message_enabled("redefined-outer-name") or
is_message_enabled("redefined-builtin")` (config-level, no line). For each
`(name, stmt)` in `node.items()` (astroid LocalsDictNodeNG.items() =
zip(keys, values) where `values()` returns `self.locals[key][0]` — the FIRST
def node per name; astroid mixin.py:180-186):
- If name in module globals → W0621 path (see §1.5).
- `elif utils.is_builtin(name) and not self._allowed_redefined_builtin(name)
  and not self._should_ignore_redefined_builtin(stmt):` →
  `add_message("redefined-builtin", args=name, node=stmt)`.

(c) **visit_global** (variables.py:1637-1643): while scanning assign_nodes
for a global name, `if isinstance(anode, nodes.AssignName) and anode.name in
module.special_attributes:` → `add_message("redefined-builtin", args=name,
node=node)` (node = the **Global** statement) and break. `Module.
special_attributes` is astroid's ModuleModel attribute set: `__path__`,
`__file__`, `__doc__`, `__name__`, `__qualname__`, `__loader__`, `__spec__`,
`__package__`, `__dict__` (+ `attr___builtins__` etc. — port the astroid
ObjectModel attribute name list verbatim from
astroid/interpreter/objectmodel.py ModuleModel). NOTE this fires only when
the module also has an `AssignName` for that name whose name is a special
attribute, i.e. `global __doc__` + module-level `__doc__ = ...`.

## 1.2 W0601/W0602/W0603/W0604 — visit_global (variables.py:1595-1665)

Decorated `only_required_for_messages("global-variable-undefined",
"global-variable-not-assigned", "global-statement",
"global-at-module-level", "redefined-builtin")` — callback skipped entirely
if all five are disabled config-wide.

```python
frame = node.frame()
if isinstance(frame, nodes.Module):
    self.add_message("global-at-module-level", node=node, confidence=HIGH)   # W0604
    return
module = frame.root()
default_message = True
module_locals = node.root().locals
for name in node.names:                       # source order of `global a, b`
    try:    assign_nodes = module.getattr(name)
    except astroid.NotFoundError: assign_nodes = []
    not_defined_locally_by_import = not any(
        isinstance(local, (nodes.Import, nodes.ImportFrom))
        for local in module_locals.get(name, ()))
    if (not utils.is_reassigned_after_current(node, name)
            and not utils.is_deleted_after_current(node, name)
            and not_defined_locally_by_import):
        self.add_message("global-variable-not-assigned", args=name, node=node,
                         confidence=HIGH)                                     # W0602
        default_message = False
        continue
    for anode in assign_nodes:
        if isinstance(anode, nodes.AssignName) and anode.name in module.special_attributes:
            self.add_message("redefined-builtin", args=name, node=node); break
        if anode.frame() is module: break          # module-level assignment
        if isinstance(anode, (nodes.ClassDef, nodes.FunctionDef)) and anode.parent is module:
            break                                  # module-level def
    else:
        if not_defined_locally_by_import:
            self.add_message("global-variable-undefined", args=name, node=node,
                             confidence=HIGH)                                 # W0601
            default_message = False
if default_message:
    self.add_message("global-statement", node=node, confidence=HIGH)          # W0603
```
Helper semantics:
- `is_reassigned_after_current(node, name)` (utils.py:1907-1912 →
  `_is_reassigned_relative_to_current` :1876-1897): any
  AssignName/ClassDef/FunctionDef `a` in `node.scope().nodes_of_class(...)`
  with `a.name == name`, `a.lineno > node.lineno`, and `a.scope() is
  node.scope()` (`_is_node_in_same_scope` :1868-1873; for candidate ClassDef/
  FunctionDef, `a.scope()` is the def's own scope... no: `candidate.scope()`
  of a FunctionDef IS the FunctionDef itself, so a nested `def name():` after
  the global stmt does NOT count — only AssignName targets in the same
  function count, plus the subtle case where candidate IS the scope).
  Actually astroid `FunctionDef.scope()` returns self → `candidate.scope()
  is node_scope` False for a def inside the function. AssignName.scope() =
  enclosing function → True.
- `is_deleted_after_current` (utils.py:1914-1922): any `del name` target in
  the same scope with `target.lineno > node.lineno`.
- `module.getattr(name)` is astroid Module.getattr: locals + special
  attributes + `__getattr__`... port: locals lookup; raises NotFoundError if
  absent.
- W0602 wins over W0601 per name (checked first, `continue`).
- W0603 fires once per `global` statement IF no W0601/W0602 fired for any of
  its names. **The redefined-builtin break does NOT clear `default_message`**
  → `global __doc__` (with module-level `__doc__=...`) yields BOTH
  redefined-builtin and W0603 on the same Global node.
- All four global messages report on the Global statement node.

## 1.3 W0611 unused-import + W0614 unused-wildcard-import

### 1.3.1 leave_module sequencing (variables.py:1426-1444)

`leave_module` is decorated `only_required_for_messages("unused-import",
"unused-wildcard-import", "redefined-builtin", "undefined-all-variable",
"invalid-all-object", "invalid-all-format", "unused-variable",
"undefined-variable")` — in full-pylint mode (or any run where at least one
of these is enabled) it runs.

```python
assert len(self._to_consume) == 1
self._check_metaclasses(node)                       # pops consumed metaclass names
not_consumed = self._to_consume.pop().to_consume    # leftover module locals
if "__all__" in node.locals:
    self._check_all(node, not_consumed)             # E0603/E0604/E0605 (notes/05)
                                                    # AND deletes __all__-exported
                                                    # names from not_consumed
self._check_globals(not_consumed)                   # W0612 module-level (§1.4.4)
if not self.linter.config.init_import and node.package:
    return                                          # __init__.py: NO unused-import
self._check_imports(not_consumed)                   # W0611/W0614; ends: del self._to_consume
self._type_annotation_names = []
```

Dedup/suppression interactions to replicate exactly:
1. `_check_metaclasses` (variables.py:3388-3456): for each top-level ClassDef
   child of the module, `_check_classdef_metaclasses` resolves the textual
   metaclass name (Name / Attribute innermost Name / Call func Name / else
   `metaclass.root().name`), applies `METACLASS_NAME_TRANSFORMS = {"_py_abc":
   "abc"}` (variables.py:54), then for EACH consumer in `self._to_consume[::-1]`
   (innermost-first, NO break across consumers — every enclosing scope that
   holds the name gets an entry) appends `(scope_locals, name)` if any local
   def node has `found_node.lineno <= klass.lineno`. After collecting, pops:
   `scope_locals.pop(name, None)` — i.e. a name used only as a metaclass is
   removed from to_consume → no W0611/W0612 for it. (Also can emit E0602 for
   undefined metaclass — notes/05.)
2. `_check_all` removes each `__all__` string element found in `not_consumed`
   (`del not_consumed[elt_name]`, variables.py:3253-3255) → names exported via
   `__all__` are never unused-import/unused-variable.
3. **`__init__.py` leak**: when `node.package` and `init_import` is False, the
   early return skips BOTH `_check_imports` AND the
   `self._type_annotation_names = []` reset. The annotation-name list then
   carries over into the NEXT module checked (checker instance is reused
   across modules) and can suppress its unused-imports. Replicate this bug.
4. `del self._to_consume` at the end of `_check_imports` (variables.py:3386)
   — harmless (visit_module reassigns) but means a module-level pragma
   disabling all eight leave_module messages leaves the attribute set.

### 1.3.2 `_fix_dot_imports` (variables.py:209-252)

Input: `not_consumed: dict[name, list[def nodes]]` (module locals leftovers).
Output: `sorted(names.items(), key=lambda a: a[1].fromlineno)` — list of
`(expanded_name, import_stmt)`.

```python
names = {}
for name, stmts in not_consumed.items():        # dict insertion order = locals order
    if any(isinstance(stmt, nodes.AssignName) and
           isinstance(stmt.assign_type(), nodes.AugAssign) for stmt in stmts):
        continue                                 # augassigned names: skip wholly
    for stmt in stmts:
        if not isinstance(stmt, (nodes.ImportFrom, nodes.Import)): continue
        for imports in stmt.names:
            second_name = None
            import_module_name = imports[0]
            if import_module_name == "*":
                second_name = name               # wildcard: local name itself
            else:
                name_matches_dotted_import = (
                    import_module_name.startswith(name)
                    and import_module_name.find(".") > -1)
                if name_matches_dotted_import or name in imports:
                    second_name = import_module_name
            if second_name and second_name not in names:
                names[second_name] = stmt        # first stmt wins per expanded name
```
- The expansion turns the local `xml` (from `import xml.etree` and
  `import xml.sax`) into two entries `xml.etree`, `xml.sax`.
- `name in imports` matches either the qname or the alias position.
- NOTE `import_module_name.startswith(name)` is a plain prefix test (no dot
  boundary) — `import xmlrpc.client` with leftover local name `xml` would
  match. Bug-for-bug.
- Sort is stable (Python sort) on `stmt.fromlineno`; entries from the same
  stmt keep dict-insertion order (which follows locals order, then
  `stmt.names` order).

### 1.3.3 `_check_imports` (variables.py:3298-3386)

```python
local_names = _fix_dot_imports(not_consumed)
checked = set()
unused_wildcard_imports: defaultdict[(modname, ImportFrom), list[str]] = defaultdict(list)
for name, stmt in local_names:
    for imports in stmt.names:                    # iterate the stmt's names again
        real_name = imported_name = imports[0]
        if imported_name == "*": real_name = name
        as_name = imports[1]
        if real_name in checked: continue
        if name not in (real_name, as_name): continue   # only the matching alias
        checked.add(real_name)
        is_type_annotation_import = (imported_name in self._type_annotation_names
                                     or as_name in self._type_annotation_names)
        is_dummy_import = (as_name and self.linter.config.dummy_variables_rgx
                           and self.linter.config.dummy_variables_rgx.match(as_name))
        if isinstance(stmt, nodes.Import) or (isinstance(stmt, nodes.ImportFrom)
                                              and not stmt.modname):
            # `import x` and `from . import x` (relative with empty modname)
            if isinstance(stmt, nodes.ImportFrom) and SPECIAL_OBJ.search(imported_name):
                continue                          # __dunder__ names re-exported
            if is_type_annotation_import or is_dummy_import: continue
            if as_name is None: msg = f"import {imported_name}"
            else:               msg = f"{imported_name} imported as {as_name}"
            if not in_type_checking_block(stmt):
                self.add_message("unused-import", args=msg, node=stmt)
        elif isinstance(stmt, nodes.ImportFrom) and stmt.modname != FUTURE:
            if SPECIAL_OBJ.search(imported_name): continue
            if _is_from_future_import(stmt, name): continue   # re-exported __future__
            if is_type_annotation_import or is_dummy_import: continue
            if imported_name == "*":
                unused_wildcard_imports[(stmt.modname, stmt)].append(name)
            else:
                if as_name is None:
                    msg = f"{imported_name} imported from {stmt.modname}"
                else:
                    msg = f"{imported_name} imported from {stmt.modname} as {as_name}"
                if not in_type_checking_block(stmt):
                    self.add_message("unused-import", args=msg, node=stmt)
for module, unused_list in unused_wildcard_imports.items():
    if len(unused_list) == 1: arg_string = unused_list[0]
    else: arg_string = f"{', '.join(i for i in unused_list[:-1])} and {unused_list[-1]}"
    self.add_message("unused-wildcard-import", args=(arg_string, module[0]),
                     node=module[1])
del self._to_consume
```
Key quirks:
- `SPECIAL_OBJ = re.compile("^_{2}[a-z]+_{2}$")` (variables.py:47) and the
  check is `.search` (anchors make it equivalent to fullmatch of
  `__[a-z]+__`).
- `_is_from_future_import` (variables.py:88-98): re-imports `stmt.modname`
  via `stmt.do_import_module(stmt.modname)`; True if the *imported module's*
  locals\[name\] contains an ImportFrom with modname `__future__`. Building
  errors → None (no suppression).
- `from __future__ import ...` (modname == FUTURE): falls through BOTH
  branches → never unused-import. (E0602-adjacent W0612 path also excludes
  it, §1.4.4.)
- The `checked` set dedups by `real_name` ACROSS statements: two statements
  importing the same dotted name → only the first (lowest fromlineno after
  sort) is reported.
- `in_type_checking_block` (utils.py:1990-2017): an `if TYPE_CHECKING:` /
  `if typing.TYPE_CHECKING:` ancestor (Name path requires lookup resolving to
  `from typing import TYPE_CHECKING` or safe-inferred Const False; Attribute
  path requires attrname == "TYPE_CHECKING" — see source for the Attribute
  branch which also handles `if TYPE_CHECKING` imported as alias).
- Message args is a single pre-formatted string (template `Unused %s`):
  - `import foo` → `Unused import foo`
  - `import foo as f` → `Unused foo imported as f`
  - `from m import foo` → `Unused foo imported from m`
  - `from m import foo as f` → `Unused foo imported from m as f`
  - `from . import foo` → `Unused import foo` (empty-modname branch)
- W0614 args: `(arg_string, modname)` where arg_string joins LOCAL names (the
  names defined by the star-import that were never used) as
  `a, b and c`. The (modname, stmt) dict keys iterate in insertion order
  (fromlineno order). Names per key in local_names order.
- W0614 is per wildcard-importing statement; the unused name list contains
  names contributed to module locals by astroid's star-import expansion that
  remain in not_consumed.

### 1.3.4 Function-scope unused imports

Emitted via `_check_is_unused` from leave_functiondef (§1.4.2, stmt is
Import/ImportFrom branch, variables.py:2844-2858), with slightly DIFFERENT
message text construction than `_check_imports`:
```python
qname, asname = import_names; name = asname or qname
...
case nodes.Import():
    if asname is not None: msg = f"{qname} imported as {asname}"
    else:                  msg = f"import {name}"
case nodes.ImportFrom():
    if asname is not None: msg = f"{qname} imported from {stmt.modname} as {asname}"
    else:                  msg = f"{name} imported from {stmt.modname}"
```
node = the import statement. NO TYPE_CHECKING / SPECIAL_OBJ / __future__ /
dummy filtering on this path (only the generic filters of §1.4.2 apply: the
dummy-variables regex IS applied via `_is_name_ignored` at the top, and
`name in self._type_annotation_names` is checked before the import branch).
For multi-name imports (`len(stmt.names) > 1`) it selects
`next((names for names in stmt.names if name in names), None)` — matches the
locals name against either tuple slot.

## 1.4 W0612 unused-variable, W0641 possibly-unused-variable, W0613 unused-argument

### 1.4.1 leave_functiondef driver (variables.py:1546-1590, alias asyncfunctiondef :1593)

```python
self._check_metaclasses(node)                       # same popping as module
if node.type_comment_returns: self._store_type_annotation_node(...)
if node.type_comment_args: for a in ...: self._store_type_annotation_node(a)
not_consumed = self._to_consume.pop().to_consume
if not (is_message_enabled("unused-variable") or
        is_message_enabled("possibly-unused-variable") or
        is_message_enabled("unused-argument")):
    return
if utils.is_error(node): return                     # body == [Raise] (utils.py:277-279)
is_method = node.is_method()
if is_method and node.is_abstract(): return         # astroid is_abstract(pass_is_abstract=True):
                                                    # abstractmethod-decorated OR body is
                                                    # raise NotImplementedError OR `pass` only
global_names = _flattened_scope_names(node.nodes_of_class(nodes.Global))
nonlocal_names = _flattened_scope_names(node.nodes_of_class(nodes.Nonlocal))
comprehension_target_names = {names assigned in any ComprehensionScope
                              generator target under node}     # :1575-1580
for name, stmts in not_consumed.items():            # locals insertion order
    self._check_is_unused(name, node, stmts[0], global_names, nonlocal_names,
                          comprehension_target_names)
```
- `_flattened_scope_names` (variables.py:292-296): union of `stmt.names`.
- `nodes_of_class` traverses the whole subtree (including nested functions!)
  so a `global x` in a nested def affects the outer check. Bug-for-bug.
- The message-enabled gate is config-level (`is_message_enabled(symbol)` with
  no line).

### 1.4.2 `_check_is_unused` (variables.py:2774-2872)

In order:
1. `_is_name_ignored(stmt, name)` (variables.py:2874-2888): regex =
   `ignored_argument_names` if `stmt` matches
   `nodes.AssignName(parent=nodes.Arguments()) | nodes.Arguments()` else
   `dummy_variables_rgx`; return `regex.match(name)` → ignored if matches.
2. `__class__` injected by astroid for methods:
   `case nodes.FunctionDef(locals={"__class__": [nodes.ClassDef()]}) if name
   == "__class__": return` (match on the checked function node).
3. stmt is Global/Import/ImportFrom AND `global_names` non-empty AND
   `_import_name_is_global(stmt, global_names)` (variables.py:277-289: any
   (import_name, alias) where alias∈global_names, or alias falsy and
   import_name∈global_names) → return.
4. `name in comprehension_target_names` → return.
5. `name in self._type_annotation_names` → return (string/comment annotations
   recorded by visit_const / _store_type_annotation_*).
6. `argnames = node.argnames()` (all params incl. vararg/kwonly/kwarg names).
   If `name in argnames`:
   - `__new__` special case (variables.py:2810-2819): scan
     `node.parent.get_children()` for any child with `.name == "__init__"`;
     if found → return (don't check `__new__`'s args at all).
   - else `_check_unused_arguments(...)` (§1.4.3) and DONE.
7. Else (a local, not an argument):
   - If `stmt.parent` is Assign/AnnAssign/Tuple/For and
     `name in nonlocal_names` → return.
   - Import-name expansion (variables.py:2828-2839, see §1.3.4): sets
     qname/asname, `name = asname or qname`.
   - `_has_locals_call_after_node(stmt, node.scope())` (variables.py:333-348):
     any `locals()` Call in the scope (skipping nested
     FunctionDef/ClassDef/Import/ImportFrom subtrees via
     `skip_klass`) where the inferred func `is_builtin_object` named
     "locals" and `stmt.lineno < call.lineno` → message_name =
     `possibly-unused-variable` (W0641), else:
     - stmt is Import → unused-import message (§1.3.4), return.
     - stmt is ImportFrom → unused-import message, return.
     - else message_name = `unused-variable`.
   - `isinstance(stmt, nodes.FunctionDef) and stmt.decorators` → return
     (a decorated nested function is "used" by its decorator).
   - `utils.is_overload_stub(node)` → return. NOTE: checks the ENCLOSING
     function `node`, not stmt (an @overload stub's body names are exempt).
   - `_is_exception_binding_used_in_handler(stmt, name)`
     (variables.py:2943-2950): stmt.parent is ExceptHandler, stmt is its
     `.name`, and any Name in the handler subtree has that name. (astroid
     scopes `except E as e` bindings: the AssignName's uses inside the
     handler don't consume because of the del-at-end semantics filter in
     get_next_to_consume — this re-adds the exemption.) → return.
   - `self.add_message(message_name, args=name, node=stmt)`.
     `stmt` is `stmts[0]` — the FIRST definition node (AssignName, FunctionDef,
     ClassDef, Arguments child, etc.). For W0612 the position is that node's.

### 1.4.3 `_check_unused_arguments` (variables.py:2890-2941) — W0613

```python
is_method = node.is_method(); klass = node.parent.frame()
if is_method and isinstance(klass, nodes.ClassDef):
    confidence = INFERENCE if utils.has_known_bases(klass) else INFERENCE_FAILURE
else:
    confidence = HIGH
if is_method:
    if node.type != "staticmethod" and name == argnames[0]: return  # self/cls
    overridden = overridden_method(klass, node.name)   # utils.py:2323-2340, lru_cache(1000)
    if overridden is not None and name in overridden.argnames(): return
    if node.name in utils.PYMETHODS and node.name not in ("__init__", "__new__"):
        return
if any(node.name.startswith(cb) or node.name.endswith(cb)
       for cb in self.linter.config.callbacks): return   # FUNCTION name, not arg!
if utils.is_registered_in_singledispatch_function(node): return  # utils.py:1515-1546
if utils.is_overload_stub(node): return
if utils.is_protocol_class(klass): return                # utils.py:1677-1696
if name in nonlocal_names: return
self.add_message("unused-argument", args=name, node=stmt, confidence=confidence)
```
- `stmt` here is the AssignName inside Arguments (stmts\[0\] from locals) —
  the column/line point at the parameter itself.
- `overridden_method`: first ancestor in `klass.local_attr_ancestors(name)`
  owning the name; KeyError/StopIteration → None; must be FunctionDef.
- `node.type`: astroid method kind ("method"/"classmethod"/"staticmethod").
- PYMETHODS = set of dunder method names (utils.py:78-193 PYMETHODS minus...
  see notes/08 §0.17).
- Note the default ignored-argument-names regex `_.*|^ignored_|^unused_`
  already filtered `_`-prefixed args in step 1 of §1.4.2.

### 1.4.4 Module-level W0612 — `_check_globals` (variables.py:3278-3295)

```python
if self._allow_global_unused_variables: return     # DEFAULT TRUE → no-op
for name, node_lst in not_consumed.items():
    for node in node_lst:
        if in_type_checking_block(node): continue
        if self._is_exception_binding_used_in_handler(node, name): continue
        if isinstance(node, nodes.AssignName) and node.name == "__all__": continue
        if (isinstance(node, nodes.ImportFrom) and name == "annotations"
                and node.modname == "__future__"): continue
        self.add_message("unused-variable", args=(name,), node=node)
```
With `--allow-global-unused-variables=n` it emits one message per leftover
def node (not per name!). args=(name,) tuple → `%r` renders `'name'`.
NOTE: imports also land here (an unused module-level import would emit BOTH
W0612 here and W0611 in `_check_imports` — pylint accepts the double report;
exception: `from __future__ import annotations`).

## 1.5 W0621 redefined-outer-name

(a) **visit_functiondef** path (variables.py:1507-1536): for each
`(name, stmt)` in `node.items()` with `name in node.root().globals` and
`stmt` not a Global node:
- skip if `globs[name][0]` is an ImportFrom from `__future__`;
- skip if ANY def node in `globs[name]` is `in_type_checking_block`;
- skip if `globs[name][0]` matches
  `nodes.AssignName(parent=nodes.ExceptHandler())` (outer `except ... as e`);
- `line = globs[name][0].fromlineno`;
- if `not self._is_name_ignored(stmt, name)` →
  `add_message("redefined-outer-name", args=(name, line), node=stmt)`.
`node.root().globals` is the module's locals dict. Note: parameters,
local assignments, nested defs, imports — any function local shadowing a
module global triggers it. `stmt` = first local def node (e.g. the
AssignName of the parameter).

(b) **except-handler queue** (variables.py:1689-1709):
`visit_excepthandler` (decorated only_required_for_messages
("redefined-outer-name")):
```python
if not isinstance(node.name, nodes.AssignName): return
for outer_except, outer_except_assign_name in self._except_handler_names_queue:
    if node.name.name == outer_except_assign_name.name:
        self.add_message("redefined-outer-name",
            args=(outer_except_assign_name.name, outer_except.fromlineno),
            node=node)                    # the INNER ExceptHandler node
        break
self._except_handler_names_queue.append((node, node.name))
```
`leave_excepthandler` pops iff `node.name and isinstance(node.name,
AssignName)`. The queue is LIFO across nesting; iteration scans outermost
first (list order) and reports the FIRST outer handler with the same name.
Report node = the inner ExceptHandler (its fromlineno = the `except` line;
node.position not set → fromlineno/col_offset of the handler).
NOTE: the queue is an instance attribute, never cleared per module — but
visit/leave are balanced so it's empty between modules.

## 1.6 W0631 undefined-loop-variable — `_loopvar_name` (variables.py:2625-2771)

Called from `visit_name` for EVERY Name/DelName (and AssignName under
AugAssign) — no message-enabled gate.

```python
astmts = [s for s in node.lookup(node.name)[1] if hasattr(s, "assign_type")]
scope = node.scope()
if isinstance(scope, (nodes.Lambda, nodes.FunctionDef)) and any(
        asmt.scope().parent_of(scope) for asmt in astmts):
    return                                   # use inside a function defined in the loop
if (not astmts
        or (astmts[0].parent == astmts[0].root() and astmts[0].parent.parent_of(node))
        or (astmts[0].is_statement
            or (not isinstance(astmts[0].parent, nodes.Module)
                and astmts[0].statement().parent_of(node)))):
    _astmts = []
else:
    _astmts = astmts[:1]
for i, stmt in enumerate(astmts[1:]):
    try: astmt_statement = astmts[i].statement()      # NOTE: astmts[i] == previous element
    except astroid.exceptions.ParentMissingError: continue
    if astmt_statement.parent_of(stmt) and not utils.in_for_else_branch(astmt_statement, stmt):
        continue
    _astmts.append(stmt)
astmts = _astmts
if len(astmts) != 1: return
assign = astmts[0].assign_type()
if not (isinstance(assign, (nodes.For, nodes.Comprehension, nodes.GeneratorExp))
        and assign.statement() is not node.statement()):
    return
```
- `node.lookup` is astroid scope_lookup + `_filter_stmts` (notes/07).
- `in_for_else_branch` (utils.py:2044-2049, lru_cache): parent is For and
  stmt is inside/equal to one of parent.orelse statements.
- Non-For assign (Comprehension / GeneratorExp) → IMMEDIATE
  `add_message("undefined-loop-variable", args=node.name, node=node)`.
- For-loop case continues:
```python
for else_stmt in assign.orelse:
    if isinstance(else_stmt, (nodes.Return, nodes.Raise, nodes.Break, nodes.Continue)):
        return
    if isinstance(else_stmt, nodes.Expr) and isinstance(else_stmt.value, nodes.Call):
        inferred_func = utils.safe_infer(else_stmt.value.func)
        if isinstance(inferred_func, nodes.FunctionDef) and inferred_func.returns:
            inferred_return = utils.safe_infer(inferred_func.returns)
            if isinstance(inferred_return, nodes.FunctionDef) and \
               inferred_return.qname() in {*TYPING_NORETURN, *TYPING_NEVER,
                                           "typing._SpecialForm"}:
                return
            if (isinstance(inferred_return, bases.Instance)
                    and inferred_return.qname() == "typing._SpecialForm"):
                return
```
  (TYPING_NORETURN/TYPING_NEVER from pylint/constants.py: the
  typing/typing_extensions NoReturn/Never qnames.)
- Walrus-in-comprehension exemption (variables.py:2716-2732): if node has a
  NamedExpr ancestor whose first Comprehension ancestor's
  ComprehensionScope's `parent.scope() is scope` and
  `node.name in comprehension_scope.locals` → return.
- Iterable length heuristic (variables.py:2734-2771):
```python
try:
    inferred = next(assign.iter.infer())
    if isinstance(inferred, astroid.Instance) and inferred.qname() == "builtins.enumerate":
        likely_call = assign.iter
        if isinstance(assign.iter, nodes.IfExp): likely_call = assign.iter.body
        if isinstance(likely_call, nodes.Call) and likely_call.args:
            inferred = next(likely_call.args[0].infer())
except astroid.InferenceError:
    self.add_message("undefined-loop-variable", args=node.name, node=node)
else:
    if isinstance(inferred, astroid.Instance) and inferred.qname() == BUILTIN_RANGE:
        return                                              # "builtins.range"
    sequences = (nodes.List, nodes.Tuple, nodes.Dict, nodes.Set, objects.FrozenSet)
    if not isinstance(inferred, sequences):
        self.add_message("undefined-loop-variable", args=node.name, node=node); return
    elements = getattr(inferred, "elts", getattr(inferred, "items", []))
    if not elements:
        self.add_message("undefined-loop-variable", args=node.name, node=node)
```
  Note `next(infer())` (NOT safe_infer): first inference result wins;
  StopIteration would propagate (astroid infer always yields ≥1 incl.
  Uninferable; Uninferable is not Instance/sequence → message).

## 1.7 W0640 cell-var-from-loop — `_check_late_binding_closure` (variables.py:2952-3000)

Called from `_check_consumer` at two points (variables.py:1828, 1847): when
the name was already consumed in the current consumer, and right after
`get_next_to_consume` returns non-empty.

```python
if not self.linter.is_message_enabled("cell-var-from-loop"): return
node_scope = node.frame()
if utils.is_default_argument(node, node_scope):       # utils.py:411-427
    node_scope = node_scope.parent.frame()
if (not isinstance(node_scope, (nodes.Lambda, nodes.FunctionDef))
        or node.name in node_scope.locals):
    return                                            # not a cell var
assign_scope, stmts = node.lookup(node.name)
if not (stmts and assign_scope.parent_of(node_scope)): return
if utils.is_comprehension(assign_scope):              # ComprehensionScope check
    self.add_message("cell-var-from-loop", node=node, args=node.name)
else:
    assignment_node = stmts[0]
    maybe_for = assignment_node
    while maybe_for and not isinstance(maybe_for, nodes.For):
        if maybe_for is assign_scope: break
        maybe_for = maybe_for.parent
    else:
        if (maybe_for and maybe_for.parent_of(node_scope)
                and not utils.is_being_called(node_scope)         # (lambda: i)()
                and node_scope.parent
                and not isinstance(node_scope.statement(), nodes.Return)):
            self.add_message("cell-var-from-loop", node=node, args=node.name)
```
- `is_being_called` (utils.py:456-458): node.parent is Call and parent.func
  is node.
- args = name (template `Cell variable %s defined in loop` — no quotes).
- node = the Name use inside the closure.
- The while/else: message only when the walk EXITS via the while condition
  (found a For); `break` (reached assign_scope first) → no message.

## 1.8 W0642 self-cls-assignment — `_check_self_cls_assign` (variables.py:3054-3085)

Called first thing in `visit_assign` (variables.py:2152-2156). visit_assign is
decorated `only_required_for_messages("unbalanced-tuple-unpacking",
"unpacking-non-sequence", "self-cls-assignment",
"unbalanced_dict_unpacking")` — note the LAST entry is a typo (underscores).
pylint's `is_message_enabled` treats unknown descriptions as raw msgids that
have no recorded disable state → returns True → **visit_assign always runs
regardless of disables** (the walker's only_required gate
`any(is_message_enabled(m))` is always satisfied). Replicate: treat the
callback as unconditional.

```python
assign_names: set[str | None] = set()
for target in node.targets:
    match target:
        case nodes.AssignName(): assign_names.add(target.name)
        case nodes.Tuple():
            assign_names.update(elt.name for elt in target.elts
                                if isinstance(elt, nodes.AssignName))
scope = node.scope()
nonlocals_with_same_name = node.scope().parent and any(
    child for child in scope.body if isinstance(child, nodes.Nonlocal))
if nonlocals_with_same_name:
    scope = node.scope().parent.scope()
if not (isinstance(scope, nodes.FunctionDef) and scope.is_method()
        and "builtins.staticmethod" not in scope.decoratornames()):
    return
argument_names = scope.argnames()
if not argument_names: return
self_cls_name = argument_names[0]
if self_cls_name in assign_names:
    self.add_message("self-cls-assignment", node=node, args=(self_cls_name,))
```
- Despite the variable name, `nonlocals_with_same_name` checks for ANY
  Nonlocal statement in the scope body (name not compared). Bug-for-bug.
- Only AssignName and top-level Tuple elements count (not List, not nested
  tuples, not Starred values).
- node = the Assign statement; args 1-tuple → `Invalid assignment to self in
  method`.

## 1.9 W0632 unbalanced-tuple-unpacking, W0644 unbalanced-dict-unpacking, (E0633)

### 1.9.1 visit_assign path (variables.py:2152-2170 → _check_unpacking :3087-3121)

After `_check_self_cls_assign`:
```python
if not isinstance(node.targets[0], (nodes.Tuple, nodes.List)): return
targets = node.targets[0].itered()       # Tuple/List.itered() == .elts
if any(isinstance(target, nodes.Starred) for target in targets): return
try:
    inferred = node.value.inferred()     # FULL inference list (not safe_infer)
    if inferred is not None and len(inferred) == 1:
        self._check_unpacking(inferred[0], node, targets)
except astroid.InferenceError: return
```
`_check_unpacking`:
```python
if utils.is_inside_abstract_class(node): return     # any ancestor ClassDef abstract
if utils.is_comprehension(node): return             # node is a comprehension node (never for Assign)
if isinstance(inferred, util.UninferableBase): return
if (isinstance(inferred.parent, nodes.Arguments) and isinstance(node.value, nodes.Name)
        and node.value.name == inferred.parent.vararg):
    return                                           # RHS is the function's *args
values = self._nodes_to_unpack(inferred)
details = _get_unpacking_extra_info(node, inferred)
if values is not None:
    if len(targets) != len(values):
        self._report_unbalanced_unpacking(node, inferred, targets, len(values), details)
elif not utils.is_iterable(inferred):
    self._report_unpacking_non_sequence(node, details)   # E0633, notes/05
```
- `_nodes_to_unpack` (variables.py:3140-3149): Tuple/List/Set/DictValues/
  DictKeys/DictItems/Dict → `.itered()`; astroid Instance with any ancestor
  qname `typing.NamedTuple` → `[i for i in node.values() if isinstance(i,
  AssignName)]`; else None.
- `_report_unbalanced_unpacking` (variables.py:3151-3172):
  ```python
  args = (details, len(targets), "" if len(targets)==1 else "s",
          values_count, "" if values_count==1 else "s")
  symbol = "unbalanced-dict-unpacking" if isinstance(inferred, DICT_TYPES) \
           else "unbalanced-tuple-unpacking"
  self.add_message(symbol, node=node, args=args, confidence=INFERENCE)
  ```
  So `a, b = {1: 2}` (Dict inferred) emits W0644 from the ASSIGN path too.
- `_get_unpacking_extra_info` (variables.py:101-122):
  - inferred in DICT_TYPES → `match node: case Assign(): more =
    node.value.as_string(); case For(): more = node.iter.as_string()` —
    returns the raw as_string (NO quotes).
  - else: same module (`node.root().name == inferred.root().name`):
    same lineno → `f"'{inferred.as_string()}'"` (quoted source); different
    lineno (truthy) → `f"defined at line {inferred.lineno}"`; lineno
    falsy → `""`.
  - different module and lineno truthy →
    `f"defined at line {inferred.lineno} of {inferred_module}"`.
- W0632 example rendering: `Possible unbalanced tuple unpacking with sequence
  defined at line 3: left side has 2 labels, right side has 3 values`.

### 1.9.2 visit_for path — W0644 only (variables.py:1343-1396)

Decorated `only_required_for_messages("unbalanced-dict-unpacking")`.
```python
if not isinstance(node.target, nodes.Tuple): return
targets = node.target.elts
inferred = utils.safe_infer(node.iter)
if not isinstance(inferred, DICT_TYPES): return     # DictValues/Keys/Items/Dict
values = self._nodes_to_unpack(inferred)
if not values: return
if isinstance(inferred, objects.DictItems):
    if len(targets) == 2 and all(len(x.elts) == 2 for x in values): return
    if any(isinstance(target, nodes.Starred) for target in targets): return
if isinstance(inferred, nodes.Dict):
    if isinstance(node.iter, nodes.Name):
        if len(targets) == 2: return     # dict-items-missing-iter overlap dodge
else:
    is_starred_targets = any(isinstance(t, nodes.Starred) for t in targets)
    for value in values:
        value_length = self._get_value_length(value)
        is_valid_star_unpack = is_starred_targets and value_length >= len(targets)
        if len(targets) != value_length and not is_valid_star_unpack:
            details = _get_unpacking_extra_info(node, inferred)
            self._report_unbalanced_unpacking(node, inferred, targets,
                                              value_length, details)
            break
```
- **CONTROL-FLOW QUIRK**: when `inferred` is a plain `nodes.Dict`, the
  `else:` branch is skipped entirely → iterating a literal/inferred Dict
  with a non-Name iter and ≠2 targets... also no message (only the
  `isinstance(node.iter, nodes.Name) and len(targets)==2` early return is
  inside, but the value loop lives in the `else` of `isinstance(inferred,
  nodes.Dict)`) — i.e. W0644-for fires ONLY for DictValues/DictKeys/
  DictItems iters (`.values()/.keys()/.items()` calls), never for a raw
  Dict. Bug-for-bug.
- `_get_value_length` (variables.py:3123-3138): `_nodes_to_unpack` length if
  not None; Const str/bytes → len(value); Subscript → `ceil((upper.value -
  lower.value) / (slice.step or 1))` (AttributeError risk accepted by
  pylint if bounds missing — propagates? No: it would crash; in practice
  values from DictItems are Tuples → first branch);
  else 1.
- node = the For node (message anchored at `for` line). DictItems values are
  2-Tuples built by astroid's `.items()` inference.

## 1.10 visit_name auxiliary hooks feeding W0611/W0612 suppression

- `visit_const` (variables.py:3502-3530, decorated only_required
  ("unused-import", "unused-variable")): for str Consts inside a type
  annotation context, unless parent (or grandparent through Tuple) Subscript
  origin is `typing.Literal`/`Annotated`, parse the string with
  `extract_node` and `_store_type_annotation_node` (collect Name leaves;
  ValueError/AstroidSyntaxError swallowed).
- `_store_type_annotation_node` (variables.py:3022-3043): Name → append
  name; Attribute → recurse on expr; Subscript → if value is
  `typing.X` Attribute append "typing" then return, else append ALL Name
  descendants; other → ignore.
- `leave_assign` / `leave_with` / `leave_for` (variables.py:2182-2186, 1398)
  store `node.type_annotation` (type comments); `visit_arguments`
  (variables.py:2188-2190) stores `node.type_comment_args`.
- `leave_classdef` (variables.py:1450-1461) marks consumed: any Name whose
  parent matches `Call(func=Attribute(expr=Name(name=name)))` inside the
  class body → first consumer in `self._to_consume` (INNERMOST-first? No:
  iteration `for consumer in self._to_consume` is OUTERMOST first — index 0
  is module!) containing `name` in to_consume gets
  `mark_as_consumed(name, all nodes)`. Suppresses unused-import for e.g.
  `six` in `class X(six.with_metaclass(...))`.

================================================================================
# 2. imports.py — ImportsChecker
================================================================================

## 2.0 Registration, options, state

Class `ImportsChecker(DeprecatedMixin, BaseChecker)`, name "imports"
(imports.py:325-337). `msgs = {**DeprecatedMixin.DEPRECATED_MODULE_MESSAGE,
**MSGS}` → owns W4901 too.

Options (imports.py:340-444):
| option | default |
|--------|---------|
| deprecated-modules | `()` |
| preferred-modules | `()` |
| import-graph / ext-import-graph / int-import-graph | `""` (reports only) |
| known-standard-library | `()` |
| known-third-party | `("enchant",)` |
| allow-any-import-level | `()` |
| allow-wildcard-with-all | `False` |
| allow-reexport-from-package | `False` |

`open()` (imports.py:461-476): resets `stats.dependencies = {}`,
`import_graph = defaultdict(set)`, `_module_pkg = {}`,
`_current_module_package = False`, caches `ignored_modules`, builds
`preferred_modules` dict from `module.split(":")` for entries containing ":",
`_allow_any_import_level` set, `_allow_reexport_package` bool.

Per-module state: `_imports_stack: list[(node, importedname)]`,
`_first_non_import_node` — reset at the END of `leave_module`
(imports.py:610-611). `visit_module` (imports.py:524-526) sets
`_current_module_package = node.package`.

Visitor methods registered (walker scans `visit_*`/`leave_*` attrs):
visit_module, visit_import, visit_importfrom, leave_module,
visit_try/visit_assignattr/visit_assign/visit_ifexp/visit_comprehension/
visit_expr/visit_if (all = `compute_first_non_import_node`,
imports.py:650-652), visit_functiondef/visit_classdef/visit_for/visit_while
(all = the functiondef recorder, imports.py:676). None are decorated with
only_required_for_messages → always run.

## 2.1 visit_import (imports.py:528-551)

```python
self._check_reimport(node)              # W0404/W0416
self._check_import_as_rename(node)      # C0414/R0402
self._check_toplevel(node)              # C0415
names = [name for name, _ in node.names]
if len(names) >= 2:
    self.add_message("multiple-imports", args=", ".join(names), node=node)  # C0410
for name in names:
    self.check_deprecated_module(node, name)        # W4901
    self._check_preferred_module(node, name)        # W0407
    imported_module = self._get_imported_module(node, name)   # E0401/E0402/E0001
    if isinstance(node.parent, nodes.Module):
        self._check_position(node)                  # C0413 — INSIDE the loop!
    if isinstance(node.scope(), nodes.Module):
        self._record_import(node, imported_module)  # for C0411/C0412 — also per name
    if imported_module is None: continue
    self._add_imported_module(node, imported_module.name)     # W0406 + graph
```
QUIRK: `_check_position` and `_record_import` run once PER NAME — `import a,
b` after the first non-import statement produces TWO C0413 messages on the
same line, and pushes two stack entries for order checking.

## 2.2 visit_importfrom (imports.py:553-579)

```python
basename = node.modname
imported_module = self._get_imported_module(node, basename)
absolute_name = get_import_name(node, basename)    # utils.py:1820-1843:
                                                   # relative → relative_to_absolute_name
self._check_import_as_rename(node)                 # C0414/R0402 (incl. from-imports!)
self._check_misplaced_future(node)                 # W0410
self.check_deprecated_module(node, absolute_name)  # W4901 on absolute name
self._check_preferred_module(node, basename)      # W0407 (basename)
self._check_wildcard_imports(node, imported_module)  # W0401
self._check_same_line_imports(node)                # W0404 duplicates in one stmt
self._check_reimport(node, basename=basename, level=node.level)  # W0404/W0416
self._check_toplevel(node)                         # C0415
if isinstance(node.parent, nodes.Module): self._check_position(node)
if isinstance(node.scope(), nodes.Module): self._record_import(node, imported_module)
if imported_module is None: return
for name, _ in node.names:
    if name != "*": self._add_imported_module(node, f"{imported_module.name}.{name}")
    else:           self._add_imported_module(node, imported_module.name)
```

## 2.3 C0410 multiple-imports
args = `", ".join(names)` (original dotted names, aliases dropped), node =
the Import. ImportFrom never triggers it.

## 2.4 C0413 wrong-import-position

State machine:
- `compute_first_non_import_node` (imports.py:613-648) for
  If/Expr/Comprehension/IfExp/Assign/AssignAttr/Try at MODULE level
  (`isinstance(node.parent, nodes.Module)`), first one wins:
  - Try containing ANY Import/ImportFrom anywhere in subtree → not recorded.
  - Assign whose targets are ALL AssignName dunders (`startswith("__") and
    endswith("__")`) → not recorded (module dunder exemption; docstring Expr
    IS recorded — but the module docstring is `Module.doc_node`, not a body
    Expr, so a leading docstring doesn't count).
- `visit_functiondef`-alias (imports.py:654-674) for
  FunctionDef/ClassDef/For/While: requires `isinstance(node.parent.scope(),
  nodes.Module)` (direct or wrapped in non-scoping stmts), walks `root` up to
  the module child; if that root is If/Try containing imports → skip; else
  record `node` (the def itself, even if nested inside module-level If).
- `_check_position(node)` (imports.py:698-715): if `_first_non_import_node`
  set:
  ```python
  if self.linter.is_message_enabled("wrong-import-position",
                                    self._first_non_import_node.fromlineno):
      self.add_message("wrong-import-position", node=node, args=node.as_string())
  else:
      self.linter.add_ignored_message("wrong-import-position", node.fromlineno, node)
  ```
  NOTE the enable check uses the FIRST NON-IMPORT node's line (a pragma on
  the offending code line suppresses all subsequent C0413, not a pragma on
  the import). `add_ignored_message` only matters for useless-suppression
  (I0021) accounting.
- args = `node.as_string()` — the import statement re-serialized by astroid
  (e.g. `import os`, `from a import (b, c)` → `from a import b, c`).

## 2.5 C0411 wrong-import-order — leave_module → `_check_imports_order` (imports.py:764-870)

Input `self._imports_stack` (module-scope imports in visit order,
ImportFrom recorded once, Import once per name).
`_record_import` (imports.py:717-740): importedname = modname (ImportFrom) or
imported module's real name or fallback `node.names[0][0].split(".")[0]`;
relative ImportFrom (level ≥ 1) prefixes ".".

For each `(node, modname)`:
- `package = "." + modname.split(".")[1]` if modname startswith "." else
  `modname.split(".")[0]`. (For `from . import x`, importedname is
  ".x" → package ".x".)
- `nested = not isinstance(node.parent, nodes.Module)`.
- `ignore_for_import_order = not is_message_enabled("wrong-import-order",
  node.fromlineno)` (pragma-sensitive per line!).
- `import_category = isort.place_module(package, config=self._isort_config)`
  where `_isort_config` (imports.py:749-762) =
  `isort.Config(extra_standard_library=known_standard_library,
  known_third_party=known_third_party)`. Categories: FUTURE, STDLIB,
  THIRDPARTY, FIRSTPARTY, LOCALFOLDER. **Port note**: this requires
  replicating isort 5/6's `place_module` for the pinned isort version in
  `.venv-pylint` (default config: stdlib list per py version, `.`-prefixed →
  LOCALFOLDER, known first party empty, default section THIRDPARTY; no
  settings-file discovery happens because Config(...) is constructed
  directly with only those two overrides).
- Dispatch (imports.py:794-869):
  - FUTURE|STDLIB → append to std_imports; `wrong_import =
    third_party_not_ignored or first_party_not_ignored or local_not_ignored`
    — NOTE Python `or` semantics: wrong_import is the FIRST non-empty of the
    three lists, not their union; if `self._is_fallback_import(node,
    wrong_import)` (any are_exclusive(import_node, node) over THAT list only
    — try/except fallback; only the STDLIB branch has this guard) →
    continue; if wrong_import and not nested → message with args:
    `(f'standard import "{full_name}"', out_of_order_string)`.
  - THIRDPARTY → append to third_party_imports + external_imports; if not
    nested: not-ignored → push to third_party_not_ignored else
    add_ignored_message; `wrong_import = first_party_not_ignored or
    local_not_ignored`; if wrong and not nested → args
    `(f'third party import "{full}"', out_of_order(None, fp, loc))`.
  - FIRSTPARTY → analogous; args `(f'first party import "{full}"',
    out_of_order(None, None, loc))`.
  - LOCALFOLDER → push only (never reported itself).
- Messages reference IMPORTS SEEN SO FAR (the not_ignored lists), so a
  stdlib import after a third-party one reports listing the earlier
  third-party imports.
- `_get_full_import_name` (imports.py:998-1021): ImportFrom →
  `f"{modname}.{names[0][0]}"`; else `names[0][0]` if its first dot-component
  == package else `package` (handles `import a, b` second entry).
- `_get_out_of_order_string` (imports.py:872-996): builds
  `third party import(s) "x", "y"`, `first party import(s) ...`,
  `local import(s) ...` fragments; each list capped by
  MAX_NUMBER_OF_IMPORT_SHOWN = 6 (constants.py:284): if more, first 3
  (`int(6//2)`) + " (...) " + last 3 (`int(-6//2)` = -3). Delimiters:
  third_party followed by `", "` if both first_party and local follow,
  `" and "` if exactly one follows; first_party fragment joined with
  `", "`+`"and "` / `" "`+`"and "` / `" "` per the delimiter code
  (imports.py:976-989 verbatim — port exactly:
  ```python
  delimiter_third_party = (", " if (first_party and local) else
                           (" and " if (first_party or local) else "")) if third_party else ""
  delimiter_first_party1 = (", " if (third_party and local) else " ") if first_party else ""
  delimiter_first_party2 = ("and " if local else "") if first_party else ""
  ```
  ).
- Template result: `standard import "os" should be placed before third party
  import "six"` etc.

## 2.6 C0412 ungrouped-imports — leave_module (imports.py:581-611)

```python
std_imports, ext_imports, loc_imports = self._check_imports_order(node)
met_import: set[str] = set(); met_from: set[str] = set()
current_package = None
for import_node, import_name in std_imports + ext_imports + loc_imports:
    met = met_from if isinstance(import_node, nodes.ImportFrom) else met_import
    package, _, _ = import_name.partition(".")
    if (current_package and current_package != package and package in met
            and not in_type_checking_block(import_node)
            and not (isinstance(import_node.parent, nodes.If)
                     and is_sys_guard(import_node.parent))):
        self.add_message("ungrouped-imports", node=import_node, args=package)
    current_package = package
    if not self.linter.is_message_enabled("ungrouped-imports", import_node.fromlineno):
        continue
    met.add(package)
self._imports_stack = []; self._first_non_import_node = None
```
- The scan order is std+external+local CONCATENATED (not source order!) —
  e.g. an interleaved stdlib import never breaks a third-party group.
- `import_name` is already the package (the lists store (node, package)), so
  partition is a no-op except for "."-prefixed locals.
- `met` distinguishes plain Import vs ImportFrom — `import x` then
  `from y import z` then `import x.q` is NOT ungrouped w.r.t. the from-set.
- A line-disabled import doesn't enter `met` (affects later groupings).
- `is_sys_guard` (utils.py:1845-1865): If test Compare with left (possibly
  Subscript of) Attribute `sys.version_info`, or Attribute `six.PY2/six.PY3`.

## 2.7 C0414 useless-import-alias / R0402 consider-using-from-import — `_check_import_as_rename` (imports.py:1120-1142)

```python
for name in node.names:
    if not all(name): return            # any (name, None) → STOP entire check (return!)
    splitted_packages = name[0].rsplit(".", maxsplit=1)
    import_name = splitted_packages[-1]
    aliased_name = name[1]
    if import_name != aliased_name: continue
    if len(splitted_packages) == 1 and (self._allow_reexport_package is False
                                        or self._current_module_package is False):
        self.add_message("useless-import-alias", node=node, confidence=HIGH)
    elif len(splitted_packages) == 2:
        self.add_message("consider-using-from-import", node=node,
                         args=(splitted_packages[0], import_name))
```
- Runs for BOTH Import and ImportFrom: `from x import y as y` →
  C0414 (len==1 split). `import a.b as b` → R0402 args ("a", "b").
- `allow-reexport-from-package=True` + current module is a package
  `__init__` → C0414 suppressed (R0402 unaffected).
- The early `return` on a no-alias name means `import a, b as b` is NOT
  flagged (first name has alias None). Bug-for-bug.

## 2.8 C0415 import-outside-toplevel — `_check_toplevel` (imports.py:1251-1275)

```python
if isinstance(node.scope(), nodes.Module): return
module_names = [f"{node.modname}.{name[0]}" if isinstance(node, nodes.ImportFrom)
                else name[0] for name in node.names]
scoped_imports = [n for n in module_names if n not in self._allow_any_import_level]
if scoped_imports:
    self.add_message("import-outside-toplevel", args=", ".join(scoped_imports), node=node)
```
Any import whose SCOPE is not the module (function/method/class bodies —
note ClassDef IS a scope, so class-body imports are flagged) — module-level
`if`/`try` imports are fine (scope still Module).

## 2.9 W0401 wildcard-import — `_check_wildcard_imports` (imports.py:1232-1249)

```python
if node.root().package: return        # skip inside __init__.py (issue #2026)
wildcard_import_is_allowed = (self.linter.config.allow_wildcard_with_all
    and imported_module is not None and "__all__" in imported_module.locals)
for name, _ in node.names:
    if name == "*" and not wildcard_import_is_allowed:
        self.add_message("wildcard-import", args=node.modname, node=node)
```

## 2.10 W0404 reimported / W0416 shadowed-import — `_check_reimport` (imports.py:1144-1171) + `_get_first_import` (:85-137)

`_check_reimport` gate: skipped only if BOTH reimported and shadowed-import
disabled (config-level).
```python
frame = node.frame(); root = node.root()
contexts = [(frame, level)]            # level None for Import, node.level for ImportFrom
if root is not frame: contexts.append((root, None))
for known_context, known_level in contexts:
    for name, alias in node.names:
        first, msg = _get_first_import(node, known_context, name, basename,
                                       known_level, alias)
        if first is not None and msg is not None:
            name = name if msg == "reimported" else alias
            self.add_message(msg, node=node, args=(name, first.fromlineno),
                             confidence=HIGH)
```
`_get_first_import(node, context, name, base, level, alias)`:
- `fullname = f"{base}.{name}" if base else name`.
- Scans `context.body` (TOP-LEVEL statements of the frame only — imports
  nested in `if`/`try` within the frame are invisible as "first"):
  - skip `first is node`; skip same-scope stmts with
    `first.fromlineno > node.fromlineno`.
  - Import: any `iname[0] == fullname` → found (msg "reimported"); else any
    name with no alias whose `imported_name == alias` (the NEW import's
    alias shadows a prior plain import) → found, msg "shadowed-import".
  - ImportFrom (only when `level == first.level`): `fullname ==
    f"{first.modname}.{imported_name}"` → reimported; or `name != "*" and
    name == imported_name and not (alias or imported_alias)` → reimported;
    or `not imported_alias and imported_name == alias` → shadowed-import.
- `if found and not astroid.are_exclusive(first, node): return first, msg`
  (try/except fallback imports exempt).
- NOTE `for first in context.body` leaves `first` bound to the LAST body stmt
  when not found — guarded by the `found` flag.
- W0404 args: (name, first.fromlineno); W0416 args: (alias,
  first.fromlineno). Both `%r` on the name.
- `_check_same_line_imports` (imports.py:690-696): Counter over an
  ImportFrom's own names; count > 1 → reimported args (name,
  node.fromlineno) — note the line arg is the node's OWN line.
- Dedup caveat: contexts list can yield the message twice (frame and root)
  for the same name if both contain matching earlier imports — pylint does
  not dedup; in practice frame==root at module level (single context).

## 2.11 W0406 import-self / graph building — `_add_imported_module` (imports.py:1055-1092)

```python
module_file = node.root().file; context_name = node.root().name
base = os.path.splitext(os.path.basename(module_file))[0]
try:
    if isinstance(node, nodes.ImportFrom) and node.level:
        importedmodname = astroid.modutils.get_module_part(importedmodname, module_file)
    else:
        importedmodname = astroid.modutils.get_module_part(importedmodname)
except ImportError: pass
if context_name == importedmodname:
    self.add_message("import-self", node=node)
elif not astroid.modutils.is_stdlib_module(importedmodname):
    if base != "__init__" and context_name not in self._module_pkg:
        self._module_pkg[context_name] = context_name.rsplit(".", 1)[0]
    dependencies_stat = self.linter.stats.dependencies
    importedmodnames = dependencies_stat.setdefault(importedmodname, set())
    if context_name not in importedmodnames: importedmodnames.add(context_name)
    self.import_graph[context_name].add(importedmodname)
    if not self.linter.is_message_enabled("cyclic-import", line=node.lineno) \
            or in_type_checking_block(node):
        self._excluded_edges[context_name].add(importedmodname)
```
- `get_module_part(dotted_name[, context_file])` (astroid modutils): strips
  trailing attribute parts until the prefix resolves to an importable module
  (e.g. `os.path.join` → `os.path`). Port per astroid spec.
- `is_stdlib_module`: top-level package in sys.stdlib_module_names.
- W0406: e.g. module `pkg.mod` doing `import pkg.mod` or `from pkg import
  mod`... — triggered when resolved imported module name equals the
  CURRENT module's astroid name. For ImportFrom: called per non-star name
  with `f"{imported_module.name}.{name}"`, so `from . import mod` inside
  `pkg/mod.py` → "pkg.mod" == context → import-self on the ImportFrom node.
- Edge exclusion is pragma-sensitive per import LINE and TYPE_CHECKING-block
  sensitive.

## 2.12 W0407 preferred-module — `_check_preferred_module` (imports.py:1094-1118)

```python
mod_compare = [mod_path]
if isinstance(node, nodes.ImportFrom):
    mod_compare = [f"{node.modname}.{name[0]}" for name in node.names]
matches = [k for k in self.preferred_modules for mod in mod_compare
           if k == mod or k in mod.split(".")[0]]
if matches:
    self.add_message("preferred-module", node=node,
                     args=(self.preferred_modules[matches[0]], matches[0]))
```
- The second test is a SUBSTRING check of the key inside the first dot
  component (e.g. key "xml" matches module "xml2lib"). Bug-for-bug.
- Only the first match (preferred_modules iteration order = config order)
  is reported, once per visit_import NAME / once per visit_importfrom.
- Default `preferred-modules = ()` → dead by default; only config makes it
  live.

## 2.13 W0410 misplaced-future — `_check_misplaced_future` (imports.py:678-688)

modname == "__future__" and `node.previous_sibling()` exists and is not
another `from __future__ import` → message (no args), node = ImportFrom.
(Docstring is not a sibling; a leading comment isn't either.)

## 2.14 W4901 deprecated-module — DeprecatedMixin.check_deprecated_module (deprecated.py:237-243)

```python
for mod_name in self.deprecated_modules():
    if mod_path == mod_name or (mod_path and mod_path.startswith(mod_name + ".")):
        self.add_message("deprecated-module", node=node, args=mod_path)
```
`deprecated_modules()` (imports.py:514-522) = set(config.deprecated_modules)
∪ every DEPRECATED_MODULES\[since_vers\] set with `since_vers <=
sys.version_info` — at runtime CPython 3.12 that is ALL entries
(imports.py:49-82): tkinter.tix, fpectl, xml.etree.cElementTree, imp,
formatter, asynchat, asyncore, smtpd, macpath, lib2to3, parser, symbol,
binhex, distutils, typing.io, typing.re, aifc, audioop, cgi, cgitb, chunk,
crypt, imghdr, msilib, mailcap, nis, nntplib, ossaudiodev, pipes, sndhdr,
spwd, sunau, sre_compile, sre_constants, sre_parse, telnetlib, uu, xdrlib.
- Set iteration order (PYTHONHASHSEED=0) determines which mod_name matches
  first, but since a message is emitted for EVERY matching mod_name (no
  break) and matches are usually unique, order only matters if a module
  matches several prefixes (e.g. "typing.io" vs nothing else — safe). A
  duplicate emission is possible if both "x" and "x.y" are deprecated and
  mod_path is "x.y.z" — emits twice.
- For visit_import: called per name with the RAW dotted name; for
  visit_importfrom: once with absolute_name (relative imports resolved via
  `Module.relative_to_absolute_name`).
- args = mod_path (`%r` quoting in template).

## 2.15 R0401 cyclic-import

### Graph
Edges added in `_add_imported_module` (§2.11): `import_graph[context_name].add
(importedmodname)` for every NON-stdlib, non-self import resolved through
`get_module_part`, INCLUDING imports of modules outside the checked set
(third-party). Both `import x` (per name) and `from m import y` (per name,
with the `m.y` → module-part reduction) feed it. Keys appear in
first-import-seen order (file processing order); values are Python sets of
strings.

### Emission — close() (imports.py:484-490)
```python
if self.linter.is_message_enabled("cyclic-import"):
    graph = self._import_graph_without_ignored_edges()   # deepcopy minus _excluded_edges
    vertices = list(graph)
    for cycle in get_cycles(graph, vertices=vertices):
        self.add_message("cyclic-import", args=" -> ".join(cycle))
```
- `close()` runs ONCE after all modules, from `_astroid_module_checker` exit:
  `for checker in reversed(_checkers): checker.close()` (pylinter.py:994-996)
  — checkers in REVERSED prepare_checkers order. Relative ordering with other
  closers matters only if several emit messages at close (imports R0401 and
  similar-lines R0801 — R0801 is symilar.py's reduce/close; replicate the
  reversed order).
- `add_message` with node=None, line=None →
  `module = self.current_name, obj = ""` and `abspath = self.current_file`
  (pylinter.py:1256-1259) — i.e. **attributed to the LAST checked module**;
  `line or 1` → line 1, `col_offset or 0` → col 0 (pylinter.py:1277-1278).
  Output line: `{lastpath}:1:0: R0401: Cyclic import (a -> b) (cyclic-import)`.
  If the last module printed no other messages, the `************* Module
  {lastmodule}` header is emitted now by the text reporter.
- Enablement: the close-time `is_message_enabled("cyclic-import")` and the
  per-message `is_message_enabled(msgid, line=1, confidence)` consult the
  CONFIG state plus the file_state of the last module (line-1 pragmas of the
  LAST module can suppress all R0401!). Per-edge suppression happens at
  graph-build time via `_excluded_edges` (line pragma on the import, or
  TYPE_CHECKING block).

### get_cycles (pylint/graph.py:164-211)
```python
def get_cycles(graph_dict, vertices):
    result = []
    for vertice in vertices:                       # insertion order of filtered graph
        _get_cycles(graph_dict, [], set(), result, vertice)
    return result

def _get_cycles(graph_dict, path, visited, result, vertice):
    if vertice in path:
        cycle = [vertice]
        for node in path[::-1]:
            if node == vertice: break
            cycle.insert(0, node)
        start_from = min(cycle)                    # canonical: rotate min to front
        index = cycle.index(start_from)
        cycle = cycle[index:] + cycle[0:index]
        if cycle not in result: result.append(cycle)
        return
    path.append(vertice)
    try:
        for node in graph_dict[vertice]:           # SET iteration → hash order!
            if node not in visited:
                _get_cycles(graph_dict, path, visited, result, node)
                visited.add(node)
    except KeyError:
        pass
    path.pop()
```
- DFS from every vertex with a FRESH path and FRESH visited set per vertex.
- `graph_dict[vertice]` raises KeyError for sink nodes not in the dict —
  swallowed. BUT the filtered graph is a defaultdict(set) deepcopy →
  `filtered_graph[node]` in `_import_graph_without_ignored_edges`
  (imports.py:478-482) only iterates existing keys; inside `_get_cycles` the
  access `graph_dict[vertice]` on a defaultdict CREATES an empty entry
  instead of raising — harmless for results, but means the KeyError branch
  is dead for the deepcopied defaultdict. Iteration `for node in
  filtered_graph` in the difference_update loop is over keys snapshot before
  mutation (Python dict iteration + setdefault during DFS would error —
  defaultdict creation during iteration of `vertices` list is safe since
  vertices was materialized first).
- **Iteration-order dependency**: inner `for node in graph_dict[vertice]`
  iterates a `set[str]` — string-hash order under PYTHONHASHSEED=0. The
  produced cycle list order AND which representative path is found first
  depend on it. Port by replicating CPython set-of-str iteration order under
  seed 0 (same infrastructure as for other hash-order-dependent outputs) or
  by snapshotting pylint's actual orderings via differential testing.
- Canonicalization: rotate so the lexicographically smallest module name is
  first; dedup by exact list equality. Cycles longer than 2 included; the
  cycle reported reflects one specific path.
- args: `" -> ".join(cycle)`.
- `-j` note: under --jobs>1 pylint calls `get_map_data` per worker and
  `reduce_map_data` (imports.py:492-512) on the main process, which
  **`update()`s** dicts (LAST worker's set wins per key — lossy!) then calls
  `close()` again. We always run sequentially; only the sequential semantics
  above need porting, but document that parallel pylint output differs.

## 2.16 `_get_imported_module` (imports.py:1023-1053) — shared E-path

Covered in notes/08 §16 (E0401/E0402 + the E0001 "Cannot import 'X' due to
'...'" syntax-error path imports.py:1032-1036). W/C/R checks above only need:
returns Module or None; `_ignore_import_failure` (imports.py:140-155) uses
ignored-modules, TYPE_CHECKING blocks, sys guards, and
`node_ignores_exception(node, ImportError)`.

================================================================================
# 3. classes/class_checker.py — ClassChecker W/C/R messages
================================================================================

## 3.0 Options & state (class_checker.py:779-845, 847-859)

| option | default |
|--------|---------|
| defining-attr-methods | `("__init__", "__new__", "setUp", "asyncSetUp", "__post_init__")` |
| valid-classmethod-first-arg | `("cls",)` |
| valid-metaclass-classmethod-first-arg | `("mcs",)` |
| exclude-protected | `("_asdict", "_fields", "_replace", "_source", "_make", "os._exit")` |
| check-protected-access-in-special-methods | `False` |

Cross-checker config: `mixin_class_rgx` (typecheck.py:856-862, default regexp
`.*[Mm]ixin`), `ignored_checks_for_mixins` (typecheck.py:877-888, default
`["no-member", "not-async-context-manager", "not-context-manager",
"attribute-defined-outside-init"]`), `dummy_variables_rgx` (variables
checker), `py_version` → `self._py38_plus = py_version >= (3, 8)` (open(),
:852-855; default py_version = interpreter (3,12) → True).

State: `self._accessed = ScopeAccessMap()` (class → attrname → access nodes;
class_checker.py:742-760) — **note: instance attribute created in __init__,
NEVER reset per module; accesses accumulate across the whole run but are
keyed by ClassDef node so no cross-module bleed**; `self._first_attrs` stack
of first-arg names.

## 3.1 First-argument checks (C0202/C0203/C0204/W0211 + E0211/E0213)

`visit_functiondef` (class_checker.py:1266-1357; alias asyncfunctiondef
:1359) — undecorated, always runs; only for `node.is_method()`.
Order of operations: `_check_useless_super_delegation` (W0246),
`_check_property_with_parameters` (R0206), then
`_check_first_arg_for_type(node, klass.type == "metaclass")`
(class_checker.py:2079-2155):
```python
if node.args.args is None: return       # builtin stub — note: no _first_attrs push!
if node.args.posonlyargs: first_arg = node.args.posonlyargs[0].name
elif node.args.args:      first_arg = node.argnames()[0]
else:                     first_arg = None
self._first_attrs.append(first_arg)
first = self._first_attrs[-1]
if node.type == "staticmethod":
    if (first_arg == "self"
            or first_arg in self.linter.config.valid_classmethod_first_arg
            or first_arg in self.linter.config.valid_metaclass_classmethod_first_arg):
        self.add_message("bad-staticmethod-argument", args=first, node=node)  # W0211
        return            # NOTE: _first_attrs[-1] left as the bad name!
    self._first_attrs[-1] = None
elif "builtins.staticmethod" in node.decoratornames():
    return                # decorator aliased to staticmethod: skip everything
elif not (node.args.args or node.args.posonlyargs or node.args.vararg or node.args.kwarg):
    self.add_message("no-method-argument", node=node, args=node.name)        # E0211
elif metaclass:
    if node.type == "classmethod":
        self._check_first_arg_config(first, valid_metaclass_classmethod_first_arg,
            node, "bad-mcs-classmethod-argument", node.name)                 # C0204
    else:
        self._check_first_arg_config(first, valid_classmethod_first_arg,
            node, "bad-mcs-method-argument", node.name)                      # C0203
elif node.type == "classmethod" or node.name == "__class_getitem__":
    self._check_first_arg_config(first, valid_classmethod_first_arg,
        node, "bad-classmethod-argument", node.name)                         # C0202
elif first != "self":
    self.add_message("no-self-argument", node=node, args=node.name)          # E0213
```
- `_check_first_arg_config` (class_checker.py:2157-2171): if `first not in
  config`: `valid = repr(config[0])` if single, else
  `", ".join(repr(v) for v in config[:-1]) + f" or {config[-1]!r}"`;
  args = (method_name, valid). Default single → `Class method m should have
  'cls' as first argument`.
- W0211 args = first (raw name, `%r` in template).
- `klass.type` is astroid's class type ("metaclass" when inheriting `type`).
- `leave_functiondef` (class_checker.py:1668-1678): pops `_first_attrs` iff
  `node.is_method() and node.args.args is not None` — symmetric with the
  push (no push happened when args.args is None).

## 3.2 W0212 protected-access

Driven by `visit_attribute` (class_checker.py:1680-1697) and `visit_assign`
(class_checker.py:1826-1837, decorated only_required("protected-access",
"no-classmethod-decorator", "no-staticmethod-decorator")):
- visit_attribute: `_check_super_without_brackets` first (§3.13); then if
  `_uses_mandatory_method_param(node)` (node.expr is Name equal to
  `_first_attrs[-1]`, or — when the stack is empty — the first positional
  arg of the closest bound enclosing FunctionDef, class_checker.py:2359-2388)
  → `self._accessed.set_accessed(node)` and return; else if
  protected-access enabled → `_check_protected_attribute_access(node)`.
- visit_assign: `_check_classmethod_declaration` (§3.12); `node =
  assign_node.targets[0]`; only when it's an AssignAttr; if
  `_uses_mandatory_method_param(node)` → return (NO set_accessed here —
  plain `self.x = 1` stores are collected as instance_attrs by astroid at
  build time instead); else `_check_protected_attribute_access(node)`.
- visit_assignattr (class_checker.py:1717-1723, decorated
  only_required("assigning-non-slot", "invalid-class-object",
  "access-member-before-definition")): AugAssign targets that use the
  mandatory param ARE marked accessed (feeds E0203/W0201 exclusion).

`_check_protected_attribute_access` (class_checker.py:1872-1969):
```python
attrname = node.attrname
if (not is_attr_protected(attrname)                        # utils.py:666-674:
        or attrname in self.linter.config.exclude_protected):
    return
# is_attr_protected: attrname[0]=="_" and attrname!="_" and not dunder
if utils.is_node_in_type_annotation_context(node): return
inferred = safe_infer(node.expr)
if (inferred and isinstance(inferred, (nodes.ClassDef, nodes.Module))
        and f"{inferred.name}.{attrname}" in self.linter.config.exclude_protected):
    return                                                  # e.g. "os._exit"
klass = node_frame_class(node)                              # utils.py:677-699
if klass is None:
    self.add_message("protected-access", node=node, args=attrname); return
match node.expr:
    case nodes.Call(func=nodes.Name(name="super")): return
if self._is_type_self_call(node.expr): return               # type(self)._x / type(cls)
inside_klass = True; outer_klass = klass
callee = node.expr.as_string()
parents_callee = callee.split(".")
for callee in reversed(parents_callee):
    if not (outer_klass and callee == outer_klass.name):
        inside_klass = False; break
    outer_klass = get_outer_class(outer_klass)              # utils.py:702-706
if not (inside_klass or callee in klass.basenames):
    match node.parent.statement():
        case nodes.Assign(targets=[nodes.AssignName(name=name)]) \
                if _is_attribute_property(name, klass):     # class_checker.py:449-476
            return                                          # b = property(lambda: self._b)
    if (self._is_classmethod(node.frame())                  # type=="classmethod" or
                                                            # name=="__class_getitem__"
            and self._is_inferred_instance(node.expr, klass)  # safe_infer → Instance
                                                              # with _proxied is klass
            and self._is_class_or_instance_attribute(attrname, klass)):
        return                                              # cls-made instance access
    licit_protected_member = not attrname.startswith("__")
    if (not self.linter.config.check_protected_access_in_special_methods
            and licit_protected_member
            and self._is_called_inside_special_method(node)):  # frame name in PYMETHODS
        return
    self.add_message("protected-access", node=node, args=attrname)
```
- **Quirk**: after the `for callee in reversed(parents_callee)` loop,
  `callee` holds the COMPONENT where the walk stopped (innermost-first); the
  fallback test `callee in klass.basenames` therefore compares that
  component (for a simple `Other._x`, callee == "Other"). For dotted callees
  like `a.b._x`, callee is "b" after the first mismatch... no — reversed:
  first iteration callee="b"; if "b" != klass.name → break with callee="b";
  the basenames test then checks "b". Replicate verbatim.
- `_is_type_self_call` (class_checker.py:1977-1981): expr is
  `Call(func=Name("type"), args=[single])` and `_is_mandatory_method_param
  (arg)`.
- `_is_attribute_property` (class_checker.py:449-476): klass.getattr(name);
  for each attr (skipping Uninferable): `next(attr.infer())` (InferenceError
  → continue); FunctionDef decorated_with_property → True; or
  `inferred.pytype() == "builtins.property"` → True.
- node may be Attribute (load) or AssignAttr (store via visit_assign).
  args = attrname (template `%s`).
- W0212 has NO mixin exemption.

## 3.3 W0201 attribute-defined-outside-init — leave_classdef → `_check_attribute_defined_outside_init` (class_checker.py:1192-1263)

leave_classdef (class_checker.py:1050-1059) decorated
only_required("unused-private-member", "attribute-defined-outside-init",
"access-member-before-definition"); calls the three private-member checks
(§3.10) then this.

```python
if ("attribute-defined-outside-init" in self.linter.config.ignored_checks_for_mixins
        and self._mixin_class_rgx.match(cnode.name)):
    return                                  # default config: Mixin classes skipped
accessed = self._accessed.accessed(cnode)
if cnode.type != "metaclass":
    self._check_accessed_members(cnode, accessed)        # E0203, notes/08
if not self.linter.is_message_enabled("attribute-defined-outside-init"): return
defining_methods = self.linter.config.defining_attr_methods
current_module = cnode.root()
for attr, nodes_lst in cnode.instance_attrs.items():     # build-time collection order
    if attr == "__dict__": continue
    nodes_lst = [n for n in nodes_lst
                 if not isinstance(n.statement(), (nodes.Delete, nodes.AugAssign))
                 and n.root() is current_module]
    if not nodes_lst: continue
    frames = (node.frame() for node in nodes_lst)
    if any(frame.name in defining_methods or is_property_setter(frame)
           for frame in frames):
        continue                       # defined in __init__/__new__/setUp/... or a setter
    for parent in cnode.instance_attr_ancestors(attr):
        attr_defined = False
        for node in parent.instance_attrs[attr]:
            if node.frame().name in defining_methods: attr_defined = True
        if attr_defined: break         # defined in an ancestor's __init__ etc.
    else:
        try: cnode.local_attr(attr)    # class attribute → fine
        except astroid.NotFoundError:
            for node in nodes_lst:
                if node.frame().name not in defining_methods:
                    if _called_in_methods(node.frame(), cnode, defining_methods):
                        continue       # the assigning method is CALLED from __init__
                    self.add_message("attribute-defined-outside-init",
                                     args=attr, node=node)
```
- `cnode.instance_attrs`: astroid's per-class map of `self.X = ...`
  AssignAttr nodes collected during build (insertion order = source order of
  first assignment per name, then appended).
- One message PER offending AssignAttr node (an attr assigned in two
  non-defining methods → two messages).
- `_called_in_methods` (class_checker.py:416-446): for each defining-method
  name, `klass.getattr(method)`; for every Call inside those, `next(
  call.func.infer())`; BoundMethod whose underlying function (unwrap
  UnboundMethod) has `.name == func.name` → True.
- `is_property_setter(frame)` (utils.py:828-830): any decorator
  `Attribute(attrname="setter")`.

## 3.4 W0221 arguments-differ / W0222 signature-differs / W0237 arguments-renamed

From visit_functiondef (class_checker.py:1282-1296): skip `__init__`
(handled by `_check_init`); find the FIRST ancestor in
`klass.local_attr_ancestors(node.name)` (MRO order, excluding klass) whose
local `node.name` maps to a FunctionDef → `_check_signature(node,
parent_function, klass)` + `_check_invalid_overridden_method(...)`; break.

`_check_signature` (class_checker.py:2272-2357):
```python
if not (isinstance(method1, FunctionDef) and isinstance(refmethod, FunctionDef)):
    self.add_message("method-check-failed", args=(method1, refmethod), node=method1)
    return                                                # F0202 (notes/08)
instance = cls.instantiate_class()
method1 = astroid.scoped_nodes.function_to_method(method1, instance)
refmethod = astroid.scoped_nodes.function_to_method(refmethod, instance)
# function_to_method wraps plain functions in UnboundMethod when bound —
# .args etc. proxy through; staticmethods stay functions.
if method1.args.args is None or refmethod.args.args is None: return
if is_attr_private(method1.name): return       # ^_{2,10}.*[^_]+_?$ (utils.py:709-714)
if is_property_setter(method1): return
arg_differ_output = _different_parameters(refmethod, method1,
                                          dummy_parameter_regex=self._dummy_rgx)
class_type = "overriding"
if len(arg_differ_output) > 0:
    for msg in arg_differ_output:
        if "Number" in msg:
            total_args_method1 = len(method1.args.args) \
                + (1 if method1.args.vararg else 0) + (1 if method1.args.kwarg else 0) \
                + (len(method1.args.kwonlyargs) if method1.args.kwonlyargs else 0)
            total_args_refmethod = ... same for refmethod ...
            error_type = "arguments-differ"
            msg_args = (msg + f"was {total_args_refmethod} in "
                        f"'{refmethod.parent.frame().name}.{refmethod.name}' and "
                        f"is now {total_args_method1} in", class_type,
                        f"{method1.parent.frame().name}.{method1.name}")
        elif "renamed" in msg:
            error_type = "arguments-renamed"
            msg_args = (msg, class_type, f"{...}.{method1.name}")
        else:
            error_type = "arguments-differ"
            msg_args = (msg, class_type, f"{...}.{method1.name}")
        self.add_message(error_type, args=msg_args, node=method1)
elif len(method1.args.defaults) < len(refmethod.args.defaults) and not method1.args.vararg:
    class_type = "overridden"
    self.add_message("signature-differs", args=(class_type, method1.name), node=method1)
```
- Template `%s %s %r method`: e.g. `Number of parameters was 3 in
  'Base.meth' and is now 2 in overriding 'Child.meth' method`, or
  `Parameter 'a' has been renamed to 'b' in overriding 'Child.meth' method`.
  Note the FIRST %s strings carry trailing context (`"Number of parameters "`
  literal includes a trailing space; the renamed message ends with "in").
- W0222 args = ("overridden", method1.name) → `Signature differs from
  overridden 'meth' method`.

`_different_parameters` (class_checker.py:316-390):
- `_positional_parameters` (:202-206): `method.args.args`; if
  `method.is_bound() and method.type in {"classmethod", "method"}` drop the
  first. (posonlyargs NOT included here!)
- If overridden (child) has vararg: original positionals filtered to names
  present in child's positionals. If child has kwarg: original kwonlyargs
  filtered to names present in child's kwonlyargs.
- `_has_different_parameters` (:262-290): zip_longest over positional
  AssignName lists; missing child param → `["Number of parameters "]`
  (early return); missing original param → child param must have a default
  (`overridden_param.parent.default_value(name)`; NoDefault →
  `["Number of parameters "]`); name comparison skipped when either side
  matches dummy_variables_rgx; differing names append
  `f"Parameter '{orig}' has been renamed to '{new}' in"`.
- `_has_different_keyword_only_parameters` (:293-313): any original kwonly
  name missing from child → Number; extra child kwonly without default →
  Number.
- Merge logic (:360-372): if both lists report and `"Number " in
  different_positional[0] and "Number " in different_kwonly[0]` (SUBSTRING
  test on the FIRST element of each — rename messages never contain
  "Number ", so in practice this means both lead with the Number entry) →
  emit single combined `"Number of parameters "` plus `different_positional
  [1:] + different_kwonly[1:]`; else concatenate both lists.
- Variadics lost (:374-380): original has kwarg/vararg and child doesn't →
  append `"Variadics removed in"`.
- `if original.name in PYMETHODS: output_messages.clear()` (:382-388) — NO
  W0221/W0237 for dunders.
- `_has_different_parameters_default_value` is NOT used here (only W0246).

`klass.local_attr_ancestors` is astroid: ancestors in MRO (or recursive
bases order if MRO fails) that have the name in `locals`.

## 3.5 W0236 invalid-overridden-method / W0239 overridden-final-method — `_check_invalid_overridden_method` (class_checker.py:1457-1505)

Same (node, parent_function) pair as §3.4:
```python
parent_is_property = decorated_with_property(parent) or
                     is_property_setter_or_deleter(parent)
current_is_property = ... same for node ...
if parent_is_property and not current_is_property:
    add_message("invalid-overridden-method",
                args=(node.name, "property", node.type), node=node)
elif not parent_is_property and current_is_property:
    add_message(..., args=(node.name, "method", "property"), node=node)
parent_is_async = isinstance(parent, nodes.AsyncFunctionDef)
current_is_async = isinstance(node, nodes.AsyncFunctionDef)
if parent_is_async and not current_is_async:
    add_message(..., args=(node.name, "async", "non-async"), node=node)
elif not parent_is_async and current_is_async:
    add_message(..., args=(node.name, "non-async", "async"), node=node)
if (decorated_with(parent, ["typing.final"])
        or uninferable_final_decorators(parent.decorators)) and self._py38_plus:
    add_message("overridden-final-method",
                args=(node.name, parent.parent.frame().name), node=node)
```
- `node.type` is "method"/"classmethod"/"staticmethod" (third arg of the
  property-mismatch message).
- `uninferable_final_decorators` (utils.py:894-941): decorators resolving to
  a typing `final` import whose safe_infer is None/Uninferable (pre-3.8
  shim); with py-version ≥ 3.8 `decorated_with` normally succeeds, but the
  uninferable path still triggers when inference fails.
- Property AND async mismatches can BOTH fire (independent ifs); W0239 also
  stacks.

## 3.6 W0223 abstract-method — visit_classdef → `_check_bases_classes` (class_checker.py:2173-2204)

visit_classdef is decorated only_required(... "abstract-method" ... — full
list at class_checker.py:861-876).
```python
def is_abstract(method): return method.is_abstract(pass_is_abstract=False)
if class_is_abstract(node): return       # utils.py:1163-1180 (lru_cache):
                                         # Protocol class, declared metaclass ABCMeta
                                         # (abc/_py_abc module), or abc.ABC ancestor
methods = sorted(unimplemented_abstract_methods(node, is_abstract).items(),
                 key=lambda item: item[0])           # SORTED BY NAME
for name, method in methods:
    owner = method.parent.frame()
    if owner is node: continue
    if name in node.locals: continue     # redefined as attribute/descriptor
    self.add_message("abstract-method", node=node,
                     args=(name, owner.name, node.name), confidence=INFERENCE)
```
- `unimplemented_abstract_methods` (utils.py:945-994, lru_cache(1024)):
  walk `reversed(node.mro())` (ResolveError → {}); for each ancestor's
  locals values: AssignName → safe_infer (None → drop name from visited;
  non-FunctionDef → drop); FunctionDef → if `is_abstract_cb(inferred)` add
  `visited[obj.name] = inferred` else remove. Net effect: a name is reported
  if its LAST definition along reversed-MRO is abstract.
- `FunctionDef.is_abstract(pass_is_abstract=False)`: astroid — decorated
  with abc abstract decorators or body raises NotImplementedError; with
  pass_is_abstract=False a `pass`-only body does NOT count.
- Message node = the ClassDef (position = `class` keyword via node.position).
- Output ordering: alphabetical by method name within the class.

## 3.7 W0231 super-init-not-called / W0233 non-parent-init-called — `_check_init` (class_checker.py:2206-2270)

Called from visit_functiondef when `node.name == "__init__"` and is_method.
Gate: skipped only when BOTH messages disabled (config-level).
```python
to_call = _ancestors_to_call(klass_node)        # class_checker.py:2391-2408:
    # {base: bound init} for base in klass_node.ancestors(recurs=False)
    # (DIRECT bases resolution incl. their ancestors? recurs=False → direct
    #  bases only, but each base entry comes from igetattr("__init__") —
    #  next() of inference; skips non-UnboundMethod and abstract inits)
not_called_yet = dict(to_call)
parents_with_called_inits: set = set()
for stmt in node.nodes_of_class(nodes.Call):
    expr = stmt.func
    if not (isinstance(expr, nodes.Attribute) and expr.attrname == "__init__"):
        continue
    match expr.expr:
        case nodes.Call(func=nodes.Name(name="super")): return   # super().__init__ →
                                                                 # WHOLE check passes
    try:
        for klass in expr.expr.infer():
            if isinstance(klass, util.UninferableBase): continue
            match klass:
                case astroid.Instance(_proxied=nodes.ClassDef(name="super") as p) \
                        if is_builtin_object(p): return
                case objects.Super(): return
            try:
                method = not_called_yet.pop(klass)
                parents_with_called_inits.add(node_frame_class(method))
            except KeyError:
                if klass not in klass_node.ancestors(recurs=False):
                    self.add_message("non-parent-init-called", node=expr,
                                     args=klass.name)             # W0233
    except astroid.InferenceError: continue
for klass, method in not_called_yet.items():
    if node_frame_class(method) in parents_with_called_inits:
        return            # NOTE: return, not continue!
    if utils.is_protocol_class(klass): return     # also return!
    if decorated_with(node, ["typing.overload"]): continue
    self.add_message("super-init-not-called", args=klass.name, node=node,
                     confidence=INFERENCE)        # W0231
```
- `_ancestors_to_call`: keys are ClassDef nodes from `ancestors(recurs=False)`
  — astroid resolves each base expression; `igetattr("__init__")` walks the
  base's OWN MRO, so a base without its own `__init__` maps to the inherited
  one (filtered to UnboundMethod — `object.__init__` from the builtins
  snapshot IS an UnboundMethod once bound? it's a FunctionDef; igetattr on a
  ClassDef yields UnboundMethod wrappers; abstract inits skipped).
- W0233 node = `expr` (the Attribute `Foo.__init__`), args = klass.name.
- W0231 node = the `__init__` FunctionDef, args = base class name, one per
  missing base, dict iteration order = insertion (direct-bases order) — BUT
  the two `return`s mean: if ANY missing base's defining class init was
  called via another base, or ANY missing base is a Protocol, the remaining
  bases are silently skipped too. Bug-for-bug.
- `klass not in klass_node.ancestors(recurs=False)` — note this calls
  ancestors(recurs=False) freshly; membership by node identity.

## 3.8 W0246 useless-parent-delegation — `_check_useless_super_delegation` (class_checker.py:1361-1447)

`_is_trivial_super_delegation` (class_checker.py:146-196): method, no
decorators, body == 1 stmt, stmt is Expr/Return whose value is
`Call(func=Attribute(expr=expr))`; `safe_infer(expr)` is `objects.Super`;
`call.func.attrname == function.name`; super's `mro_pointer ==
function.parent.scope()` and `super.type` is Instance named like the scope.

Then:
- `__hash__` exemption when the class also defines `__eq__`
  (mymethods scan, :1379-1382).
- Find `meth_node`: first FunctionDef for the name in
  `klass.local_attr_ancestors(function.name)`; bail (return) if it's not a
  FunctionDef, OR `_has_different_parameters_default_value(meth_node.args,
  function.args)` (class_checker.py:216-259: any param where exactly one
  side has a default, types differ, or ASTROID_TYPE_COMPARATORS
  (class_checker.py:54-61: Const value eq, ClassDef qname **comparing the
  bound methods `a.qname == b.qname` — method objects, always unequal unless
  same node! bug**, Tuple/List elts identity-list eq, Dict items eq, Name
  set(infer()) eq) say different/unhandled), OR
  `meth_node.args.args is None and function.argnames() != ["self"]`.
- Vararg guard (:1413-1419): meth_node has vararg and (function lacks vararg
  or has MORE positional args) → return.
- Annotation guard (:1421-1431): string-compare non-None annotation lists
  (posonlyargs_annotations + annotations); both non-empty and different →
  return. Return-annotation difference (both non-None, as_string differs) →
  return.
- `_definition_equivalent_to_call(params, args)` (class_checker.py:123-143)
  with `params = _signature_from_arguments(function.args)` (:111-120 —
  positionals = `chain(arguments.posonlyargs, arguments.args)` with any arg
  literally NAMED "self" dropped, regardless of position!) and `args =
  _signature_from_call(call)` (:81-108). Equivalence: kwargs name must be
  **-starred in call (and vice versa); vararg likewise; every kwonly param
  passed as keyword; positional name lists exactly equal; no extra call
  keywords beyond args/kwonly.
- Message: `add_message("useless-parent-delegation", node=function,
  args=(function.name,), confidence=INFERENCE)`.

## 3.9 R0206 property-with-parameters — `_check_property_with_parameters` (class_checker.py:1449-1455)

`len(node.args.arguments) > 1 and decorated_with_property(node) and not
is_property_setter(node)` → message (no args), HIGH, node = FunctionDef.
`Arguments.arguments` (astroid node_classes.py:794-809) = posonlyargs + args
+ vararg_node + kwonlyargs + kwarg_node — so `def p(self, *, x)` counts 2 →
flagged. `decorated_with_property` (utils.py:805-815 →
`_is_property_decorator` :846-866): decorator infers to builtins.property /
functools.cached_property ClassDef or subclass, or a one-return factory
function chain.

## 3.10 W0238 unused-private-member — leave_classdef, three scans

All names "private" per `is_attr_private` (utils.py:709-714): regex
`^_{2,10}.*[^_]+_?$` (≥2 leading underscores, not dunder).

(a) `_check_unused_private_functions` (class_checker.py:1061-1115): for each
FunctionDef in the class subtree with private name: nested-function exemption
(parent scope is FunctionDef and the name appears as a Name there); scan all
Name/Attribute nodes in the class for a use:
- Name with same name → used.
- Attribute: skip if `child.attrname != function_def.name or child.scope()
  == function_def` (recursive calls don't count); used if `child.expr` is
  Name in {"self", "cls", node.name}; used if `child.expr` is a Call whose
  safe_infer is a ClassDef named like the class (`type(self).__m()`).
If unused: build dotted repr through enclosing scopes up to the class:
`function_repr = f"{outer_level_names}.{function_def.name}({function_def.args.as_string()})"`
→ args = `(node.name, function_repr.lstrip("."))`, node = the FunctionDef.
Rendered: ``Unused private member `Klass.__meth(self, x=1)` ``.

(b) `_check_unused_private_variables` (class_checker.py:1117-1138): for each
private AssignName in the subtree (skipping Arguments-parented = params):
scan Name/Attribute children: Name equal → used; Attribute with non-Name
expr → counted as used (break!); Attribute with matching attrname and expr
name in ("self", "cls", node.name) → used. Else args = (node.name,
assign_name.name), node = AssignName.

(c) `_check_unused_private_attributes` (class_checker.py:1140-1190): for each
private AssignAttr with Name expr: `acceptable_obj_names = ["self"]` plus,
when assigned inside `__new__`, names returned by its Return statements;
scan all Attribute nodes with same attrname and Name expr:
- assign expr name in {"cls", node.name} and access expr name in
  {"cls", "self", node.name} → used (break);
- assign via acceptable names → access via self (break);
- both via class name (break).
Else args = (node.name, assign_attr.attrname), node = AssignAttr.

## 3.11 W0244 redefined-slots-in-subclass / C0205 single-string-used-for-slots (+E0236/E0238/E0242 in 08)

`_check_slots` (class_checker.py:1547-1582) from visit_classdef: for each
inferred `__slots__` (via `node.ilookup("__slots__")`):
- not iterable/comprehension → E0238 (notes/08); Const → **C0205
  single-string-used-for-slots** (no args, node=ClassDef), continue;
- values = dict keys or itered(); per-element E0236/E0242 (notes/08);
- `_check_redefined_slots(node, slots, values)` (class_checker.py:1612-1636):
  ```python
  slots_names = self._get_slots_names(values)   # Const values + safe_infer .value strs
  ancestors_slots_names = {slot.value
      for ancestor in node.local_attr_ancestors("__slots__")
      for slot in ancestor.slots() or []}
  redefined_slots = ancestors_slots_names.intersection(slots_names)
  if redefined_slots:
      self.add_message("redefined-slots-in-subclass",
          args=([name for name in slots_names if name in redefined_slots],),
          node=slots_node)
  ```
  args is a 1-tuple containing a LIST → `%r` renders `['a', 'b']` in
  slots_names order. node = the slots value node (e.g. the Tuple literal).

## 3.12 R0202 no-classmethod-decorator / R0203 no-staticmethod-decorator — `_check_classmethod_declaration` (class_checker.py:1839-1870)

From visit_assign (before the protected-access target check):
```python
match node.value:
    case nodes.Call(func=nodes.Name(name="classmethod" | "staticmethod" as name),
                    args=[nodes.Name(name=method_name), *_]): pass
    case _: return
msg = "no-classmethod-decorator" if name == "classmethod" else "no-staticmethod-decorator"
parent_class = node.scope()
if not isinstance(parent_class, nodes.ClassDef): return
if any(method_name == member.name for member in parent_class.mymethods()):
    self.add_message(msg, node=node.targets[0])
```
- Requires the first call arg to be a bare Name matching one of the class's
  own methods (`mymethods()` = locals values that are FunctionDef).
- node = `targets[0]` (the AssignName, e.g. `meth = classmethod(meth)` →
  reported at `meth`). No args.

## 3.13 W0245 super-without-brackets — `_check_super_without_brackets` (class_checker.py:1699-1712)

From visit_attribute (unconditional, before protected-access):
frame is FunctionDef whose parent frame is ClassDef; `node.parent` is Call;
`node.expr` is Name; name == "super" → message HIGH, node = `node.expr`
(the bare `super` Name), no args. Catches `super.foo()` (missing `()`).

## 3.14 R0205 useless-object-inheritance / W0240 subclassed-final-class / W0213 implicit-flag-alias

All from visit_classdef:
- `_check_proper_bases` (class_checker.py:995-1022): per base,
  `ancestor = safe_infer(base)`; falsy → skip. After the E0239 logic
  (notes/08): `if isinstance(ancestor, ClassDef) and ancestor.is_subtype_of
  ("enum.Enum"): self._check_enum_base(node, ancestor)`; then
  `if ancestor.name == "object": add_message("useless-object-inheritance",
  args=node.name, node=node)` — name-only check (`object.__name__`), so a
  user class named `object` also triggers. args = class name (`%r`).
- `_check_typing_final` (class_checker.py:1024-1043): `if not
  self._py38_plus: return`; per base: safe_infer → ClassDef and
  (`decorated_with(ancestor, ["typing.final"])` or
  `uninferable_final_decorators(ancestor.decorators)`) →
  `add_message("subclassed-final-class", args=(node.name, ancestor.name),
  node=node)`.
- `_check_enum_base` (class_checker.py:937-993):
  - E0244 part (notes/08): ancestor `__members__` non-empty Dict → flagged
    unless all member defs are valueless AnnAssigns.
  - W0213 part: `if ancestor.is_subtype_of("enum.IntFlag")`:
    ```python
    assignments = defaultdict(list)
    for assign_name in node.nodes_of_class(nodes.AssignName):
        match assign_name.parent:
            case nodes.Assign(value=object(value=int() as value)):
                assignments[value].append(assign_name)
    bit_flags = defaultdict(set)
    for flag in assignments:
        for bit in (i for i, c in enumerate(reversed(bin(flag))) if c == "1"):
            bit_flags[bit].add(flag)
    overlaps = defaultdict(list)
    for flags in bit_flags.values():
        source, *conflicts = sorted(flags)
        for conflict in conflicts: overlaps[conflict].append(source)
    for overlap in overlaps:
        for assignment_node in assignments[overlap]:
            self.add_message("implicit-flag-alias", node=assignment_node,
                args={"overlap": f"<{node.name}.{assignment_node.name}: {overlap}>",
                      "sources": ", ".join(
                          f"<{node.name}.{assignments[source][0].name}: {source}> "
                          f"({overlap} & {source} = {overlap & source})"
                          for source in overlaps[overlap])},
                confidence=INFERENCE)
    ```
    - `case nodes.Assign(value=object(value=int() as value))` — matches any
      Assign whose `.value` node HAS a `.value` attribute that is an int
      (i.e. Const int; `bool` is an int subclass — `X = True` would match!).
    - `bin(flag)` of negative ints contains "-"; enumerate(reversed(...))
      treats "-" not "1" — negative values only set bits for their digits;
      replicate Python's bin() exactly.
    - Named-template message: `Flag member <E.B: 3> shares bit positions
      with <E.A: 1> (3 & 1 = 1)`.
    - Set/dict ordering: `bit_flags[bit]` is a set of INTs → CPython small-int
      hash = identity → iteration ascending for non-negative ints within a
      table; `sorted(flags)` removes the order dependency for source
      selection; `overlaps` dict insertion order follows bit_flags VALUES
      iteration (bit index ascending).

## 3.15 E-checks done elsewhere
visit_classdef also calls `_check_consistent_mro` (E0240/E0241),
`_check_slots` E-parts, `_check_proper_bases` E0239/E0244,
`_check_declare_non_slot` (E0245) — all notes/08. visit_assignattr's
`_check_in_slots` (E0237) and `_check_invalid_class_object` (E0243) —
notes/08. `_check_accessed_members` (E0203) — notes/08 §3.

================================================================================
# 4. exceptions.py — ExceptionsChecker W messages
================================================================================

Checker name "exceptions"; option `overgeneral-exceptions` default
`("builtins.BaseException", "builtins.Exception")` (exceptions.py:289-299).
`open()` caches `_builtin_exceptions` = names of all builtins inheriting
BaseException (exceptions.py:28-33, 301-303).
`_is_overgeneral_exception(exc)` (:653-654): `exc.qname() in
config.overgeneral_exceptions`.

## 4.1 visit_raise (exceptions.py:305-331) — W0707/W0715/W0719 (+E0702/E0704/E0705/E0710/E0711 in 08)

Decorated only_required("misplaced-bare-raise", "raising-bad-type",
"raising-non-exception", "notimplemented-raised", "bad-exception-cause",
"raising-format-tuple", "raise-missing-from", "broad-exception-raised").
```python
if node.exc is None: self._check_misplaced_bare_raise(node); return   # E0704
if node.cause is None: self._check_raise_missing_from(node)           # W0707
else: self._check_bad_exception_cause(node)                           # E0705
expr = node.exc
ExceptionRaiseRefVisitor(self, node).visit(expr)      # W0719 / E0711 / W0715
inferred = utils.safe_infer(expr)
if inferred is None or isinstance(inferred, util.UninferableBase): return
ExceptionRaiseLeafVisitor(self, node).visit(inferred) # E0702/E0710 (notes/08)
```
Visitor dispatch: `visit_<classname.lower()>` else visit_default
(exceptions.py:183-199).

### W0719 broad-exception-raised — ExceptionRaiseRefVisitor.visit_name (:205-227)
```python
if node.name == "NotImplemented": → E0711, return
try: exceptions = [c for _, c in _annotated_unpack_infer(node)
                   if isinstance(c, nodes.ClassDef)]
except astroid.InferenceError: return
for exception in exceptions:
    if self._checker._is_overgeneral_exception(exception):
        add_message("broad-exception-raised", args=exception.name,
                    node=self._node, confidence=INFERENCE)   # node = the Raise
```
`visit_call` (:229-237) forwards `visit_name(node.func)` when func is a Name
— so `raise Exception("x")` triggers W0719 via the call path.
`_annotated_unpack_infer` (exceptions.py:36-54): List/Tuple → safe_infer per
element (skipping falsy/Uninferable); else `stmt.infer(context)` yielding
each non-Uninferable result.
args = exception.name (bare name, `%s` after "exception: ").

### W0715 raising-format-tuple — visit_call (:232-237)
```python
match node.args:
    case [nodes.Const(value=str() as msg), _, *_]:        # ≥2 positional args,
        if "%" in msg or ("{" in msg and "}" in msg):     # first a str literal
            add_message("raising-format-tuple", node=self._node, confidence=HIGH)
```
node = the Raise statement.

### W0707 raise-missing-from — `_check_raise_missing_from` (:371-415)
```python
containing_except_node = utils.find_except_wrapper_node_in_scope(node)
    # utils.py:1011-1025: nearest ExceptHandler ancestor, stopping (None) at
    # any LocalsDictNodeNG (function/class boundary)
if not containing_except_node: return
if containing_except_node.name is None:           # except without `as exc`
    class_of_old_error = "Exception"
    if isinstance(containing_except_node.type, (nodes.Name, nodes.Tuple)):
        class_of_old_error = containing_except_node.type.as_string()
    add_message("raise-missing-from", node=node,
        args=(f"'except {class_of_old_error} as exc' and ", node.as_string(), "exc"),
        confidence=HIGH)
elif (isinstance(node.exc, nodes.Call) and isinstance(node.exc.func, nodes.Name)) \
     or (isinstance(node.exc, nodes.Name)
         and node.exc.name != containing_except_node.name.name):
    add_message("raise-missing-from", node=node,
        args=("", node.as_string(), containing_except_node.name.name),
        confidence=HIGH)
```
Template: `Consider explicitly re-raising using %s'%s from %s'` →
e.g. `Consider explicitly re-raising using 'except ValueError as exc' and
'raise MyError('x') from exc'` (first variant; note the embedded quotes come
from the template's literal `'`s around `%s from %s`), or
`Consider explicitly re-raising using 'raise MyError('x') from exc'`.
- bare `except:` (type None) → class_of_old_error stays "Exception".
- `raise mod.Err(...)` (Call func Attribute) in a named handler → NOT
  flagged (only Name funcs). `raise exc` of the caught name → not flagged.

## 4.2 visit_try / visit_trystar (exceptions.py:563-651) — W0702/W0705/W0706/W0711/W0718 (+E0701/E0712)

`visit_trystar` (decorated only_required("bare-except",
"broad-exception-caught", "try-except-raise", "binary-op-exception",
"bad-except-order", "catching-non-exception", "duplicate-except")) simply
calls `visit_try`. `visit_try` itself is UNDECORATED → always runs for Try.

```python
self._check_try_except_raise(node)                  # W0706
exceptions_classes = []
nb_handlers = len(node.handlers)
for index, handler in enumerate(node.handlers):
    if handler.type is None:
        if not _is_raising(handler.body):           # any direct Raise child stmt
            self.add_message("bare-except", node=handler, confidence=HIGH)  # W0702
        if index < (nb_handlers - 1):               # E0701 "empty except..." (08)
            ...
    elif isinstance(handler.type, nodes.BoolOp):
        self.add_message("binary-op-exception", node=handler,
                         args=handler.type.op, confidence=HIGH)             # W0711
    else:
        try: exceptions = list(_annotated_unpack_infer(handler.type))
        except astroid.InferenceError: continue
        for part, exception in exceptions:
            if isinstance(exception, astroid.Instance) and \
               utils.inherit_from_std_ex(exception):
                exception = exception._proxied
            self._check_catching_non_exception(handler, exception, part)    # E0712
            if not isinstance(exception, nodes.ClassDef): continue
            exc_ancestors = [a for a in exception.ancestors()
                             if isinstance(a, nodes.ClassDef)]
            for previous_exc in exceptions_classes:
                if previous_exc in exc_ancestors:                           # E0701
                    ...
            if self._is_overgeneral_exception(exception) and \
               not _is_raising(handler.body):
                self.add_message("broad-exception-caught", args=exception.name,
                                 node=handler.type, confidence=INFERENCE)   # W0718
            if exception in exceptions_classes:
                self.add_message("duplicate-except", args=exception.name,
                                 node=handler.type, confidence=INFERENCE)   # W0705
        exceptions_classes += [exc for _, exc in exceptions]
```
- W0702 node = the ExceptHandler (position: the `except` keyword line/col).
  Suppressed when the handler body directly contains a Raise statement
  (`_is_raising`, exceptions.py:57-59 — top-level statements only).
- W0711 args = `handler.type.op` ("or"/"and").
- W0718 node = handler.type (the Name/Tuple element's parent expr — actually
  the WHOLE handler.type expression), args = class name. Same `_is_raising`
  exemption as W0702. Catching `except (Exception, ValueError):` flags
  Exception (per-part loop) with node = the whole Tuple (handler.type).
- W0705: `exception in exceptions_classes` — node identity equality of
  ClassDef across handlers (same inferred class). Membership against ALL
  previously accumulated (including same-handler earlier parts — added only
  AFTER the handler loop, so duplicates WITHIN one tuple don't trigger
  W0705; they're caught per `_check_same_line_imports`-like? No — within one
  handler, exceptions_classes isn't updated until after, so `except (E, E):`
  does NOT emit W0705. Bug-for-bug.)
- Instance→_proxied unwrap before all checks (an `except exc_instance`
  pattern).

## 4.3 W0706 try-except-raise — `_check_try_except_raise` (exceptions.py:478-533)

```python
def gather_exceptions_from_handler(handler):
    exceptions = []
    if handler.type:
        exceptions_in_handler = utils.safe_infer(handler.type)
        if isinstance(exceptions_in_handler, nodes.Tuple):
            exceptions = list({exception for exception in exceptions_in_handler.elts
                               if isinstance(exception, (nodes.Name, nodes.Attribute))})
        elif exceptions_in_handler: exceptions = [exceptions_in_handler]
        else: return None
    return exceptions

bare_raise = False
handler_having_bare_raise = None
exceptions_in_bare_handler = []
for handler in node.handlers:
    if bare_raise:
        excs_in_current_handler = gather_exceptions_from_handler(handler)
        if not excs_in_current_handler: break
        if exceptions_in_bare_handler is None: break
        for exc_in_current_handler in excs_in_current_handler:
            inferred_current = utils.safe_infer(exc_in_current_handler)
            if any(utils.is_subclass_of(utils.safe_infer(e), inferred_current)
                   for e in exceptions_in_bare_handler):
                bare_raise = False; break
    if _is_raising([handler.body[0]]):       # FIRST statement is a Raise
        if handler.body[0].exc is None:      # a BARE raise
            bare_raise = True
            handler_having_bare_raise = handler
            exceptions_in_bare_handler = gather_exceptions_from_handler(handler)
else:
    if bare_raise:
        self.add_message("try-except-raise", node=handler_having_bare_raise)
```
- The message fires only via the for-ELSE: the two inner `break`s (handler
  with un-inferable/empty types after a bare-raise handler, or None
  exceptions) SKIP the message entirely. Bug-for-bug.
- Semantics: flag a handler whose first stmt is bare `raise`, unless a LATER
  handler catches a SUPERCLASS of something the bare handler caught (i.e. the
  bare raise is a deliberate re-raise filter).
- `{exception for ...}` set of Name/Attribute NODES → hash by id → the list
  order is id-order; only membership matters downstream (any()), so no
  output dependency.
- node = the ExceptHandler with the bare raise; no args.
- `is_subclass_of` (utils.py:1647-1663): both ClassDef; any ancestor of
  child `is_subtype` of parent (astroid helpers.is_subtype; NonDeducible →
  continue).

## 4.4 W0716 wrong-exception-operation — visit_binop / visit_compare (exceptions.py:535-561)

Both decorated only_required("wrong-exception-operation"). Trigger:
`isinstance(node.parent, nodes.ExceptHandler)` (the BinOp/Compare IS the
handler.type expression).
- visit_binop: both sides safe_infer to Tuple-or-Uninferable → if op == "+"
  return (tuple concat OK); else suggestion = `Did you mean '({left} +
  {right})' instead?`; otherwise suggestion = `Did you mean '({left},
  {right})' instead?` (as_string of operands). args = (suggestion,).
- visit_compare: suggestion = `Did you mean '({left.as_string()},
  {", ".join(o.as_string() for _, o in node.ops)})' instead?`; args =
  (suggestion,).
- node = the BinOp/Compare. Template: `Invalid exception operation. %s`.

================================================================================
# 5. method_args.py — MethodArgsChecker
================================================================================

Option `timeout-methods` default: `("requests.api.delete", "requests.api.get",
"requests.api.head", "requests.api.options", "requests.api.patch",
"requests.api.post", "requests.api.put", "requests.api.request")`
(method_args.py:46-66).
visit_call decorated only_required("missing-timeout",
"positional-only-arguments-expected").

## 5.1 W3101 missing-timeout — `_check_missing_timeout` (method_args.py:75-99)

```python
inferred = utils.safe_infer(node.func)
call_site = arguments.CallSite.from_call(node)     # astroid CallSite
if (inferred and not call_site.has_invalid_keywords()
        and isinstance(inferred, (nodes.FunctionDef, nodes.ClassDef,
                                  bases.UnboundMethod))
        and inferred.qname() in self.linter.config.timeout_methods):
    keyword_arguments = [keyword.arg for keyword in node.keywords]
    keyword_arguments.extend(call_site.keyword_arguments)
    if "timeout" not in keyword_arguments:
        self.add_message("missing-timeout", node=node,
                         args=(node.func.as_string(),), confidence=INFERENCE)
```
- `call_site.keyword_arguments`: keys resolved from `**{...}` dict literals;
  `has_invalid_keywords()`: any `**expr` whose keys aren't all const strings
  → bail.
- args = func source text (`requests.get`), template quotes with `'…'`.

## 5.2 E3102 positional-only-arguments-expected — `_check_positional_only_arguments_expected` (method_args.py:101-125)

```python
inferred_func = utils.safe_infer(node.func)
while isinstance(inferred_func, (astroid.BoundMethod, astroid.UnboundMethod)):
    inferred_func = inferred_func._proxied
if not (isinstance(inferred_func, nodes.FunctionDef)
        and inferred_func.args.posonlyargs):
    return
if inferred_func.args.kwarg: return        # **kwargs can absorb them
pos_args = [a.name for a in inferred_func.args.posonlyargs]
kws = [k.arg for k in node.keywords if k.arg in pos_args]
if not kws: return
self.add_message("positional-only-arguments-expected", node=node,
    args=(node.func.as_string(), ", ".join(f"'{k}'" for k in kws)),
    confidence=INFERENCE)
```
(`None` kwargs — `**d` — have k.arg None, never in pos_args.)

================================================================================
# 6. R1704 redefined-argument-from-local (RefactoringChecker)
================================================================================

Owned by refactoring_checker.py (msg def :292-299). WarningScope.LINE
(`node_scope: false` in msgs.rs — block pragmas expand by line, not by node
block). `self._dummy_rgx = config.dummy_variables_rgx` (cached_property in
that checker).

`_check_redefined_argument_from_local(name_node)` (refactoring_checker.py:733-752):
```python
if self._dummy_rgx and self._dummy_rgx.match(name_node.name): return
if not name_node.lineno: return
scope = name_node.scope()
if not isinstance(scope, nodes.FunctionDef): return
for defined_argument in scope.args.nodes_of_class(nodes.AssignName,
                                                  skip_klass=(nodes.Lambda,)):
    if defined_argument.name == name_node.name:
        self.add_message("redefined-argument-from-local", node=name_node,
                         args=(name_node.name,))
```
Call sites:
- visit_for (:760-766): every AssignName in `node.target` subtree.
- visit_excepthandler (:768-771): `node.name` when AssignName.
- visit_with (:773-789): for each `(var, names)` in node.items, every
  AssignName under `names`.
Only these three binding forms — plain assignments don't trigger it.
node = the AssignName; args 1-tuple → `Redefining argument with the local
name 'x'`.

================================================================================
# 7. Ordering, dedup and cross-cutting notes
================================================================================

1. **Within-module callback order**: all the visit_*/leave_* callbacks above
   run in the prepared-checker walk order already extracted empirically
   (notes/02). Message emission order within one node's visit is the source
   order shown in each section (e.g. visit_import: W0404/W0416 → C0414/R0402
   → C0415 → C0410 → per-name W4901 → W0407 → E0401 → C0413 → ...).
2. **leave_module order (variables)**: metaclass pops → __all__ deletions →
   _check_globals → init-gate → _check_imports. W0611 messages come out in
   `_fix_dot_imports` fromlineno-sorted order, W0614 after all W0611 in
   wildcard-dict insertion order.
3. **leave_module order (imports)**: C0411 messages stream during
   `_check_imports_order` in imports-stack order; C0412 afterwards in
   std+external+local concatenated order.
4. **close() messages (R0401)**: after ALL files; attributed to the last
   module (current_name/current_file), line 1 col 0; cycle list order =
   DFS over insertion-ordered vertices with set-hash-ordered neighbor
   iteration (PYTHONHASHSEED=0); checkers close in reversed registration
   order (pylinter.py:995).
5. **Hash-order dependencies**: R0401 neighbor sets (str hash); W4901
   deprecated_modules set iteration (str hash — affects only multi-match
   duplicates); W0213 bit_flags values (int sets — ascending for small
   non-negative ints).
6. **lru_cache'd helpers** (`unimplemented_abstract_methods`,
   `class_is_abstract`, `overridden_method`, `in_for_else_branch`,
   `is_overload_stub`): caches persist across modules keyed by node
   identity — pure functions of the tree, safe to memoize in Rust the same
   way.
7. **Stateful checker fields surviving across modules**:
   variables: `_type_annotation_names` leaks after `__init__.py` (§1.3.1);
   `_except_handler_names_queue` balanced; `_reported_type_checking_usage_scopes`
   never cleared (keyed by name → scope nodes; cross-module false-suppression
   theoretically possible if names collide — keys are plain name strings but
   values are scope nodes compared with `node.scope() in list` → cross-module
   scope objects never match; only memory growth).
   classes: `_accessed` map never cleared (keyed by ClassDef — no bleed);
   `_first_attrs` balanced.
   imports: `_imports_stack`/`_first_non_import_node` reset in leave_module;
   `import_graph`/`_excluded_edges`/`_module_pkg`/`stats.dependencies`
   accumulate by design (R0401, RP reports).
8. **only_required_for_messages semantics**: the ASTWalker registers a
   callback only if `any(is_message_enabled(symbol))` over the decorator
   list at prepare time... — actually pylint checks at walk-registration via
   `get_message_definition`-independent `linter.is_message_enabled`; unknown
   symbols (the `unbalanced_dict_unpacking` typo, §1.8) default to True.
   Config-level only (no line) — pragmas don't unregister callbacks; they
   suppress at add_message time.
9. **Score/exit**: every message here contributes its category bit when
   displayed: W=4, R=8, C=16 (MSG_TYPES_STATUS); they also enter the score
   formula via stats (notes/02 owns the footer spec).
10. **`%` formatting pitfalls**: W0213 uses dict args (named); W0244 renders
    a list via `%r`; W0612-from-_check_globals and W0642/R1704 pass 1-tuples;
    W0611/W0614/C0410..C0415 pass pre-joined strings. A literal `%` inside
    interpolated content (e.g. C0413's as_string of an import with a `%`?
    impossible in import syntax) is safe everywhere except W0716 suggestions
    containing `%` from as_string (e.g. `except (A % B):` → suggestion has
    `%` but interpolation already happened by then — `msg % args` happens
    once with args present, so `%` inside ARGS is safe; only templates carry
    format specs).

================================================================================
# 8. Porting checklist / risk register
================================================================================

- [ ] isort `place_module` replication (C0411/C0412) — pin the isort version
      from .venv-pylint; need FUTURE/STDLIB/THIRDPARTY/FIRSTPARTY/LOCALFOLDER
      classification incl. `extra_standard_library` and `known_third_party`
      ("enchant") and "."-prefix → LOCALFOLDER. isort may classify FIRSTPARTY
      via filesystem src-path probing (Config(directory=cwd)) — verify
      empirically which corpora modules classify FIRSTPARTY vs THIRDPARTY.
- [ ] astroid `modutils.get_module_part` / `is_stdlib_module` for the import
      graph (R0401/W0406) — needs the module-resolution layer of pyinfer.
- [ ] `Module.relative_to_absolute_name` for W4901-on-relative-imports and
      get_import_name.
- [ ] CPython set-iteration order under PYTHONHASHSEED=0 for `_get_cycles`
      neighbor order (R0401 output order). Alternative: implement CPython
      str-hash (siphash13 with fixed seed) + set table semantics — already
      needed elsewhere? confirm with harness diffs on a cyclic corpus.
- [ ] astroid `CallSite.from_call` keyword resolution (W3101).
- [ ] `function_to_method` / `instantiate_class` wrappers for W0221 family
      (positional dropping uses `method.type` + `is_bound()` of the WRAPPED
      method — UnboundMethod proxies `.args`).
- [ ] `FunctionDef.is_abstract` both variants (pass_is_abstract True for
      W0612-skip, False for W0223).
- [ ] `ClassDef.is_subtype_of(qname)` (enum checks, method-hidden),
      `ClassDef.slots()` with its caching quirk (notes/08 E0237).
- [ ] `extract_node` for visit_const string annotations (W0611 suppression)
      — needs a mini parse of annotation strings; errors swallowed.
- [ ] `astroid.builtin_lookup`, `Module.scope_attrs`,
      `Module.special_attributes` name lists from astroid object model.
