# 07 — astroid 4.0.4 inference engine core: exact behavioral spec for the Rust port

Pinned sources:

- astroid 4.0.4 package root: `/Users/adamraudonis/Desktop/Projects/prylint/reference/astroid/astroid`
  (all `astroid/...` paths below are relative to this root)
- pylint 4.0.5 package root: `/Users/adamraudonis/Desktop/Projects/prylint/reference/pylint/pylint`
- Ground-truth runtime: CPython 3.12.12.

This document specifies the **minimal faithful subset** of the astroid inference engine
required by the in-scope pylint checks (E/F category, see §23 dependency map), with exact
control flow, early-bailout paths, caching semantics, and iteration-order notes. Code is
quoted verbatim where the logic is subtle. Line numbers refer to the pinned files.

---

## Table of contents

1. The two-object system (AST nodes vs inference proxies)
2. `Uninferable` semantics
3. `InferenceContext` / `CallContext` / caches
4. `NodeNG.infer` — the single entry point; inference decorators; inference tips
5. `util.safe_infer`
6. `bases._infer_stmts`
7. Name lookup: `lookup` → `scope_lookup` → `_filter_stmts` (full algorithm), `are_exclusive`
8. `Name` / `AssignName` / `AssignAttr` inference
9. The `assigned_stmts` protocol (every implementation)
10. `Arguments` inference and `CallSite.infer_argument`
11. Call inference: `Call._infer`, `FunctionDef.infer_call_result` & friends
12. Attribute access: `Instance` / `ClassDef` / `Module` getattr & igetattr; object models
13. MRO: C3 merge, error modes, `ancestors()` fallback
14. Operator protocols: BinOp / AugAssign / UnaryOp, `dunder_lookup`, `helpers.object_type`
15. Subscript inference and the `getitem` implementations
16. `bool_value` per node type; `Compare`; `BoolOp`; `IfExp`; f-strings
17. Container / Const inference (`BaseContainer`, `Dict`, `Const`)
18. Exceptions: `ExceptionInstance`, `unpack_infer`, except-handler binding
19. Builtins bootstrapping and the builtin brains
20. Module import machinery; relative imports (the E0402 trigger path)
21. Iteration-order, sorting and nondeterminism notes
22. Numeric limits & guards table
23. Dependency map: in-scope check → required inference features

---

## 1. The two-object system

astroid's inference results are a mix of:

**(a) AST nodes** (`NodeNG` subclasses) — these are yielded directly when a node infers to
itself or to another syntactic node: `Const`, `List`, `Tuple`, `Set`, `Dict`, `Slice`,
`FunctionDef`, `Lambda`, `ClassDef`, `Module`, `Unknown`, `EmptyNode`, …
All of these are *hashable by identity* (no `__eq__`/`__hash__` overridden), which matters
for the path set and caches.

**(b) Proxy objects** defined in `astroid/bases.py` and `astroid/objects.py` — runtime-value
stand-ins that wrap (`_proxied`) a `ClassDef`/`FunctionDef`:

- `bases.Proxy` (bases.py:111-150) — base; `__getattr__` forwards unknown attributes to
  `self._proxied`, and `infer(context)` **yields self** (a proxy always infers to itself).
  ```python
  def __getattr__(self, name: str) -> Any:
      if name == "_proxied":
          return self.__class__._proxied
      if name in self.__dict__:
          return self.__dict__[name]
      return getattr(self._proxied, name)
  ```
  Consequence for the port: any attribute read that isn't defined on the proxy (e.g.
  `instance.name`, `instance.lineno`) transparently reads from the proxied `ClassDef`.
- `bases.BaseInstance` (bases.py:231-345) — lookup machinery shared by `Instance`,
  `Generator`, `UnionType` and constant containers.
- `bases.Instance` (bases.py:348-435) — "an instance of class X". `pytype()` returns
  `self._proxied.qname()`, `display_type()` is `"Instance of"`.
- `bases.UnboundMethod` (bases.py:438-530) — a plain function retrieved through a class.
  `pytype` etc. forwarded to `_proxied` (a `FunctionDef`).
- `bases.BoundMethod(UnboundMethod)` (bases.py:533-677) — function + `self.bound`
  (the instance/class it's bound to). `implicit_parameters()` is 1 (0 for `__new__`).
- `bases.Generator(BaseInstance)` (bases.py:680-722) — result of calling a generator
  function; `pytype()` = `"builtins.generator"`, `callable()` is False,
  `bool_value()` is True; `parent` is the `FunctionDef`. `_proxied` is the synthetic
  `generator` ClassDef built at bootstrap (raw_building.py:626-647).
- `bases.AsyncGenerator` — same, `pytype()` `"builtins.async_generator"`.
- `bases.UnionType` (bases.py:745-778) — result of `X | Y` on classes (PEP 604).
- `objects.ExceptionInstance(bases.Instance)` (objects.py:232-246) — instance of an
  exception class; supplies `args`, `__traceback__`, etc. via object models (§18).
- `objects.Super` (objects.py:55-229) — proxy for `super()` calls.
- `objects.FrozenSet` — an AST-like container (BaseContainer subclass).
- `objects.Property(FunctionDef)` (objects.py:334-364) — result of inferring a
  property-decorated function or `property(...)` call; `type` = `"property"`,
  `infer_call_result` **always raises** `InferenceError("Properties are not callable")`.
- `objects.PartialFunction(FunctionDef)` (objects.py:277-326) — `functools.partial` result.
- `objects.DictItems/DictKeys/DictValues` (objects.py:262-274) — `dict.items()` etc.

Also: `Const`, `List`, `Tuple`, `Set`, `Dict` are AST nodes that **inherit from
`Instance`** (`class Const(_base_nodes.NoChildrenNode, Instance)` node_classes.py:2014;
`class BaseContainer(_base_nodes.ParentAssignNode, Instance, ...)` node_classes.py:269;
`Dict.__bases__` is patched to `(NodeNG, DictInstance)` at objects.py:331). Their
`_proxied` is the corresponding builtin `ClassDef` set during bootstrapping (§19);
for `Const` it is the **class-level property** `_proxied = property(_set_proxied)`
(raw_building.py:624) which maps `type(self.value)` → builtin ClassDef.

`Uninferable` (§2) is the third kind of result.

`InferenceResult = NodeNG | Proxy | UninferableBase` (astroid/typing.py).

---

## 2. `Uninferable` semantics — astroid/util.py:19-51

```python
class UninferableBase:
    """Special inference object, which is returned when inference fails."""

    def __repr__(self) -> Literal["Uninferable"]:
        return "Uninferable"
    __str__ = __repr__

    def __getattribute__(self, name: str) -> Any:
        if name == "next":
            raise AttributeError("next method should not be called")
        if name.startswith("__") and name.endswith("__"):
            return object.__getattribute__(self, name)
        if name == "accept":
            return object.__getattribute__(self, name)
        return self

    def __call__(self, *args: Any, **kwargs: Any) -> UninferableBase:
        return self

    def __bool__(self) -> Literal[False]:
        return False
    __nonzero__ = __bool__

Uninferable: Final = UninferableBase()
```

Exact semantics the port must replicate:

- **`bool(Uninferable)` is `False`** (NOT True). This is load-bearing: code like
  `if not transformed or isinstance(transformed, UninferableBase)` and
  `if any(not elem for elem in (key, safe_value))` (node_classes.py:2502) relies on it.
- Any **non-dunder attribute access returns `Uninferable` itself** (`Uninferable.callable`
  → `Uninferable`, then calling it → `Uninferable`, which is falsy). Exception:
  accessing `.next` raises `AttributeError`; `accept` is real.
- It is a singleton; identity checks (`result is util.Uninferable`) and
  `isinstance(x, UninferableBase)` are both used in the codebase — they are equivalent.
- `Uninferable(…)` returns `Uninferable`.

`BadUnaryOperationMessage` / `BadBinaryOperationMessage` (util.py:54-108) are *not*
exceptions; they are sentinel objects yielded by `_infer_unaryop`/`_infer_binop` and
harvested by `type_errors()` (used directly by pylint E1130/E1131). Their `__str__`:

- unary (util.py:83-95): `"bad operand type for unary {op}: {operand_type}"` where
  `operand_type` is `operand.name` if present, else the `.name` of
  `helpers.object_type(operand)`, else `object_type.as_string()`.
- binary (util.py:106-108): `"unsupported operand type(s) for {op}: {left.name!r} and {right.name!r}"`.

---

## 3. `InferenceContext` / `CallContext` / caches — astroid/context.py

### 3.1 Global inference cache

```python
_InferenceCache = dict[
    tuple["nodes.NodeNG", str | None, str | None, str | None], Sequence["nodes.NodeNG"]
]
_INFERENCE_CACHE: _InferenceCache = {}
```
(context.py:19-23). NOTE: despite the type annotation saying `str | None` for the last
two slots, the actual key inserted is
`(node, context.lookupname, context.callcontext, context.boundnode)`
(node_ng.py:154) — a `CallContext` object and an inference-result object, **compared and
hashed by identity** (neither defines `__eq__`). `InferenceContext.inferred` is a property
returning this *global* dict (context.py:100-108). It is invalidated only by
`AstroidManager.clear_cache()` → `_invalidate_cache()`.

Practical consequence: because each `Call._infer` constructs a fresh `CallContext`, the
cache key with non-None callcontext is effectively per-call-site-evaluation; the common
cache hits are the `(node, lookupname, None, None)` keys.

### 3.2 `InferenceContext` (context.py:30-161)

Fields (slots): `path`, `lookupname`, `callcontext`, `boundnode`, `extra_context`,
`constraints`, `_nodes_inferred`. Class attr `max_inferred = 100`.

- `path: set[tuple[NodeNG, str | None]]` — the recursion-guard set of
  **(node, lookupname) pairs**:
  ```python
  def push(self, node) -> bool:
      name = self.lookupname
      if (node, name) in self.path:
          return True            # already visiting => caller must produce nothing
      self.path.add((node, name))
      return False
  ```
- `nodes_inferred` is stored in a shared single-element list `_nodes_inferred` so that
  **clones share the counter** (context.py:49-98). It is incremented once per yielded
  result in `NodeNG.infer` and in explicit-inference loops.
- `clone()` (context.py:123-136): copies `path` (shallow set copy) and `constraints`
  (shallow dict copy); **shares** `callcontext`, `boundnode`, `extra_context` and the
  nodes_inferred cell. `lookupname` is NOT copied (fresh None).
- `restore_path()` (context.py:138-142): context manager that snapshots `path` and
  restores it after the block (used by `ClassDef.ancestors`).
- `copy_context(context)` (context.py:184-189): `context.clone()` or fresh
  `InferenceContext()`.
- `bind_context_to_node(context, node)` (context.py:192-204): `copy_context` then set
  `boundnode = node`.
- `is_empty()` (context.py:144-154): all of path/nodes_inferred/callcontext/boundnode/
  lookupname/extra_context/constraints falsy. Used by the inference-tip cache.

### 3.3 `CallContext` (context.py:164-181)

```python
def __init__(self, args, keywords=None, callee=None):
    self.args = args                      # list of positional argument *nodes*
    if keywords:
        arg_value_pairs = [(arg.arg, arg.value) for arg in keywords]
    else:
        arg_value_pairs = []
    self.keywords = arg_value_pairs       # list[(name|None, value-node)]
    self.callee = callee
```

### 3.4 Constraints (astroid/constraint.py)

`context.constraints: dict[str, dict[If|IfExp, set[Constraint]]]` — populated by
`Name._infer` via `get_constraints(node, frame)`. The only constraint type implemented
in astroid 4.0.4 is `NoneConstraint` (matches `x is None` / `x is not None` tests in
enclosing `If` statements); in `_infer_stmts` constraints filter out inferred values
inconsistent with the test. For the port this affects e.g.
`if x is not None: x.foo()` (filters out the `Const(None)` candidate).
Satisfaction rule: for a constraint `x is None` with `negate=False`, only
`Const(None)` survives; with `negate=True`, `Const(None)` and `Uninferable` are filtered
out. (constraint.py — `NoneConstraint.satisfied_by`: returns True if `inferred` is
Uninferable? No: `if isinstance(inferred, util.UninferableBase): return True` — Uninferable
always satisfies; then `self.negate ^ _matches(inferred, self.CONST_NONE)`.)

---

## 4. `NodeNG.infer` — astroid/nodes/node_ng.py:121-176

Quoted verbatim; this is the single dispatch point the port must mirror exactly:

```python
def infer(self, context=None, **kwargs):
    if context is None:
        context = InferenceContext()
    else:
        context = context.extra_context.get(self, context)
    if self._explicit_inference is not None:
        # explicit_inference is not bound, give it self explicitly
        try:
            for result in self._explicit_inference(self, context, **kwargs):
                context.nodes_inferred += 1
                yield result
            return
        except UseInferenceDefault:
            pass

    key = (self, context.lookupname, context.callcontext, context.boundnode)
    if key in context.inferred:
        yield from context.inferred[key]
        return

    results = []

    # Limit inference amount to help with performance issues with
    # exponentially exploding possible results.
    limit = AstroidManager().max_inferable_values     # default 100
    for i, result in enumerate(self._infer(context=context, **kwargs)):
        if i >= limit or (context.nodes_inferred > context.max_inferred):
            results.append(util.Uninferable)
            yield util.Uninferable
            break
        results.append(result)
        yield result
        context.nodes_inferred += 1

    # Cache generated results for subsequent inferences of the
    # same node using the same context
    context.inferred[key] = tuple(results)
    return
```

Key behaviors:

1. **`extra_context` swap**: if the caller put this node in `context.extra_context`
   (done by `Call._infer._populate_context_lookup` for call arguments), the context
   stored there replaces the incoming one.
2. **Explicit inference (inference tips)** runs first; `UseInferenceDefault` falls
   through to the default `_infer`.
3. **Cache check** against the global `_INFERENCE_CACHE` (see §3.1).
4. **Limits**: `max_inferable_values` (manager default 100) limits results *per node*;
   `context.max_inferred` (=100) limits *total results in the context tree*. On
   exceeding either, append-and-yield one `Uninferable` and stop. NOTE the cache is
   **not** written in this break path (the `break` skips the write? No —) careful:
   `break` exits the loop, then `context.inferred[key] = tuple(results)` **does** run,
   so a truncated (with trailing Uninferable) result list IS cached.
5. If `_infer` raises `InferenceError`, nothing is cached and the error propagates.

`NodeNG._infer` default raises `InferenceError("No inference function for {node!r}.")`
(node_ng.py:551-558). Nodes that infer to themselves: `Module`, `ClassDef`,
`FunctionDef` (unless property — §11.2), `Lambda`, `Const`, `Slice`, `List/Tuple/Set`
(unless starred/namedexpr elements), `Dict` (unless `**` unpacking), `FrozenSet`,
`Super`, `Property`, `TypeAlias`, `TypeVar`, `TypeVarTuple`, `ParamSpec` (yields self).

### 4.1 Inference decorators — astroid/decorators.py

```python
def path_wrapper(func):
    @functools.wraps(func)
    def wrapped(node, context=None, _func=func, **kwargs):
        """Wrapper function handling context."""
        if context is None:
            context = InferenceContext()
        if context.push(node):
            return                       # already on path -> EMPTY generator

        yielded = set()
        for res in _func(node, context, **kwargs):
            # unproxy only true instance, not const, tuple, dict...
            if res.__class__.__name__ == "Instance":
                ares = res._proxied
            else:
                ares = res
            if ares not in yielded:
                yield res
                yielded.add(ares)
    return wrapped
```
(decorators.py:25-54). Notes:
- The membership test is **exact class name** `"Instance"` — `ExceptionInstance`,
  `Const`, `Generator` etc. are NOT unproxied for dedup. Two distinct `Instance`
  objects proxying the same ClassDef dedupe to one.
- `context.push` adds `(node, context.lookupname)`; the path entry is **never removed**
  within this context (only `restore_path`/clones rewind it). An empty return here is
  what eventually surfaces as `StopIteration` → "no values".

```python
def yes_if_nothing_inferred(func):
    def inner(*args, **kwargs):
        generator = func(*args, **kwargs)
        try:
            yield next(generator)
        except StopIteration:
            yield util.Uninferable          # empty -> ONE Uninferable
            return
        yield from generator
    return inner

def raise_if_nothing_inferred(func):
    def inner(*args, **kwargs):
        generator = func(*args, **kwargs)
        try:
            yield next(generator)
        except StopIteration as error:
            if error.args:
                raise InferenceError(**error.args[0]) from error
            raise InferenceError("StopIteration raised without any error information.") from error
        except RecursionError as error:
            raise InferenceError(f"RecursionError raised with limit {sys.getrecursionlimit()}.") from error
        yield from generator
    return inner
```
(decorators.py:57-96). So: a generator-returning `_infer` that produces **nothing**
either yields one `Uninferable` (yes_…) or raises `InferenceError` (raise_…).
Generators that `return {dict}` convert that dict into `InferenceError` kwargs
(several `assigned_stmts` implementations use this).

Decorator assignments per node (must match exactly):

| node._infer | decorators |
|---|---|
| `Name`, `AssignName`, `AssignAttr`, `Attribute`, `Subscript`, `Call`, `AugAssign`, `BoolOp`, `UnaryOp`, `Import`, `ImportFrom`, `Global`, `EmptyNode` | `raise_if_nothing_inferred` + `path_wrapper` |
| `BinOp` | **`yes_if_nothing_inferred`** + `path_wrapper` (node_classes.py:1556-1563) |
| `IfExp`, `BaseContainer`, `Arguments`, `AssignName.infer_lhs`, `Subscript.infer_lhs` | `raise_if_nothing_inferred` only (no path_wrapper) |
| `Const`, `Dict`, `Slice`, `Module`, `ClassDef`, `FunctionDef`, `Lambda`, `Compare`, `JoinedStr`, `FormattedValue`, etc. | none |

### 4.2 Inference tips — astroid/inference_tip.py

`inference_tip(f)` returns a transform that sets `node._explicit_inference =
_inference_tip_cached(f)`. The cached wrapper (inference_tip.py:37-86):

- key `(func, node, context)`; **context normalized to None if `context.is_empty()`**.
- recursion guard `_CURRENTLY_INFERRING` on `(func, node)`: re-entry raises
  `UseInferenceDefault` (falls back to default inference).
- the cache is an `OrderedDict` capped at **64** entries (LRU pop of oldest).

Transforms (including inference tips) are applied by `TransformVisitor` once per module
build, bottom-up; predicates decide which nodes get `_explicit_inference` (see §19 for
the builtin predicates).

---

## 5. `util.safe_infer` — astroid/util.py:137-159

```python
def safe_infer(node, context=None):
    """Return None if inference failed or if there is some ambiguity (more than
    one node has been inferred)."""
    if isinstance(node, UninferableBase):
        return node
    try:
        inferit = node.infer(context=context)
        value = next(inferit)
    except (InferenceError, StopIteration):
        return None
    try:
        next(inferit)
        return None  # None if there is ambiguity on the inferred node
    except InferenceError:
        return None  # there is some kind of ambiguity
    except StopIteration:
        return value
```

This is *the* conservatism primitive: pylint's `utils.safe_infer` is a richer variant
(it accepts multiple results when they are all the same type — documented in the
pylint-utils note), but astroid-internal callers use this one. Exactly-one result or
bust. Note `safe_infer(Uninferable)` returns `Uninferable` (truthiness False!).

---

## 6. `bases._infer_stmts` — astroid/bases.py:153-204

Used by name lookup, igetattr (Module/Class/Function/Instance), ImportFrom, Global.

```python
def _infer_stmts(stmts, context, frame=None):
    inferred = False
    constraint_failed = False
    if context is not None:
        name = context.lookupname
        context = context.clone()
        if name is not None:
            constraints = context.constraints.get(name, {})
        else:
            constraints = {}
    else:
        name = None
        constraints = {}
        context = InferenceContext()

    for stmt in stmts:
        if isinstance(stmt, UninferableBase):
            yield stmt
            inferred = True
            continue
        context.lookupname = stmt._infer_name(frame, name)
        try:
            stmt_constraints: set[Constraint] = set()
            for constraint_stmt, potential_constraints in constraints.items():
                if not constraint_stmt.parent_of(stmt):
                    stmt_constraints.update(potential_constraints)
            for inf in stmt.infer(context=context):
                if all(constraint.satisfied_by(inf) for constraint in stmt_constraints):
                    yield inf
                    inferred = True
                else:
                    constraint_failed = True
        except NameInferenceError:
            continue
        except InferenceError:
            yield Uninferable
            inferred = True

    if not inferred and constraint_failed:
        yield Uninferable
    elif not inferred:
        raise InferenceError("Inference failed for all members of {stmts!r}.", ...)
```

Notes:
- `stmt._infer_name(frame, name)`: default returns None (node_ng.py:547-549); overridden
  to return `name` for `ImportFrom`/`Import` (_base_nodes.py:145-146), `Global`
  (node_classes.py:2980-2981), `Try`/`TryStar` (3882, 3983), and for `Arguments` returns
  `name` only `if self.parent is frame` (node_classes.py:779-782).
- An `InferenceError` from one stmt yields **Uninferable** and continues;
  `NameInferenceError` is silently skipped.

---

## 7. Name lookup

### 7.1 Entry: `LookupMixIn.lookup` — astroid/nodes/_base_nodes.py:259-290

```python
@lru_cache  # noqa
def lookup(self, name):
    return self.scope().scope_lookup(self, name)
```
**`@lru_cache` on a method** — unbounded cache keyed by `(self, name)` identity. The
cache lives for the process (cleared by `AstroidManager.clear_cache`, manager.py:459-470).
Returns `(scope_node, list_of_assignment_nodes)`.

`NodeNG.scope()` walks to the first parent defining a scope (Module, FunctionDef,
ClassDef, Lambda, GeneratorExp/ListComp/SetComp/DictComp) — node_ng.py:300-309;
overridden by `LocalsDictNodeNG.scope()` → self (mixin.py:52-58).
Special: `Decorators.scope()` (node_classes.py:2219+) returns `self.parent.parent.scope()`
(decorators live OUTSIDE the function scope). `Arguments` default values similarly get
redirected via the `offset=-1`/parent-frame logic below.

### 7.2 `_scope_lookup` (shared tail) — astroid/nodes/scoped_nodes/mixin.py:78-98

```python
def _scope_lookup(self, node, name, offset=0):
    try:
        stmts = _filter_stmts(node, self.locals[name], self, offset)
    except KeyError:
        stmts = ()
    if stmts:
        return self, stmts

    # Handle nested scopes: since class names do not extend to nested
    # scopes (e.g., methods), we find the next enclosing non-class scope
    pscope = self.parent and self.parent.scope()
    while pscope is not None:
        if not isinstance(pscope, scoped_nodes.ClassDef):
            return pscope.scope_lookup(node, name)
        pscope = pscope.parent and pscope.parent.scope()

    # self is at the top level of a module, or is enclosed only by ClassDefs
    return builtin_lookup(name)
```

**Class scopes are skipped** when walking outward (Python closure semantics).
`builtin_lookup(name)` (scoped_nodes/utils.py:17-35): returns
`(builtins_module, builtins_module.locals.get(name, []))`; `"__dict__"` returns `()`.

### 7.3 Per-scope `scope_lookup` overrides

**Module** (scoped_nodes.py:312-333): if `name in {"__name__","__doc__","__file__","__path__","__package__"}`
and not shadowed in `self.locals`, return `self, self.getattr(name)` (or `self, []` on
AttributeInferenceError). Else `_scope_lookup`.

**Lambda** (scoped_nodes.py:995-1023) and **FunctionDef** (scoped_nodes.py:1658-1682):

```python
def scope_lookup(self, node, name, offset=0):           # FunctionDef
    if name == "__class__":
        if self.parent and isinstance(frame := self.parent.frame(), ClassDef):
            return self, [frame]

    if (self.args.defaults and node in self.args.defaults) or (
        self.args.kw_defaults and node in self.args.kw_defaults
    ):
        if not self.parent:
            raise ParentMissingError(target=self)
        frame = self.parent.frame()
        # line offset to avoid that def func(f=func) resolve the default
        # value to the defined function
        offset = -1
    else:
        # check this is not used in function decorators
        frame = self
    return frame._scope_lookup(node, name, offset)
```
(`node in self.args.defaults` is a **list membership identity check** on the default
value expressions; note it only catches the default node itself, not nested children —
faithful port must keep that.)

**ClassDef** (scoped_nodes.py:2104-2155):

```python
def scope_lookup(self, node, name, offset=0):
    # If the name looks like a builtin name, just try to look
    # into the upper scope of this class. ...
    lookup_upper_frame = (
        isinstance(node.parent, node_classes.Decorators)
        and name in AstroidManager().builtins_module
    )
    if (
        any(
            node == base or (base.parent_of(node) and not self.type_params)
            for base in self.bases
        )
        or lookup_upper_frame
    ):
        # name in the bases of a class -> resolve in enclosing frame
        if not self.parent:
            raise ParentMissingError(target=self)
        frame = self.parent.frame()
        # line offset to avoid that class A(A) resolve the ancestor to
        # the defined class
        offset = -1
    else:
        frame = self
    return frame._scope_lookup(node, name, offset)
```

**ComprehensionScope** (mixin.py:199-205): `scope_lookup = LocalsDictNodeNG._scope_lookup`.

### 7.4 `_filter_stmts` — astroid/filter_statements.py:50-240 — FULL ALGORITHM

This underpins both inference and pylint's used-before-assignment family. Quoted in
full (with the helpers) because every branch matters:

```python
def _get_filtered_node_statements(base_node, stmt_nodes):
    statements = [(node, node.statement()) for node in stmt_nodes]
    # Next we check if we have ExceptHandlers that are parent
    # of the underlying variable, in which case the last one survives
    if len(statements) > 1 and all(
        isinstance(stmt, nodes.ExceptHandler) for _, stmt in statements
    ):
        statements = [
            (node, stmt) for node, stmt in statements if stmt.parent_of(base_node)
        ]
    return statements


def _is_from_decorator(node) -> bool:
    """Return whether the given node is the child of a decorator."""
    return any(isinstance(parent, nodes.Decorators) for parent in node.node_ancestors())


def _get_if_statement_ancestor(node):
    """Return the first parent node that is an If node (or None)."""
    for parent in node.node_ancestors():
        if isinstance(parent, nodes.If):
            return parent
    return None


def _filter_stmts(base_node, stmts, frame, offset):
    # if offset == -1, my actual frame is not the inner frame but its parent
    #
    # class A(B): pass
    #
    # we need this to resolve B correctly
    if offset == -1:
        myframe = base_node.frame().parent.frame()
    else:
        myframe = base_node.frame()
        # If the frame of this node is the same as the statement
        # of this node, then the node is part of a class or
        # a function definition and the frame of this node should be the
        # the upper frame, not the frame of the definition.
        # For more information why this is important,
        # see Pylint issue #295.
        # For example, for 'b', the statement is the same
        # as the frame / scope:
        #
        # def test(b=1):
        #     ...
        if base_node.parent and base_node.statement() is myframe and myframe.parent:
            myframe = myframe.parent.frame()

    mystmt: _base_nodes.Statement | None = None
    if base_node.parent:
        mystmt = base_node.statement()

    # line filtering if we are in the same frame
    #
    # take care node may be missing lineno information (this is the case for
    # nodes inserted for living objects)
    if myframe is frame and mystmt and mystmt.fromlineno is not None:
        assert mystmt.fromlineno is not None, mystmt
        mylineno = mystmt.fromlineno + offset
    else:
        # disabling lineno filtering
        mylineno = 0

    _stmts: list[nodes.NodeNG] = []
    _stmt_parents = []
    statements = _get_filtered_node_statements(base_node, stmts)
    for node, stmt in statements:
        # line filtering is on and we have reached our location, break
        if stmt.fromlineno and stmt.fromlineno > mylineno > 0:
            break
        # Ignore decorators with the same name as the
        # decorated function
        # Fixes issue #375
        if mystmt is stmt and _is_from_decorator(base_node):
            continue
        if node.has_base(base_node):
            break

        if isinstance(node, nodes.EmptyNode):
            # EmptyNode does not have assign_type(), so just add it and move on
            _stmts.append(node)
            continue

        assign_type = node.assign_type()
        _stmts, done = assign_type._get_filtered_stmts(base_node, node, _stmts, mystmt)
        if done:
            break

        optional_assign = assign_type.optional_assign
        if optional_assign and assign_type.parent_of(base_node):
            # we are inside a loop, loop var assignment is hiding previous
            # assignment
            _stmts = [node]
            _stmt_parents = [stmt.parent]
            continue

        if isinstance(assign_type, nodes.NamedExpr):
            # If the NamedExpr is in an if statement we do some basic control flow inference
            if_parent = _get_if_statement_ancestor(assign_type)
            if if_parent:
                # If the if statement is within another if statement we append the node
                # to possible statements
                if _get_if_statement_ancestor(if_parent):
                    optional_assign = False
                    _stmts.append(node)
                    _stmt_parents.append(stmt.parent)
                # Else we assume that it will be evaluated
                else:
                    _stmts = [node]
                    _stmt_parents = [stmt.parent]
            else:
                _stmts = [node]
                _stmt_parents = [stmt.parent]

        # XXX comment various branches below!!!
        try:
            pindex = _stmt_parents.index(stmt.parent)
        except ValueError:
            pass
        else:
            # we got a parent index, this means the currently visited node
            # is at the same block level as a previously visited node
            if _stmts[pindex].assign_type().parent_of(assign_type):
                # both statements are not at the same block level
                continue
            # if currently visited node is following previously considered
            # assignment and both are not exclusive, we can drop the
            # previous one. For instance in the following code ::
            #
            #   if a:
            #     x = 1
            #   else:
            #     x = 2
            #   print x
            #
            # we can't remove neither x = 1 nor x = 2 when looking for 'x'
            # of 'print x'; while in the following ::
            #
            #   x = 1
            #   x = 2
            #   print x
            #
            # we can remove x = 1 when we see x = 2
            #
            # moreover, on loop assignment types, assignment won't
            # necessarily be done if the loop has no iteration, so we don't
            # want to clear previous assignments if any (hence the test on
            # optional_assign)
            if not (optional_assign or nodes.are_exclusive(_stmts[pindex], node)):
                del _stmt_parents[pindex]
                del _stmts[pindex]

        # If base_node and node are exclusive, then we can ignore node
        if nodes.are_exclusive(base_node, node):
            continue

        # An AssignName node overrides previous assignments if:
        #   1. node's statement always assigns
        #   2. node and base_node are in the same block (i.e., has the same parent as base_node)
        if isinstance(node, (nodes.NamedExpr, nodes.AssignName)):
            if isinstance(stmt, nodes.ExceptHandler):
                # If node's statement is an ExceptHandler, then it is the variable
                # bound to the caught exception. If base_node is not contained within
                # the exception handler block, node should override previous assignments;
                # otherwise, node should be ignored, as an exception variable
                # is local to the handler block.
                if stmt.parent_of(base_node):
                    _stmts = []
                    _stmt_parents = []
                else:
                    continue
            elif not optional_assign and mystmt and stmt.parent is mystmt.parent:
                _stmts = []
                _stmt_parents = []
        elif isinstance(node, nodes.DelName):
            # Remove all previously stored assignments
            _stmts = []
            _stmt_parents = []
            continue
        # Add the new assignment
        _stmts.append(node)
        if isinstance(node, nodes.Arguments) or isinstance(
            node.parent, nodes.Arguments
        ):
            # Special case for _stmt_parents when node is a function parameter;
            # in this case, stmt is the enclosing FunctionDef, which is what we
            # want to add to _stmt_parents, not stmt.parent. ...
            _stmt_parents.append(stmt)
        else:
            _stmt_parents.append(stmt.parent)
    return _stmts
```

Inputs: `stmts` is `frame.locals[name]` — the list of *defining nodes* in **insertion
order** (set by the rebuilder in source order via `set_local`, mixin.py:100-111; the
list can contain `AssignName`, `DelName`, `FunctionDef`, `ClassDef`, `Import`,
`ImportFrom`, `Arguments`(for vararg/kwarg), `EmptyNode`, …).

Supporting definitions:

- `NodeNG.statement()` (node_ng.py:276-285): nearest enclosing `is_statement` node
  including self; Module raises `StatementMissing`.
- `assign_type()` dispatch:
  - `FilterStmtsBaseNode.assign_type` → self (`FunctionDef`, `ClassDef`, `Lambda`,
    `Import`, `ImportFrom`) — _base_nodes.py:90-102
  - `AssignTypeNode.assign_type` → self (`Assign`/`AnnAssign`/`AugAssign`/`Delete`/
    `For`/`With`/`ExceptHandler`/`NamedExpr`/`MatchAs`/`MatchStar`/`MatchMapping`/
    `TypeAlias`/`TypeVar`/…) — _base_nodes.py:105-119
  - `ParentAssignNode.assign_type` → `self.parent.assign_type()` (`AssignName`,
    `AssignAttr`, `DelName`, `DelAttr`, `Starred`, `Tuple`/`List` in store ctx) —
    _base_nodes.py:122-126
  - `Comprehension.assign_type` → self — node_classes.py:1983-1989
- `optional_assign` is the class attr; True only for `For` (and subclass `AsyncFor`)
  and `Comprehension` (node_classes.py:1951-1952; For sets `optional_assign = True`).
- `_get_filtered_stmts` variants:
  - `FilterStmtsBaseNode._get_filtered_stmts` (_base_nodes.py:93-99):
    `if self.statement() is mystmt: return [node], True` else `(_stmts, False)`.
  - `AssignTypeNode._get_filtered_stmts` (_base_nodes.py:111-119):
    `if self is mystmt: return _stmts, True`; `if self.statement() is mystmt:
    return [node], True`; else `(_stmts, False)`.
  - `Comprehension._get_filtered_stmts` (node_classes.py:1991-2005):
    `if self is mystmt and isinstance(lookup_node, (Const, Name)): return [lookup_node], True`;
    `elif self.statement() is mystmt: return [node], True`; else `(stmts, False)`.
- `has_base`: ClassDef returns `node in self.bases` (identity list membership,
  scoped_nodes.py:2248-2256); all other nodes return False (node_ng.py:582-589).

### 7.5 `are_exclusive` — astroid/nodes/node_classes.py:116-186

Quoted verbatim (used by `_filter_stmts` and by pylint checkers directly):

```python
def are_exclusive(stmt1, stmt2, exceptions=None) -> bool:
    # index stmt1's parents
    stmt1_parents = {}
    children = {}
    previous = stmt1
    for node in stmt1.node_ancestors():
        stmt1_parents[node] = 1
        children[node] = previous
        previous = node
    # climb among stmt2's parents until we find a common parent
    previous = stmt2
    for node in stmt2.node_ancestors():
        if node in stmt1_parents:
            # if the common parent is a If or Try statement, look if
            # nodes are in exclusive branches
            if isinstance(node, If) and exceptions is None:
                c2attr, c2node = node.locate_child(previous)
                c1attr, c1node = node.locate_child(children[node])
                if "test" in (c1attr, c2attr):
                    # If any node is `If.test`, then it must be inclusive with
                    # the other node (`If.body` and `If.orelse`)
                    return False
                if c1attr != c2attr:
                    # different `If` branches (`If.body` and `If.orelse`)
                    return True
            elif isinstance(node, Try):
                c2attr, c2node = node.locate_child(previous)
                c1attr, c1node = node.locate_child(children[node])
                if c1node is not c2node:
                    first_in_body_caught_by_handlers = (
                        c2attr == "handlers" and c1attr == "body"
                        and previous.catch(exceptions))
                    second_in_body_caught_by_handlers = (
                        c2attr == "body" and c1attr == "handlers"
                        and children[node].catch(exceptions))
                    first_in_else_other_in_handlers = (
                        c2attr == "handlers" and c1attr == "orelse")
                    second_in_else_other_in_handlers = (
                        c2attr == "orelse" and c1attr == "handlers")
                    if any((first_in_body_caught_by_handlers,
                            second_in_body_caught_by_handlers,
                            first_in_else_other_in_handlers,
                            second_in_else_other_in_handlers)):
                        return True
                elif c2attr == "handlers" and c1attr == "handlers":
                    return previous is not children[node]
            return False
        previous = node
    return False
```

`ExceptHandler.catch(exceptions)` (node_classes.py:2652-2659): True if `self.type is
None or exceptions is None`, else `any(node.name in exceptions for node in
self.type._get_name_nodes())`.

---

## 8. `Name` / `AssignName` / `AssignAttr` inference

### 8.1 `Name._infer` — node_classes.py:568-596 (decorators: raise_if_nothing_inferred, path_wrapper)

```python
def _infer(self, context=None, **kwargs):
    from astroid.constraint import get_constraints
    from astroid.helpers import _higher_function_scope

    frame, stmts = self.lookup(self.name)
    if not stmts:
        # Try to see if the name is enclosed in a nested function
        # and use the higher (first function) scope for searching.
        parent_function = _higher_function_scope(self.scope())
        if parent_function:
            _, stmts = parent_function.lookup(self.name)

        if not stmts:
            raise NameInferenceError(name=self.name, scope=self.scope(), context=context)
    context = copy_context(context)
    context.lookupname = self.name
    context.constraints[self.name] = get_constraints(self, frame)

    return _infer_stmts(stmts, context, frame)
```

`_higher_function_scope` (helpers.py, bottom): walks `current = node` up while
`current.parent` is not a `FunctionDef`; returns that FunctionDef or None.
`AssignName.infer_lhs` (node_classes.py:454-481) is the **same algorithm** (without
path_wrapper). Note: `NameInferenceError` subclasses `InferenceError`.

### 8.2 `AssignName._infer` — node_classes.py:440-452

```python
def _infer(self, context=None, **kwargs):
    if isinstance(self.parent, AugAssign):
        return self.parent.infer(context)

    stmts = list(self.assigned_stmts(context=context))
    return _infer_stmts(stmts, context)
```
(decorators: raise_if_nothing_inferred + path_wrapper). `AssignAttr._infer`
(node_classes.py:1165-1177) is identical; `AssignAttr.infer_lhs` =
`_infer_attribute` (§12.1).

---

## 9. The `assigned_stmts` protocol — astroid/protocols.py

`x.assigned_stmts(node=..., context=..., assign_path=...)` yields the *value nodes*
(not yet inferred, except where noted) bound to an assignment target. `assign_path`
is a list of indices recording the position of the target within nested tuples.

Wiring (class attr assignments):
- `AssignName.assigned_stmts = protocols.assend_assigned_stmts` (node_classes.py:435)
- `AssignAttr.assigned_stmts = protocols.assend_assigned_stmts` (1157)
- `Assign.assigned_stmts = protocols.assign_assigned_stmts` (1251)
- `AugAssign.assigned_stmts = protocols.assign_assigned_stmts` (1373)
- `AnnAssign.assigned_stmts = protocols.assign_annassigned_stmts` (1310)
- `Tuple/List.assigned_stmts = protocols.sequence_assigned_stmts` (4064 / 3263 area)
- `For/Comprehension.assigned_stmts = protocols.for_assigned_stmts` (1978; For ~2662)
- `With.assigned_stmts = protocols.with_assigned_stmts` (~4471)
- `Starred.assigned_stmts = protocols.starred_assigned_stmts` (3671)
- `Arguments.assigned_stmts = protocols.arguments_assigned_stmts` (774)
- `ExceptHandler.assigned_stmts = protocols.excepthandler_assigned_stmts` (2616)
- `NamedExpr.assigned_stmts = protocols.named_expr_assigned_stmts`
- `MatchMapping/MatchStar/MatchAs` → respective `match_*_assigned_stmts`
- `TypeAlias.assigned_stmts = protocols.assign_assigned_stmts` (4136-4146)
- `TypeVar/TypeVarTuple/ParamSpec → generic_type_assigned_stmts` (yields `Const(None)`)

Implementations (protocols.py):

**`assend_assigned_stmts`** (343-349): `return self.parent.assigned_stmts(node=self,
context=context)` — delegate to the parent (Assign/For/Tuple/…).

**`assign_assigned_stmts`** (447-466, raise_if_nothing_inferred):
```python
if not assign_path:
    yield self.value
    return None
yield from _resolve_assignment_parts(self.value.infer(context), assign_path, context)
return {...}   # InferenceError payload if nothing was yielded
```
So `x = expr` yields the raw `expr` node (the caller then infers it).

**`assign_annassigned_stmts`** (469-479): wraps the above, mapping `None` → `Uninferable`.
NOTE an `AnnAssign` without value (`x: int`) yields its `self.value` which is None →
this branch turns it into Uninferable.

**`_resolve_assignment_parts`** (482-519) — unpacking `a, (b, c) = ...`:
```python
def _resolve_assignment_parts(parts, assign_path, context):
    assign_path = assign_path[:]
    index = assign_path.pop(0)
    for part in parts:
        assigned = None
        if isinstance(part, nodes.Dict):
            try:
                assigned, _ = part.items[index]     # key at index (dict iteration)
            except IndexError:
                return
        elif hasattr(part, "getitem"):
            index_node = nodes.Const(index)
            try:
                assigned = part.getitem(index_node, context)
            except (AstroidTypeError, AstroidIndexError):
                return
        if not assigned:
            return
        if not assign_path:
            yield assigned          # don't infer the last part
        elif isinstance(assigned, util.UninferableBase):
            return
        else:
            try:
                yield from _resolve_assignment_parts(
                    assigned.infer(context), assign_path, context)
            except InferenceError:
                return
```

**`sequence_assigned_stmts`** (319-340): find `index = self.elts.index(node)` (identity
membership; `ValueError` → `InferenceError`), `assign_path.insert(0, index)`, delegate
to `self.parent.assigned_stmts(node=self, context, assign_path)`.

**`for_assigned_stmts`** (290-316, raise_if_nothing_inferred):
```python
if isinstance(self, nodes.AsyncFor) or getattr(self, "is_async", False):
    # Skip inferring of async code for now
    return {...}                       # -> InferenceError
if assign_path is None:
    for lst in self.iter.infer(context):
        if isinstance(lst, (nodes.Tuple, nodes.List)):
            yield from lst.elts        # each element is a candidate value
else:
    yield from _resolve_looppart(self.iter.infer(context), assign_path, context)
```
Bail-outs: iterables that don't infer to a literal Tuple/List yield nothing →
`raise_if_nothing_inferred` → InferenceError → name infers to Uninferable upstream.
`_resolve_looppart` (249-287) is the loop analogue of `_resolve_assignment_parts`:
pops the first index, for each inferred iterable part calls `part.itered()`
(skip non-itered / TypeError), special-case `itered[index] is Const|Name → itered=[part]`,
then for each `stmt in itered` does `stmt.getitem(Const(index))` with
`(AttributeError, AstroidTypeError, AstroidIndexError)` → continue; recurses if path
remains; `Uninferable` → break.

**`with_assigned_stmts`** (605-682, raise_if_nothing_inferred): finds the context
manager expr for this target via `next(mgr for (mgr, vars) in self.items if vars == node)`,
then `_infer_context_manager` (567-602):
- infer the mgr (first result; StopIteration → InferenceError);
- if `bases.Generator`: only handled when the generator function is decorated with
  `contextlib.contextmanager` (checks each decorator's first inferred value qname ==
  `"contextlib.contextmanager"`); yields `next(inferred.infer_yield_types())`;
- elif `bases.Instance`: `enter = next(inferred.igetattr("__enter__"))`; must be a
  `BoundMethod` else InferenceError; yields `enter.infer_call_result(self, context)`;
- else InferenceError.
With an `assign_path`, walks `.elts[index]` through the result (Wrong type / IndexError
→ InferenceError).

**`excepthandler_assigned_stmts`** (522-564): see §18.

**`named_expr_assigned_stmts`** (685-701): `if self.target == node: yield from
self.value.infer(context)` else InferenceError.

**`starred_assigned_stmts`** (704-899, **yes_if_nothing_inferred**): full algorithm for
`a, *b = ...` and `for a, *b in ...`; statement must be Assign or For (else
InferenceError). For Assign: lhs must be a `BaseContainer` else yield Uninferable;
more than one Starred in targets → InferenceError; rhs := first inferred value of
`stmt.value` (errors → Uninferable); rhs must have `.itered()` (TypeError → Uninferable);
then deque-unpacks left-to-right then right-to-left and yields a synthetic
`nodes.List` of the middle. For For: similar, with `lookups` mapping; yields synthetic
List per element or Uninferable.

**`arguments_assigned_stmts`** (416-444): see §10.

**`match_*`**: MatchMapping/MatchStar yield nothing (→ Uninferable via
yes_if_nothing_inferred); MatchAs yields the Match subject only when it's a bare
capture (`case x:`) i.e. `self.pattern is None` and parent chain is
MatchCase→Match (930-945).

---

## 10. `Arguments` inference and `CallSite`

### 10.1 `Arguments._infer` — node_classes.py:1023-1032 (raise_if_nothing_inferred)

```python
if context is None or context.lookupname is None:
    raise InferenceError(node=self, context=context)
return _arguments_infer_argname(self, context.lookupname, context)
```

### 10.2 `arguments_assigned_stmts` — protocols.py:416-444

```python
try:
    node_name = node.name
except AttributeError:
    node_name = None

if context and context.callcontext:
    callee = context.callcontext.callee
    while hasattr(callee, "_proxied"):
        callee = callee._proxied
else:
    return _arguments_infer_argname(self, node_name, context)
if node and getattr(callee, "name", None) == node.frame().name:
    # reset call context/name
    callcontext = context.callcontext
    context = copy_context(context)
    context.callcontext = None
    args = arguments.CallSite(callcontext, context=context)
    return args.infer_argument(self.parent, node_name, context)
return _arguments_infer_argname(self, node_name, context)
```
The `callee.name == node.frame().name` guard is a *name string comparison* (not
identity) — calls bind arguments only when the call context's callee has the same name
as the parameter's function.

### 10.3 `_arguments_infer_argname` — protocols.py:352-413 (no call context)

```python
if not self.arguments:
    yield util.Uninferable
    return

args = [arg for arg in self.arguments if arg.name not in [self.vararg, self.kwarg]]
functype = self.parent.type
# first argument of instance/class method
if (args and getattr(self.arguments[0], "name", None) == name
        and functype != "staticmethod"):
    cls = self.parent.parent.scope()
    is_metaclass = isinstance(cls, nodes.ClassDef) and cls.type == "metaclass"
    # If this is a metaclass, then the first argument will always
    # be the class, not an instance.
    if context.boundnode and isinstance(context.boundnode, bases.Instance):
        cls = context.boundnode._proxied
    if is_metaclass or functype == "classmethod":
        yield cls
        return
    if functype == "method":
        yield cls.instantiate_class()
        return

if context and context.callcontext:
    callee = context.callcontext.callee
    while hasattr(callee, "_proxied"):
        callee = callee._proxied
    if getattr(callee, "name", None) == self.parent.name:
        call_site = arguments.CallSite(context.callcontext, context.extra_context)
        yield from call_site.infer_argument(self.parent, name, context)
        return

if name == self.vararg:
    vararg = nodes.const_factory(())
    vararg.parent = self
    if not args and self.parent.name == "__init__":
        cls = self.parent.parent.scope()
        vararg.elts = [cls.instantiate_class()]
    yield vararg
    return
if name == self.kwarg:
    kwarg = nodes.const_factory({})
    kwarg.parent = self
    yield kwarg
    return
# if there is a default value, yield it. And then yield Uninferable to reflect
# we can't guess given argument value
try:
    context = copy_context(context)
    yield from self.default_value(name).infer(context)
    yield util.Uninferable
except NoDefault:
    yield util.Uninferable
```
=> **`self` infers to `Instance(cls)`, `cls` to the ClassDef**; vararg → empty Tuple,
kwarg → empty Dict; defaulted params → default value(s) **plus Uninferable**;
everything else → Uninferable. This is central to typecheck checks on `self.attr`.

`Arguments.default_value(argname)` (node_classes.py:930-955): checks kwonly first
(`kw_defaults[index]`, None → NoDefault), else positional with offset
`idx = index - (len(args) - len(self.defaults) - len(self.kw_defaults))`; `idx >= 0`
→ `self.defaults[idx]`, else NoDefault. (NOTE the formula includes
`len(self.kw_defaults)` — a long-standing quirk; replicate as-is.)

### 10.4 `CallSite` — astroid/arguments.py (full file read; key parts)

Constructor (15-54): `_unpacked_args = _unpack_args(args)`,
`_unpacked_kwargs = _unpack_keywords(keywords)`; `positional_arguments` filters out
`UninferableBase` entries; `keyword_arguments` likewise.

`_unpack_args` (123-139): `Starred` arg → `safe_infer(arg.value)`; Uninferable or no
`.elts` → append Uninferable; else extend with `inferred.elts`. Other args appended raw.

`_unpack_keywords` (88-121): `name is None` (i.e. `**expr`) → `safe_infer(value)` must
be a `nodes.Dict` else Uninferable; each dict key must safe-infer to `Const(str)` else
entry Uninferable; duplicate key → mark `duplicated_keywords` and store Uninferable.

`has_invalid_arguments()` = `len(positional_arguments) != len(_unpacked_args)`;
`has_invalid_keywords()` analogous. (pylint typecheck's CallSite use mirrors this.)

`infer_argument(funcnode, name, context)` (141-309), exact flow:
1. funcnode must be FunctionDef|Lambda else InferenceError.
2. `name in self.duplicated_keywords` → InferenceError.
3. If `name` in keyword_arguments → return `kwvalue.infer(context)`.
4. If `len(positional_arguments) > len(funcnode.args.args)` and no vararg and no
   posonlyargs → InferenceError ("Too many positional arguments…").
5. `positional = positional_arguments[:len(funcnode.args.args)]`;
   `vararg = positional_arguments[len(funcnode.args.args):]`.
6. `argindex = None` if name is the vararg/kwarg name, else
   `funcnode.args.find_argname(name)[0]` (index within `arguments` =
   posonlyargs + args + vararg_node + kwonlyargs + kwarg_node).
7. Move keyword args that fill missing positionals into `positional` (mutates a copy).
8. If `argindex is not None`:
   - `argindex == 0` and functype in {method, classmethod}:
     - boundnode None & method & positional → return `positional[0].infer(context)`
     - boundnode None → `boundnode = funcnode.parent.frame()`
     - boundnode is ClassDef and the method's scope is that class's metaclass →
       return `iter((boundnode,))`
     - method → instantiate boundnode if not Instance; return `iter((boundnode,))`
     - classmethod → `iter((boundnode,))`
   - if functype in {method, classmethod} and boundnode: `argindex -= 1`
   - `return self.positional_arguments[argindex].infer(context)` (IndexError → fall on).
9. `funcnode.args.kwarg == name` → invalid keywords → InferenceError; else synthesize
   a `nodes.Dict` of leftover kwargs (kwonly excluded) and return it.
10. `funcnode.args.vararg == name` → invalid args → InferenceError; else synthesize a
    `nodes.Tuple` of `vararg` and return it.
11. default value: `funcnode.args.default_value(name).infer(context)`; NoDefault →
    final InferenceError ("No value found for argument {arg}…").

---

## 11. Call inference

### 11.1 `Call._infer` — node_classes.py:1744-1784 (raise_if_nothing_inferred + path_wrapper)

```python
def _infer(self, context=None, **kwargs):
    callcontext = copy_context(context)
    callcontext.boundnode = None
    if context is not None:
        callcontext.extra_context = self._populate_context_lookup(context.clone())

    for callee in self.func.infer(context):
        if isinstance(callee, util.UninferableBase):
            yield callee
            continue
        try:
            if hasattr(callee, "infer_call_result"):
                callcontext.callcontext = CallContext(
                    args=self.args, keywords=self.keywords, callee=callee)
                yield from callee.infer_call_result(caller=self, context=callcontext)
        except InferenceError:
            continue
    return InferenceErrorInfo(node=self, context=context)
```
`_populate_context_lookup` maps each argument node (for `Starred`, its `.value`) and
each keyword value to the original context — consumed by `NodeNG.infer`'s
`extra_context` swap so argument inference escapes the callee's path.

Bailouts: non-callable callees (no `infer_call_result` attr) are silently skipped;
`InferenceError` from one callee skipped. If nothing yielded at all →
raise_if_nothing_inferred → InferenceError.

### 11.2 `FunctionDef._infer` — scoped_nodes.py:1521-1541

If decorated and `bases._is_property(self)` → yields an `objects.Property` wrapper;
else yields self. `_is_property` (bases.py:69-108): checks `decoratornames()`
against `PROPERTIES = {"builtins.property", "abc.abstractproperty",
"functools.cached_property", "enum.property"(3.11+)}`, then *unqualified* last-segment
match against `POSSIBLE_PROPERTIES = {"cached_property", "cachedproperty",
"lazyproperty", "lazy_property", "reify", "lazyattribute", "lazy_attribute",
"LazyProperty", "lazy", "cache_readonly", "DynamicClassAttribute"}`, then inferred
decorator ClassDefs that are subtypes of a property class (including the
`Subscript`-base `functools.cached_property` special case, bases.py:94-106).

### 11.3 `FunctionDef.infer_call_result` — scoped_nodes.py:1555-1636 — EXACT

```python
def infer_call_result(self, caller, context=None):
    if context is None:
        context = InferenceContext()
    if self.is_generator():
        if isinstance(self, AsyncFunctionDef):
            generator_cls = bases.AsyncGenerator
        else:
            generator_cls = bases.Generator
        result = generator_cls(self, generator_initial_context=context)
        yield result
        return
    # ... with_metaclass hack (scoped_nodes.py:1577-1615): if the function is
    # named "with_metaclass" with exactly 1 positional arg + vararg, builds a
    # hidden temporary ClassDef with the metaclass and bases from the call.
    returns = self._get_return_nodes_skip_functions()

    first_return = next(returns, None)
    if not first_return:
        if self.body:
            if self.is_abstract(pass_is_abstract=True, any_raise_is_abstract=True):
                yield util.Uninferable
            else:
                yield node_classes.Const(None)
            return

        raise InferenceError("The function does not have any return statements")

    for returnnode in itertools.chain((first_return,), returns):
        if returnnode.value is None:
            yield node_classes.Const(None)
        else:
            try:
                yield from returnnode.value.infer(context)
            except InferenceError:
                yield util.Uninferable
```

Definitive answers for E1111/E1128 semantics:

- **Generator function** (has a `Yield`/`YieldFrom` reachable without crossing a nested
  function or lambda — `is_generator()` scoped_nodes.py:1511-1519 intersects
  `_get_yield_nodes_skip_lambdas` and `_get_yield_nodes_skip_functions` sets) →
  yields a `bases.Generator` instance (AsyncFunctionDef → `AsyncGenerator`).
- **No `return` statement at all**:
  - body **non-empty** (docstring does NOT count — the rebuilder strips it into
    `doc_node`, rebuilder.py:75-88): `is_abstract(pass_is_abstract=True,
    any_raise_is_abstract=True)` → `Uninferable`; else → **`Const(None)`** (a fresh
    Const with `parent=SYNTHETIC_ROOT`, lineno None).
  - body **empty** (e.g. function whose only statement was its docstring) →
    `InferenceError` → the Call infers to nothing from this callee.
- **`return` without value** → fresh `Const(None)`.
- **`return expr`** → all inferred values of expr; an InferenceError there yields
  Uninferable (per return node).
- Return nodes are collected in source order, *skipping nested functions* but not
  other compound statements (`MultiLineBlockNode._get_return_nodes_skip_functions`,
  _base_nodes.py:206-211 — only iterates `_multi_line_block_fields`; for FunctionDef
  that's `("body",)`; `If` has `("body","orelse")`, `Try` has body/handlers/orelse/
  finalbody, etc. `Return._get_return_nodes_skip_functions` yields self,
  node_classes.py:3513-3514).

`is_abstract` — scoped_nodes.py:1475-1509 — exact (note the **first-statement-only**
loop, a real quirk):

```python
def is_abstract(self, pass_is_abstract=True, any_raise_is_abstract=False) -> bool:
    if self.decorators:
        for node in self.decorators.nodes:
            try:
                inferred = next(node.infer())
            except (InferenceError, StopIteration):
                continue
            if inferred and inferred.qname() in {
                "abc.abstractproperty", "abc.abstractmethod",
            }:
                return True

    for child_node in self.body:
        if isinstance(child_node, node_classes.Raise):
            if any_raise_is_abstract:
                return True
            if child_node.raises_not_implemented():
                return True
        return pass_is_abstract and isinstance(child_node, node_classes.Pass)
    # empty function is the same as function with a single "pass" statement
    if pass_is_abstract:
        return True
    return False
```
(the `for ... return` means ONLY `self.body[0]` is examined; a function whose first
statement is `raise <anything>` is "abstract" under `any_raise_is_abstract=True`,
hence infers to Uninferable not Const(None) — prevents E1128 false positives.)
`Raise.raises_not_implemented` (node_classes.py:3470-3479): any Name node inside
`self.exc` named exactly `"NotImplementedError"`.

`infer_yield_result` (scoped_nodes.py:1543-1553): for each `Yield` in
`self.nodes_of_class(Yield)`: bare yield → `Const(None)`; else if
`yield_.scope() == self` infer its value. (`Generator.infer_yield_types` delegates
here with the stored creation context, bases.py:703-704.)

### 11.4 `Lambda.infer_call_result` — scoped_nodes.py:987-993

`return self.body.infer(context)`. No Const(None) logic, no generator logic
(a lambda can't contain yield in Py3).

### 11.5 `ClassDef.infer_call_result` — scoped_nodes.py:2071-2102 — calling a class

```python
def infer_call_result(self, caller, context=None):
    if self.is_subtype_of("builtins.type", context) and len(caller.args) == 3:
        result = self._infer_type_call(caller, context)
        yield result
        return

    dunder_call = None
    try:
        metaclass = self.metaclass(context=context)
        if metaclass is not None:
            # Only get __call__ if it's defined locally for the metaclass.
            if "__call__" in metaclass.locals:
                dunder_call = next(metaclass.igetattr("__call__", context))
    except (AttributeInferenceError, StopIteration):
        pass

    if dunder_call and dunder_call.qname() != "builtins.type.__call__":
        context = bind_context_to_node(context, self)
        context.callcontext.callee = dunder_call
        yield from dunder_call.infer_call_result(caller, context)
    else:
        yield self.instantiate_class()
```
- `type("X", bases, attrs)` 3-arg form is reconstructed into a synthetic ClassDef
  (`_infer_type_call`, scoped_nodes.py:2017-2069; name must infer to Const str else
  Uninferable; bases must infer to Tuple/List; members Dict optional).
- A metaclass with a *locally defined* `__call__` intercepts instantiation.
- Otherwise → `instantiate_class()` (scoped_nodes.py:2303-2316):
  ```python
  try:
      if any(cls.name in EXCEPTION_BASE_CLASSES for cls in self.mro()):
          return objects.ExceptionInstance(self)
  except MroError:
      pass
  return bases.Instance(self)
  ```
  `EXCEPTION_BASE_CLASSES = frozenset({"Exception", "BaseException"})` — matched by
  **bare class name** anywhere in the MRO.

### 11.6 `UnboundMethod.infer_call_result` — bases.py:472-527

If `self._proxied.name == "__new__"` and the owner qname starts with `"builtins."`
(but isn't `builtins.type`): `_infer_builtin_new` — `cls(…)`-style: with ≥2 call args,
a Const second arg produces `const_factory(value)`; else infers `caller.args[0]`:
Uninferable → yield it, ClassDef → `Instance(cls)`, anything else →
`raise InferenceError` (note: the raise occurs after the first yield branch —
the loop body unconditionally hits `raise InferenceError` after handling one inferred
value; replicate exactly). Otherwise delegates to `self._proxied.infer_call_result`.

### 11.7 `BoundMethod.infer_call_result` — bases.py:656-674

`context = bind_context_to_node(context, self.bound)`. Special-case
`type.__new__(mcs, name, bases, attrs)` with exactly 4 args via `_infer_type_new_call`
(bases.py:555-654): validates mcs is ClassDef subtype of builtins.type, name a Const
str, bases a Tuple of ClassDefs, attrs a Dict (each check returning None → fall through
to normal call). Otherwise `super().infer_call_result(caller, context)`.

### 11.8 `BaseInstance.infer_call_result` (calling an instance) — bases.py:317-345

```python
context = bind_context_to_node(context, self)
inferred = False
# If the call is an attribute on the instance, we infer the attribute itself
if isinstance(caller, nodes.Call) and isinstance(caller.func, nodes.Attribute):
    for res in self.igetattr(caller.func.attrname, context):
        inferred = True
        yield res
# Otherwise we infer the call to the __call__ dunder normally
for node in self._proxied.igetattr("__call__", context):
    if isinstance(node, UninferableBase) or not node.callable():
        continue
    if isinstance(node, BaseInstance) and node._proxied is self._proxied:
        inferred = True
        yield node          # Prevent recursion.
        continue
    for res in node.infer_call_result(caller, context):
        inferred = True
        yield res
if not inferred:
    raise InferenceError(node=self, caller=caller, context=context)
```

`Instance.callable()` (bases.py:375-380): `self._proxied.getattr("__call__",
class_context=False)` succeeds → True, AttributeInferenceError → False.
`FunctionDef/Lambda/ClassDef.callable()` → True; `Generator.callable()` → False;
NodeNG default → False; `Const`: inherits Instance.callable → consults the proxied
builtin class (ints/strs have no `__call__` → False).

---

## 12. Attribute access

### 12.1 `Attribute._infer` / `AssignAttr.infer_lhs` → `_infer_attribute` — node_classes.py:1076-1112

```python
def _infer_attribute(node, context=None, **kwargs):
    for owner in node.expr.infer(context):
        if isinstance(owner, util.UninferableBase):
            yield owner
            continue

        context = copy_context(context)
        old_boundnode = context.boundnode
        try:
            context.boundnode = owner
            if isinstance(owner, (ClassDef, Instance)):
                frame = owner if isinstance(owner, ClassDef) else owner._proxied
                context.constraints[node.attrname] = get_constraints(node, frame=frame)
            if node.attrname == "argv" and owner.name == "sys":
                # sys.argv will never be inferable during static analysis
                yield util.Uninferable
            else:
                yield from owner.igetattr(node.attrname, context)
        except (AttributeInferenceError, InferenceError, AttributeError):
            pass
        finally:
            context.boundnode = old_boundnode
    return InferenceErrorInfo(node=node, context=context)
```
Bailouts: failed igetattr on one owner is silently swallowed; combined with
raise_if_nothing_inferred + path_wrapper at the `Attribute._infer` level
(node_classes.py:2925-2930). Note the hardcoded `sys.argv` special case.

### 12.2 `Instance.getattr` — `BaseInstance.getattr` — bases.py:243-272 — EXACT ORDER

```python
def getattr(self, name, context=None, lookupclass=True):
    try:
        values = self._proxied.instance_attr(name, context)
    except AttributeInferenceError as exc:
        if self.special_attributes and name in self.special_attributes:
            return [self.special_attributes.lookup(name)]
        if lookupclass:
            # Class attributes not available through the instance
            # unless they are explicitly defined.
            return self._proxied.getattr(name, context, class_context=False)
        raise AttributeInferenceError(target=self, attribute=name, context=context) from exc
    # since we've no context information, return matching class members as well
    if lookupclass:
        try:
            return values + self._proxied.getattr(name, context, class_context=False)
        except AttributeInferenceError:
            pass
    return values
```

So the order is: **(1) instance attrs (incl. ancestors' instance attrs), (2) special
attributes (`__class__`, `__module__`, `__doc__`, `__dict__` from `InstanceModel`,
§12.7), (3) class attrs with `class_context=False`** — and when instance attrs exist,
class attrs are *appended*.

`ClassDef.instance_attr(name)` (scoped_nodes.py:2281-2301): copies
`self.instance_attrs.get(name, [])` plus, for every `class_node in
self.instance_attr_ancestors(name)` (ancestors() order), `class_node.instance_attrs
[name]`; filters out `DelAttr` nodes; empty → AttributeInferenceError.
`instance_attrs` is populated by the builder for every `self.x = ...` / `self.x: T = ...`
assignment in any method of the class (delayed-assattr handling in builder.py — keyed
on the first method argument name; includes assignments in nested functions whose
first arg matches).

### 12.3 `Instance.igetattr` — `BaseInstance.igetattr` — bases.py:274-297

```python
def igetattr(self, name, context=None):
    if not context:
        context = InferenceContext()
    try:
        context.lookupname = name
        # XXX frame should be self._proxied, or not ?
        get_attr = self.getattr(name, context, lookupclass=False)
        yield from _infer_stmts(self._wrap_attr(get_attr, context), context, frame=self)
    except AttributeInferenceError:
        try:
            # fallback to class.igetattr since it has some logic to handle
            # descriptors. But only if the _proxied is the Class.
            if self._proxied.__class__.__name__ != "ClassDef":
                raise
            attrs = self._proxied.igetattr(name, context, class_context=False)
            yield from self._wrap_attr(attrs, context)
        except AttributeInferenceError as error:
            raise InferenceError(**vars(error)) from error
```
Note `lookupclass=False` first (only instance attrs + special attrs), then the
class-level **igetattr** (with descriptor/property logic) as fallback. The exact-name
check `self._proxied.__class__.__name__ != "ClassDef"` means subclasses of ClassDef
(none in practice) would skip.

`_wrap_attr` (bases.py:299-315): `UnboundMethod` → if `_is_property(attr)` yield
`attr.infer_call_result(self, context)` results, else `BoundMethod(attr, self)`;
`nodes.Lambda` → BoundMethod if its first arg is literally named `self`; everything
else passes through.

### 12.4 `ClassDef.getattr` — scoped_nodes.py:2318-2373 — EXACT

```python
def getattr(self, name, context=None, class_context=True):
    if not name:
        raise AttributeInferenceError(...)

    # don't modify the list in self.locals!
    values: list[InferenceResult] = list(self.locals.get(name, []))
    for classnode in self.ancestors(recurs=True, context=context):
        values += classnode.locals.get(name, [])

    if name in self.special_attributes and class_context and not values:
        result = [self.special_attributes.lookup(name)]
        return result

    if class_context:
        values += self._metaclass_lookup_attribute(name, context)

    result: list[InferenceResult] = []
    for value in values:
        if isinstance(value, node_classes.AssignName):
            stmt = value.statement()
            # Ignore AnnAssigns without value, which are not attributes in the purest sense.
            if isinstance(stmt, node_classes.AnnAssign) and stmt.value is None:
                continue
        result.append(value)

    if not result:
        raise AttributeInferenceError(...)
    return result
```

- Own locals first, then **all ancestors in `ancestors()` order** (§13.4) — i.e. an MRO-ish
  prefix DFS, *not* C3.
- `special_attributes` = `ClassModel` (§12.7) — only when class_context and no real values.
- Metaclass attribute lookup `_metaclass_lookup_attribute` (scoped_nodes.py:2375-2386):
  `@lru_cache(maxsize=1024)` on (self, name, context-identity!); collects into a
  **set** from implicit metaclass (`type`) and the declared metaclass; via
  `_get_attribute_from_metaclass` (2388-2415): classmethods → BoundMethod bound to
  their wrapping class (or self), staticmethods → plain function, plain methods →
  `BoundMethod(attr, self)`, Property → as-is. Since the result is a set of freshly
  created BoundMethod objects, **iteration order is id-based / nondeterministic** (§21).
- Bare `AnnAssign` declarations (`x: int` in class body) are filtered out.

### 12.5 `ClassDef.igetattr` — scoped_nodes.py:2417-2514 — EXACT (descriptors!)

```python
def igetattr(self, name, context=None, class_context=True):
    context = copy_context(context)
    context.lookupname = name

    metaclass = self.metaclass(context=context)
    try:
        attributes = self.getattr(name, context, class_context=class_context)
        # If we have more than one attribute, make sure that those starting from
        # the second one are from the same scope. This is to account for modifications
        # to the attribute happening *after* the attribute's definition (e.g. AugAssigns on lists)
        if len(attributes) > 1:
            first_attr, attributes = attributes[0], attributes[1:]
            first_scope = first_attr.parent.scope()
            attributes = [first_attr] + [
                attr for attr in attributes
                if attr.parent and attr.parent.scope() == first_scope
            ]
        functions = [attr for attr in attributes if isinstance(attr, FunctionDef)]
        setter = None
        for function in functions:
            dec_names = function.decoratornames(context=context)
            for dec_name in dec_names:
                if dec_name is util.Uninferable:
                    continue
                if dec_name.split(".")[-1] == "setter":
                    setter = function
            if setter:
                break
        if functions:
            # Prefer only the last function, unless a property is involved.
            last_function = functions[-1]
            attributes = [
                a for a in attributes
                if a not in functions or a is last_function or bases._is_property(a)
            ]

        for inferred in bases._infer_stmts(attributes, context, frame=self):
            # yield Uninferable object instead of descriptors when necessary
            if not isinstance(inferred, node_classes.Const) and isinstance(
                inferred, bases.Instance):
                try:
                    inferred._proxied.getattr("__get__", context)
                except AttributeInferenceError:
                    yield inferred
                else:
                    yield util.Uninferable        # descriptor instance -> opaque
            elif isinstance(inferred, objects.Property):
                function = inferred.function
                if not class_context:
                    if not context.callcontext and not setter:
                        context.callcontext = CallContext(
                            args=function.args.arguments, callee=function)
                    # Through an instance so we can solve the property
                    yield from function.infer_call_result(caller=self, context=context)
                elif metaclass and function.parent.scope() is metaclass:
                    yield from function.infer_call_result(caller=self, context=context)
                else:
                    yield inferred
            else:
                yield function_to_method(inferred, self)
    except AttributeInferenceError as error:
        if not name.startswith("__") and self.has_dynamic_getattr(context):
            # class handle some dynamic attributes, return a Uninferable object
            yield util.Uninferable
        else:
            raise InferenceError(str(error), target=self, attribute=name,
                                 context=context) from error
```

`function_to_method` (scoped_nodes.py:166-174):
```python
if isinstance(n, FunctionDef):
    if n.type == "classmethod":  return bases.BoundMethod(n, klass)
    if n.type == "property":     return n
    if n.type != "staticmethod": return bases.UnboundMethod(n)
return n
```

`has_dynamic_getattr` (scoped_nodes.py:2516-2538): True if the class (or ancestors)
define `__getattr__` or `__getattribute__` whose root module is not `"builtins"` and is
`pure_python`. **Dunder lookups (`name.startswith("__")`) never get the dynamic-getattr
Uninferable fallback** — they raise. This drives pylint E1101-family conservatism and
also matters for E1129 etc.

`FunctionDef.type` (scoped_nodes.py:1313-1384, cached_property): returns
"function"/"method"/"classmethod"/"staticmethod" by checking: extra_decorators
(`name = staticmethod(name)` assignments in the class body, scoped_nodes.py:1221-1259);
`__new__`/`__init_subclass__`/`__class_getitem__` → classmethod; decorators by literal
name in `BUILTIN_DESCRIPTORS = {"classmethod", "staticmethod", "builtins.classmethod",
"builtins.staticmethod"}`, by `builtins.X` Attribute, by inferring Call decorators
(`_infer_decorator_callchain`), and finally by inferring each decorator and checking
`ancestors()` for subtypes of builtins.classmethod/staticmethod (InferenceError →
ignored).

### 12.6 `Module.getattr` / `Module.igetattr` — scoped_nodes.py:350-397

```python
def getattr(self, name, context=None, ignore_locals=False):
    if not name:
        raise AttributeInferenceError(...)
    result = []
    name_in_locals = name in self.locals

    if name in self.special_attributes and not ignore_locals and not name_in_locals:
        result = [self.special_attributes.lookup(name)]
        if name == "__name__":
            main_const = node_classes.const_factory("__main__")
            main_const.parent = AstroidManager().builtins_module
            result.append(main_const)
    elif not ignore_locals and name_in_locals:
        result = self.locals[name]
    elif self.package:
        try:
            result = [self.import_module(name, relative_only=True)]
        except (AstroidBuildingError, SyntaxError) as exc:
            raise AttributeInferenceError(...) from exc
    result = [n for n in result if not isinstance(n, node_classes.DelName)]
    if result:
        return result
    raise AttributeInferenceError(...)
```
Notes: a package module falls back to importing a **submodule** with the attr name.
`igetattr` = `_infer_stmts(self.getattr(name, context), context, frame=self)` with
`context.lookupname = name` (this is how `import x; x.y` works: the locals entry for
an imported name is the `Import` node; `_infer_stmts` sets lookupname and the
`Import._infer` resolves it).

`FunctionDef.getattr`/`Lambda.getattr` (scoped_nodes.py:1298-1311, 1047-1060):
`instance_attrs` then `special_attributes` (FunctionModel); igetattr =
`_infer_stmts(self.getattr(...))` wrapped in InferenceError on failure.

`Slice.igetattr` (node_classes.py:3595-3611): `start`/`stop`/`step` → the
corresponding child wrapped via `_wrap_attribute` (falsy → `const_factory(attr)` i.e.
Const(None)); else class getattr on builtin `slice`.

### 12.7 Object models — astroid/interpreter/objectmodel.py

`ObjectModel` (objectmodel.py:80-133): attributes discovered by reflection — every
`attr_<name>` property is exported as `<name>` (with `attr___dict__` → `__dict__` etc.);
`lookup(name)` returns the property value or raises AttributeInferenceError;
`__contains__` checks the attribute list. Models the port needs:

- `ObjectModel` base provides `__new__` (BoundMethod of synthetic
  `def __new__(self, cls): return cls()` parented to builtins `object`) and `__init__`
  (synthetic `def __init__(self, *args, **kwargs): return None`) — objectmodel.py:135-164.
- `ModuleModel`: `__name__` (Const str), `__doc__`, `__file__`, `__dict__`,
  `__package__`, `__path__`, `__spec__`, `__loader__`, `__cached__`, `builtins`.
- `FunctionModel`: `__name__`, `__doc__`, `__qualname__`, `__defaults__`,
  `__annotations__`, `__dict__`, `__kwdefaults__`, `__module__`, `__get__`, `__ne__`…
- `ClassModel`: `__module__`, `__name__`, `__qualname__`, `__doc__`, `__mro__`
  (Tuple of mro ClassDefs; raises for old-style), `mro` (bound method), `__bases__`,
  `__class__`, `__subclasses__`, `__dict__`, `__call__`, `__annotations__`.
- `InstanceModel`: `__class__` → `self._instance._proxied`; `__module__` →
  Const(root().qname()); `__doc__` → Const(doc); `__dict__`.
- `ExceptionInstanceModel(InstanceModel)`: `args` → empty `nodes.Tuple`;
  `__traceback__` → instance of TracebackType class. Subclasses per builtin exception
  (`BUILTIN_EXCEPTIONS` mapping, objectmodel.py:811-833): SyntaxError adds `text`;
  ExceptionGroup adds `exceptions`; ImportError adds `name`/`path`; OSError family
  adds `filename`/`errno`/`strerror`/`filename2`; UnicodeDecodeError adds `object`.
- `BoundMethodModel`/`UnboundMethodModel`: `__func__`, `__self__`, `__class__`.
- `GeneratorModel` (and Async): generator attrs (`send`, `throw`, `close`, … built from
  the real generator type members) + ContextManagerModel `__enter__`/`__exit__`.
- `DictModel`: `attr_items`/`attr_keys`/`attr_values` → `DictItems/DictKeys/DictValues`
  proxies wrapping the Dict.
- `PropertyModel`: `fget`, `fset`, `setter`, `deleter`, `getter`.
- `SuperModel`: `__thisclass__`, `__self_class__`, `__self__`, `__class__`.

### 12.8 `Super.igetattr` — objects.py:146-229

Quoted in essentials: `__class__` from special attrs; `super_mro()` (objects.py:94-125)
computes `mro_type.mro()` sliced after `self.mro_pointer` (SuperError/MroError →
AttributeInferenceError). Then for each class in the remaining MRO with `name in
cls.locals`, infers `cls[name]` via `_infer_stmts`; FunctionDef results are wrapped:
classmethod → `BoundMethod(inferred, cls)`; method accessed from classmethod scope →
plain function; `_class_based` or staticmethod → plain; `Property` → infer_call_result
of the underlying function; else `BoundMethod(inferred, cls)`. If nothing found, falls
back to SuperModel special attributes, else AttributeInferenceError.

---

## 13. MRO

### 13.1 `_c3_merge` — scoped_nodes.py:72-107 (verbatim above in source; summary)

Standard C3: repeatedly take the head of the first sequence that does not appear in
the *tail* of any sequence; remove it from all heads. No candidate →
`InconsistentMroError` with message `"Cannot create a consistent method resolution
order for MROs {mros} of class {cls!r}."`. Candidate equality is **node identity**
(`in s2[1:]` uses `__eq__` = identity).

### 13.2 `clean_duplicates_mro` — scoped_nodes.py:146-163

For each linearization, dedupe key is `(node.lineno, node.qname())`; a repeat raises
`DuplicateBasesError("Duplicates found in MROs {mros} for {cls!r}.")`.

### 13.3 `_compute_mro` / `mro()` — scoped_nodes.py:2837-2863

```python
def _compute_mro(self, context=None):
    if self.qname() == "builtins.object":
        return [self]
    inferred_bases = list(self._inferred_bases(context=context))
    bases_mro = []
    for base in inferred_bases:
        if base is self:
            continue
        mro = base._compute_mro(context=context)
        bases_mro.append(mro)
    unmerged_mro = [[self], *bases_mro, inferred_bases]
    unmerged_mro = clean_duplicates_mro(unmerged_mro, self, context)
    clean_typing_generic_mro(unmerged_mro)
    return _c3_merge(unmerged_mro, self, context)
```
`_inferred_bases` (scoped_nodes.py:2803-2835): no bases & not object → yields builtin
`object`; else for each base expression takes the **last** inferred value
(`_infer_last`, scoped_nodes.py:177-183: iterate `arg.infer(context.clone())` keeping
the final result, starting from Uninferable; InferenceError → skip the base);
Instance → its `_proxied`; non-ClassDef → skip; hidden (`hide`, from `with_metaclass`
hack) → yield its `.bases` instead.
`clean_typing_generic_mro` (110-143): removes a duplicated `typing.Generic` entry.
`mro()` raises `DuplicateBasesError` / `InconsistentMroError` (both `MroError`,
subclass of `ResolveError`); also note recursion: cyclic bases will hit Python
recursion limits — callers catch `MroError` only, so pylint guards with
`has_known_bases` first in several checks.

### 13.4 `ancestors()` (fallback used when MRO fails) — scoped_nodes.py:2167-2211

```python
def ancestors(self, recurs=True, context=None):
    yielded = {self}
    if context is None:
        context = InferenceContext()
    if not self.bases and self.qname() != "builtins.object":
        yield builtin_lookup("object")[1][0]
        return

    for stmt in self.bases:
        with context.restore_path():
            try:
                for baseobj in stmt.infer(context):
                    if not isinstance(baseobj, ClassDef):
                        if isinstance(baseobj, bases.Instance):
                            baseobj = baseobj._proxied
                        else:
                            continue
                    if not baseobj.hide:
                        if baseobj in yielded:
                            continue
                        yielded.add(baseobj)
                        yield baseobj
                    if not recurs:
                        continue
                    for grandpa in baseobj.ancestors(recurs=True, context=context):
                        if grandpa is self:
                            # This class is the ancestor of itself.
                            break
                        if grandpa in yielded:
                            continue
                        yielded.add(grandpa)
                        yield grandpa
            except InferenceError:
                continue
```
Order: prefix DFS over base expressions left-to-right; every base's full ancestor chain
is exhausted before the next base. Implicit `object` ONLY when there are no bases at
all (a class with bases that all fail inference yields nothing!).
`local_attr_ancestors` (2213-2232) prefers `self.mro(context)[1:]`, falling back to
`ancestors()` on MroError. `instance_attr_ancestors` (2234-2246) always uses
`ancestors()`.

`is_subtype_of(type_name)` (scoped_nodes.py:2004-2015): `self.qname() == type_name or
any(anc.qname() == type_name for anc in self.ancestors(context))`.

### 13.5 `ClassDef.type` — `_class_type` — scoped_nodes.py:1750-1785

Returns "class" / "exception" / "metaclass", memoized on `klass._type`:
metaclass if `_is_metaclass` (name == "type", or any inferred base chain reaching a
metaclass, scoped_nodes.py:1714-1747); "exception" if `klass.name.endswith("Exception")`
(!); else inherited from the first non-"class" direct ancestor (metaclass not
propagated to non-metaclasses). pylint's exceptions checker uses `inherits_from_std_ex`
rather than this, but `_class_type` feeds `Arguments` self/cls inference (§10.3).

---

## 14. Operator protocols

### 14.1 dunder lookup — astroid/interpreter/dunder_lookup.py (full file quoted earlier)

```python
def lookup(node, name, context=None) -> list:
    if isinstance(node, (nodes.List, nodes.Tuple, nodes.Const, nodes.Dict, nodes.Set)):
        return _builtin_lookup(node, name)          # node.locals of the *proxied* builtin class? No:
    if isinstance(node, astroid.Instance):
        return _lookup_in_mro(node, name)
    if isinstance(node, nodes.ClassDef):
        return _class_lookup(node, name, context=context)
    raise AttributeInferenceError(attribute=name, target=node)
```
- `_builtin_lookup(node, name)` = `node.locals.get(name, [])` — for Const/List/…,
  `.locals` resolves through `Proxy.__getattr__` to **the proxied builtin ClassDef's
  locals** (no ancestors!). Empty → AttributeInferenceError.
- `_lookup_in_mro(node, name)` = `node.locals.get(name, [])` + every
  `ancestor.locals.get(name, [])` over `node.ancestors(recurs=True)` — again via the
  proxied class for Instances. Empty → AttributeInferenceError.
- `_class_lookup`: for `ClassDef`, looks the dunder up **on the metaclass** (None →
  AttributeInferenceError). This is why `SomeClass + 1` doesn't find `__add__` defined
  on SomeClass — type slots semantics.
- IMPORTANT: this returns *all* matching defs in MRO order (own class first).
  `_invoke_binop_inference` uses `methods[0]`.

### 14.2 BinOp inference — node_classes.py:1528-1563 and _base_nodes.py:325-672

`BinOp._infer` = `yes_if_nothing_inferred(path_wrapper(...))` over
`_filter_operation_errors(self._infer_binop, context, util.BadBinaryOperationMessage)`
— i.e. **public inference replaces Bad*Message with Uninferable**, while pylint's
E1131 calls `type_errors()` to retrieve them:

```python
def type_errors(self, context=None):
    bad = []
    try:
        for result in self._infer_binop(context=context):
            if result is util.Uninferable:
                raise InferenceError
            if isinstance(result, util.BadBinaryOperationMessage):
                bad.append(result)
    except InferenceError:
        return []
    return bad
```
(node_classes.py:1496-1515; identical pattern for AugAssign:1378-1397 and
UnaryOp:4296-4315.) **If ANY result is Uninferable → empty list** (suppresses E1130/E1131).

`_infer_binop` (1528-1554):
```python
context = context or InferenceContext()
lhs_context = copy_context(context)
rhs_context = copy_context(context)
lhs_iter = left.infer(context=lhs_context)
rhs_iter = right.infer(context=rhs_context)
for lhs, rhs in itertools.product(lhs_iter, rhs_iter):
    if any(isinstance(value, util.UninferableBase) for value in (rhs, lhs)):
        yield util.Uninferable
        return
    try:
        yield from self._infer_binary_operation(lhs, rhs, self, context,
                                                self._get_binop_flow)
    except _NonDeducibleTypeHierarchy:
        yield util.Uninferable
```

`_infer_binary_operation` (_base_nodes.py:620-672):
```python
context, reverse_context = OperatorNode._get_binop_contexts(context, left, right)
left_type = helpers.object_type(left)
right_type = helpers.object_type(right)
methods = flow_factory(left, left_type, binary_opnode, right, right_type,
                       context, reverse_context)
for method in methods:
    try:
        results = list(method())
    except AttributeError:
        continue
    except AttributeInferenceError:
        continue
    except InferenceError:
        yield util.Uninferable
        return
    else:
        if any(isinstance(result, util.UninferableBase) for result in results):
            yield util.Uninferable
            return
        if all(map(OperatorNode._is_not_implemented, results)):
            continue
        not_implemented = sum(
            1 for result in results if OperatorNode._is_not_implemented(result))
        if not_implemented and not_implemented != len(results):
            # Can't infer yet what this is.
            yield util.Uninferable
            return
        yield from results
        return
# The operation doesn't seem to be supported so let the caller know about it
yield util.BadBinaryOperationMessage(left_type, binary_opnode.op, right_type)
```
- `_is_not_implemented(const)`: `isinstance(const, nodes.Const) and const.value is
  NotImplemented`.
- All-NotImplemented → try the next method (reflected); mixed → Uninferable; any
  Uninferable → Uninferable; otherwise first successful method wins.
- Exhausting all methods → **BadBinaryOperationMessage(left_type, op, right_type)** —
  the E1131 payload.

`_get_binop_flow` (_base_nodes.py:559-618):
- same type (`left_type.qname() == right_type.qname()`): only `left.__op__(right)`.
- left subtype of right (`helpers.is_subtype`): only `left.__op__(right)`.
- left supertype of right: `right.__rop__(left)` then `left.__op__(right)`.
- unrelated: `left.__op__(right)` then `right.__rop__(left)`.
- plus, for `op == "|"` when both sides are ClassDef/UnionType/Const(None):
  append `_bin_op_or_union_type` producing a `bases.UnionType` (PEP 604).

`is_subtype/is_supertype` (helpers.py): `_type_check(t1, t2)` = `t1 in t2.mro()[:-1]`;
**both types must have fully known bases** (`has_known_bases`) else
`_NonDeducibleTypeHierarchy` → the caller yields Uninferable. MroError →
`_NonDeducibleTypeHierarchy` too.

`_invoke_binop_inference` (_base_nodes.py:386-423):
```python
methods = dunder_lookup.lookup(instance, method_name)   # AttributeInferenceError -> skip method
context = bind_context_to_node(context, instance)
method = methods[0]
context.callcontext.callee = method
if isinstance(instance, nodes.Const) and isinstance(instance.value, str) and op == "%":
    return iter(OperatorNode._infer_old_style_string_formatting(instance, other, context))
try:
    inferred = next(method.infer(context=context))
except StopIteration as e:
    raise InferenceError(node=method, context=context) from e
if isinstance(inferred, util.UninferableBase):
    raise InferenceError
if not isinstance(instance,
        (nodes.Const, nodes.Tuple, nodes.List, nodes.ClassDef, bases.Instance)):
    raise InferenceError
return instance.infer_binary_op(opnode, op, other, context, inferred)
```
The per-type `infer_binary_op` implementations (protocols.py):

- **Const** `const_infer_binary_op` (103-136, yes_if_nothing_inferred): if other is
  Const → compute `BIN_OP_IMPL[op](self.value, other.value)` via real Python operators;
  guard: `**` with int/float operand > 1e5 → NotImplemented Const (anti-DoS);
  TypeError → Const(NotImplemented); other Exception → Uninferable.
  `str % nonconst` → Uninferable. Else → Const(NotImplemented).
  (But note: `str % ...` is intercepted earlier by
  `_infer_old_style_string_formatting`, _base_nodes.py:350-384: Tuple of all-Const →
  real `%` fold; Dict of Const:Const → fold; Const → fold; failures
  (TypeError/KeyError/ValueError) → Uninferable.)
- **Tuple/List** `tl_infer_binary_op` (176-220): `+` with same container class →
  new container with elements; elements are passed through `_filter_uninferable_nodes`
  which **infers each element** (Uninferable → `UNATTACHED_UNKNOWN`); `*` with
  Const int → `_multiply_seq_by_int` (guard: `len(elts)*value > 1e8` → `[Uninferable]`;
  value ≤ 0 → empty); `*` with Instance having `__index__` → multiply by that;
  everything else → Const(NotImplemented).
- **Instance** `Instance.infer_binary_op` (bases.py:356-365, yes_if_nothing_inferred):
  `return method.infer_call_result(self, context)` — i.e. infer the dunder's return.
- **ClassDef** `instance_class_infer_binary_op` (223-232): same (metaclass dunder).

### 14.3 AugAssign — node_classes.py:1413-1448

`_infer_augassign`: `lhs_iter = self.target.infer_lhs(context)`,
`rhs_iter = self.value.infer(rhs_context)` (rhs context is `context.clone()`);
product; Uninferable short-circuit; `_infer_binary_operation(..., self._get_aug_flow)`.
`_get_aug_flow` (_base_nodes.py:502-557): always tries `left.__iop__(right)` first,
then per type relation `left.__op__`/`right.__rop__` (quoted verbatim in source above).
`AugAssign._infer` filters errors like BinOp (raise_if_nothing_inferred + path_wrapper).

### 14.4 UnaryOp — node_classes.py:4249-4399 — EXACT (E1130)

`UNARY_OP_METHOD = {"+": "__pos__", "-": "__neg__", "~": "__invert__", "not": None}`.

`_infer_unaryop` (4326-4388), per inferred operand:
1. `operand.infer_unary_op(self.op)` — defined only for Const
   (`_infer_unary_op(self.value, op)` via real Python operator; `NotImplemented`
   operand passes through), Tuple (`tuple(self.elts)`), List (`self.elts`), Set
   (`set(self.elts)` — note: this calls `set()` on nodes, hashable by id), Dict
   (`dict(self.items)`). A real `TypeError` from the operator →
   `BadUnaryOperationMessage(operand, self.op, exc)`.
2. `AttributeError` (no `infer_unary_op` on the result type) →
   - `op == "not"`: `bool_value()`; not Uninferable → `const_factory(not bool_value)`,
     else Uninferable.
   - else: operand must be `Instance` or `ClassDef`, otherwise
     `BadUnaryOperationMessage` (e.g. unary minus on a Module/function).
     Then `dunder_lookup.lookup(operand, meth)`; AttributeInferenceError →
     `BadUnaryOperationMessage`. `meth = methods[0]`; `inferred = next(meth.infer())`;
     Uninferable or not callable → **continue (silently)**. Then with
     boundnode=operand, callcontext=CallContext([], callee=inferred):
     `result = next(inferred.infer_call_result(self, context), None)`;
     None → yield operand ("failed to infer, return the same type"); else yield result.
     `AttributeInferenceError` inside → BadUnaryOperationMessage; `InferenceError`
     → Uninferable.

`UnaryOp._infer` filters Bad messages to Uninferable; `type_errors()` (4296-4315) is
the pylint E1130 entry — same all-or-nothing Uninferable suppression as BinOp.

### 14.5 `helpers.object_type` — helpers.py (quoted in source above)

`_object_type(node)` infers the node and maps each result:
ClassDef → its metaclass if any else builtins `type`; Lambda/UnboundMethod/FunctionDef
→ proxy class `function` / `builtin_function_or_method` / `method`
(fresh `build_class(cls_name, builtins)` each call — **new ClassDef objects each
time**, equality by qname only); Module → proxy class `module`; `Unknown` →
InferenceError; Uninferable → itself; Proxy/Slice/Super → `_proxied`.
`object_type` = `set(_object_type(...))`; **len != 1 → Uninferable**; InferenceError →
Uninferable. (Set of nodes — identity-based; two yields of the same ClassDef collapse.)

---

## 15. Subscript inference and `getitem`

### 15.1 `Subscript._infer_subscript` — node_classes.py:3729-3795 — EXACT (already quoted in full above; summary of bailouts)

For each inferred `value` of `self.value` (Uninferable → yield Uninferable, STOP):
for each inferred `index` of `self.slice` (Uninferable → yield Uninferable, STOP):
- determine `index_value`:
  - `value.__class__ == Instance` (exact class!) → the raw index node;
  - `index.__class__ == Instance` → `helpers.class_instance_as_index(index)`
    (calls `__index__` via igetattr/infer_call_result, must produce Const int,
    helpers.py) else sentinel;
  - else the inferred index itself.
- sentinel → InferenceError.
- `assigned = value.getitem(index_value, context)`; ANY of `AstroidTypeError,
  AstroidIndexError, AstroidValueError, AttributeInferenceError, AttributeError` →
  InferenceError (the **AttributeError** catch handles objects without getitem).
- `self is assigned` or assigned Uninferable → yield Uninferable, STOP.
- else `yield from assigned.infer(context)`.
Decorators: raise_if_nothing_inferred + path_wrapper (infer_lhs: only the former).

### 15.2 `getitem` implementations

- **`Const.getitem`** (node_classes.py:2098-2136): index must be Const or Slice
  (else AstroidTypeError). Only `str`/`bytes` values are subscriptable: returns
  `Const(self.value[index_value])`; IndexError → AstroidIndexError; TypeError →
  AstroidTypeError; ValueError → AstroidValueError; any other value type →
  AstroidTypeError(f"{self!r} (value={self.value})").
- **`Tuple.getitem` / `List.getitem`** → `_container_getitem(self, self.elts, index)`
  (node_classes.py:236-266): Slice index → `_infer_slice` → new container of
  `elts[slice]`; Const index → `elts[index.value]`; ValueError→AstroidValueError,
  IndexError→AstroidIndexError, TypeError→AstroidTypeError; other index types →
  AstroidTypeError.
- **`Dict.getitem`** (node_classes.py:2401-2432): iterates `self.items` in source
  order; `DictUnpack` keys: safe_infer the value, must be Dict, recurse (errors →
  continue); otherwise `for inferredkey in key.infer(context)`: Uninferable → continue;
  `Const == Const` (by `==` on the python values) → return the value node. Not found
  → `AstroidIndexError(index)`.
- **`Instance.getitem`** (bases.py:416-435): `method = next(self.igetattr("__getitem__"))`;
  must be a BoundMethod else InferenceError; must have exactly 2 parameters
  (`len(method.args.arguments) != 2` → AstroidTypeError); result =
  `next(method.infer_call_result(self, new_context), None)` with
  callcontext args=[index].
- **`ClassDef.getitem`** (scoped_nodes.py:2540-2590): `dunder_lookup.lookup(self,
  "__getitem__")` (metaclass!); fallback to local `__class_getitem__` (getattr);
  neither → AstroidTypeError. Calls `methods[0].infer_call_result(self, ctx)` →
  first result or Uninferable; `AttributeError` with EmptyNode method &
  `pytype()=="builtins.type"` → return self (builtin generics like `list[int]`);
  InferenceError → Uninferable.
- **`_infer_slice`** (node_classes.py:221-233): builds a real Python `slice` from
  Const int/None bounds (each bound via `_slice_value`, 194-218: Const int/None
  directly; None child → None; otherwise first inferred value if Const int/None;
  else sentinel). Any sentinel → AstroidTypeError.

`Slice._infer` yields self (3626-3629).

---

## 16. `bool_value`, `Compare`, `BoolOp`, `IfExp`, f-strings

### 16.1 `bool_value(context=None)` per type

| type | value |
|---|---|
| NodeNG default | `Uninferable` (node_ng.py:745-763) |
| `Const` | `bool(self.value)`; **`NotImplemented` → True on 3.12** (Uninferable on 3.14+) (node_classes.py:2165-2175) |
| `BaseContainer` (List/Tuple/Set/FrozenSet) | `bool(self.elts)` (node_classes.py:323-328) |
| `Dict` | `bool(self.items)` (2434-2440) |
| `Module`, `ClassDef`, `FunctionDef`, `Lambda`, `GeneratorExp`, `Generator`, `BoundMethod`, `UnboundMethod`, `UnionType` | `True` |
| `ListComp`/`SetComp`/`DictComp` | `Uninferable` |
| `Instance` | §16.2 |

### 16.2 `Instance.bool_value` — bases.py:388-414

`__bool__` result via `_infer_method_result_truth` (bases.py:207-228: igetattr the
method, must be callable, infer_call_result first value; Uninferable → Uninferable;
infer the value and take ITS `bool_value()`); on InferenceError/AttributeInferenceError
fall back to `__len__` the same way; both missing → **True**.

### 16.3 `Compare._infer` — node_classes.py:1907-1931 (no decorators)

Chained comparison folding: each side's full inference list; `_do_compare`
(1859-1905): `is`/`is not` → Uninferable always; every (left,right) product pair must
be literal-evaluable via `ast.literal_eval(node.as_string())` (errors → Uninferable);
real comparison op applied; mixed True/False across pairs → Uninferable; TypeError →
AstroidTypeError → Uninferable result. Yields `Const(True/False)` or Uninferable.
Short-circuits across ops chain on first non-True.

### 16.4 `BoolOp._infer` — node_classes.py:1633-1685 (quoted earlier)

Cartesian product of all operand inferences; any Uninferable in a pair → Uninferable;
`bool_value()` of each; Uninferable bool → Uninferable; yields the first operand
whose bool matches the short-circuit predicate (`or` → truthy; `and` → falsy), else
the last value.

### 16.5 `IfExp._infer` — node_classes.py:3101-3142 (raise_if_nothing_inferred)

Infers the test with `context.clone()`; all test results must agree on a bool value
(`test.bool_value()`); disagreement / Uninferable / InferenceError → condition None →
infer BOTH branches (body with lhs_context, orelse with rhs_context — both
copy_context of the input).

### 16.6 f-strings — `JoinedStr._infer` / `FormattedValue._infer` (node_classes.py:4704-4853, quoted earlier)

`FormattedValue`: for each inferred format_spec (default `Const("")`): non-Const →
one Uninferable; for each inferred value: `format(value_to_format, format_spec.value)`
→ Const; ValueError/TypeError → Uninferable.
`JoinedStr`: empty values → `Const("")`; else cartesian combination of each part's
inference; non-Const parts become the marker string `"{Uninferable}"`, and any result
containing that marker is collapsed into a single Uninferable yield.

---

## 17. Container / Const inference

- `BaseContainer._infer` (node_classes.py:340-362, raise_if_nothing_inferred):
  if any element is `Starred`/`NamedExpr` → build a new same-type node with elements
  from `_infer_sequence_helper` (364-386: starred → `safe_infer(elt.value)` must
  have `.elts`, else InferenceError; namedexpr → safe_infer value; plain → as-is);
  else **yield self**.
- `Dict._infer` (2442-2457): any `DictUnpack` key → `_infer_map` (2485-2506:
  safe_infer each key and value; `**` must safe-infer to Dict; failures →
  InferenceError; merge with `as_string()`-keyed replacement, 2459-2483) into a new
  Dict; else yield self.
- `Const._infer` → yield self.
- `EmptyNode._infer` (2568-2581): no underlying object → Uninferable; else
  `AstroidManager().infer_ast_from_something(self.object)` (lookup of the live
  object's class in its module's AST; AstroidError → Uninferable). EmptyNodes appear
  in raw-built (C extension / builtins) module members.
- `Unknown._infer` → yields Uninferable (node_classes.py:5002+).
- `EvaluatedObject._infer` → yields the stored `value`.

---

## 18. Exceptions support

### 18.1 `ExceptionInstance` — objects.py:232-246

An `Instance` subclass; `special_attributes` is selected per concrete exception qname
from `objectmodel.BUILTIN_EXCEPTIONS` (default `ExceptionInstanceModel` → provides
`args` (empty Tuple) and `__traceback__`). Produced by:
- `ClassDef.instantiate_class()` when any MRO entry is named `Exception`/`BaseException`
  (§11.5) — i.e. `raise SomeError(...)` infers the Call → ExceptionInstance;
- `excepthandler_assigned_stmts` for `except E as e:` binding.

### 18.2 `excepthandler_assigned_stmts` — protocols.py:522-564

```python
def _generate_assigned():
    for assigned in node_classes.unpack_infer(self.type):
        if isinstance(assigned, nodes.ClassDef):
            assigned = objects.ExceptionInstance(assigned)
        yield assigned

if isinstance(self.parent, node_classes.TryStar):
    # except* -> ExceptionGroup instance whose .exceptions contains the caught ones
    eg = next(node_classes.unpack_infer(extract_node(
        "from builtins import ExceptionGroup\nExceptionGroup")))
    assigned = objects.ExceptionInstance(eg)
    assigned.instance_attrs["exceptions"] = [nodes.List.from_elements(_generate_assigned())]
    yield assigned
else:
    yield from _generate_assigned()
```

### 18.3 `unpack_infer` — node_classes.py:89-113 (raise_if_nothing_inferred; quoted earlier)

Recursively flattens List/Tuple elements then infers; a node inferring to itself is
yielded as-is; Uninferable yielded as-is. Used directly by pylint's exceptions checker
(E0701/E0712) on `except (A, B):` type expressions.

---

## 19. Builtins bootstrapping & brains

### 19.1 Bootstrapping — astroid/raw_building.py:598-735 (`_astroid_bootstrapping`)

astroid does **not** parse a builtins.pyi. It introspects the live CPython `builtins`
module with `InspectBuilder` (raw_building.py:422-585):

- `inspect_build(builtins)` creates `Module("builtins", pure_python=False,
  package=False)` and walks `dir(obj)` (sorted by `dir()` = alphabetical) building:
  functions via `inspect.signature` (`object_build_function` → `build_function` with
  arg names/defaults; signature failures → method descriptor with **`args=None`**,
  i.e. unknown signature — `Arguments.args is None` downstream!), classes via
  `object_build_class` (recursing into members; bases by `__name__` strings as `Name`
  nodes), method/data descriptors, constants via `const_factory`, everything else →
  `EmptyNode` dummies. Members whose `__module__` differs become `ImportFrom`
  pseudo-nodes (`attach_import_node`) or dummies. Exception classes get
  `instance_attrs` populated from a live throwaway instance (`member()` —
  `_base_class_object_build`, raw_building.py:354-388).
- Then `CONST_CLS` proxies are wired (raw_building.py:608-624): `list/tuple/dict/set`
  node classes get `_proxied = <builtins ClassDef>`; `NoneType`, `NotImplementedType`,
  `Ellipsis` get synthetic empty classes; `Const._proxied` becomes the property
  mapping by `type(value)`.
- Synthetic `generator`, `async_generator`, `UnionType` ClassDefs are built from the
  live `types.GeneratorType` etc. and become `Generator._proxied` etc.
  (raw_building.py:626-694).
- Extra builtin types registered into the builtins module if absent:
  GetSetDescriptorType, GeneratorType, MemberDescriptorType, NoneType,
  NotImplementedType, FunctionType, MethodType, BuiltinFunctionType, ModuleType,
  TracebackType (696-726).
- Finally `brain_builtin_inference.on_bootstrap()` **extends `str` and `bytes`** with
  parsed method stubs (STR_CLASS/BYTES_CLASS source templates,
  brain_builtin_inference.py:46-159): join/replace/format/encode/decode/capitalize/
  title/lower/upper/swapcase/index/find/count/strip/lstrip/rstrip/rjust/center/ljust —
  each returning `''`/`b''`/`0` so `"x".upper()` infers to `Const('')`. The methods are
  *replaced* in `str`'s locals (`class_node.locals[method.name] = [method]`).

Implication for the port: builtin classes' member sets are those of CPython 3.12.12's
real builtins (plus the str/bytes overrides). Function signatures for C builtins
come from `inspect.signature` where available, else `args=None` (unknown).

### 19.2 Builtin call transforms — brain_builtin_inference.py:1066-1106 (`register`)

`register_builtin_transform(manager, transform, name)` attaches an inference tip to
`Call` nodes whose predicate `_builtin_filter_predicate` (162-189) matches:
`node.func` is `Name(name)` (NOT verified to resolve to the real builtin!), or for
`"dict.fromkeys"` an Attribute `dict.fromkeys`. A special carve-out skips
`type(...)` calls in module `re` assigned to `Pattern`/`Match`.
The wrapper `_transform_wrapper` (201-218) sets result.parent/lineno/col_offset from
the call node if missing.

Registered: `bool, super, callable, property, getattr, hasattr, tuple, set, list,
dict, frozenset, type, slice, isinstance, issubclass, len, str, int, dict.fromkeys`
— each implementation quoted/summarized in §19.3. Plus:
- ClassDef tip `_infer_object__new__decorator` when decorated with literal
  `@object.__new__` (712-732);
- Call tip `_infer_copy_method` for any `<expr>.copy()` where every inferred receiver
  is Dict/List/Set/FrozenSet → yields the receivers (983-996);
- Call tip `_infer_str_format_call` for `"...".format(...)`/`name.format(...)`
  (predicate `_is_str_format_call`, 999-1009): all args must safe-infer to Const else
  Uninferable; real `str.format` applied; AttributeError/IndexError/KeyError/
  TypeError/ValueError → Uninferable (1012-1063).

### 19.3 Key builtin inference functions (all raise `UseInferenceDefault` to fall back)

- `infer_bool` (650-670): 0 args → Const(False); 1 arg → its first inferred value's
  `bool_value()`, Uninferable-safe; >1 args → default.
- `infer_type` (673-678): exactly 1 arg → `helpers.object_type(arg)`.
- `infer_len` (841-861): no kwargs, exactly 1 positional; `helpers.object_len`
  (helpers.py — safe_infer; recursion guard for self-referential `__len__`;
  Const str/bytes → len; List/Set/Tuple/FrozenSet → len(elts); Dict → len(items);
  else `object_type(...).igetattr("__len__")` → infer_call_result must give Const int
  (`pytype() == "builtins.int"`); Instance-of-int result → 0).
- `infer_isinstance` (780-814) / `infer_issubclass` (735-777): via
  `helpers.object_isinstance/object_issubclass`; Uninferable → default; returns
  Const(bool).
- `infer_getattr`/`infer_hasattr` (533-585): args[0]/args[1] first-inferred; attr must
  be Const str; getattr: `next(obj.igetattr(attr))`, failures → default of 3rd arg
  or UseInferenceDefault; hasattr: `obj.getattr(attr)` → Const(True)/Const(False)
  (AttributeInferenceError)/Uninferable.
- `infer_callable` (588-607): 1 arg; first inferred value's `.callable()` → Const.
- `infer_property` (610-647): args[0] must first-infer to FunctionDef/Lambda →
  `objects.Property` (parent=SYNTHETIC_ROOT).
- `infer_super` (445-506): only inside a method/classmethod scope; 0-arg or 2-arg
  forms; builds `objects.Super(mro_pointer, mro_type, self_class=wrapping class,
  scope, call)`.
- container builders `infer_tuple/list/set/frozenset` (300-359): 0 args → empty node;
  1 arg → conversion of literal containers / Const str-bytes / Dict keys (Dict keys
  must be Const else default); non-literal elements become `EvaluatedObject` of their
  safe-inferred value; >1 args → default.
- `infer_dict` (391-442): handles `dict()`, `dict(**kw)`, `dict(iterable)`,
  `dict(iterable, **kw)`, `dict(mapping)`; `CallSite.has_invalid_arguments()` or
  invalid keywords → default; iterable elements must be 2-element literal pairs.
- `infer_int` (880-909): no kwargs; arg must first-infer to Const int/str; bad str
  parses → Const(0); else Const(int(value)); no args → Const(0).
- `infer_str` (864-877): ANY str() call (no kwargs) → `Const("")`.
- `infer_slice` (681-709): 1-3 Const int/None args → `Slice` node; else default.
- `infer_dict_fromkeys` (912-980): builds Dict of (key, Const(None)) for literal
  iterables of Consts; anything odd → empty Dict.

### 19.4 functools brain — brain_functools.py (quoted in full above)

- `functools.partial(...)` calls (predicate: callee literally named `partial` or
  `functools.partial`) → `objects.PartialFunction` whose `infer_call_result` prepends
  `filled_args`/merges `filled_keywords` into the active CallContext
  (objects.py:304-323). Bailouts: <1 positional; ==1 positional and no kwargs; wrapped
  fn must first-infer to FunctionDef; unknown kwargs → UseInferenceDefault.
- `@functools.lru_cache` functions get `special_attributes = LruWrappedModel`
  (adds `__wrapped__`, `cache_info`, `cache_clear`) — relevant to E1102/E1120 only via
  attribute lookups.
- functools.wraps has no brain; `@wraps(f)`-decorated functions infer as themselves.

### 19.5 typing brain — brain_typing.py (read 28-330)

Bits that matter for in-scope checks:
- `typing.TypeVar(...)`/`NewType(...)` calls → synthetic class with a `Meta`
  metaclass whose `__getitem__` returns self (TYPING_TYPE_TEMPLATE) — makes
  `T[int]` and `Optional[T]` subscriptable.
- Subscript `typing.X[...]`: `infer_typing_attr` — only when `node.value` infers to a
  qname starting with `typing.` and NOT in `TYPING_ALIAS`; `typing.Generic`/
  `typing.Annotated` get an injected `__class_getitem__` (CLASS_GETITEM_TEMPLATE) and
  infer to themselves; other typing members re-infer through the synthetic template
  class.
- `_alias(...)` assignments inside typing.py itself get ClassDefs injected so that
  `typing.List[int]` etc. resolve (`infer_typing_alias`, plus
  `_forbid_class_getitem_access` monkeypatching `node.getattr` for some).
- `TypedDict` → synthetic ClassDef based on dict.
- PEP 695 generic classes (non-empty `type_params`) get `__class_getitem__` injected
  via inference tip (196-207).

### 19.6 property brain

There is no separate `brain_property.py`; property support is the combination of
(1) `infer_property` builtin transform (§19.3), (2) `FunctionDef._infer` →
`objects.Property` for decorated functions (§11.2), and (3) the descriptor handling
in `ClassDef.igetattr` / `Instance._wrap_attr` (§12.3/§12.5): accessing a property
through an instance infers the **getter's return values**; through the class yields
the `Property` object itself. `Property.infer_call_result` raises (not callable) —
this drives pylint `not-callable` (E1102) suppression/triggering behavior on
properties.

---

## 20. Module import machinery

### 20.1 `AstroidManager` (Borg singleton) — manager.py:50-128

Shared `brain` dict: `astroid_cache` (modname → Module), `_mod_file_cache`
((modname, contextfile) → ModuleSpec or cached AstroidImportError),
`max_inferable_values=100`, `_failed_import_hooks`, `module_denylist`, transforms.
`builtins_module` = `astroid_cache["builtins"]`.

### 20.2 `ast_from_module_name` — manager.py:195-276 — EXACT flow

1. `modname is None` → AstroidBuildingError.
2. denylist → AstroidImportError.
3. cache hit (`use_cache`) → return.
4. `"__main__"` → empty stub module (`string_build("")`).
5. `file_from_module_name(modname, context_file)` → `ModuleSpec` via
   `interpreter/_import/spec.py` finder chain `_SPEC_FINDERS = (ImportlibFinder,
   ExplicitNamespacePackageFinder, ZipFinder, PathSpecFinder)` (spec.py:339); each
   submodule path segment resolved in turn; failures → ImportError →
   wrapped & **cached** as AstroidImportError in `_mod_file_cache` (manager.py:301-322).
   `ImportlibFinder` resolves: builtins (C_BUILTIN), frozen (PY_FROZEN),
   suffix search over `[C_EXTENSION suffixes, PY_SOURCE suffixes, PY_COMPILED]` in
   sys.path-like dirs, `__init__.py`-bearing dirs → PKG_DIRECTORY, namespace dirs →
   PY_NAMESPACE.
6. by spec type: PY_ZIPMODULE → zip import; C_BUILTIN/C_EXTENSION → live-import +
   `InspectBuilder` (extensions outside the whitelist → empty stub);
   PY_COMPILED → AstroidImportError; PY_NAMESPACE → synthetic namespace package
   module; PY_FROZEN → source build if location known; else `ast_from_file(location)`
   (parse + TransformVisitor).
7. On AstroidBuildingError → `_failed_import_hooks` chain (e.g. brain_six) → re-raise.

### 20.3 `Import._infer` / `ImportFrom._infer` (node_classes.py:3195-3215 / 2855-2887; quoted earlier)

Both require `context.lookupname` (set by `_infer_stmts` from the locals entry name)
else InferenceError.
- `Import`: `real_name(name)` maps the asname back (`import a.b` binds `a` →
  real_name("a") = "a"); yields `do_import_module(real_name)`; AstroidBuildingError →
  InferenceError.
- `ImportFrom`: `real_name(name)` (AttributeInferenceError → InferenceError, pylint
  issue #4692 guard); `module = do_import_module()`; then
  `module.getattr(name, ignore_locals=module is self.root())` → `_infer_stmts`.
  The `ignore_locals` flag handles `from . import x` resolving inside the same
  package `__init__`.

`real_name(asname)` (_base_nodes.py:174-188): scans `self.names` for a match where
`"*"` → asname; unaliased dotted names match on the first segment; no match →
AttributeInferenceError.

`do_import_module` (_base_nodes.py:148-172): level from `self.level` (ImportFrom only);
**cache bypass** when `mymodule.relative_to_absolute_name(modname, level) ==
mymodule.name` (self-import); calls `mymodule.import_module(modname, level=level,
relative_only=bool(level and level >= 1), use_cache=...)`.

`Module.import_module` (scoped_nodes.py:439-475): `absmodname =
relative_to_absolute_name(modname, level)`; try absolute; on AstroidBuildingError:
`relative_only` → re-raise; `modname == absmodname` → re-raise; else retry plain
`modname`.

### 20.4 `relative_to_absolute_name` — scoped_nodes.py:477-523 — EXACT (E0402 source)

```python
def relative_to_absolute_name(self, modname: str, level: int | None) -> str:
    if self.absolute_import_activated() and level is None:
        return modname
    if level:
        if self.package:
            level = level - 1
            package_name = self.name.rsplit(".", level)[0]
        elif (
            self.path
            and not os.path.exists(os.path.dirname(self.path[0]) + "/__init__.py")
            and os.path.exists(os.path.dirname(self.path[0]) + "/" + modname.split(".")[0])
        ):
            level = level - 1
            package_name = ""
        else:
            package_name = self.name.rsplit(".", level)[0]
        if level and self.name.count(".") < level:
            raise TooManyLevelsError(level=level, name=self.name)
    elif self.package:
        package_name = self.name
    else:
        package_name = self.name.rsplit(".", 1)[0]

    if package_name:
        if not modname:
            return package_name
        return f"{package_name}.{modname}"
    return modname
```

**E0402 (`relative-beyond-top-level`)** is raised in pylint's imports checker
(`pylint/checkers/imports.py:1023-1031`, `_get_imported_module`):
```python
try:
    return importnode.do_import_module(modname)
except astroid.TooManyLevelsError:
    if _ignore_import_failure(importnode, modname, self._ignored_modules):
        return None
    self.add_message("relative-beyond-top-level", node=importnode)
```
i.e. the condition is exactly `level (after the package decrement) > number of dots in
the importing module's dotted name`. Note `rsplit(".", level)[0]` with level possibly
larger than the dot count silently returns the first segment — the TooManyLevels check
happens *after* computing package_name and only when `level` is still truthy.

---

## 21. Iteration-order / sorting / nondeterminism notes

1. **`locals` dict + per-name lists**: `dict[str, list]`, insertion-ordered by source
   order (rebuilder visits in AST order). `_filter_stmts` and getattr depend on this
   order. Port: preserve exact insertion order.
2. **`instance_attrs`**: ordered by builder visit order of methods (the delayed
   assattr pass runs in tree order).
3. **`InferenceContext.path` is a `set`** — only membership-tested, order irrelevant.
4. **`_metaclass_lookup_attribute` returns a `set()`** (scoped_nodes.py:2375-2386) —
   iteration order is Python-set order over freshly created objects (id-hash) →
   effectively arbitrary. It only matters when a name exists on both the class chain
   and the metaclass, and then only for result ordering (rare); safe_infer collapses
   ambiguity to None either way. The port may use insertion order (implicit-meta
   first, declared metaclass second) — closest deterministic approximation.
5. **`helpers._object_type` collapses via a `set`**; only the cardinality (==1) is
   observable. `decoratornames()` also returns a set; consumers do membership tests
   only — except `ClassDef.igetattr`'s setter scan iterates it (`for dec_name in
   dec_names`) but only tests suffix equality, order-insensitive.
6. **`ClassDef._all_slots` sorts**: `sorted(set(slots), key=lambda item: item.value)`
   (scoped_nodes.py:2798) — slot Const nodes deduped by identity then sorted by string
   value. (pylint's slots checks E0238/E0239 read `slots()`.)
7. **`dir(obj)` in InspectBuilder** is alphabetical → builtins module locals are in
   alphabetical member order.
8. **`path_wrapper` dedupe set** uses node identity (and `_proxied` for exact-class
   `Instance`) — first occurrence wins; order of yields preserved otherwise.
9. **lru_caches**: `LookupMixIn.lookup` (unbounded), `_metaclass_lookup_attribute`
   (1024), inference-tip cache (64, FIFO eviction of oldest), `_INFERENCE_CACHE`
   (unbounded global). Caches persist across modules within one pylint run —
   functional behavior should be cache-transparent except for the truncation
   interactions in §4 (cached truncated lists ARE replayed).

---

## 22. Numeric limits & guards table

| guard | value | location | effect |
|---|---|---|---|
| `AstroidManager.max_inferable_values` | 100 | manager.py:63 | per-node result cap → Uninferable tail |
| `InferenceContext.max_inferred` | 100 | context.py:47 | per-context-tree total result cap → Uninferable tail |
| inference-tip cache size | 64 | inference_tip.py:80 | FIFO eviction |
| `_metaclass_lookup_attribute` lru | 1024 | scoped_nodes.py:2375 | cache |
| `**` const-fold bailout | operand > 1e5 | protocols.py:113-119 | yields Const(NotImplemented) |
| seq `*` int bailout | `len(elts)*value > 1e8` | protocols.py:149-151 | `[Uninferable]` elements |
| `RecursionError` in raise_if_nothing_inferred | sys limit | decorators.py:89-92 | → InferenceError |

---

## 23. Dependency map: in-scope check → required inference features

The pylint message → astroid feature matrix below lists, for each in-scope checker
family, the minimal inference machinery it exercises (verified against pylint 4.0.5
checker sources; the checkers themselves are specified in the sibling notes files).

### typecheck (pylint/checkers/typecheck.py) — E11xx
- **E1102 not-callable**: `safe_infer(node.func)` (pylint's variant) + `.callable()`
  on the result → needs: Call func inference (Name/Attribute paths §8/§12),
  `callable()` semantics (§11.8), Property objects (callable() True but pylint
  special-cases `_is_property`/descriptors), `ClassDef.igetattr("__call__")`.
- **E1111 assignment-from-no-return / E1128 assignment-from-none**:
  `FunctionDef.infer_call_result` exact `Const(None)` vs Uninferable vs
  InferenceError semantics (§11.3) — including is_abstract first-statement quirk,
  generator → Generator, docstring-only body → InferenceError.
- **E1120/E1121/E1123/E1124/E1125 (no-value-for-parameter, too-many-function-args,
  unexpected-keyword-arg, redundant-keyword-arg, missing-kwoa)**: `CallSite`
  (`from_call`, `has_invalid_arguments`, unpacking semantics §10.4), callee
  inference incl. BoundMethod `implicit_parameters` (§1), functools.partial brain
  (§19.4), `__init__` resolution via `ClassDef.igetattr`/object-model `__init__`
  fallback (§12.7).
- **E1126/E1127/E1144 (invalid-sequence-index, invalid-slice-index,
  invalid-slice-step)**: subscript value/index inference (§15), Const/Slice
  folding, `__index__` protocol (`class_instance_as_index`).
- **E1129 (not-context-manager)**: `with` mgr inference; `igetattr("__enter__"/
  "__exit__")` on Instance/ClassDef (§12), contextmanager-decorated generators
  (§9 `_infer_context_manager`).
- **E1130 invalid-unary-operand-type**: `UnaryOp.type_errors()` (§14.4) including
  the all-or-nothing Uninferable suppression.
- **E1131 unsupported-binary-operation**: `BinOp.type_errors()` (§14.2) — dunder
  lookup on both operands, reflected method flow, NotImplemented Const semantics,
  `object_type`/`is_subtype` with `_NonDeducibleTypeHierarchy` bailout.
- **E1133 not-an-iterable / E1134 not-a-mapping**: inferred value capability checks
  (`itered`, `__iter__`/`__getitem__` getattr on class) — §12, §15.
- **E1135 unsupported-membership-test / E1136 unsubscriptable-object / E1137
  unsupported-assignment-operation / E1138 unsupported-delete-operation**:
  `safe_infer` + dunder presence via `getattr` on ClassDef/Instance incl. metaclass
  path for classes (§12.4, §14.1), `has_dynamic_getattr` conservatism (§12.5).
- **E1139 invalid-metaclass**: `ClassDef.metaclass()`/`declared_metaclass()`
  (§11.5, scoped_nodes.py:2626-2690) + `object_type`.
- **E1141 dict-iter-missing-items**: For-loop target arity + inferred Dict with
  Tuple keys (§9 for_assigned_stmts, Dict node shape).
- **E1143 unhashable-member**: `__hash__` getattr → Const(None) detection (§12).
- **E1101-family is excluded** but its helpers (igetattr) are shared.

### classes / special methods — E02xx, E03xx
- **E0202 method-hidden**: class locals + `instance_attr` ancestors lookup (§12.2).
- **E0203 access-member-before-definition**: instance_attrs population (§12.2) +
  pylint-side flow; `are_exclusive` (§7.5).
- **E0211/E0213 (no-method-argument / no-self-argument)**: `Arguments` shape only.
- **E0236-E0238 (invalid-slots-object, single-string-used-for-slots, invalid-slots)**:
  `ClassDef.slots()/_islots` — `igetattr("__slots__")`, `itered()`, Const-str filter,
  mro-based `_all_slots` incl. sorted dedup (§21.6, scoped_nodes.py:2695-2801).
- **E0239 inheriting non-class**: base expression inference → ClassDef check
  (`_inferred_bases`/ancestors §13.4).
- **E0240 inconsistent-mro / E0241 duplicate-bases**: `mro()` raising
  `InconsistentMroError` / `DuplicateBasesError` exactly as §13.1-13.3.
- **E0243/E0244 invalid-class-object/invalid-enum-extension etc.**: `__class__`
  assignment inference (safe_infer → ClassDef).
- **E0245 (no __init__ in slots ancestors)** etc.: ancestors walk.
- **E03xx unexpected-special-method-signature / invalid-*-returned (E0301-E0312)**:
  method body return inference — `infer_call_result` (§11.3) + result `pytype()` /
  `bool_value()` / Const checks; `Generator`/iterator detection for E0301
  (`is_generator`, igetattr("__next__")).

### exceptions — E07xx
- **E0701 bad-except-order**: `unpack_infer` on handler types (§18.3) + ancestor
  subtype relations (`mro`/ancestors §13).
- **E0702 raising-bad-type / E0710 raising-non-exception / E0712
  catching-non-exception**: `safe_infer`/`unpack_infer` of raise/except exprs;
  ClassDef vs Instance vs Const discrimination; `inherits_from_std_ex`-style ancestor
  walks; `has_known_bases` conservatism (helpers.py).
- **E0704 misplaced-bare-raise**: AST-only.
- **E0705 bad-exception-cause**: safe_infer of `raise ... from X` cause → ClassDef/
  Instance/Const(None) checks.
- **E0711 notimplemented-raised**: name-based (no inference).

### strings — E13xx
- **E1300-E1307 (% formatting and str.format families)**: `safe_infer` of the RHS
  of `BinOp('%')` → literal Dict/Tuple/Const shapes incl. Dict key Const folding
  (§17, §14.2 `_infer_old_style_string_formatting` interplay — note pylint analyses
  the format string itself; inference supplies the argument shapes);
  for `.format()`: `safe_infer(func.expr)` → Const str (and the
  `_infer_str_format_call` tip can fold the whole call §19.2);
  CallSite for arg counting.
- **E1310 bad-str-strip-call**: Const str arg only.

### logging — E12xx
- **E1200/E1201 (logging-unsupported-format / format-truncated), E1205/E1206
  (too-many/too-few-args)**: resolution of the logging module/logger objects:
  `Name` → `Import`/`ImportFrom` inference (§20.3) so `logging` resolves to the real
  stdlib parsed module; `node.func.expr` inference → Instance of `logging.Logger`
  (via `getLogger` return inference — FunctionDef.infer_call_result through the
  parsed stdlib source) or the module itself; ancestor checks
  (`is_subtype_of("logging.Logger")`-ish via ancestors()). Format-arg counting is
  pylint-side on Const format strings (safe_infer of the first arg).

### imports — E0402 (and F0002 indirectly)
- `do_import_module` → `relative_to_absolute_name` → `TooManyLevelsError` (§20.4);
  module spec resolution & caching (§20.2).

### fatal — F0001/F0002/F0010/F0011
- Not inference-dependent (file access / unexpected exceptions / config parsing),
  but F0002 ("unexpected AstroidError") means any panic in the port's inference layer
  must be caught at the per-module boundary and reported with the same template.

### E0202/F0202 (method-check-failed)
- F0202 is raised when pylint's classes checker fails to process a method —
  the port should mirror pylint's broad exception guard around method processing.

### misc / others
- **E0601/E0602-family (used-before-assignment / undefined-variable)** (E06xx in
  scope): heavy users of `lookup`/`_filter_stmts` (§7) and `are_exclusive` (§7.5);
  builtin_lookup for builtin names (§7.2).
- **E1003 bad-super-call**: `Super` object construction & `super_mro` errors (§12.8,
  §19.3 infer_super).
- **E1507/E1519/E1520 (envvar / singledispatch)**: Const-arg inference of calls
  (safe_infer → Const str) and decorator inference.
- **E17xx (async)**: `AsyncFunctionDef` detection, `__aenter__`/`__aexit__` igetattr,
  AsyncGenerator (§1, §11.3).
- **E25xx (non-ascii / unicode)**: no inference.
- **E3102 positional-only**: Arguments.posonlyargs shape.
- **E3701 invalid-field-call**: dataclasses brain (out of this doc's scope; the
  brain registers inference tips for `field()` calls).
- **E47xx (threading/lock)**: `with` manager inference (§9) + qname checks of
  inferred Instance `_proxied`.

---

## Appendix A — exception taxonomy (astroid/exceptions.py)

```
AstroidError
├── AstroidBuildingError          ("Failed to import module {modname}.")
│   ├── AstroidImportError
│   │   └── TooManyLevelsError    ("Relative import with too many levels ({level}) for module {name!r}")
│   └── AstroidSyntaxError
├── NoDefault
├── ResolveError
│   ├── MroError
│   │   ├── DuplicateBasesError
│   │   └── InconsistentMroError
│   ├── SuperError
│   ├── InferenceError            ("Inference failed for {node!r}.")
│   │   └── NameInferenceError    ("{name!r} not found in {scope!r}.")
│   └── AttributeInferenceError   ("{attribute!r} not found on {target!r}.")
├── AstroidTypeError / AstroidIndexError / AstroidValueError
├── _NonDeducibleTypeHierarchy (internal)
└── UseInferenceDefault / InferenceOverwriteError / StatementMissing / ParentMissingError
```
Catch-site discipline matters: `AttributeInferenceError` is NOT an `InferenceError`;
code that catches only one will propagate the other (e.g. `_infer_attribute` catches
both plus `AttributeError`).

## Appendix B — `fromlineno` specifics used by reporting

- `NodeNG.fromlineno` = `self.lineno` or `_fixed_source_line()` (first child/parent
  with a lineno, else 0) — node_ng.py:399-443.
- `FunctionDef.fromlineno` (scoped_nodes.py:1386-1400): `lineno + sum(decorator
  node line spans)` — i.e. the `def` line, not the first decorator. (Python 3.8+
  ast already points lineno at `def`, but astroid recomputes; for decorated functions
  `self.lineno` is set to the first decorator's line by the rebuilder and this
  property adds the decorator spans back.) ClassDef has a similar `fromlineno`.
- `Arguments.fromlineno` = `max(super().fromlineno, parent.fromlineno or 0)`
  (node_classes.py:784-791).
- `tolineno` = `end_lineno` if present else last child's tolineno (node_ng.py:409-424).

## Appendix C — minimal port checklist (behaviors easily missed)

1. `bool(Uninferable) is False`; attribute access on it returns itself.
2. `path_wrapper` early-return makes generators EMPTY (→ StopIteration semantics),
   and decorators translate that into InferenceError or a single Uninferable
   depending on the node type table in §4.1.
3. The global inference cache key includes callcontext/boundnode **by identity**;
   truncated result lists are cached.
4. `_filter_stmts` `mylineno` only filters when the *frames match* and offset/mystmt
   conditions hold; `offset=-1` paths (class bases, default args) shift the frame up.
5. `FunctionDef.infer_call_result`: docstring-only body raises InferenceError (body
   list excludes the docstring).
6. `ClassDef.igetattr` "last function wins" filtering and the descriptor-instance →
   Uninferable rule.
7. `Instance.getattr` returns instance attrs **plus** class attrs (concatenated).
8. dunder lookup never consults the instance, and for classes goes to the metaclass.
9. BinOp `type_errors()` returns [] if ANY result was Uninferable.
10. `dict`/`tuple`-style builtin transforms trigger on *names*, not resolved builtins
    (a local `def list(): ...` call still hits the tip predicate but the tip itself
    usually falls back via UseInferenceDefault... it does NOT check shadowing —
    `list("ab")` with a local `list` function WILL be folded by the brain. Bug-for-bug.)
11. `sys.argv` attribute access is hardcoded Uninferable.
12. `str(x)` infers to `Const("")` regardless of x.
13. `Const(NotImplemented).bool_value()` is True on 3.12.
14. The `with_metaclass` hack in `FunctionDef.infer_call_result` (scoped_nodes.py:
    1577-1615) creates hidden classes — `ancestors()` unhides via `.hide` flag.
