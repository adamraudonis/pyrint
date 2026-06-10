# 05 — `pylint/checkers/variables.py`: VariablesChecker

**Spec for bug-for-bug Rust port.**

Sources (pinned):
- pylint 4.0.5 — `/Users/adamraudonis/Desktop/Projects/prylint/reference/pylint/pylint/checkers/variables.py` (3534 lines). All `variables.py:NNN` citations below refer to this file.
- pylint 4.0.5 — `pylint/checkers/utils.py` (cited as `utils.py:NNN`).
- pylint 4.0.5 — `pylint/checkers/base/basic_error_checker.py` (E0118 only).
- astroid 4.0.4 — `astroid/...` paths as cited.
- Ground truth runtime: CPython 3.12.12.

In-scope messages for this document:

| id | symbol | template | args | report node |
|----|--------|----------|------|-------------|
| E0601 | used-before-assignment | `"Using variable %r before assignment"` | `node.name` (single str) | the `Name`/`AssignName`/`DelName` node being checked |
| E0602 | undefined-variable | `"Undefined variable %r"` | `node.name` (single str); in `_check_classdef_metaclasses` it is the 1-tuple `(name,)` | the `Name` node; for metaclass case the `ClassDef` node |
| E0603 | undefined-all-variable | `"Undefined variable name %r in __all__"` | `(elt_name,)` 1-tuple of str | the element node `elt` inside the `__all__` literal |
| E0604 | invalid-all-object | `"Invalid object %r in __all__, must contain only strings"` | `elt.as_string()` (single str) | the element node `elt` |
| E0605 | invalid-all-format | `"Invalid format for __all__, must be tuple or list"` | none | `node=module`, but explicit `line=assigned.tolineno`, `col_offset=assigned.col_offset` |
| E0606 | possibly-used-before-assignment | `"Possibly using variable %r before assignment"` | `node.name` (single str) | the `Name` node |
| E0118 | used-prior-global-declaration | `"Name %r is used prior to global declaration"` | `(name,)` 1-tuple | the `Name` node — **lives in `basic_error_checker.py`, NOT here**; see §16 |

E0611 (no-name-in-module) is explicitly out of scope per task and is skipped here (it is emitted by `visit_import` / `visit_importfrom` / `_check_module_attrs`, variables.py:2096–2144, 3179–3218).

Message templates are defined in `MSGS`, variables.py:351–501 (E0601 at 352, E0602 at 361, E0603 at 366, E0604 at 371, E0605 at 376, E0606 at 381).

Message position resolution (pylint/lint/pylinter.py:1195–1230, `_add_one_message`): when a `node` is passed and the node has no `position` attribute set (only FunctionDef/ClassDef get `position`; Name nodes never do):
```
line         = line passed or node.fromlineno
col_offset   = col_offset passed or node.col_offset
end_lineno   = node.end_lineno
end_col_offset = node.end_col_offset
```
Caveats: the final tuple uses `line or 1` and `col_offset or 0` (pylinter.py:1277–1278), so a 0 col_offset stays 0, and an explicitly-passed col_offset of 0 falls back to `node.col_offset` because of the `if not col_offset:` truthiness test (pylinter.py:1225) — for Name nodes this is irrelevant in practice since the explicit values are only passed for E0605.

Confidence values used (these do not affect output with default `--confidence=` empty, but document for fidelity): HIGH, INFERENCE, CONTROL_FLOW, and the default `UNDEFINED` when no confidence kwarg is passed.

---

## 1. Checker lifecycle and configuration

`VariablesChecker(BaseChecker)`, `name = "variables"` (variables.py:1224–1237).

Relevant options (variables.py:1238–1324):
- `additional-builtins` — csv, **default `()`** (variables.py:1259–1269). Used by `_is_builtin` (variables.py:2456–2457) and the final E0602 fallback (variables.py:1749) and metaclass check (variables.py:3451).
- All other options (`dummy-variables-rgx`, `callbacks`, `ignored-argument-names`, `allow-global-unused-variables`, `allowed-redefined-builtins`, `redefining-builtins-modules`, `init-import`) only affect W-category messages (unused-*) and are out of scope, except `init-import`/`node.package` which gates whether `_check_imports` runs in `leave_module` (no E messages there).

State (variables.py:1326–1337):
```python
self._to_consume: list[NamesConsumer] = []
self._type_annotation_names: list[str] = []          # only affects unused-import
self._except_handler_names_queue = []                # only affects redefined-outer-name (W0621)
self._reported_type_checking_usage_scopes: dict[str, list[LocalsDictNodeNG]] = {}
self._postponed_evaluation_enabled = False
```

`open()` (variables.py:1339–1341):
```python
py_version = self.linter.config.py_version
self._py314_plus = py_version >= (3, 14)
```
`py_version` defaults to the running interpreter → `(3, 12)` → `_py314_plus = False` for the ground-truth runtime. (If a user sets `--py-version=3.14`, postponed evaluation is unconditionally on; for the port assume 3.12 → False.)

`_reported_type_checking_usage_scopes` persists **across modules** (it's instance state, only created in `__init__`). The `_to_consume` stack is recreated per module in `visit_module` and `del`eted at the very end of `_check_imports` (variables.py:3386).

### Walker behaviour

pylint's ASTWalker performs a pre-order traversal: `visit_X(node)` → recurse into children (in `_astroid_fields` order) → `leave_X(node)`. `visit_name` carries an explicit warning (variables.py:1675–1679): it must run for **every** Name node — it is NOT gated by `only_required_for_messages`, because consumption bookkeeping must always happen.

`leave_module` IS decorated with `only_required_for_messages("unused-import", "unused-wildcard-import", "redefined-builtin", "undefined-all-variable", "invalid-all-object", "invalid-all-format", "unused-variable", "undefined-variable")` (variables.py:1416–1425). Under the target invocation E0602/E0603/E0604/E0605 are enabled, so `leave_module` always runs.

---

## 2. Scope-consumer stack: which visits push/pop `NamesConsumer`

| AST node | visit | scope_type | leave behaviour |
|----------|-------|-----------|-----------------|
| Module | `visit_module` 1401 | `"module"` | `leave_module` 1426: pop, run `_check_metaclasses`, `_check_all`, `_check_globals`, `_check_imports` |
| ClassDef | `visit_classdef` 1446 | `"class"` | `leave_classdef` 1450: hidden-ancestor-name consumption, pop |
| Lambda | `visit_lambda` 1463 | `"lambda"` | pop only |
| GeneratorExp | `visit_generatorexp` 1472 | `"comprehension"` | pop only |
| DictComp | `visit_dictcomp` 1481 | `"comprehension"` | pop only |
| SetComp | `visit_setcomp` 1490 | `"comprehension"` | pop only |
| ListComp | `visit_listcomp` 2173 | `"comprehension"` | pop only |
| FunctionDef / AsyncFunctionDef | `visit_functiondef` 1499 (`visit_asyncfunctiondef = visit_functiondef`, 1592) | `"function"` | `leave_functiondef` 1546: `_check_metaclasses`, pop, unused-variable checks (W only) |

`visit_module` (variables.py:1401–1414):
```python
self._to_consume = [NamesConsumer(node, "module")]
self._postponed_evaluation_enabled = (
    self._py314_plus or is_postponed_evaluation_enabled(node)
)
# then a loop emitting only W0622 redefined-builtin — out of scope
```
`is_postponed_evaluation_enabled(node)` (utils.py:1608–1611) = `"annotations" in node.root().future_imports`. astroid populates `future_imports` from `from __future__ import ...` statements at post-build (astroid/builder.py:166–169).

`leave_module` (variables.py:1426–1444):
```python
assert len(self._to_consume) == 1
self._check_metaclasses(node)                  # can emit E0602, see §15
not_consumed = self._to_consume.pop().to_consume
if "__all__" in node.locals:
    self._check_all(node, not_consumed)        # E0603/E0604/E0605, see §14
self._check_globals(not_consumed)              # W0612 only
if not self.linter.config.init_import and node.package:
    return
self._check_imports(not_consumed)              # W0611/W0614 only; ends with `del self._to_consume`
self._type_annotation_names = []
```
Note `_check_all` receives the **post-consumption** leftover dict `to_consume` and mutates it (`del not_consumed[elt_name]`), which feeds into the W-checks afterwards.

`leave_classdef` (variables.py:1450–1461) — runs for every class; consumes names used as `name.attr(...)` calls anywhere inside the class body (e.g. `six.with_metaclass`):
```python
for name_node in node.nodes_of_class(nodes.Name):
    match name_node.parent:
        case nodes.Call(func=nodes.Attribute(expr=nodes.Name(name=name))):
            for consumer in self._to_consume:          # OUTER → INNER (index 0 = module)
                if name in consumer.to_consume:
                    consumer.mark_as_consumed(name, consumer.to_consume[name])
                    break
self._to_consume.pop()
```
Iteration order pitfalls: `nodes_of_class` is pre-order source order; the consumer loop scans from the **module consumer first** (list order), unlike all other lookups which go inner→outer. This affects only consumption bookkeeping (W messages), not E messages directly, but it can mark names consumed which suppresses later "already consumed → RETURN" fast path differences. Match detail: the matched Name is `name_node.parent.func.expr`, i.e. the pattern fires when *any* Name's parent is a `Call` whose func is `Attribute(expr=Name)` — the bound `name` is that of the *attribute base*, not necessarily `name_node` itself (when `name_node` is an argument of such a call, the base name still gets consumed — repeatedly, once per Name child of the Call).

---

## 3. `NamesConsumer` (variables.py:504–1220)

```python
def __init__(self, node, scope_type):
    self.node = node
    self.scope_type = scope_type
    self.to_consume = copy.copy(node.locals)          # SHALLOW dict copy; the list values are SHARED with node.locals
    self.consumed = {}
    self.consumed_uncertain = defaultdict(list)
    self.names_under_always_false_test: set[str] = set()
    self.names_defined_under_one_branch_only: set[str] = set()
```
(variables.py:522–531)

**Copy semantics**: `to_consume` shares list objects with `node.locals`. Nothing ever mutates those lists in place — `mark_as_consumed` builds new lists and `del`etes keys — so `node.locals` stays intact. `consumed_uncertain` is a `defaultdict(list)`; it gets read with `[...]` in several places, which inserts empty entries (harmless but means `name in consumed_uncertain` may be True with an empty list — relevant for the CONTROL_FLOW confidence test in `_report_unfound_name_definition`, see §10: an empty list still triggers `elif node.name in current_consumer.consumed_uncertain: confidence = CONTROL_FLOW` **only if** the key was previously created; `get_next_to_consume` uses `+=` on `self.consumed_uncertain[node.name]` which *always* creates the key for the checked name once any of the four uncertainty filters run with non-None found_nodes... no — `defaultdict.__getitem__` creates the key even when the added list is empty. So after `get_next_to_consume` runs filters for name N, key N exists with possibly empty list. `_report_unfound_name_definition` checks `node.name in current_consumer.consumed_uncertain` — key existence, NOT emptiness. Port must replicate: key is created whenever the corresponding filter block executed, i.e. whenever `found_nodes` was non-empty at that filter stage.)

`mark_as_consumed` (variables.py:547–559):
```python
def mark_as_consumed(self, name, consumed_nodes):
    unconsumed = [n for n in self.to_consume[name] if n not in set(consumed_nodes)]
    self.consumed[name] = consumed_nodes
    if unconsumed:
        self.to_consume[name] = unconsumed
    else:
        del self.to_consume[name]
```
Note `self.consumed[name] = consumed_nodes` *replaces* any previous consumed entry. `self.to_consume[name]` raises KeyError if absent — callers guarantee presence (see §6).

### 3.1 What is in `node.locals` (astroid construction)

Locals dicts are built during the astroid AST rebuild in pre-order:
- `AssignName` and `DelName` nodes are registered via `rebuilder._save_assignment` (astroid/rebuilder.py:494–501):
  ```python
  if self._global_names and node.name in self._global_names[-1]:
      node.root().set_local(node.name, node)      # `global x` declared → lands in MODULE locals
  else:
      node.parent.set_local(node.name, node)      # walks up to nearest scope
  ```
  **Consequence**: inside `def f(): global x; x = 1`, the `x` AssignName is in **module** locals and NOT in `f.locals`. (`_global_names` is a stack of per-function dicts of declared global names, pushed in `_visit_functiondef`.)
- `FunctionDef`/`ClassDef` register **themselves** under their name (`add_local_node`).
- `Import` registers the import node under the first dotted component or asname; `ImportFrom` names are added **after the whole module is built** in `builder._post_build` → `add_from_names_to_locals` (astroid/builder.py:159–246):
  ```python
  for name, asname in node.names:
      if name == "*":
          try:
              imported = node.do_import_module()
          except AstroidBuildingError:
              continue                       # failed wildcard import → NO names added at all
          for name in imported.public_names():
              add_local(...)                 # each name -> the ImportFrom node
      else:
          name = asname or name
          add_local(...)
  ```
  `add_local` appends the ImportFrom node to `scope.locals[name]` and **re-sorts that one list** by `key=lambda n: n.fromlineno or 0` (builder.py:221–226). So locals lists are sorted by line for names that mix from-imports with other definitions; pure non-import locals lists are in build order (which equals source order). Dict *key* insertion order: keys first created at post-build (from-imports of otherwise-unassigned names) come after all body-assigned keys. This only matters for iterations over `locals`/`to_consume` keys (W messages, `_fix_dot_imports` which sorts anyway).
  - `imported.public_names()` = `[name for name in module.locals if not name.startswith("_")]` (astroid scoped_nodes.py:575–581). NOTE: this is NOT `wildcard_import_names` (which honours `__all__`; scoped_nodes.py:525–573) — the builder uses `public_names`.
  - **Failed wildcard import ⇒ undefined names**: there is NO "unresolvable scope" suppression in pylint 4 VariablesChecker. If astroid cannot import the wildcard target, every use of a name expected from it is E0602. If astroid CAN import it, names resolve via locals. (Grep confirms variables.py has no `*`-handling on the E06xx paths; `*` appears only in `_fix_dot_imports`/`_check_imports` for W0614.)
- `TypeVar`/`ParamSpec`/`TypeVarTuple` (PEP 695, astroid/rebuilder.py:1478–1717) create an `AssignName` child (`.name`) whose parent is the TypeVar-like node; `_save_assignment` registers it in the **owning scope** of the `type_params` list — i.e. the `ClassDef`/`FunctionDef`/`TypeAlias` node itself. So `class C[T]:` has `"T" in C.locals`, value `[AssignName(T)]` with `assign_parent` → the `TypeVar` node.

---

## 4. Entry points: `visit_name` / `visit_assignname` / `visit_delname`

variables.py:1667–1687:
```python
def visit_assignname(self, node: nodes.AssignName) -> None:
    if isinstance(node.assign_type(), nodes.AugAssign):
        self.visit_name(node)

def visit_delname(self, node: nodes.DelName) -> None:
    self.visit_name(node)

def visit_name(self, node: nodes.Name | nodes.AssignName | nodes.DelName) -> None:
    stmt = node.statement()
    if stmt.fromlineno is None:
        # name node from an astroid built from live code, skip
        assert not stmt.root().file.endswith(".py")
        return
    self._undefined_and_used_before_checker(node, stmt)
    self._loopvar_name(node)          # W0631 undefined-loop-variable only — IGNORE for this port
```
- AugAssign targets (`x += 1`) are treated as *uses* (load-before-store semantics). `assign_type()` for an `AssignName` resolves via `ParentAssignNode.assign_type()` → `parent.assign_type()` (astroid `_base_nodes.py:122–126`); the parent of the target AssignName in `x += 1` is the `AugAssign` itself (an `AssignTypeNode`, returns self).
- `del x` (DelName) is treated as a use too — `del undefined_name` raises NameError at runtime and pylint reports E0602 for it.
- `_loopvar_name` (variables.py:2625–2771) only ever emits W0631; under `-E` it does nothing observable. Skip in port.
- `_check_late_binding_closure` (variables.py:2952–3000) is guarded by `if not self.linter.is_message_enabled("cell-var-from-loop"): return` (line 2959) — a no-op under the target config. Its call sites inside `_check_consumer` can be treated as no-ops.

---

## 5. `_undefined_and_used_before_checker` (variables.py:1711–1759)

```python
def _undefined_and_used_before_checker(self, node, stmt):
    frame = stmt.scope()
    start_index = len(self._to_consume) - 1
    base_scope_type = self._to_consume[start_index].scope_type   # innermost consumer's type

    for i in range(start_index, -1, -1):          # inner → outer
        current_consumer = self._to_consume[i]

        if self._should_node_be_skipped(node, current_consumer, i == start_index):
            continue

        action, nodes_to_consume = self._check_consumer(
            node, stmt, frame, current_consumer, base_scope_type
        )
        if nodes_to_consume:
            # Any nodes added to consumed_uncertain by get_next_to_consume()
            # should be added back so that they are marked as used.
            nodes_to_consume += current_consumer.consumed_uncertain[node.name]
            current_consumer.mark_as_consumed(node.name, nodes_to_consume)
        if action is VariableVisitConsumerAction.CONTINUE:
            continue
        if action is VariableVisitConsumerAction.RETURN:
            return

    # we have not found the name, if it isn't a builtin, that's an undefined name !
    if not (
        node.name in nodes.Module.scope_attrs
        or utils.is_builtin(node.name)
        or node.name in self.linter.config.additional_builtins
        or (
            node.name == "__class__"
            and any(
                i.is_method()
                for i in node.node_ancestors()
                if isinstance(i, nodes.FunctionDef)
            )
        )
    ) and not utils.node_ignores_exception(node, NameError):
        self.add_message("undefined-variable", args=node.name, node=node)
```

Key facts:
- `frame = stmt.scope()` — the scope of the **statement** containing the node, which differs from `node.frame()` for e.g. names inside function-definition headers (defaults/annotations whose statement is the FunctionDef itself → scope is the enclosing scope... no: `stmt.scope()` for a FunctionDef statement returns the FunctionDef itself since FunctionDef is a scope. The distinction frame-vs-node.frame matters in `_is_variable_violation`; see comment at variables.py:2287–2288).
- `nodes_to_consume += current_consumer.consumed_uncertain[node.name]` mutates the list returned from `_check_consumer` and appends uncertain nodes so they count as consumed (defaultdict access creates the key).
- **Final-fallback E0602** fires only when *every* consumer loop iteration ended in CONTINUE (or was skipped). Conditions to suppress:
  1. `node.name in nodes.Module.scope_attrs` — exactly `{"__name__", "__doc__", "__file__", "__path__", "__package__"}` (astroid scoped_nodes.py:218–224). Note `__spec__`, `__loader__`, `__builtins__`, `__debug__` are NOT in this set but ARE covered by `utils.is_builtin` (below).
  2. `utils.is_builtin(node.name)` — see §13.1 for the exact 157-name list + `__builtins__`.
  3. `node.name in additional_builtins` (default empty).
  4. `node.name == "__class__"` and any ancestor FunctionDef `is_method()` — implicit closure cell in methods.
  5. `utils.node_ignores_exception(node, NameError)` — wrapped in `try:` whose handler catches `NameError` (by literal name match) or in `with contextlib.suppress(NameError)`. Full algorithm §13.6.

  When none of these hold → `add_message("undefined-variable", args=node.name, node=node)` (no confidence → UNDEFINED).

---

## 6. `_should_node_be_skipped` (variables.py:1761–1808)

Decides whether a given consumer level is skipped entirely for this name node:

```python
def _should_node_be_skipped(self, node, consumer, is_start_index):
    if consumer.scope_type == "class":
        # bases of the class are not part of the class body
        if utils.is_ancestor_name(consumer.node, node) or (
            not is_start_index and self._ignore_class_scope(node)
        ):
            if any(node.name == param.name.name for param in consumer.node.type_params):
                return False              # PEP 695 type param: do NOT skip
            return True

        match node.parent:
            case nodes.Keyword(parent=nodes.ClassDef()):
                return True               # e.g. class A(metaclass=M): the M lookup skips class scopes

    elif consumer.scope_type == "function" and self._defined_in_function_definition(
        node, consumer.node
    ):
        if any(node.name == param.name.name for param in consumer.node.type_params):
            return False
        # name used in default/annotation/decorator → skip the function's own scope
        return True

    elif consumer.scope_type == "lambda" and utils.is_default_argument(node, consumer.node):
        return True

    return False
```

Helpers:

`utils.is_ancestor_name(frame, node)` (utils.py:447–453):
```python
if not isinstance(frame, nodes.ClassDef):
    return False
return any(node in base.nodes_of_class(nodes.Name) for base in frame.bases)
```
True when the name appears (at any depth) inside one of the class's `bases` expressions.

`utils.is_default_argument(node, scope=None)` (utils.py:411–427): scope defaults to `node.scope()`; True iff scope is FunctionDef/Lambda and `node` *is* one of the Name nodes inside `scope.args.defaults` or non-None `scope.args.kw_defaults` (identity check over `default_node.nodes_of_class(nodes.Name)`).

`_defined_in_function_definition(node, frame)` (variables.py:2205–2227):
```python
in_annotation_or_default_or_decorator = False
if isinstance(frame, nodes.FunctionDef) and node.statement() is frame:
    in_annotation_or_default_or_decorator = (
        (
            node in frame.args.annotations
            or node in frame.args.posonlyargs_annotations
            or node in frame.args.kwonlyargs_annotations
            or node is frame.args.varargannotation
            or node is frame.args.kwargannotation
        )
        or frame.args.parent_of(node)
        or (frame.decorators and frame.decorators.parent_of(node))
        or (frame.returns and (node is frame.returns or frame.returns.parent_of(node)))
    )
return in_annotation_or_default_or_decorator
```
Note `node.statement() is frame` — only names in the function *header* (the FunctionDef is their statement); also note the truthiness of `frame.decorators and ...` (returns None/False fine).

`_ignore_class_scope(node)` (variables.py:2584–2622) — "should we ignore (skip) this class-scope consumer?" Returns True ⇢ skip:
```python
name = node.name
frame = node.statement().scope()
in_annotation_or_default_or_decorator = self._defined_in_function_definition(node, frame)
in_ancestor_list = utils.is_ancestor_name(frame, node)
if in_annotation_or_default_or_decorator or in_ancestor_list:
    frame_locals = frame.parent.scope().locals
else:
    frame_locals = frame.locals
return not (
    (isinstance(frame, nodes.ClassDef) or in_annotation_or_default_or_decorator)
    and not self._in_lambda_or_comprehension_body(node, frame)
    and name in frame_locals
)
```
i.e. the class consumer is *used* (not skipped) only when: the statement's scope is the class itself (or the name sits in a function header), AND the node is not inside a lambda/comprehension body relative to that frame, AND the name exists in the appropriate locals dict. Docstring examples at variables.py:2588–2606 (class attribute used as default/annotation in method header is "fair game").

`_in_lambda_or_comprehension_body(node, frame)` (variables.py:2229–2257):
```python
child = node
parent = node.parent
while parent is not None:
    if parent is frame:
        return False
    match parent:
        case nodes.Lambda() if child is not parent.args:
            return True       # lambda body has no access to class attrs
        case nodes.Comprehension() if child is not parent.iter:
            return True       # only the iter of a comprehension has access
        case nodes.ComprehensionScope() if not (parent.generators and child is parent.generators[0]):
            return True       # only first generator has access
    child = parent
    parent = parent.parent
return False
```

**This is the implementation of Python's "class scopes are invisible to nested scopes" rule.** When a class-scope consumer is skipped, the loop continues outward; the use of a class attribute from a method body therefore resolves against the function/module scopes, mirroring CPython.

---

## 7. `NamesConsumer.get_next_to_consume` (variables.py:561–654)

Called by `_check_consumer`. Returns:
- `None` → caller CONTINUEs to outer consumer ("special case"),
- `[]` → all candidate definitions were filtered out → used-before-assignment path,
- non-empty list → definitions to evaluate.

Full control flow (quoting structure verbatim-equivalent):

```python
name = node.name
parent_node = node.parent
found_nodes = self.to_consume.get(name)          # None if name not a local of this scope
node_statement = node.statement()

# (a) `x = x` self-definition: if the use is the RHS of the very Assign that first
#     defines the name in this scope, pretend not found here
if (found_nodes
        and isinstance(parent_node, nodes.Assign)
        and parent_node == found_nodes[0].parent):
    lhs = found_nodes[0].parent.targets[0]
    if isinstance(lhs, nodes.AssignName) and lhs.name == name:
        found_nodes = None

# (b) `for x in x:` — the iter use must not resolve to the for target
if (found_nodes
        and isinstance(parent_node, nodes.For)
        and parent_node.iter == node
        and parent_node.target in found_nodes):
    other_definitions = [fn for fn in found_nodes if fn != parent_node.target]
    found_nodes = other_definitions if other_definitions else None

# (c) nonlocal declared in node.frame() → return unfiltered
if _is_nonlocal_name(node, node.frame()):
    return found_nodes

# (d) a ComprehensionScope intervenes between node and its frame → return unfiltered
if VariablesChecker._comprehension_between_frame_and_node(node):
    return found_nodes

# (e) filter: definitions that are the `except X as name:` binding of a handler
#     that does NOT contain `node` are dropped SILENTLY (no consumed_uncertain entry)
if found_nodes:
    found_nodes = [
        n for n in found_nodes
        if not isinstance(n.statement(), nodes.ExceptHandler)
        or n.statement().parent_of(node)
    ]

# (f)..(i) four uncertainty filters; each appends to consumed_uncertain[name]
#          and removes from found_nodes:
if found_nodes:
    uncertain = self._uncertain_nodes_if_tests(found_nodes, node)
    self.consumed_uncertain[node.name] += uncertain; found_nodes = minus(uncertain)
if found_nodes:
    uncertain = self._uncertain_nodes_in_except_blocks(found_nodes, node, node_statement)
    ... same ...
if found_nodes:
    uncertain = self._uncertain_nodes_in_try_blocks_when_evaluating_finally_blocks(
        found_nodes, node_statement, name)
    ... same ...
if found_nodes:
    uncertain = self._uncertain_nodes_in_try_blocks_when_evaluating_except_blocks(
        found_nodes, node_statement)
    ... same ...
return found_nodes
```

Notes:
- Filter (a): only the FIRST definition's parent and only `targets[0]` are examined. `x = x` where `x` was already defined earlier in this scope still hits this branch **only if** `found_nodes[0].parent` is this very Assign — i.e. only when this assignment is the *first* local definition. Then `found_nodes=None` → outer scope lookup → potentially fine (`x` from outer scope) or E0602 at the end.
- Filter (b): for `for x in x:` with no other definitions → `None` (outer lookup), with others → those.
- (c) `_is_nonlocal_name(node, frame)` (variables.py:320–330): frame must be FunctionDef and some `Nonlocal` stmt in `frame.body` (top level only!) lists the name **and is before the node** per `_is_before` (variables.py:308–317: strictly smaller lineno, or same lineno and smaller col_offset).
- (d) `_comprehension_between_frame_and_node(node)` (variables.py:3010–3020): first ancestor of type ComprehensionScope exists and `node.frame().parent_of(that_scope)`. Since astroid makes comprehensions their own frames, `node.frame()` for a node inside a comprehension IS the ComprehensionScope, so `parent_of` is False; this matters for lambdas nested in comprehensions etc. (bug #1731 family).
- (e) is the *exception-binding* filter: `except E as e:` — the `e` AssignName's `statement()` is the ExceptHandler itself (its parent is the handler). Statements *inside* the handler body have their own statement. Dropped nodes from (e) do NOT go to `consumed_uncertain`.

---

## 8. The uncertainty machinery (all `_uncertain_nodes_*` and friends)

### 8.1 `_uncertain_nodes_if_tests(found_nodes, node)` (variables.py:759–809)

Marks definitions guarded by `if` tests as uncertain, unless control flow guarantees definition:

```python
uncertain_nodes = []
for other_node in found_nodes:
    match other_node:
        case nodes.AssignName():               name = other_node.name
        case nodes.Import() | nodes.ImportFrom(): name = node.name
        case nodes.FunctionDef() | nodes.ClassDef(): name = other_node.name
        case _: continue                       # any other def type: never uncertain here

    all_if = [n for n in other_node.node_ancestors()
              if isinstance(n, nodes.If) and not n.parent_of(node)]
    if not all_if:
        continue                               # not under an if (excluding ifs that also contain the use)

    closest_if = all_if[0]
    if isinstance(node, nodes.AssignName) and node.frame() is not closest_if.frame():
        continue                               # AugAssign-target use in another frame: certain
    if closest_if.parent_of(node):
        continue                               # use inside the same closest if: certain

    outer_if = all_if[-1]
    if NamesConsumer._node_guarded_by_same_test(node, outer_if):
        continue                               # use guarded by an equivalent test: certain

    if self._inferred_to_define_name_raise_or_return(name, outer_if):
        continue                               # all paths define/raise/return: certain

    uncertain_nodes.append(other_node)
return uncertain_nodes
```
Order facts: `node_ancestors()` yields nearest-first, so `all_if[0]` is the innermost If not containing the use; `all_if[-1]` the outermost.

### 8.2 `_node_guarded_by_same_test(node, other_if)` (variables.py:811–845) — verbatim

```python
if isinstance(other_if.test, nodes.NamedExpr):
    other_if_test = other_if.test.target
else:
    other_if_test = other_if.test
other_if_test_as_string = other_if_test.as_string()
other_if_test_all_inferred = utils.infer_all(other_if_test)
for ancestor in node.node_ancestors():
    if not isinstance(ancestor, (nodes.If, nodes.IfExp)):
        continue
    if ancestor.test.as_string() == other_if_test_as_string:
        return True
    if isinstance(ancestor.test, nodes.Name):
        continue
    all_inferred = utils.infer_all(ancestor.test)
    if len(all_inferred) == len(other_if_test_all_inferred):
        if any(not isinstance(test, nodes.Const)
               for test in (*all_inferred, *other_if_test_all_inferred)):
            continue
        if {test.value for test in all_inferred} != {
            test.value for test in other_if_test_all_inferred
        }:
            continue
        return True
return False
```
- Textual equality of `as_string()` is the primary test (e.g. both guarded by `if TYPE_CHECKING:` or `if sys.platform == "win32":`).
- The inferred-constant comparison uses **set** equality of `Const.value`s — order-independent; values must be hashable (Const values always are).
- `utils.infer_all` (utils.py:1413–1422): `list(node.infer())`, `[]` on InferenceError; `@lru_cache(maxsize=512)` keyed on (node, context) identity.

### 8.3 `_inferred_to_define_name_raise_or_return(name, node)` (variables.py:656–700) — verbatim logic

```python
match node:
    case nodes.Try():
        # Allow either a path through try/else/finally OR a path through ALL except handlers
        try_except_node = node
        if node.finalbody:
            try_except_node = next((child for child in node.nodes_of_class(nodes.Try)), None)
        handlers = try_except_node.handlers if try_except_node else []
        return NamesConsumer._defines_name_raises_or_returns_recursive(name, node) or all(
            NamesConsumer._defines_name_raises_or_returns_recursive(name, handler)
            for handler in handlers
        )
    case nodes.With() | nodes.For() | nodes.While():
        return NamesConsumer._defines_name_raises_or_returns_recursive(name, node)
    case nodes.Match():
        return all(
            NamesConsumer._defines_name_raises_or_returns_recursive(name, case)
            for case in node.cases
        )
    case nodes.If():
        return self._inferred_to_define_name_raise_or_return_for_if_node(name, node)
    case _:
        raise AssertionError
```
Subtlety: for a Try with a finalbody, `node.nodes_of_class(nodes.Try)` — pre-order including `node` itself — `next(...)` returns `node` itself (it is the first Try in its own subtree). So `try_except_node` is effectively `node` again; the `if node.finalbody` dance only matters in astroid versions where try/finally wraps an inner try/except — in astroid 4 a single `Try` holds handlers+finalbody, so `handlers = node.handlers`. Port note: replicate the literal behaviour (`next` over pre-order self-inclusive iteration → self).

For Match: `all(...)` over cases — `all([])` is True for a Match with no cases (impossible syntactically). **No check for a catch-all `case _:`** here (compare `_defines_name_raises_or_returns_recursive` which has the same property) — so a Match whose every case binds the name counts as defining even without wildcard case. This is a known false-negative-tolerant choice; replicate.

### 8.4 `_inferred_to_define_name_raise_or_return_for_if_node(name, node)` (variables.py:702–737) — verbatim

```python
# Be permissive if there is a break or a continue
if any(node.nodes_of_class(nodes.Break, nodes.Continue)):
    return True

# Is there an assignment in this node itself, e.g. in named expression?
if NamesConsumer._defines_name_raises_or_returns(name, node):
    return True

test = node.test.value if isinstance(node.test, nodes.NamedExpr) else node.test
all_inferred = utils.infer_all(test)
only_search_if = False
only_search_else = True

for inferred in all_inferred:
    if not isinstance(inferred, nodes.Const):
        only_search_else = False
        continue
    val = inferred.value
    only_search_if = only_search_if or (val != NotImplemented and val)
    only_search_else = only_search_else and not val

# Only search else branch when test condition is inferred to be false
if all_inferred and only_search_else:
    self.names_under_always_false_test.add(name)
    return self._branch_handles_name(name, node.orelse)
# Search both if and else branches
if_branch_handles = self._branch_handles_name(name, node.body)
else_branch_handles = self._branch_handles_name(name, node.orelse)
if if_branch_handles ^ else_branch_handles:
    self.names_defined_under_one_branch_only.add(name)
elif name in self.names_defined_under_one_branch_only:
    self.names_defined_under_one_branch_only.remove(name)
return if_branch_handles and else_branch_handles
```

**THIS is where E0606 vs E0601 and the INFERENCE confidence are decided** — by populating `names_under_always_false_test` (test statically false, e.g. `if TYPE_CHECKING:` where TYPE_CHECKING infers to `False`) and `names_defined_under_one_branch_only` (exactly one branch handles the name). Note `only_search_if` is computed but **never used** — replicate (dead code). Note the XOR add/remove dance: a later If node for the same name can remove it from the one-branch set.

### 8.5 `_branch_handles_name(name, body)` (variables.py:739–757)

```python
return any(
    NamesConsumer._defines_name_raises_or_returns(name, if_body_stmt)
    or (
        isinstance(if_body_stmt, (nodes.If, nodes.Try, nodes.With, nodes.For, nodes.While, nodes.Match))
        and self._inferred_to_define_name_raise_or_return(name, if_body_stmt)
    )
    for if_body_stmt in body
)
```

### 8.6 `_defines_name_raises_or_returns(name, node)` (variables.py:928–980) — verbatim

```python
if isinstance(node, (nodes.Raise, nodes.Assert, nodes.Return, nodes.Continue)):
    return True
if isinstance(node, nodes.Expr) and isinstance(node.value, nodes.Call):
    if utils.is_terminating_func(node.value):
        return True
    if (isinstance(node.value.func, nodes.Name)
            and node.value.func.name == "assert_never"):
        return True
if (isinstance(node, nodes.AnnAssign) and node.value
        and isinstance(node.target, nodes.AssignName)
        and node.target.name == name):
    return True
if isinstance(node, nodes.Assign):
    for target in node.targets:
        for elt in utils.get_all_elements(target):
            if isinstance(elt, nodes.Starred):
                elt = elt.value
            if isinstance(elt, nodes.AssignName) and elt.name == name:
                return True
if isinstance(node, nodes.If):
    if any(child_named_expr.target.name == name
           for child_named_expr in node.nodes_of_class(nodes.NamedExpr)):
        return True
if isinstance(node, (nodes.Import, nodes.ImportFrom)) and any(
    (node_name[1] and node_name[1] == name)
    or (node_name[0] == name)
    or (node_name[0].startswith(name + "."))
    for node_name in node.names
):
    return True
if isinstance(node, nodes.With) and any(
    isinstance(item[1], nodes.AssignName) and item[1].name == name
    for item in node.items
):
    return True
if isinstance(node, (nodes.ClassDef, nodes.FunctionDef)) and node.name == name:
    return True
if isinstance(node, nodes.ExceptHandler) and node.name and node.name.name == name:
    return True
return False
```
Notes: `Assert` counts as terminating (an assert may raise); `Break` does NOT count here (only in the If-node pre-check); walrus targets only count when the node is an If; `utils.get_all_elements` (utils.py:259–267) recursively flattens Tuple/List targets.

`utils.is_terminating_func(call)` (utils.py:2211–2254): func must be Attribute/Name and parent not Lambda; infer all values of `call.func`; True if any inferred `qname()` ∈ `TERMINATING_FUNCS_QNAMES = {"_sitebuiltins.Quitter", "sys.exit", "posix._exit", "nt._exit", "unittest.case.TestCase.fail"}` (utils.py:240–248); else for FunctionDef inferred (unwrapping BoundMethod→UnboundMethod proxies), if returns annotation is a Name inferring (via safe_infer) to a qname in `TYPING_NORETURN = {"typing.NoReturn", "typing_extensions.NoReturn"}` or `TYPING_NEVER = {"typing.Never", "typing_extensions.Never"}` (pylint/constants.py:110–121); AsyncFunctionDef only counts when the call is awaited. `StopIteration`/`InferenceError` → False.

### 8.7 `_defines_name_raises_or_returns_recursive(name, node)` (variables.py:982–1014) — verbatim

```python
for stmt in node.get_children():
    if NamesConsumer._defines_name_raises_or_returns(name, stmt):
        return True
    match stmt:
        case nodes.If() | nodes.With():
            if any(NamesConsumer._defines_name_raises_or_returns(name, nested_stmt)
                   for nested_stmt in stmt.get_children()):
                return True
        case nodes.Try() if (
            not stmt.finalbody
            and NamesConsumer._defines_name_raises_or_returns_recursive(name, stmt)
        ):
            return True
        case nodes.Match():
            return all(
                NamesConsumer._defines_name_raises_or_returns_recursive(name, case)
                for case in stmt.cases
            )
return False
```
**Beware**: the `Match` case `return`s immediately (even if False) — statements after a Match in the same body are never examined. One-level-deep peeking for If/With (children only, not recursive). Try recursion only when no finalbody. Replicate exactly.

### 8.8 `_uncertain_nodes_in_except_blocks(found_nodes, node, node_statement)` (variables.py:847–926) — verbatim

```python
uncertain_nodes = []
for other_node in found_nodes:
    other_node_statement = other_node.statement()
    closest_except_handler = utils.get_node_first_ancestor_of_type(
        other_node_statement, nodes.ExceptHandler)
    if not closest_except_handler:
        continue
    if closest_except_handler.parent_of(node):
        continue
    closest_try_except: nodes.Try = closest_except_handler.parent
    try_block_returns = any(isinstance(s, nodes.Return) for s in closest_try_except.body)
    else_block_returns = any(isinstance(s, nodes.Return) for s in closest_try_except.orelse)
    else_block_exits = any(
        isinstance(s, nodes.Expr) and isinstance(s.value, nodes.Call)
        and utils.is_terminating_func(s.value)
        for s in closest_try_except.orelse)
    else_block_continues = any(isinstance(s, nodes.Continue) for s in closest_try_except.orelse)
    if (else_block_continues
            and isinstance(node_statement.parent, (nodes.For, nodes.While))
            and closest_try_except.parent.parent_of(node_statement)):
        continue

    if try_block_returns or else_block_returns or else_block_exits:
        if (isinstance(node_statement.parent, nodes.Try)
                and node_statement in node_statement.parent.finalbody
                and closest_try_except.parent.parent_of(node_statement)):
            uncertain_nodes.append(other_node)
        elif (isinstance(node_statement.parent, nodes.Try)
                and node_statement in node_statement.parent.orelse
                and closest_try_except.parent.parent_of(node_statement)):
            uncertain_nodes.append(other_node)
        elif all(
            NamesConsumer._defines_name_raises_or_returns_recursive(node.name, handler)
            for handler in closest_try_except.handlers
        ):
            continue

    if NamesConsumer._check_loop_finishes_via_except(node, closest_try_except):
        continue

    uncertain_nodes.append(other_node)
return uncertain_nodes
```
**Bug-for-bug warning**: in the `try_block_returns or ...` branch, when the first two sub-branches append `other_node`, control then **falls through** to the `_check_loop_finishes_via_except` check and the final `uncertain_nodes.append(other_node)` — meaning the same node can be appended **twice** (the duplicates flow into `consumed_uncertain`; semantics unaffected since only emptiness/membership is used, but list contents differ — replicate to be safe). Only the `elif all(...): continue` path skips.

Doc comment (E0601's description, variables.py:354–359): "Assignments in except blocks are assumed not to have occurred when evaluating statements outside the block, except when the associated try block contains a return statement."

### 8.9 `_check_loop_finishes_via_except(node, other_node_try_except)` (variables.py:1016–1089)

Special-case for https://github.com/pylint-dev/pylint/issues/5683 — the only non-break exit of a loop is the except handler, and the use is in the loop's `else`:

```python
if not other_node_try_except.orelse:
    return False
closest_loop = utils.get_node_first_ancestor_of_type(node, (nodes.For, nodes.While))
if closest_loop is None:
    return False
if not any(else_statement is node or else_statement.parent_of(node)
           for else_statement in closest_loop.orelse):
    return False                      # node not guarded by loop-else
for inner_else_statement in other_node_try_except.orelse:
    if isinstance(inner_else_statement, nodes.Break):
        break_stmt = inner_else_statement
        break
else:
    return False                      # no break in try's else

def _try_in_loop_body(other_node_try_except, loop) -> bool:
    return any(loop_body_statement is other_node_try_except
               or loop_body_statement.parent_of(other_node_try_except)
               for loop_body_statement in loop.body)

if not _try_in_loop_body(other_node_try_except, closest_loop):
    for ancestor in closest_loop.node_ancestors():
        if isinstance(ancestor, (nodes.For, nodes.While)):
            if _try_in_loop_body(other_node_try_except, ancestor):
                break
    else:
        return False                  # no shared ancestor loop

for loop_stmt in closest_loop.body:
    if NamesConsumer._recursive_search_for_continue_before_break(loop_stmt, break_stmt):
        break
else:
    return True                       # no continue found → special case holds
return False
```

`_recursive_search_for_continue_before_break(stmt, break_stmt)` (variables.py:1091–1110) — verbatim:
```python
if stmt is break_stmt:
    return False
if isinstance(stmt, nodes.Continue):
    return True
for child in stmt.get_children():
    if isinstance(stmt, (nodes.For, nodes.While)):
        continue                      # NOTE: checks `stmt`, not `child` — skips ALL children of loops
    if NamesConsumer._recursive_search_for_continue_before_break(child, break_stmt):
        return True
return False
```
The `isinstance(stmt, ...)` (not `child`!) inside the loop means: if `stmt` is a loop, every child is skipped — replicate this literally.

### 8.10 `_uncertain_nodes_in_try_blocks_when_evaluating_except_blocks(found_nodes, node_statement)` (variables.py:1112–1159) — verbatim

```python
uncertain_nodes = []
closest_except_handler = utils.get_node_first_ancestor_of_type(node_statement, nodes.ExceptHandler)
if closest_except_handler is None:
    return uncertain_nodes
for other_node in found_nodes:
    other_node_statement = other_node.statement()
    if other_node_statement is closest_except_handler:
        continue                       # the binding of the very handler guarding node: executes
    (other_node_try_ancestor, other_node_try_ancestor_visited_child) = \
        utils.get_node_first_ancestor_of_type_and_its_child(other_node_statement, nodes.Try)
    if other_node_try_ancestor is None:
        continue
    if other_node_try_ancestor_visited_child not in other_node_try_ancestor.body:
        continue                       # definition not in the try BODY (it's in else/finally/handlers)
    if not any(
        closest_except_handler in other_node_try_ancestor.handlers
        or other_node_try_ancestor_except_handler in closest_except_handler.node_ancestors()
        for other_node_try_ancestor_except_handler in other_node_try_ancestor.handlers
    ):
        continue                       # the except we're in is unrelated to that try
    uncertain_nodes.append(other_node)
return uncertain_nodes
```
"If we are inside `except:` of try T, definitions in T's `try:` body may not have run."

### 8.11 `_uncertain_nodes_in_try_blocks_when_evaluating_finally_blocks(found_nodes, node_statement, name)` (variables.py:1161–1220) — verbatim

```python
uncertain_nodes = []
(closest_try_finally_ancestor, child_of_closest_try_finally_ancestor) = \
    utils.get_node_first_ancestor_of_type_and_its_child(node_statement, nodes.Try)
if closest_try_finally_ancestor is None:
    return uncertain_nodes
if child_of_closest_try_finally_ancestor not in closest_try_finally_ancestor.finalbody:
    return uncertain_nodes             # node not in a finally
for other_node in found_nodes:
    other_node_statement = other_node.statement()
    (other_node_try_finally_ancestor, child_of_other_node_try_finally_ancestor) = \
        utils.get_node_first_ancestor_of_type_and_its_child(other_node_statement, nodes.Try)
    if other_node_try_finally_ancestor is None:
        continue
    if child_of_other_node_try_finally_ancestor not in other_node_try_finally_ancestor.body:
        continue                       # definition must be in a try body
    if (other_node_try_finally_ancestor is not closest_try_finally_ancestor
            and not any(
                other_node_final_statement is closest_try_finally_ancestor
                or other_node_final_statement.parent_of(closest_try_finally_ancestor)
                for other_node_final_statement in other_node_try_finally_ancestor.finalbody)):
        continue
    # Is the name defined in all exception clauses?
    if other_node_try_finally_ancestor.handlers and all(
        NamesConsumer._defines_name_raises_or_returns_recursive(name, handler)
        for handler in other_node_try_finally_ancestor.handlers
    ):
        continue
    uncertain_nodes.append(other_node)
return uncertain_nodes
```
"From a `finally:` block, assignments in the corresponding `try:` body may not have run — unless every handler also defines/raises/returns."

`utils.get_node_first_ancestor_of_type_and_its_child` (utils.py:1973–1987): walks `node_ancestors()` tracking the previous hop, returns `(ancestor, child)` or `(None, None)`.

---

## 9. `_check_consumer` — the core (variables.py:1811–2019)

Returns `(action, nodes_to_consume_or_None)`.

### 9.1 Already-consumed fast path (1820–1829)

```python
if node.name in current_consumer.consumed:
    if utils.is_func_decorator(current_consumer.node) or not isinstance(
        node, nodes.ComprehensionScope
    ):
        self._check_late_binding_closure(node)        # W0640 only; no-op under -E
        return (VariableVisitConsumerAction.RETURN, None)
```
`node` is always a Name/AssignName/DelName, never a ComprehensionScope → `not isinstance(...)` is **always True** → the guard is vestigial (`is_func_decorator` never needs evaluating, although Python will short-circuit only if the first operand is True; evaluation order: `is_func_decorator` IS called first, but result is irrelevant). **Effective behaviour: once consumed in this consumer → RETURN immediately (no message).** Port as unconditional.

### 9.2 Definition lookup (1831–1845)

```python
found_nodes = current_consumer.get_next_to_consume(node)
if found_nodes is None:
    return (CONTINUE, None)
if not found_nodes:
    is_reported = self._report_unfound_name_definition(node, current_consumer)   # E0601/E0606 here
    nodes_to_consume = current_consumer.consumed_uncertain[node.name]
    nodes_to_consume = self._filter_type_checking_definitions_from_consumption(
        node, nodes_to_consume, is_reported)
    return (RETURN, nodes_to_consume)
```
(§10 and §11 below.)

### 9.3 Definition bookkeeping (1847–1851)

```python
self._check_late_binding_closure(node)               # no-op under -E
defnode = utils.assign_parent(found_nodes[0])
defstmt = defnode.statement()
defframe = defstmt.frame()
```
`utils.assign_parent` (utils.py:461–465): climb while node is AssignName/Tuple/List; e.g. for `a, b = ...` returns the Assign; for a TypeVar's AssignName returns the TypeVar node; for `for x in ...` target returns the For; for function args the Arguments node... (an argument AssignName's parent is Arguments → stop: Arguments is not AssignName/Tuple/List, so `defnode = AssignName`? No — climb only while *current* is AssignName/Tuple/List: start AssignName → climb to parent Arguments → Arguments not in set → return Arguments). So `defnode` = first non-(AssignName/Tuple/List) ancestor-or-self.

**Order dependency**: `found_nodes[0]` — first definition in the (line-sorted, see §3.1) locals list that survived filtering.

### 9.4 Recursive class reference in lambda (1853–1886) — verbatim condition

```python
is_recursive_klass: bool = (
    frame is defframe
    and defframe.parent_of(node)
    and isinstance(defframe, nodes.ClassDef)
    and node.name == defframe.name
)

if (
    is_recursive_klass
    and utils.get_node_first_ancestor_of_type(node, nodes.Lambda)
    and not (
        utils.is_default_argument(node)
        and node.scope().parent.scope() is defframe
    )
):
    # Self-referential class references are fine in lambdas
    # unless directly a default arg of a lambda whose parent scope is the class
    return (VariableVisitConsumerAction.RETURN, None)   # do NOT consume
```
Docstring examples at variables.py:1872–1879 (`MyName3` valid, `MyName4` invalid).

### 9.5 `_is_variable_violation` (1888–1901) → §12

```python
(maybe_before_assign, annotation_return, use_outer_definition) = self._is_variable_violation(
    node, defnode, stmt, defstmt, frame, defframe, base_scope_type, is_recursive_klass)

if use_outer_definition:
    return (CONTINUE, None)
```

### 9.6 The E0601 emission block (1906–1988)

```python
if (
    maybe_before_assign
    and not utils.is_defined_before(node)
    and not astroid.are_exclusive(stmt, defstmt, ("NameError",))
):
    # Used and defined in the same place, e.g `x += 1` and `del x`
    defined_by_stmt = defstmt is stmt and isinstance(node, (nodes.DelName, nodes.AssignName))
    if (
        is_recursive_klass
        or defined_by_stmt
        or annotation_return
        or isinstance(defstmt, nodes.Delete)
    ):
        if not utils.node_ignores_exception(node, NameError):
            # Handle postponed evaluation of annotations
            if not (
                self._postponed_evaluation_enabled
                and isinstance(stmt, (nodes.AnnAssign, nodes.FunctionDef, nodes.Arguments))
                and node.name in node.root().locals
            ):
                if defined_by_stmt:
                    return (CONTINUE, [node])
                return (CONTINUE, None)
        # else: fall through to final `return (RETURN, found_nodes)`

    elif base_scope_type != "lambda":
        # E0601 may *not* occur in lambda scope.
        # Skip postponed evaluation of annotations and unevaluated annotations
        # inside a function body as well as TypeAlias nodes.
        if not (
            self._postponed_evaluation_enabled
            and (
                isinstance(stmt, nodes.AnnAssign)
                or isinstance(stmt, nodes.FunctionDef)
                and node not in {*(stmt.args.defaults or ()), *(stmt.args.kw_defaults or ())}
            )
            or isinstance(stmt, nodes.AnnAssign)
            and utils.get_node_first_ancestor_of_type(stmt, nodes.FunctionDef)
            or isinstance(stmt, nodes.TypeAlias)
        ):
            self.add_message("used-before-assignment", args=node.name, node=node, confidence=HIGH)
            return (RETURN, found_nodes)
        # else: fall through to final return

    elif base_scope_type == "lambda":
        # E0601 in class-level scope via lambdas:
        #   class A:
        #      x = lambda attr: f + attr
        #      f = 42
        if (
            isinstance(frame, nodes.ClassDef)
            and node.name in frame.locals
            and stmt.fromlineno <= defstmt.fromlineno
        ):
            self.add_message("used-before-assignment", args=node.name, node=node, confidence=HIGH)
        # falls through to final return (consume found_nodes)
```
Operator precedence in the big `if not (...)` (lines 1945–1959): `and` binds tighter than `or`, so it reads:
```
NOT (   (PEE and (AnnAssign(stmt) or (FunctionDef(stmt) and node not in defaults∪kw_defaults)))
     or (AnnAssign(stmt) and stmt has a FunctionDef ancestor)
     or TypeAlias(stmt) )
```
- With postponed evaluation (`from __future__ import annotations`): any use whose statement is an AnnAssign is exempt; uses in a FunctionDef header are exempt **except** in default values (defaults are evaluated eagerly even under PEP 563).
- Without postponed evaluation: AnnAssign inside a function body is exempt (function-local annotations are never evaluated at runtime); `type X = ...` (TypeAlias) RHS exempt (lazy).
- The guards before the block:
  - `utils.is_defined_before(node)` — §13.2 (definition by enclosing For/With/comprehension/lambda/except-binding or earlier-on-same-line semicolon sibling).
  - `astroid.are_exclusive(stmt, defstmt, ("NameError",))` — §13.9; with the `exceptions` list, If-branch exclusivity is NOT considered, only try/except relations: use-in-handler-catching-NameError vs def-in-body counts as exclusive (and def in else vs use in handlers, distinct handlers, etc.).
- `defined_by_stmt` (`defstmt is stmt` and node is DelName/AssignName): e.g. `x += 1` as the very first binding, or `del x` with x's only binding being that Delete. Returns `(CONTINUE, [node])` — consumes **the name node itself** (so subsequent uses see it consumed) and continues to outer scopes (potentially finding a real outer definition or ending in E0602 at the fallback).
- `isinstance(defstmt, nodes.Delete)`: name's first surviving definition is a `del` → CONTINUE (outer scopes may define it; else final E0602).
- If `node_ignores_exception` or the postponed-evaluation condition holds inside the first branch → **fall through** to the final `return (RETURN, found_nodes)` at 2019: consume without message.

### 9.7 The elif-chain after the E0601 block (1990–2018)

These run only when the §9.6 compound condition was False (i.e. NOT maybe-before-assign, or defined-before, or exclusive):

```python
elif not self._is_builtin(node.name) and self._is_only_type_assignment(node, defstmt):
    if node.scope().locals.get(node.name):
        self.add_message("used-before-assignment", args=node.name, node=node, confidence=HIGH)
    else:
        self.add_message("undefined-variable", args=node.name, node=node, confidence=HIGH)
    return (RETURN, found_nodes)

elif isinstance(defstmt, nodes.ClassDef) and defnode not in defframe.type_params:
    return self._is_first_level_self_reference(node, defstmt, found_nodes)

elif isinstance(defnode, nodes.NamedExpr):
    if isinstance(defnode.parent, nodes.IfExp):
        if self._is_never_evaluated(defnode, defnode.parent):
            self.add_message("undefined-variable", args=node.name, node=node, confidence=INFERENCE)
            return (RETURN, found_nodes)

return (VariableVisitConsumerAction.RETURN, found_nodes)
```

- **annotation-only variables** (`_is_only_type_assignment`, §12.4): `var: int` then `print(var)` → E0601 if the name is in `node.scope().locals` (same-scope annotation) else E0602 (e.g. class-level annotation used in method).
- **First-level self-reference** (`_is_first_level_self_reference`, variables.py:2535–2555) — defstmt is the ClassDef that defines the name (i.e. the name *is* a class used inside its own body at method-header level):
  ```python
  if node.frame().parent == defstmt and node.statement() == node.frame():
      if utils.is_node_in_type_annotation_context(node):
          if not self._postponed_evaluation_enabled:
              return (CONTINUE, None)        # eager annotations: class not yet defined → look outward (may end E0602)
          return (RETURN, None)              # postponed: fine, do not consume
      match node.parent:
          case nodes.Call(parent=nodes.Arguments()):
              return (CONTINUE, None)        # default value `def m(self, x=MyClass())` → outward
  return (RETURN, found_nodes)
  ```
  Conditions: `node.frame().parent == defstmt` — node is in a method (frame) directly inside the class; `node.statement() == node.frame()` — node is in the method *header* (annotation/default).
- **Never-evaluated walrus** (`_is_never_evaluated`, variables.py:2557–2571):
  ```python
  match utils.safe_infer(defnode_parent.test):
      case nodes.Const(value=True) if defnode == defnode_parent.orelse:  return True
      case nodes.Const(value=False) if defnode == defnode_parent.body:   return True
      case _: return False
  ```
  `x if (cond) else (y := 1)` where cond infers True → using `y` later: E0602 with INFERENCE confidence.

---

## 10. `_report_unfound_name_definition` (variables.py:2021–2068) — E0601 vs E0606 decision

Called when `get_next_to_consume` returned `[]` (all definitions filtered as uncertain / except-binding). Returns True iff a message was emitted.

```python
if (
    self._postponed_evaluation_enabled
    and utils.is_node_in_type_annotation_context(node)
) or utils.is_node_in_pep695_type_context(node):
    return False
if self._is_builtin(node.name):
    return False
if self._is_variable_annotation_in_function(node):
    return False
if self._has_nonlocal_in_enclosing_frame(
    node, current_consumer.consumed_uncertain.get(node.name, [])
):
    return False
if (
    node.name in self._reported_type_checking_usage_scopes
    and node.scope() in self._reported_type_checking_usage_scopes[node.name]
):
    return False

confidence = HIGH
if node.name in current_consumer.names_under_always_false_test:
    confidence = INFERENCE
elif node.name in current_consumer.consumed_uncertain:
    confidence = CONTROL_FLOW

if node.name in current_consumer.names_defined_under_one_branch_only:
    msg = "possibly-used-before-assignment"          # E0606
else:
    msg = "used-before-assignment"                   # E0601

self.add_message(msg, args=node.name, node=node, confidence=confidence)
return True
```

Suppression guards, in order:
1. Postponed annotations + node inside an annotation context (`is_node_in_type_annotation_context`, utils.py:1614–1637: climbs parents until Module; True if the current chain node is the `annotation` of an AnnAssign, one of `Arguments.annotations`/`posonlyargs_annotations`/`kwonlyargs_annotations`/`varargannotation`/`kwargannotation`, or `FunctionDef.returns`).
2. PEP 695 context (`is_node_in_pep695_type_context`, utils.py:1640–1644: any ancestor TypeAlias/TypeVar/ParamSpec/TypeVarTuple).
3. Builtins (`_is_builtin`: additional_builtins config OR `utils.is_builtin`).
4. `_is_variable_annotation_in_function` (variables.py:2573–2582): node lies inside the `annotation` of an AnnAssign that itself is inside a FunctionDef:
   ```python
   ann_assign = utils.get_node_first_ancestor_of_type(node, nodes.AnnAssign)
   return (ann_assign
           and (node is ann_assign.annotation or ann_assign.annotation.parent_of(node))
           and utils.get_node_first_ancestor_of_type(ann_assign, nodes.FunctionDef))
   ```
5. `_has_nonlocal_in_enclosing_frame` (variables.py:2459–2476) — verbatim:
   ```python
   defining_frames = {definition.frame() for definition in uncertain_definitions}
   frame = node.frame()
   is_enclosing_frame = False
   while frame and not is_enclosing_frame:
       is_enclosing_frame = all(
           (frame is defining_frame) or frame.parent_of(defining_frame)
           for defining_frame in defining_frames
       )
       if is_enclosing_frame and _is_nonlocal_name(node, frame):
           return True
       frame = frame.parent.frame() if frame.parent else None
   return False
   ```
   (Note: `all([])` is True — with NO uncertain definitions the first frame is "enclosing".)
6. Already reported for this name in this scope via the TYPE_CHECKING tracking dict.

**E0606 fires** iff `node.name ∈ consumer.names_defined_under_one_branch_only` at the time of reporting (populated/cleared by `_inferred_to_define_name_raise_or_return_for_if_node`, §8.4); otherwise E0601. Confidence: INFERENCE if under an always-false test, else CONTROL_FLOW if `consumed_uncertain` has the key (it virtually always does on this path — key created by the `+=` even with empty additions), else HIGH (possible when only the silent except-binding filter (e) emptied the list, since that filter doesn't touch `consumed_uncertain`... but note the key may have been created by an earlier filter stage for a previous sibling check of the same name in this consumer — port must track key existence faithfully).

## 11. `_filter_type_checking_definitions_from_consumption` (variables.py:2070–2094)

```python
type_checking_definitions = {
    n for n in nodes_to_consume
    if isinstance(n, (nodes.Import, nodes.ImportFrom, nodes.ClassDef))
    and in_type_checking_block(n)
}
if type_checking_definitions and is_reported:
    self._reported_type_checking_usage_scopes.setdefault(node.name, []).append(node.scope())
return [n for n in nodes_to_consume if n not in type_checking_definitions]
```
Definitions inside `if TYPE_CHECKING:` blocks are NOT consumed (so later uses in *other* scopes can be re-reported), and a (name → scope) record suppresses duplicate reports within the same scope (guard 6 in §10).

`in_type_checking_block(node)` (utils.py:1990–2017) — verbatim:
```python
for ancestor in node.node_ancestors():
    if not isinstance(ancestor, nodes.If):
        continue
    if isinstance(ancestor.test, nodes.Name):
        if ancestor.test.name != "TYPE_CHECKING":
            continue
        lookup_result = ancestor.test.lookup(ancestor.test.name)[1]
        if not lookup_result:
            return False
        maybe_import_from = lookup_result[0]
        if (isinstance(maybe_import_from, nodes.ImportFrom)
                and maybe_import_from.modname == "typing"):
            return True
        match safe_infer(ancestor.test):
            case nodes.Const(value=False):
                return True
    elif isinstance(ancestor.test, nodes.Attribute):
        if ancestor.test.attrname != "TYPE_CHECKING":
            continue
        match safe_infer(ancestor.test.expr):
            case nodes.Module(name="typing"):
                return True
return False
```
Accepts `if TYPE_CHECKING:` (name imported from typing, or any name literally `TYPE_CHECKING` that infers to `Const(False)`) and `if typing.TYPE_CHECKING:`/`if t.TYPE_CHECKING:` where the expr infers to the typing module. Note the **hard `return False`** when a bare `TYPE_CHECKING` name has no lookup result (does not continue scanning outer ancestors).

**TYPE_CHECKING semantics summary for E06xx**: pylint does NOT treat `if TYPE_CHECKING:` imports as undefined at runtime by default. Instead: (1) `_uncertain_nodes_if_tests` usually treats them as certain when the use is guarded by the same test, or flags them via `names_under_always_false_test` (since `typing.TYPE_CHECKING` infers to `False` in astroid) giving used-before-assignment with INFERENCE confidence for *runtime* uses; (2) consumed bookkeeping excludes them via the function above. A runtime (unguarded, non-annotation) use of a TYPE_CHECKING-only import ⇒ E0601 with INFERENCE confidence (because the definition was filtered by `_uncertain_nodes_if_tests` → always-false test → `names_under_always_false_test`).

---

## 12. `_is_variable_violation` and friends

### 12.1 `_is_variable_violation` (variables.py:2259–2413) — verbatim

```python
maybe_before_assign = True
annotation_return = False
use_outer_definition = False
if frame is not defframe:
    maybe_before_assign = _detect_global_scope(node, frame, defframe)
elif defframe.parent is None:
    # we are at the module level, check the name is not defined in builtins
    if (node.name in defframe.scope_attrs or astroid.builtin_lookup(node.name)[1]):
        maybe_before_assign = False
else:
    # we are in a local scope, check the name is not defined in global or builtin scope
    # skip this lookup if name is assigned later in function scope/lambda
    forbid_lookup = (
        isinstance(frame, nodes.FunctionDef)
        or isinstance(node.frame(), nodes.Lambda)
    ) and _assigned_locally(node)
    if not forbid_lookup and defframe.root().lookup(node.name)[1]:
        maybe_before_assign = False
        use_outer_definition = stmt == defstmt and not isinstance(defnode, nodes.Comprehension)
    elif node.name in defframe.locals:
        maybe_before_assign = not _is_nonlocal_name(node, defframe)

if (base_scope_type == "lambda"
        and isinstance(frame, nodes.ClassDef)
        and node.name in frame.locals):
    # bar = None
    # foo = lambda bar=bar: bar
    maybe_before_assign = not (
        isinstance(defnode, nodes.Arguments)
        and node in defnode.defaults
        and frame.locals[node.name][0].fromlineno < defstmt.fromlineno
    )
elif isinstance(defframe, nodes.ClassDef) and isinstance(frame, nodes.FunctionDef):
    # Special rules for function return annotations.
    if node is frame.returns:
        if defframe.parent_of(frame.returns):
            annotation_return = True
            if frame.returns.name in defframe.locals:
                definition = defframe.locals[node.name][0]
                maybe_before_assign = (
                    definition.lineno is not None
                    and definition.lineno >= frame.lineno
                )
            else:
                maybe_before_assign = True
        elif (
            (defframe_parent := next(defframe.node_ancestors()))
            and isinstance(defframe_parent, nodes.Module)
            and (frame_ancestors := tuple(frame.node_ancestors()))
            and any(isinstance(a, nodes.FunctionDef) for a in frame_ancestors)
            and frame_ancestors[-1] is defframe_parent
        ):
            annotation_return = True
            maybe_before_assign = False
    if isinstance(node.parent, nodes.Arguments):
        maybe_before_assign = stmt.fromlineno <= defstmt.fromlineno
elif is_recursive_klass:
    maybe_before_assign = True
else:
    maybe_before_assign = (maybe_before_assign and stmt.fromlineno <= defstmt.fromlineno)
    if maybe_before_assign and stmt.fromlineno == defstmt.fromlineno:
        if (isinstance(defframe, nodes.FunctionDef)
                and frame is defframe
                and defframe.parent_of(node)
                and (defnode in defframe.type_params
                     or stmt is not defstmt)):       # single-statement function on def line
            maybe_before_assign = False
        elif (isinstance(defstmt, NODES_WITH_VALUE_ATTR)
                and VariablesChecker._maybe_used_and_assigned_at_once(defstmt)
                and frame is defframe
                and defframe.parent_of(node)
                and stmt is defstmt):
            # x = b if (b := True) else False
            maybe_before_assign = False
        elif (isinstance(defnode, nodes.NamedExpr)
                and frame is defframe
                and defframe.parent_of(stmt)
                and stmt is defstmt
                and _is_before(defnode, node)):
            # (b := 2) and b  → safe;  (b := b) → self-referencing
            maybe_before_assign = defnode.value is node or any(
                a is defnode.value for a in node.node_ancestors())
        elif (isinstance(defframe, nodes.ClassDef)
                and defnode in defframe.type_params):
            # class Child[_T](Parent[_T])
            maybe_before_assign = False

return maybe_before_assign, annotation_return, use_outer_definition
```

Key takeaways for the port:

- **The fundamental same-scope line rule** (final else): used-before-assignment is possible only when `stmt.fromlineno <= defstmt.fromlineno` — the use's statement starts at or before the definition's statement. This is what makes "function bodies referencing later module names" fine: in that case `frame is not defframe` and `_detect_global_scope` applies instead. Within one scope, *use line > def line ⇒ never E0601 from this path* (pylint deliberately does not do full dataflow; "defined before CALL" heuristics for nested functions reduce to scope difference).
- `frame is not defframe` → `_detect_global_scope` (§12.2): only class-vs-class/module sharing can keep `maybe_before_assign=True`.
- Module level (`defframe.parent is None`): a module-level name shadowing a builtin or a module scope_attr is never "before assignment" (e.g. `print = print` idiom; `x = __name__`).
- Local scopes: if the name resolves in the module scope (`defframe.root().lookup(node.name)[1]` — full astroid lookup from the module, see §13.10) AND it is not locally assigned in a function/lambda (`_assigned_locally`, variables.py:299–305: any AssignName with that name anywhere under `node.scope()` — `nodes_of_class(AssignName)` ignores scope boundaries of nested functions? No: `nodes_of_class` recurses into nested scopes too! So an assignment in a *nested* function also counts — replicate; plus `_find_frame_imports` (variables.py:255–274): any Import/ImportFrom in the frame binding the name, unless the name is declared `global` in the frame) → not before-assign; `use_outer_definition=True` (→ CONTINUE to outer consumer) iff `stmt == defstmt` and defnode is not a Comprehension.
- Nonlocal: name in defframe.locals & nonlocal declared (before the node) ⇒ not a violation.
- The class-level-lambda-default rule, the method-return-annotation rules (`annotation_return`), and the four same-line walrus/typeparam carve-outs are quoted above; replicate literally.
- `NODES_WITH_VALUE_ATTR = (Assign, AnnAssign, AugAssign, Expr, Return, Match, TypeAlias)` (variables.py:65–73).

### 12.2 `_detect_global_scope(node, frame, defframe)` (variables.py:125–200) — verbatim

```python
def_scope = scope = None
if frame and frame.parent:
    scope = frame.parent.scope()
if defframe and defframe.parent:
    def_scope = defframe.parent.scope()
if (isinstance(frame, nodes.ClassDef)
        and scope is not def_scope
        and scope is utils.get_node_first_ancestor_of_type(node, nodes.FunctionDef)):
    return False        # class nested under a function; defframe elsewhere → not shared
if isinstance(frame, nodes.FunctionDef):
    if frame.parent_of(defframe):
        return node.lineno < defframe.lineno
    if not isinstance(node.parent, (nodes.FunctionDef, nodes.Arguments)):
        return False
break_scopes = []
for current_scope in (scope or frame, def_scope):
    parent_scope = current_scope
    while parent_scope:
        if not isinstance(parent_scope, (nodes.ClassDef, nodes.Module)):
            break_scopes.append(parent_scope)
            break
        if parent_scope.parent:
            parent_scope = parent_scope.parent.scope()
        else:
            break
if len(set(break_scopes)) > 1:
    return False
return frame.lineno < defframe.lineno
```
Purpose: `class B(C)` before `class C` at the same (module/class-chain) global scope → NameError → returns True (=maybe_before_assign). Hidden under a function → False. Then in §9.6 the message is only actually emitted via the `elif base_scope_type != "lambda"` branch.

### 12.3 `_maybe_used_and_assigned_at_once(defstmt)` (variables.py:2415–2454) — verbatim

```python
if isinstance(defstmt, nodes.Match):
    return any(case.guard for case in defstmt.cases)
if isinstance(defstmt, nodes.IfExp):
    return True
if isinstance(defstmt, nodes.TypeAlias):
    return True
if isinstance(defstmt.value, nodes.BaseContainer):
    return any(
        VariablesChecker._maybe_used_and_assigned_at_once(elt)
        for elt in defstmt.value.elts
        if isinstance(elt, (*NODES_WITH_VALUE_ATTR, nodes.IfExp, nodes.Match))
    )
match value := defstmt.value:
    case nodes.IfExp():
        return True
    case nodes.Lambda(body=nodes.IfExp()):
        return True
    case nodes.Dict() if any(
        isinstance(item[0], nodes.IfExp) or isinstance(item[1], nodes.IfExp)
        for item in value.items
    ):
        return True
    case nodes.Call():
        pass
    case _:
        return False
return any(
    any(isinstance(kwarg.value, nodes.IfExp) for kwarg in call.keywords)
    or any(isinstance(arg, nodes.IfExp) for arg in call.args)
    or (isinstance(call.func, nodes.Attribute) and isinstance(call.func.expr, nodes.IfExp))
    for call in value.nodes_of_class(klass=nodes.Call)
)
```

### 12.4 `_is_only_type_assignment(node, defstmt)` (variables.py:2478–2533) — verbatim

```python
if not (isinstance(defstmt, nodes.AnnAssign) and defstmt.value is None):
    return False
defstmt_frame = defstmt.frame()
node_frame = node.frame()
parent = node
while parent is not defstmt_frame.parent and parent is not None:   # actual code: `while parent not in {defstmt_frame.parent, None}:`
    parent_scope = parent.scope()

    # Find out if any nonlocals receive values in nested functions
    for inner_func in parent_scope.nodes_of_class(nodes.FunctionDef):
        if inner_func is parent_scope:
            continue
        if any(node.name in nl.names for nl in inner_func.nodes_of_class(nodes.Nonlocal)) \
           and any(node.name == an.name for an in inner_func.nodes_of_class(nodes.AssignName)):
            return False

    local_refs = parent_scope.locals.get(node.name, [])
    for ref_node in local_refs:
        if defstmt_frame == node_frame and ref_node.lineno > node.lineno:
            break                       # later refs in same frame: irrelevant; refs are ordered
        if (
            not isinstance(ref_node.parent, nodes.AnnAssign)
            or ref_node.parent.value
        ) and not (
            isinstance(ref_node.parent, nodes.NamedExpr)
            and any(a is ref_node.parent.value for a in node.node_ancestors())
        ):
            return False                # there is a real value assignment
    parent = parent_scope.parent
return True
```
(Original uses `while parent not in {defstmt_frame.parent, None}:` — set membership by equality; nodes use identity equality by default so equivalent.) True ⇒ "name only ever annotated, never valued" ⇒ E0601 (same-scope locals) or E0602 (§9.7).

---

## 13. Helper predicate reference (pylint utils + astroid)

### 13.1 `utils.is_builtin(name)` — the EXACT builtin set

utils.py:282–293:
```python
builtins = builtins.__dict__.copy()
SPECIAL_BUILTINS = ("__builtins__",)

def is_builtin(name: str) -> bool:
    return name in builtins or name in SPECIAL_BUILTINS
```
For CPython 3.12.12 `builtins.__dict__` has exactly these **157 keys** (verified against the pinned venv):
```
ArithmeticError, AssertionError, AttributeError, BaseException, BaseExceptionGroup,
BlockingIOError, BrokenPipeError, BufferError, BytesWarning, ChildProcessError,
ConnectionAbortedError, ConnectionError, ConnectionRefusedError, ConnectionResetError,
DeprecationWarning, EOFError, Ellipsis, EncodingWarning, EnvironmentError, Exception,
ExceptionGroup, False, FileExistsError, FileNotFoundError, FloatingPointError,
FutureWarning, GeneratorExit, IOError, ImportError, ImportWarning, IndentationError,
IndexError, InterruptedError, IsADirectoryError, KeyError, KeyboardInterrupt,
LookupError, MemoryError, ModuleNotFoundError, NameError, None, NotADirectoryError,
NotImplemented, NotImplementedError, OSError, OverflowError, PendingDeprecationWarning,
PermissionError, ProcessLookupError, RecursionError, ReferenceError, ResourceWarning,
RuntimeError, RuntimeWarning, StopAsyncIteration, StopIteration, SyntaxError,
SyntaxWarning, SystemError, SystemExit, TabError, TimeoutError, True, TypeError,
UnboundLocalError, UnicodeDecodeError, UnicodeEncodeError, UnicodeError,
UnicodeTranslateError, UnicodeWarning, UserWarning, ValueError, Warning,
ZeroDivisionError, __build_class__, __debug__, __doc__, __import__, __loader__,
__name__, __package__, __spec__, abs, aiter, all, anext, any, ascii, bin, bool,
breakpoint, bytearray, bytes, callable, chr, classmethod, compile, complex, copyright,
credits, delattr, dict, dir, divmod, enumerate, eval, exec, exit, filter, float,
format, frozenset, getattr, globals, hasattr, hash, help, hex, id, input, int,
isinstance, issubclass, iter, len, license, list, locals, map, max, memoryview, min,
next, object, oct, open, ord, pow, print, property, quit, range, repr, reversed,
round, set, setattr, slice, sorted, staticmethod, str, sum, super, tuple, type,
vars, zip
```
plus `__builtins__`. NOTE: `__name__`, `__doc__`, `__spec__`, `__loader__`, `__package__`, `__debug__` are builtins-module attributes and thus ALWAYS pass `is_builtin`. `__file__` and `__path__` are NOT in builtins — they are covered only by `nodes.Module.scope_attrs = {"__name__","__doc__","__file__","__path__","__package__"}` in the final-fallback check (§5) and module-level `_is_variable_violation` (§12.1). Consequence: `__file__` used inside a function still passes (it resolves through... actually `__file__` inside a function: consumers find nothing, fallback checks `scope_attrs` → contains `__file__` → suppressed, regardless of scope). `__spec__` passes via is_builtin.

`astroid.builtin_lookup(name)` (astroid/nodes/scoped_nodes/utils.py:17–35): returns `(builtins_module, builtins_module.locals.get(name, []))`; `"__dict__"` special-cased to `()`. The astroid builtins module locals are built from the live `builtins` module — same name set as above (plus inference internals). Used in `_is_variable_violation` module-level branch.

### 13.2 `utils.is_defined_before(var_node)` (utils.py:353–408) — full algorithm

```python
varname = var_node.name
for parent in var_node.node_ancestors():
    defnode = defnode_in_scope(var_node, varname, parent)
    if defnode is None:
        continue
    defnode_scope = defnode.scope()
    if isinstance(defnode_scope, (*COMP_NODE_TYPES, nodes.Lambda, nodes.FunctionDef)):
        # Avoid the case where var_node_scope is a nested function
        if isinstance(defnode_scope, nodes.FunctionDef):
            var_node_scope = var_node.scope()
            if var_node_scope is not defnode_scope and isinstance(var_node_scope, nodes.FunctionDef):
                return False
        return True
    if defnode.lineno < var_node.lineno:
        return True
    # `defnode` and `var_node` on the same line
    for defnode_anc in defnode.node_ancestors():
        if defnode_anc.lineno != var_node.lineno:
            continue
        if isinstance(defnode_anc, (nodes.For, nodes.While, nodes.With, nodes.Try, nodes.ExceptHandler)):
            return True
# possibly multiple statements on the same line using semicolon separator
stmt = var_node.statement()
_node = stmt.previous_sibling()
lineno = stmt.fromlineno
while _node and _node.fromlineno == lineno:
    for assign_node in _node.nodes_of_class(nodes.AssignName):
        if assign_node.name == varname:
            return True
    for imp_node in _node.nodes_of_class((nodes.ImportFrom, nodes.Import)):
        if varname in [name[1] or name[0] for name in imp_node.names]:
            return True
    _node = _node.previous_sibling()
return False
```
with `defnode_in_scope(var_node, varname, scope)` (utils.py:305–350) — verbatim:
```python
if isinstance(scope, nodes.If):
    for node in scope.body:
        if isinstance(node, nodes.Nonlocal) and varname in node.names:
            return node
        if isinstance(node, nodes.Assign):
            for target in node.targets:
                if isinstance(target, nodes.AssignName) and target.name == varname:
                    return target
elif isinstance(scope, (COMP_NODE_TYPES, nodes.For)):     # ListComp/SetComp/DictComp/GeneratorExp/For
    for ass_node in scope.nodes_of_class(nodes.AssignName):
        if ass_node.name == varname:
            return ass_node
elif isinstance(scope, nodes.With):
    for expr, ids in scope.items:
        if expr.parent_of(var_node):
            break
        if ids and isinstance(ids, nodes.AssignName) and ids.name == varname:
            return ids
elif isinstance(scope, (nodes.Lambda, nodes.FunctionDef)):
    if scope.args.is_argument(varname):
        if scope.args.parent_of(var_node):
            try:
                scope.args.default_value(varname)
                scope = scope.parent
                defnode = defnode_in_scope(var_node, varname, scope)
            except astroid.NoDefault:
                pass
            else:
                return defnode
        return scope
    if getattr(scope, "name", None) == varname:
        return scope
elif isinstance(scope, nodes.ExceptHandler):
    if isinstance(scope.name, nodes.AssignName):
        ass_node = scope.name
        if ass_node.name == varname:
            return ass_node
return None
```
`COMP_NODE_TYPES = (ListComp, SetComp, DictComp, GeneratorExp)` (utils.py:40–45).

### 13.3 `utils.in_for_else_branch(parent, stmt)` (utils.py:2043–2048)

```python
@lru_cache
def in_for_else_branch(parent, stmt):
    return isinstance(parent, nodes.For) and any(
        else_stmt.parent_of(stmt) or else_stmt == stmt for else_stmt in parent.orelse
    )
```
(Only used by `_loopvar_name` W0631 — listed for completeness.)

### 13.4 `utils.is_func_decorator(node)` (utils.py:430–444)

Walk ancestors; True at the first `Decorators` node; break (False) at the first statement or Lambda/ComprehensionScope/ListComp ancestor.

### 13.5 `utils.get_node_first_ancestor_of_type` (utils.py:1963–1970) / `..._and_its_child` (1973–1987)

Simple nearest-ancestor isinstance scans (see §8.11 for the child-tracking variant).

### 13.6 `utils.node_ignores_exception(node, NameError)` (utils.py:1148–1159)

```python
managing_handlers = get_exception_handlers(node, exception)
if managing_handlers:
    return True
return any(get_contextlib_suppressors(node, exception))
```
- `find_try_except_wrapper_node` (utils.py:997–1008): climb `node.parent` chain until the parent is `ExceptHandler` or `Try`; return that parent (or None). **No scope-boundary stop** — a function defined inside a try counts! (conservatism, replicate).
- `get_exception_handlers` (utils.py:1061–1078): if the wrapper is a `Try` → list of handlers where `error_of_type(handler, NameError)`; if the wrapper is an ExceptHandler (node inside a handler body) or None → `[]`.
- `error_of_type` (utils.py:778–802): `handler.type` must be non-None; `handler.catch({"NameError"})` → astroid `ExceptHandler.catch` (node_classes.py:2652–2659): `any(name_node.name in exceptions for name_node in self.type._get_name_nodes())` — i.e. **literal name match** on Name nodes in the handler type expression (`except Exception` does NOT catch NameError for this purpose; `except (ValueError, NameError)` does; aliases don't).
- `get_contextlib_suppressors` (utils.py:1110–1131): ancestors that are `With` whose items contain a Call whose func safe-infers to ClassDef qname `contextlib.suppress` and whose args safe-infer to a ClassDef named `NameError` (or a Tuple containing one) — `_suppresses_exception` utils.py:1088–1107.

### 13.7 `utils.safe_infer` (utils.py:1348–1410)

First inferred value; None if InferenceError on first; iterate the rest — if any subsequent inferred value has a different `_get_python_type_of_node` → None (ambiguity); InferenceError mid-iteration → None; StopIteration → value. (compare_constants/compare_constructors options unused on our paths.)

### 13.8 `_is_before`, `_is_nonlocal_name`, `_assigned_locally`, `_find_frame_imports`, `_flattened_scope_names`

All quoted earlier (§7 / §12.1); definitions at variables.py:308–317, 320–330, 299–305, 255–274, 292–296. `_flattened_scope_names(iterator)` = union of `stmt.names` over Global/Nonlocal nodes.

### 13.9 `astroid.are_exclusive(stmt1, stmt2, exceptions)` (astroid/nodes/node_classes.py:116–186) — verbatim

```python
stmt1_parents = {}; children = {}
previous = stmt1
for node in stmt1.node_ancestors():
    stmt1_parents[node] = 1
    children[node] = previous
    previous = node
previous = stmt2
for node in stmt2.node_ancestors():
    if node in stmt1_parents:
        if isinstance(node, If) and exceptions is None:
            c2attr, c2node = node.locate_child(previous)
            c1attr, c1node = node.locate_child(children[node])
            if "test" in (c1attr, c2attr):
                return False
            if c1attr != c2attr:
                return True
        elif isinstance(node, Try):
            c2attr, c2node = node.locate_child(previous)
            c1attr, c1node = node.locate_child(children[node])
            if c1node is not c2node:
                first_in_body_caught_by_handlers = (
                    c2attr == "handlers" and c1attr == "body" and previous.catch(exceptions))
                second_in_body_caught_by_handlers = (
                    c2attr == "body" and c1attr == "handlers" and children[node].catch(exceptions))
                first_in_else_other_in_handlers = (c2attr == "handlers" and c1attr == "orelse")
                second_in_else_other_in_handlers = (c2attr == "orelse" and c1attr == "handlers")
                if any((first_in_body_caught_by_handlers, second_in_body_caught_by_handlers,
                        first_in_else_other_in_handlers, second_in_else_other_in_handlers)):
                    return True
            elif c2attr == "handlers" and c1attr == "handlers":
                return previous is not children[node]
        return False
    previous = node
return False
```
In `_check_consumer` it's called with `exceptions=("NameError",)` → If-branch exclusivity disabled; only try/except relations apply (handler `catch(("NameError",))` is True for bare handlers since `self.type is None → return True`! See ExceptHandler.catch: `if self.type is None or exceptions is None: return True`).

### 13.10 astroid name lookup (`node.lookup`, `scope_lookup`, `_filter_stmts`)

Used by `_is_variable_violation` (`defframe.root().lookup(node.name)`), `in_type_checking_block`, `_loopvar_name`, `_check_late_binding_closure`.

- `LookupMixIn.lookup(name)` (astroid/nodes/_base_nodes.py:259–276): `self.scope().scope_lookup(self, name)`; **`@lru_cache`** on the bound method.
- `Module.scope_lookup` (scoped_nodes.py:312–333): if `name in scope_attrs and name not in locals` → `(module, module.getattr(name))` (special attributes model); else `_scope_lookup`.
- `Lambda.scope_lookup` (995–1023) / `FunctionDef.scope_lookup` (1658–1682): if the lookup node is one of `args.defaults`/`args.kw_defaults` → resolve in `parent.frame()` with `offset=-1`; FunctionDef additionally: `name == "__class__"` inside a method → `(self, [enclosing ClassDef])`.
- `ClassDef.scope_lookup` (2104–2155): if node is in `self.bases` (or `name in builtins` for Decorators parents) → parent frame with `offset=-1`; else self.
- `ComprehensionScope.scope_lookup = LocalsDictNodeNG._scope_lookup` (mixin.py:202).
- `LocalsDictNodeNG._scope_lookup` (mixin.py:78–98): `_filter_stmts(node, self.locals[name], self, offset)`; if empty → climb to the next enclosing **non-class** scope (class scopes are skipped for nested lookups!) → recurse; at top → `builtin_lookup(name)`.
- `_filter_stmts` (astroid/filter_statements.py:50–240): full text quoted in the repo file; essentials for the port:
  - line filtering: only when `myframe is frame and mystmt`; `mylineno = mystmt.fromlineno + offset`; statements with `stmt.fromlineno > mylineno > 0` break the loop (definitions after the use are ignored); `myframe` is hoisted to the parent frame when `base_node.statement() is myframe` (defaults/decorators, pylint issue #295).
  - ExceptHandler-only statement lists: when ALL candidate statements are ExceptHandlers, keep only those containing base_node (`_get_filtered_node_statements`, filter_statements.py:22–34).
  - decorator self-reference skip (`mystmt is stmt and _is_from_decorator(base_node)`).
  - `node.has_base(base_node)` → break.
  - `assign_type()._get_filtered_stmts` hooks: FilterStmtsBaseNode (Import/From/Arguments...) keeps only `[node]` when `self.statement() is mystmt`; AssignTypeNode returns `(_stmts, True)` when `self is mystmt` (stop without adding).
  - `optional_assign` (True for `For`, `Comprehension`, `NamedExpr`): a loop assignment that contains base_node short-circuits to `[node]`; otherwise loop assignments never delete previous candidates.
  - NamedExpr control-flow special-case (lines 143–159): inside a *nested* if → append; inside one if → assume evaluated, replace; no if → replace.
  - same-block-level pruning via `_stmt_parents.index(stmt.parent)` + `are_exclusive`.
  - `are_exclusive(base_node, node)` → skip candidate.
  - AssignName/NamedExpr in ExceptHandler: if handler contains base_node → reset accumulated; else skip. Non-optional assign in same block as mystmt → reset accumulated (last assignment wins).
  - DelName → reset accumulated, skip.

### 13.11 Module special attributes vs scope_attrs

`Module.getattr` (scoped_nodes.py:350–377): special_attributes (`__name__`, `__doc__`, `__file__`, `__path__` (packages), `__package__`, `__dict__`, `__spec__`, ... per ModuleModel, astroid/interpreter/objectmodel.py:177–229) only resolve when `name not in self.locals`; `__name__` also appends a synthetic `Const("__main__")`. DelName entries are filtered out of getattr results (line 374).

---

## 14. `__all__` checks: E0603 / E0604 / E0605 — `_check_all` (variables.py:3220–3276)

Gate: `leave_module` calls `_check_all(node, not_consumed)` only when `"__all__" in node.locals` (variables.py:1433–1434). Any binding creates the key: plain assign, AugAssign target, for-target, `global __all__` assignment in a function (lands in module locals per §3.1), an import `from m import __all__`, even `__all__: list`. Conditional assignment (`if X: __all__ = [...]`) also counts.

```python
def _check_all(self, node: nodes.Module, not_consumed: Consumption) -> None:
    try:
        assigned = next(node.igetattr("__all__"))
    except astroid.InferenceError:
        return                                    # conservatism: cannot infer -> silent
    if isinstance(assigned, util.UninferableBase):
        return                                    # Uninferable -> silent
    if assigned.pytype() not in {"builtins.list", "builtins.tuple"}:
        line, col = assigned.tolineno, assigned.col_offset
        self.add_message("invalid-all-format", line=line, col_offset=col, node=node)
        return                                    # E0605; note position = inferred VALUE node
    for elt in getattr(assigned, "elts", ()):
        try:
            elt_name = next(elt.infer())
        except astroid.InferenceError:
            continue
        if isinstance(elt_name, util.UninferableBase):
            continue
        if not elt_name.parent:
            continue                              # synthetic/no-parent inferred nodes skipped
        if not (isinstance(elt_name, nodes.Const) and isinstance(elt_name.value, str)):
            self.add_message("invalid-all-object", args=elt.as_string(), node=elt)
            continue                              # E0604
        elt_name = elt_name.value
        if elt_name in not_consumed:
            del not_consumed[elt_name]            # mutates leftovers (affects W-checks only)
            continue
        if elt_name not in node.locals:
            if not node.package:
                self.add_message("undefined-all-variable", args=(elt_name,), node=elt)   # E0603
            else:
                basename = os.path.splitext(node.file)[0]
                if os.path.basename(basename) == "__init__":
                    name = node.name + "." + elt_name
                    try:
                        astroid.modutils.file_from_modpath(name.split("."))
                    except ImportError:
                        self.add_message("undefined-all-variable", args=(elt_name,), node=elt)
                    except SyntaxError:
                        pass                       # later yielded as syntax-error for that file
```

Details, in evaluation order:

1. **`igetattr("__all__")` and the FIRST inferred value.** `Module.igetattr` (scoped_nodes.py:379–397) runs `_infer_stmts(self.getattr("__all__"), ...)` with `lookupname="__all__"`. `getattr` returns `node.locals["__all__"]` (line-sorted list of all binding nodes, DelNames filtered). `next(...)` takes the FIRST successfully inferred value:
   - `__all__ = ["a"]` → that List node itself (lists infer to themselves). `pytype()` = `"builtins.list"`. `elts` are the ORIGINAL element nodes → message positions point at real source.
   - `__all__ = ("a",)` → Tuple → `"builtins.tuple"`. OK.
   - `__all__ = "ab"` / a set / a dict / a call → pytype mismatch → **E0605** at `line=assigned.tolineno, col_offset=assigned.col_offset` (the *value expression*'s end line / col!), `node=node` (module — supplies the path/module fields; end_lineno/end_col_offset come from the Module node, both None/0-ish; the explicit line wins per §0).
   - `__all__ = [...] + ["b"]` → astroid folds the BinOp into a new synthetic List whose elts reference the original Const nodes — E0603/E0604 then report at the original element nodes.
   - `__all__ = [...]` followed by `__all__ += [...]` / `__all__.extend(...)`: the first inferred value is processed; whether later AugAssign parts are seen depends purely on astroid inference of the FIRST locals entry (for a plain first assignment: only the first list's elements are validated).
   - If the first binding infers to Uninferable but a later one infers fine, `next()` returns the **first non-failing** result from `_infer_stmts` (which skips statements raising InferenceError but yields Uninferable results) — Uninferable → silent return.
2. **Per-element inference**: `next(elt.infer())`. A Name element (e.g. `__all__ = [foo]` where `foo = "x"`) infers to the Const → if str, treated as exporting `"x"`! Inference failure (undefined name in the list, e.g. `__all__ = [undefined]` → first inferred is Uninferable) → `continue` (silent; the Name itself was already separately checked by visit_name/E0602 machinery).
3. **`if not elt_name.parent: continue`** — inferred constants synthesized without parents (e.g. results of some operations) are skipped — conservatism.
4. **E0604**: inferred value is not a string Const (e.g. `__all__ = [1]`, `[b"x"]`, `[SomeClass]`). args = `elt.as_string()` — the **source text** of the element (`%r` adds quotes: `Invalid object '1' in __all__...`), node = `elt` (original element node).
5. **E0603**: the string is not a key of `not_consumed` and not a key of `node.locals` → undefined. Since `node.locals` contains imports, classes, functions, global-declared assignments and wildcard-imported names, any of those satisfy the check. For packages (`__init__.py`): a submodule name is additionally resolved on disk via `astroid.modutils.file_from_modpath` — `ImportError` → E0603; `SyntaxError` → suppressed. args = `(elt_name,)` (1-tuple), node = `elt`.

Note `node.package` is True for `__init__.py` modules (astroid/builder.py:201–208); the basename re-check `os.path.basename(basename) == "__init__"` is thus nearly always True for packages.

---

## 15. E0602 from `_check_metaclasses` (variables.py:3388–3456)

Runs in `leave_module` and `leave_functiondef` (before popping module consumer; after… order: `leave_module` calls it FIRST, then pops). For each **direct child** ClassDef of the scope (`node.get_children()` — not recursive!):

`_check_classdef_metaclasses(klass, parent_node)` (variables.py:3401–3456):
```python
if not klass._metaclass:
    return []                                  # no explicit metaclass= keyword
consumed = []
metaclass = klass.metaclass()                  # astroid inference (may be None)
name = ""
match klass._metaclass:
    case nodes.Name(name=name): pass
    case nodes.Attribute(expr=attr):
        while not isinstance(attr, nodes.Name):
            attr = attr.expr
        name = attr.name
    case nodes.Call(func=nodes.Name(name=name)): pass
    case _ if metaclass:
        name = metaclass.root().name

found = False
name = METACLASS_NAME_TRANSFORMS.get(name, name)   # {"_py_abc": "abc"} (variables.py:54)
if name:
    for to_consume in self._to_consume[::-1]:      # INNER → OUTER this time
        scope_locals = to_consume.to_consume
        found_nodes = scope_locals.get(name, [])
        for found_node in found_nodes:
            if found_node.lineno <= klass.lineno:
                consumed.append((scope_locals, name))
                found = True
                break
    nodes_in_parent_scope = parent_node.locals.get(name, [])
    for found_node_parent in nodes_in_parent_scope:
        if found_node_parent.lineno <= klass.lineno:
            found = True
            break
if (not found and not metaclass
        and not (name in nodes.Module.scope_attrs
                 or utils.is_builtin(name)
                 or name in self.linter.config.additional_builtins)):
    self.add_message("undefined-variable", node=klass, args=(name,))
return consumed
```
- Note args is the 1-tuple `(name,)` here (elsewhere E0602 args is the bare string — same rendering).
- Report node is the **ClassDef** (so line = `klass.fromlineno`; ClassDef has `position` set to the `class NAME` keyword span — position.lineno/col are used per pylinter.py:1213–1221).
- Conservatism: if astroid CAN infer the metaclass (`metaclass` non-None) → never E0602. Only an explicitly-spelled, never-defined, non-builtin metaclass name that isn't found in any consumer (note: the inner→outer scan does NOT break across consumers on `found`; it appends a `(scope_locals, name)` per matching consumer) triggers.
- `_check_metaclasses` then pops consumed names from the recorded scope dicts: `scope_locals.pop(name, None)` (variables.py:3398–3399) — affects unused-import bookkeeping only.

---

## 16. E0118 used-prior-global-declaration — NOT in variables.py

Lives in `pylint/checkers/base/basic_error_checker.py`:
- Message def: basic_error_checker.py:256–262 — `"Name %r is used prior to global declaration"`, `used-prior-global-declaration`, minversion (3,6).
- Emission: `visit_functiondef` (basic_error_checker.py:333–335, also bound to asyncfunctiondef at 367) → `_check_name_used_prior_global` (369–393):

```python
def _check_name_used_prior_global(self, node: nodes.FunctionDef) -> None:
    scope_globals = {
        name: child
        for child in node.nodes_of_class(nodes.Global)
        for name in child.names
        if child.scope() is node
    }
    if not scope_globals:
        return
    for node_name in node.nodes_of_class(nodes.Name):
        if node_name.scope() is not node:
            continue
        name = node_name.name
        corresponding_global = scope_globals.get(name)
        if not corresponding_global:
            continue
        global_lineno = corresponding_global.fromlineno
        if global_lineno and global_lineno > node_name.fromlineno:
            self.add_message("used-prior-global-declaration", node=node_name, args=(name,))
```
Facts: only `nodes.Name` (Load context — astroid's AssignName/DelName are distinct classes, so stores and `del`s do NOT trigger E0118, even though CPython's SyntaxError covers those too — known false-negative, replicate); `nodes_of_class` recurses into nested functions but the `scope() is node` filters keep only this function's direct names/global statements; dict-comprehension means **the last Global statement for a name wins** (pre-order); report node = the Name, args = `(name,)`.

---

## 17. Edge-case catalogue (explicit answers to porting questions)

1. **Names assigned in a loop body and used in the while-condition / earlier in the loop**: same-scope rule `stmt.fromlineno <= defstmt.fromlineno` (§12.1 final else) means `while not done: ... done = True` reports E0601 at the `while` test only if the use's line ≤ def's line — for a while-test on line N and assignment on line N+2, `N <= N+2` is True → candidate. Then §9.6 requires `not utils.is_defined_before(node)` and not exclusive. There is no special while-loop forgiveness; HOWEVER the *first iteration really would* NameError, except when the variable is also assigned before the loop — in which case `_filter_stmts`/locals give the earlier assignment as `found_nodes[0]` and the line test passes (def line < use line → False → no message). For `for`-loops: the For target is an `optional_assign` in `_filter_stmts`, and in `_is_variable_violation` local-scope branch `_assigned_locally` blocks the global lookup. The W0631 checker (out of scope) handles post-loop uses.
2. **Function default values are evaluated in the enclosing scope**: implemented three ways — astroid `Lambda/FunctionDef.scope_lookup` offset=-1 trick (§13.10), `_should_node_be_skipped`'s function/lambda branches (§6), and the class-level-lambda-default rule in `_is_variable_violation`. Keyword-only defaults (`kw_defaults`) are included everywhere alongside `defaults` (note `kw_defaults` may contain `None` entries; `is_default_argument` filters them, the `_check_consumer` postponed-eval set `{*(stmt.args.kw_defaults or ())}` does not — None in a set is harmless).
3. **Decorators referencing class attributes**: decorators of a method are part of the function header (`_defined_in_function_definition` includes `frame.decorators.parent_of(node)`), so the function's own consumer is skipped; the class consumer is then consulted (`_ignore_class_scope` returns False = use it, when the name is in the class frame locals and node isn't in a lambda/comprehension body). astroid's `ClassDef.scope_lookup` also has the Decorators+builtin-name escape hatch.
4. **Nested functions closing over later-defined names**: `frame is not defframe` → `_detect_global_scope`. For a FunctionDef frame: `if frame.parent_of(defframe): return node.lineno < defframe.lineno`; if node.parent isn't FunctionDef/Arguments → False → no E0601. So `def f(): return helper()` before `def helper()` is fine (pylint only reports used-before-assignment for same-scope or shared-global-scope class chains).
5. **Self-referencing definitions** (`x = x`): handled by `get_next_to_consume` filter (a) → None → outer consumer lookup → fine if outer defines it, else final E0602.
6. **`del x` then use**: DelName registered in locals; astroid `_filter_stmts` clears prior assignments at a DelName; `get_next_to_consume` then yields the DelName (or later defs); `_check_consumer` 1915–1920: `isinstance(defstmt, nodes.Delete)` → CONTINUE (None) → ends with fallback E0602 if nothing outer.
7. **Augmented assignment `x += 1` with no prior binding**: visit_assignname → visit_name; `defined_by_stmt=True` (defstmt is stmt) → `(CONTINUE, [node])` (consume self) → outer scopes → fallback E0602 if unresolved. With a prior binding, normal rules.
8. **Walrus in comprehensions/while/if**: NamedExpr is `optional_assign`; `_filter_stmts` NamedExpr special-case; in `_check_consumer` the same-line carve-outs (§12.1) plus the never-evaluated-IfExp E0602 (§9.7). A walrus inside a comprehension binds in the enclosing frame in astroid (comprehension-scope walrus targets are hoisted — see `_loopvar_name`'s walrus handling at 2716–2732 (W-only)).
9. **try/except/finally/else**: §8 covers all five uncertainty mechanisms. Summary: defs in `try:` are uncertain from `except:` and `finally:` (8.10, 8.11); defs in `except:` are uncertain from outside unless try/else returns/exits and all handlers define-or-raise (8.8); loop-else + break-in-try-else special case (8.9); the use being moved to `consumed_uncertain` produces E0601 with CONTROL_FLOW confidence via `_report_unfound_name_definition` when no certain definition remains.
10. **if/elif chains and Match**: elif is nested `If` in `orelse`; `_inferred_to_define_name_raise_or_return_for_if_node` recurses via `_branch_handles_name` on `orelse`. Match handled by `all(... for case in node.cases)` — no wildcard-case requirement.
11. **Star imports**: see §3.1 — resolvable wildcard expands names into locals; failed wildcard adds nothing (uses → E0602). There is NO blanket suppression.
12. **Builtins**: §13.1. `__file__`/`__path__` only via `scope_attrs`; everything else incl. `__spec__`, `__loader__`, `__debug__`, `__builtins__` via `is_builtin`.
13. **Metaclass references**: §15.
14. **`__class__` in methods**: two mechanisms — astroid FunctionDef.scope_lookup special case (affects `lookup()`-based paths) and the explicit fallback exemption (§5).
15. **PEP 695 type parameter scopes**: type-param AssignNames live in the owning ClassDef/FunctionDef locals (§3.1); `_should_node_be_skipped` un-skips them; `_is_variable_violation` same-line carve-outs for `defnode in defframe.type_params`; `_report_unfound_name_definition` guard 2 suppresses anything inside TypeAlias/TypeVar/ParamSpec/TypeVarTuple subtrees.
16. **String annotations**: `visit_const` (variables.py:3502–3530) only feeds `_type_annotation_names` (unused-import suppression). String annotation *contents* are never name-checked (no E0602 inside string annotations). Non-string runtime annotations follow normal rules subject to the postponed-evaluation exemptions in §9.6/§10.
17. **Conditional imports** (`try: import x / except ImportError: x = None`): the two defs are exclusive branch-wise; uncertainty filters: import in try body uncertain from except (8.10) but the handler assigning the name satisfies `_defines_name_raises_or_returns_recursive` in (8.11-style checks)… net effect: use after the try/except sees both candidate defs; the if-tests filter doesn't apply (no If); except-block filter (8.8) marks the *except* def uncertain unless try returns; but the *try* def remains certain from code after the Try (8.10 requires the use to be inside a handler; 8.11 requires use in finally) → found_nodes non-empty → def line < use line → no message. ✔ no false positive.
18. **`sys.modules` manipulation, dynamic globals()**: no special handling in this checker (no qname-based special cases for globals()/vars()/locals() on the E06xx paths; `_has_locals_call_after_node` affects only W0641).
19. **Lambda scopes**: `base_scope_type == "lambda"` suppresses generic E0601 (variables.py:1939 comment: "E0601 may *not* occur in lambda scope") except the class-frame case at 1968–1988. Also note `base_scope_type` is the scope_type of the **innermost** consumer at the time of the name visit — for a name inside a lambda nested in a function it is `"lambda"` regardless of which consumer is currently examined.
20. **Names under always-false `if`** (`if False:`, `if TYPE_CHECKING:` runtime use): definition filtered as uncertain → `names_under_always_false_test` → E0601 with INFERENCE confidence (or E0606 if one-branch). Note `val != NotImplemented and val` — a test inferring to `NotImplemented` does NOT count as true.
21. **Comprehension scopes**: each comprehension pushes its own consumer; names defined by generators live in the ComprehensionScope locals; class-attribute access rules per `_in_lambda_or_comprehension_body` (first generator's iter only); `get_next_to_consume` bail (d) skips filtering when a comprehension intervenes between node and frame.
22. **Order of reporting**: messages are emitted during the AST walk at the position of each Name node (pre-order, source order). pylint sorts final output per file by line; the port should emit per (line, col) of the cited nodes.

---

## 18. Iteration-order / determinism summary

- `self._to_consume` is a list (stack); checked inner→outer in `_undefined_and_used_before_checker` (range start_index..0), outer→inner in `leave_classdef` consumption, inner→outer (`[::-1]`) in `_check_classdef_metaclasses`.
- `node.locals` is an insertion-ordered dict; values are lists in source order except from-import names whose lists are re-sorted by `fromlineno or 0` at post-build (§3.1).
- `consumed_uncertain` is a `defaultdict(list)` — key creation on read access matters (§3, §10).
- `names_under_always_false_test` / `names_defined_under_one_branch_only` are sets — membership only; the one-branch set supports add/remove churn across successive If evaluations of the same name (§8.4).
- `set(consumed_nodes)` / `uncertain_nodes_set` — membership only.
- `{test.value for ...}` set equality in `_node_guarded_by_same_test` — order-insensitive by design.
- `_get_filtered_node_statements`, `_filter_stmts` accumulators — list order = source order of candidates.
- `infer_all` is `@lru_cache(512)`, `LookupMixIn.lookup` is `@lru_cache` — caching only, no semantic effect (but note lookup caching means later mutations of locals (none happen) wouldn't be observed).

## 19. Early-bailout / conservatism master list (false-positive guards)

Every one of these MUST exist in the port; absence ⇒ false positives:

1. `visit_name`: `stmt.fromlineno is None` → skip (variables.py:1681–1684).
2. `_should_node_be_skipped` — all three scope-type skips + type_params overrides (§6).
3. `get_next_to_consume`: `x = x` → None; `for x in x` → None/others; nonlocal → unfiltered; comprehension-between → unfiltered (§7).
4. All four uncertainty filters only *defer* (consumed_uncertain) — they never directly emit.
5. `_uncertain_nodes_if_tests` per-node `continue`s: non-handled def types; no enclosing if; AssignName-use in different frame; closest_if contains use; same-test guard; inferred-to-define guard (§8.1).
6. `_report_unfound_name_definition`: 6 suppression guards (§10).
7. `_check_consumer`: consumed fast-path RETURN; recursive-class-in-lambda RETURN; `use_outer_definition` CONTINUE; `is_defined_before` guard; `are_exclusive(..., ("NameError",))` guard; `node_ignores_exception(NameError)` guard; postponed-annotation/TypeAlias/function-annotation exemptions; lambda base-scope suppression (§9).
8. `_is_variable_violation`: builtin/global/nonlocal resolution; all `maybe_before_assign=False` carve-outs (§12.1).
9. `_is_only_type_assignment`: requires AnnAssign w/o value AND no later real value anywhere up the scope chain AND no nonlocal-assigning inner function (§12.4).
10. `_is_never_evaluated`: requires safe_infer of the IfExp test to a literal True/False Const (§9.7).
11. Final fallback E0602: scope_attrs / builtins / additional_builtins / `__class__`-in-method / node_ignores_exception (§5).
12. `_check_all`: InferenceError → return; Uninferable → return; element InferenceError/Uninferable/no-parent → continue; package submodule file resolution; SyntaxError → pass (§14).
13. `_check_classdef_metaclasses`: inferable metaclass → silent; found in any consumer or parent locals (lineno ≤ class lineno) → silent; builtins → silent (§15).
14. `in_type_checking_block`'s hard `return False` on missing lookup (§11).
15. `safe_infer` ambiguity → None (treated as "don't know" by all callers) (§13.7).
