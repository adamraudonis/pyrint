# 06 — typecheck: inference-based type errors (E1102, E1111, E112x, E113x, E114x, E1701)

Pinned sources:

- pylint 4.0.5 — `reference/pylint/pylint/checkers/typecheck.py` (2355 lines)
- pylint 4.0.5 — `reference/pylint/pylint/checkers/utils.py` (2358 lines)
- pylint 4.0.5 — `reference/pylint/pylint/checkers/async_checker.py` (E1701)
- astroid 4.0.4 — `reference/astroid/astroid/arguments.py` (CallSite, 309 lines), plus
  `bases.py`, `nodes/node_classes.py`, `nodes/scoped_nodes/scoped_nodes.py`, `util.py`,
  `helpers.py`, `interpreter/dunder_lookup.py`, `objects.py`, `brain/brain_functools.py`,
  `context.py`.

All file:line citations below refer to these pinned files. Verified behavior was
cross-checked against the live venv at `/Users/adamraudonis/Desktop/Projects/prylint/.venv-pylint`
(pylint 4.0.5 / astroid 4.0.4).

In-scope messages covered by this document:

| id | symbol | checker class | visit method |
|----|--------|---------------|--------------|
| E1102 | not-callable | TypeChecker | `visit_call` (via `_check_not_callable`) |
| E1111 | assignment-from-no-return | TypeChecker | `visit_assign` |
| E1120 | no-value-for-parameter | TypeChecker | `visit_call` |
| E1121 | too-many-function-args | TypeChecker | `visit_call` |
| E1123 | unexpected-keyword-arg | TypeChecker | `visit_call` |
| E1124 | redundant-keyword-arg | TypeChecker | `visit_call` |
| E1125 | missing-kwoa | TypeChecker | `visit_call` |
| E1126 | invalid-sequence-index | TypeChecker | `visit_subscript` |
| E1127 | invalid-slice-index | TypeChecker | `visit_subscript` (via `_check_invalid_sequence_index`) |
| E1128 | assignment-from-none | TypeChecker | `visit_assign` |
| E1129 | not-context-manager | TypeChecker | `visit_with` |
| E1130 | invalid-unary-operand-type | TypeChecker | `visit_unaryop` |
| E1131 | unsupported-binary-operation | TypeChecker | `visit_binop` (effectively dead on py≥3.10, see §5.7) |
| E1132 | repeated-keyword | TypeChecker | `visit_call` |
| E1133 | not-an-iterable | IterableChecker | `visit_for/asyncfor/yieldfrom/call/listcomp/dictcomp/setcomp/generatorexp` |
| E1134 | not-a-mapping | IterableChecker | `visit_call` |
| E1135 | unsupported-membership-test | TypeChecker | `visit_compare` |
| E1136 | unsubscriptable-object | TypeChecker | `visit_subscript` |
| E1137 | unsupported-assignment-operation | TypeChecker | `visit_subscript` |
| E1138 | unsupported-delete-operation | TypeChecker | `visit_subscript` |
| E1139 | invalid-metaclass | TypeChecker | `visit_classdef` |
| E1141 | dict-iter-missing-items | TypeChecker | `visit_for` |
| E1142 | await-outside-async | TypeChecker | `visit_await` |
| E1143 | unhashable-member | TypeChecker | `visit_dict`, `visit_set`, `visit_subscript` |
| E1144 | invalid-slice-step | TypeChecker | `visit_subscript` (via `_check_invalid_slice_index`) |
| E1145 | async-context-manager-with-regular-with | TypeChecker | `visit_with` |
| E1701 | not-async-context-manager | AsyncChecker | `visit_asyncwith` |

Out of scope here (excluded by the run config or other notes): E1101/I1101 (no-member,
excluded), W1113/W1114/W1115/W1116/W1117 (W category, disabled under `-E`; W1117's
side-effects on E-message control flow *are* documented in §5.3), E1700
(yield-inside-async-function — note: it IS an E-id and lives in AsyncChecker; included
briefly in §5.15 for completeness).

Registration (typecheck.py:2353-2355):

```python
def register(linter: PyLinter) -> None:
    linter.register_checker(TypeChecker(linter))
    linter.register_checker(IterableChecker(linter))
```

and async_checker.py:96-97 registers `AsyncChecker(linter)`. Both `TypeChecker` and
`IterableChecker` have `name = "typecheck"`; `AsyncChecker` has `name = "async"`.

---

## 1. Shared infrastructure

### 1.1 Exact message format strings (typecheck.py:220-413, async_checker.py:27-42)

```python
"E1102": ("%s is not callable", "not-callable", ...),
"E1111": ("Assigning result of a function call, where the function has no return",
          "assignment-from-no-return", ...),
"E1120": ("No value for argument %s in %s call", "no-value-for-parameter", ...),
"E1121": ("Too many positional arguments for %s call", "too-many-function-args", ...),
"E1123": ("Unexpected keyword argument %r in %s call", "unexpected-keyword-arg", ...),
"E1124": ("Argument %r passed by position and keyword in %s call",
          "redundant-keyword-arg", ...),
"E1125": ("Missing mandatory keyword argument %r in %s call", "missing-kwoa", ...),
"E1126": ("Sequence index is not an int, slice, or instance with __index__",
          "invalid-sequence-index", ...),
"E1127": ("Slice index is not an int, None, or instance with __index__",
          "invalid-slice-index", ...),
"E1128": ("Assigning result of a function call, where the function returns None",
          "assignment-from-none", ..., {"old_names": [("W1111", "old-assignment-from-none")]}),
"E1129": ("Context manager '%s' doesn't implement __enter__ and __exit__.",
          "not-context-manager", ...),
"E1145": ("Context manager '%s' is async and should be used with 'async with'.",
          "async-context-manager-with-regular-with", ...),
"E1130": ("%s", "invalid-unary-operand-type", ...),
"E1131": ("%s", "unsupported-binary-operation", ...),
"E1132": ("Got multiple values for keyword argument %r in function call",
          "repeated-keyword", ...),
"E1135": ("Value '%s' doesn't support membership test", "unsupported-membership-test", ...),
"E1136": ("Value '%s' is unsubscriptable", "unsubscriptable-object", ...),
"E1137": ("%r does not support item assignment", "unsupported-assignment-operation", ...),
"E1138": ("%r does not support item deletion", "unsupported-delete-operation", ...),
"E1139": ("Invalid metaclass %r used", "invalid-metaclass", ...),
"E1141": ("Unpacking a dictionary in iteration without calling .items()",
          "dict-iter-missing-items", ...),
"E1142": ("'await' should be used within an async function", "await-outside-async", ...),
"E1143": ("'%s' is unhashable and can't be used as a %s in a %s", "unhashable-member",
          ..., {"old_names": [("E1140", "unhashable-dict-key")]}),
"E1144": ("Slice step cannot be 0", "invalid-slice-step", ...),
# IterableChecker (typecheck.py:2257-2270)
"E1133": ("Non-iterable value %s is used in an iterating context", "not-an-iterable", ...),
"E1134": ("Non-mapping value %s is used in a mapping context", "not-a-mapping", ...),
# AsyncChecker (async_checker.py:28-41)
"E1700": ("Yield inside async function", "yield-inside-async-function", ..., {"minversion": (3, 5)}),
"E1701": ("Async context manager '%s' doesn't implement __aenter__ and __aexit__.",
          "not-async-context-manager", ..., {"minversion": (3, 5)}),
```

`%` formatting is applied as `msg % args` (single-arg tuples are passed as 1-tuples).
`%r` means Python `repr()` of the arg.

### 1.2 Visit-method gating: `only_required_for_messages` + ASTWalker

utils.py:480-501 — the decorator merely stores `func.checks_msgs = messages`.

ast_walker.py:37-40:

```python
def _is_method_enabled(self, method: AstCallback) -> bool:
    if not hasattr(method, "checks_msgs"):
        return True
    return any(self.linter.is_message_enabled(m) for m in method.checks_msgs)
```

`is_message_enabled` (message_state_handler.py:315-345): unknown message
names/symbols do **not** raise — they fall back to being treated as raw message ids:

```python
try:
    msgids = self.linter.msgs_store.message_id_store.get_active_msgids(msg_descr)
except exceptions.UnknownMessageError:
    # The linter checks for messages that are not registered
    # due to version mismatch, just treat them as message IDs for now.
    msgids = [msg_descr]
return any(self._is_one_message_enabled(msgid, line) for msgid in msgids)
```

and `_is_one_message_enabled` with `line=None` is `self._msgs_state.get(msgid, True)`
(message_state_handler.py:285-286) — i.e. unknown ids default to **enabled**.

**Bug to replicate (E1141)**: `TypeChecker.visit_for` is decorated
`@only_required_for_messages("dict-items-missing-iter")` (typecheck.py:2203) but the real
symbol is `dict-iter-missing-items`. Because of the unknown-message fallback above the
method is *always* registered, even if E1141 is disabled. The actual emission check still
happens inside `add_message`, so this typo only affects which visit methods run (and is
behaviorally invisible for the target invocation where E1141 is enabled).

Under the target invocation (`pylint . -E --disable=...`): `-E` calls
`disable_noerror_messages` (message_state_handler.py:234-239) which disables every
message category except `E` and `F`. Therefore: visit methods gated only on W/C/R
messages never run; methods gated on a mix run (e.g. `visit_assign` is gated on
`assignment-from-no-return`, `assignment-from-none`, `non-str-assignment-to-dunder-name`
— it runs because the two E messages are enabled, but `_check_dundername_is_string`'s
`add_message("non-str-assignment-to-dunder-name")` is filtered at emission time).

Methods with no decorator always run: `TypeChecker.visit_call`,
`TypeChecker.visit_assignattr`, `TypeChecker.visit_delattr`.

### 1.3 Message location attribution

`BaseChecker.add_message(msgid, node=node, args=..., confidence=...)` forwards to
`PyLinter.add_message` → `_add_one_message` (pylinter.py:1195-1230):

```python
if node:
    if node.position:
        if not line: line = node.position.lineno
        if not col_offset: col_offset = node.position.col_offset
        if not end_lineno: end_lineno = node.position.end_lineno
        if not end_col_offset: end_col_offset = node.position.end_col_offset
    else:
        if not line: line = node.fromlineno
        if not col_offset: col_offset = node.col_offset
        if not end_lineno: end_lineno = node.end_lineno
        if not end_col_offset: end_col_offset = node.end_col_offset
```

`node.position` is only set on ClassDef/FunctionDef-like nodes (it is the
keyword-to-name span, e.g. `class Foo` — for ClassDef it excludes decorators). For all
other nodes `fromlineno`/`col_offset`/`end_lineno`/`end_col_offset` are used. Note
`fromlineno` for FunctionDef/ClassDef skips decorator lines
(scoped_nodes.py:1386-1400). None of the in-scope checks pass an explicit `line=`.

The checks in this doc report at these nodes:

- E1102: the `nodes.Call` node.
- E1111/E1128: the `nodes.Assign` statement node.
- E1120/E1121/E1123/E1124/E1125/E1132: the `nodes.Call` node.
- E1126: the `nodes.Subscript` node.
- E1127: each offending slice-component expression node (`node.lower`/`node.upper`/`node.step` of the inferred `nodes.Slice`).
- E1144: the `node.step` expression node.
- E1129/E1145: the `nodes.With` node.
- E1130: the `nodes.UnaryOp` node.
- E1131: the `nodes.BinOp` node.
- E1133/E1134: the iterable/mapping *expression* node passed to the check (e.g. `node.iter`, `stararg.value`, `kwarg.value`, `gen.iter`, `node.value` of YieldFrom).
- E1135: the right-hand operand node of the `in`/`not in` comparison.
- E1136/E1137/E1138: `node.value` of the Subscript (NOT the subscript itself).
- E1139: the `nodes.ClassDef` node (which has `position`, so the report lands on the `class Name` keyword span).
- E1141: the `nodes.For` node.
- E1142: the `nodes.Await` node.
- E1143: the offending key node (visit_dict), element node (visit_set), or — careful — `node.value` (the Dict literal) for the visit_subscript variant.
- E1701: the `nodes.AsyncWith` node.

### 1.4 Confidence levels used by in-scope emissions

`from pylint.interfaces import HIGH, INFERENCE` (typecheck.py:55). Default if omitted is
`UNDEFINED`. Confidence only matters for `--confidence` filtering (default config has all
confidences enabled), but record them anyway:

- INFERENCE: E1111 (builtin-no-return path only), E1125, E1143 (all three call sites), E1145, E1131 (union-syntax path), W1116.
- HIGH: E1121-from-`_check_isinstance_args`, E1120-from-`_check_isinstance_args`, E1144, W1117.
- UNDEFINED (default): everything else (E1102, E1111 main path, E1120, E1121, E1123, E1124, E1126, E1127, E1128, E1129, E1130, E1132, E1133, E1134, E1135, E1136, E1137, E1138, E1139, E1141, E1142, E1701).

---

## 2. Configuration options and defaults (typecheck.py:839-983, async_checker.py:44-46)

| option | default | used by (in scope) |
|--------|---------|--------------------|
| `ignore-on-opaque-inference` | `True` | only `visit_attribute` (no-member; out of scope) |
| `mixin-class-rgx` | `".*[Mm]ixin"` (regex) | E1701 mixin skip (`AsyncChecker.open` caches it as `self._mixin_class_rgx`); also `TypeChecker.open` caches it but TypeChecker only uses it for no-member |
| `ignore-mixin-members` | `True` | deprecated; **not read** by any in-scope check |
| `ignored-checks-for-mixins` | `["no-member", "not-async-context-manager", "not-context-manager", "attribute-defined-outside-init"]` | E1129 (`"not-context-manager" in ...`), E1701 (`"not-async-context-manager" in ...`) |
| `ignore-none` | `True` | no-member only |
| `ignored-classes` | `("optparse.Values", "thread._local", "_thread._local", "argparse.Namespace")` | no-member only — **not consulted by any in-scope check** |
| `generated-members` | `()` | no-member only |
| `contextmanager-decorators` | `["contextlib.contextmanager"]` | E1129 generator branch |
| `missing-member-hint-distance` / `missing-member-max-choices` / `missing-member-hint` | 1 / 1 / True | no-member only |
| `signature-mutators` | `[]` | visit_call early bail (E1120/21/23/24/25) |
| `py-version` (base option, base_options.py:356-365) | `sys.version_info[:2]` of the running interpreter → **(3, 12)** for the ground-truth runtime | `TypeChecker.open`: `_py310_plus = py_version >= (3,10)` → True; `_py314_plus = py_version >= (3,14)` → False. Gates E1131 (dead when `_py310_plus`) and postponed-evaluation handling |

`TypeChecker.open` (typecheck.py:985-990) and `visit_module` (992-995):

```python
def open(self) -> None:
    py_version = self.linter.config.py_version
    self._py310_plus = py_version >= (3, 10)
    self._py314_plus = py_version >= (3, 14)
    self._postponed_evaluation_enabled = False
    self._mixin_class_rgx = self.linter.config.mixin_class_rgx

def visit_module(self, node: nodes.Module) -> None:
    self._postponed_evaluation_enabled = (
        self._py314_plus or is_postponed_evaluation_enabled(node)
    )
```

With py-version (3,12): `_postponed_evaluation_enabled` is True iff the module has
`from __future__ import annotations` (utils.py:1608-1611: `"annotations" in
module.future_imports`).

---

## 3. pylint/checkers/utils.py helpers (exact semantics)

### 3.1 `safe_infer` (utils.py:1347-1410) — THE core guard

```python
@lru_cache(maxsize=1024)
def safe_infer(
    node: nodes.NodeNG,
    context: InferenceContext | None = None,
    *,
    compare_constants: bool = False,
    compare_constructors: bool = False,
) -> InferenceResult | None:
    """Return the inferred value for the given node.

    Return None if inference failed or if there is some ambiguity (more than
    one node has been inferred of different types)...
    """
    inferred_types: set[str | None] = set()
    try:
        infer_gen = node.infer(context=context)
        value = next(infer_gen)
    except astroid.InferenceError:
        return None
    except Exception as e:  # pragma: no cover
        raise AstroidError from e

    if not isinstance(value, util.UninferableBase):
        inferred_types.add(_get_python_type_of_node(value))

    try:
        for inferred in infer_gen:
            inferred_type = _get_python_type_of_node(inferred)
            if inferred_type not in inferred_types:
                return None  # If there is ambiguity on the inferred node.
            if (
                compare_constants
                and isinstance(inferred, nodes.Const)
                and isinstance(value, nodes.Const)
                and inferred.value != value.value
            ):
                return None
            if (
                isinstance(inferred, nodes.FunctionDef)
                and isinstance(value, nodes.FunctionDef)
                and function_arguments_are_ambiguous(inferred, value)
            ):
                return None
            if (
                compare_constructors
                and isinstance(inferred, nodes.ClassDef)
                and isinstance(value, nodes.ClassDef)
                and class_constructors_are_ambiguous(inferred, value)
            ):
                return None
    except astroid.InferenceError:
        return None  # There is some kind of ambiguity
    except StopIteration:
        return value
    except Exception as e:  # pragma: no cover
        raise AstroidError from e
    return value if len(inferred_types) <= 1 else None
```

Key semantics, exactly:

- The *first* inferred value is kept as `value`; if it is `Uninferable` its type is NOT
  added to `inferred_types` (so `inferred_types` stays empty).
- For every subsequent inferred value: its python-type string (`pytype()` result via
  `_get_python_type_of_node`, utils.py:1340-1344, or `None` when the object has no
  callable `pytype`) must already be in `inferred_types`, otherwise → `None`.
  - **Consequence**: if the first value is Uninferable and there is a second value of
    any type, `inferred_type not in inferred_types` (empty set) → `None`.
  - If the only value is Uninferable, the function returns **Uninferable itself** (not
    None). Callers that test `if not inferred:` handle this because
    `UninferableBase.__bool__` is `False` (astroid util.py:42-43). Callers that test
    `is None or isinstance(..., UninferableBase)` handle it explicitly.
  - If multiple values all share the same pytype string (e.g. two different `int`
    Consts), the FIRST one is returned (unless one of the compare_* rules below fires).
- `compare_constants=False` for all in-scope call sites except none; (it is used by
  other checkers). `compare_constructors=True` is used **only** for
  `visit_call`'s `safe_infer(node.func, compare_constructors=True)` (typecheck.py:1459).
- Two FunctionDefs are "ambiguous" (→ None) per `function_arguments_are_ambiguous`
  (utils.py:1425-1448):

```python
def function_arguments_are_ambiguous(func1, func2) -> bool:
    if func1.argnames() != func2.argnames():
        return True
    pairs_of_defaults = [
        (func1.args.defaults, func2.args.defaults),
        (func1.args.kw_defaults, func2.args.kw_defaults),
    ]
    for zippable_default in pairs_of_defaults:
        if None in zippable_default:
            continue
        if len(zippable_default[0]) != len(zippable_default[1]):
            return True
        for default1, default2 in zip(*zippable_default):
            match (default1, default2):
                case [nodes.Const(), nodes.Const()]:
                    return default1.value != default2.value
                case [nodes.Name(), nodes.Name()]:
                    return default1.name != default2.name
                case _:
                    return True
    return False
```

  Note the early `return` inside the inner loop: only the FIRST pair of defaults is
  actually compared; a `(Const, Const)` pair returns immediately with the equality
  result. (Bug-for-bug: replicate exactly.)

- `class_constructors_are_ambiguous` (utils.py:1451-1463): looks up
  `local_attr("__init__")[0]` on both classes (NotFoundError → False, i.e. NOT
  ambiguous); non-FunctionDef constructors → False; else delegates to
  `function_arguments_are_ambiguous`.

- **lru_cache**: keyed on `(node, context, compare_constants, compare_constructors)`.
  `NodeNG.__hash__` is identity. Contexts are freshly constructed at most call sites so
  context≠None calls effectively never hit the cache; context=None calls are cached
  across the entire run (maxsize 1024, LRU eviction).

### 3.2 astroid's `safe_infer` is DIFFERENT (astroid/util.py:137-159)

```python
def safe_infer(node, context=None):
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
        return None
    except StopIteration:
        return value
```

No type-comparison: *any* second inferred result → None. Used by `CallSite._unpack_args`
/ `_unpack_keywords` (arguments.py:99,106,129) and by `visit_subscript`'s decorator
handling (`astroid.util.safe_infer(inferred.decorators.nodes[0])`, typecheck.py:2189).
Do not confuse the two.

### 3.3 `has_known_bases` (utils.py:1466-1484)

```python
def has_known_bases(klass, context=None) -> bool:
    try:
        return klass._all_bases_known
    except AttributeError:
        pass
    for base in klass.bases:
        result = safe_infer(base, context=context)
        if (
            not isinstance(result, nodes.ClassDef)
            or result is klass
            or not has_known_bases(result, context=context)
        ):
            klass._all_bases_known = False
            return False
    klass._all_bases_known = True
    return True
```

Memoized on the node itself (`_all_bases_known`). Called with `Instance` objects too —
attribute access proxies through to the `_proxied` ClassDef (so the memo lands on the
class). A base that infers to None/Uninferable/non-class, or a direct self-reference, or
a base with unknown bases → False.

### 3.4 Protocol-support helpers (E1133/E1134/E1135/E1136/E1137/E1138)

Constants (utils.py:58-66): `ITER_METHOD="__iter__"`, `AITER_METHOD="__aiter__"`,
`GETITEM_METHOD="__getitem__"`, `CLASS_GETITEM_METHOD="__class_getitem__"`,
`SETITEM_METHOD="__setitem__"`, `DELITEM_METHOD="__delitem__"`,
`CONTAINS_METHOD="__contains__"`, `KEYS_METHOD="keys"`.

`_supports_protocol_method` (utils.py:1189-1210):

```python
def _supports_protocol_method(value: nodes.NodeNG, attr: str) -> bool:
    try:
        attributes = value.getattr(attr)
    except astroid.NotFoundError:
        return False

    first = attributes[0]

    # Return False if a constant is assigned
    if isinstance(first, nodes.AssignName):
        this_assign_parent = get_node_first_ancestor_of_type(
            first, (nodes.Assign, nodes.NamedExpr)
        )
        if this_assign_parent is None:  # pragma: no cover
            return True
        if isinstance(this_assign_parent.value, nodes.BaseContainer):
            if all(isinstance(n, nodes.Const) for n in this_assign_parent.value.elts):
                return False
        if isinstance(this_assign_parent.value, nodes.Const):
            return False
    return True
```

i.e. `value.getattr(attr)` must succeed (Instance.getattr = instance attrs + class MRO
attrs; ClassDef.getattr = MRO + metaclass special attrs — astroid surface). If the first
found attribute is an `AssignName` whose enclosing Assign/NamedExpr assigns a Const or a
container of only-Consts (e.g. `__getitem__ = None`, `__iter__ = (1,2)`), the protocol
is NOT supported. Anything else (FunctionDef, etc.) → supported.

`_supports_protocol` (utils.py:1275-1301) — the dispatcher over inferred-value kinds:

```python
def _supports_protocol(value, protocol_callback) -> bool:
    match value:
        case nodes.ClassDef():
            if not has_known_bases(value):
                return True
            # classobj can only be iterable if it has an iterable metaclass
            meta = value.metaclass()
            if meta is not None:
                if protocol_callback(meta):
                    return True
        case astroid.BaseInstance():
            if not has_known_bases(value):
                return True
            if value.has_dynamic_getattr():
                return True
            if protocol_callback(value):
                return True

        case nodes.ComprehensionScope():
            return True

        case bases.Proxy(_proxied=astroid.BaseInstance() as p) if has_known_bases(p):
            return protocol_callback(p)

    return False
```

Conservatism baked in: unknown bases → assume supported (True); dynamic
`__getattr__`/`__getattribute__` → assume supported. Note match-case order: ClassDef is
checked before BaseInstance (a ClassDef is not a BaseInstance, but Const/List/Tuple/Set/
Dict *are* `bases.Instance` subclasses — node_classes.py:269,2014,2321,3263,3517,4017 —
so literals go through the BaseInstance arm with `_proxied` = the builtin class).
For a ClassDef the protocol is looked up on its **metaclass** (a class object `C` is
iterable only if `type(C)` defines `__iter__`). If metaclass() is None → falls to
`return False`.

`has_dynamic_getattr` (scoped_nodes.py:2516-2538): True if class (or its MRO) defines
`__getattr__` or `__getattribute__` that is not from builtins and comes from a
pure-python module:

```python
def _valid_getattr(node):
    root = node.root()
    return root.name != "builtins" and getattr(root, "pure_python", None)
```

Wrappers (utils.py:1223-1252, 1304-1337):

```python
def _supports_mapping_protocol(value):      # __getitem__ AND keys
def _supports_membership_test_protocol(value):  # __contains__
def _supports_iteration_protocol(value):    # __iter__ OR __getitem__
def _supports_async_iteration_protocol(value):  # __aiter__
def _supports_getitem_protocol(value):      # __getitem__
def _supports_setitem_protocol(value):      # __setitem__
def _supports_delitem_protocol(value):      # __delitem__

def is_iterable(value, check_async=False) -> bool:
    if check_async:
        protocol_check = _supports_async_iteration_protocol
    else:
        protocol_check = _supports_iteration_protocol
    return _supports_protocol(value, protocol_check)

def is_mapping(value) -> bool:
    return _supports_protocol(value, _supports_mapping_protocol)

def supports_membership_test(value) -> bool:
    supported = _supports_protocol(value, _supports_membership_test_protocol)
    return supported or is_iterable(value)

def supports_getitem(value, node) -> bool:
    if isinstance(value, nodes.ClassDef):
        if _supports_protocol_method(value, CLASS_GETITEM_METHOD):
            return True
        if is_postponed_evaluation_enabled(node) and is_node_in_type_annotation_context(node):
            return True
    return _supports_protocol(value, _supports_getitem_protocol)

def supports_setitem(value, _) -> bool:
    return _supports_protocol(value, _supports_setitem_protocol)

def supports_delitem(value, _) -> bool:
    return _supports_protocol(value, _supports_delitem_protocol)
```

`supports_getitem` specifics for ClassDef values (i.e. subscripting a *class*):

1. `__class_getitem__` found via `value.getattr` → supported. astroid brains inject
   `__class_getitem__` into many stdlib classes (e.g. brain_collections adds it to
   `collections.abc` classes and `deque`/`OrderedDict`/`defaultdict` on relevant
   pythons; brain_typing handles `typing` generics; builtins like `list`/`dict` get it
   from the real builtins stub on py3.9+). So `list[int]`, `dict[str, int]`,
   `type[Foo]`, `Optional[...]`, `Union[...]` etc. resolve as supported through normal
   getattr — there is **no special-case list in pylint** for PEP 585.
   (`SUBSCRIPTABLE_CLASSES_PEP585`, utils.py:195-236, exists but is **unused** in
   pylint 4.0.5 — confirmed by grep.)
2. Module has `from __future__ import annotations` AND the subscript appears inside a
   type-annotation context (see §3.8) → supported.
3. Otherwise the ClassDef arm of `_supports_protocol` runs: metaclass `__getitem__`.

### 3.5 `is_inside_abstract_class` (utils.py:1255-1272) + `class_is_abstract` (1162-1186)

```python
def _is_abstract_class_name(name: str) -> bool:
    lname = name.lower()
    is_mixin = lname.endswith("mixin")
    is_abstract = lname.startswith("abstract")
    is_base = lname.startswith("base") or lname.endswith("base")
    return is_mixin or is_abstract or is_base

def is_inside_abstract_class(node: nodes.NodeNG) -> bool:
    while node is not None:
        if isinstance(node, nodes.ClassDef):
            if class_is_abstract(node):
                return True
            name = getattr(node, "name", None)
            if name is not None and _is_abstract_class_name(name):
                return True
        node = node.parent
    return False
```

```python
@lru_cache(maxsize=1024)
def class_is_abstract(node: nodes.ClassDef) -> bool:
    # Protocol classes are considered "abstract"
    if is_protocol_class(node):
        return True
    # Only check for explicit metaclass=ABCMeta on this specific class
    meta = node.declared_metaclass()
    if meta is not None:
        if meta.name == "ABCMeta" and meta.root().name in ABC_MODULES:  # {"abc","_py_abc"}
            return True
    for ancestor in node.ancestors():
        if ancestor.name == "ABC" and ancestor.root().name in ABC_MODULES:
            return True
    for method in node.methods():
        if method.parent.frame() is node:
            if method.is_abstract(pass_is_abstract=False):
                return True
    return False
```

`is_protocol_class` (utils.py:1677-1697): qname in
`{"typing.Protocol", "typing_extensions.Protocol", ".Protocol"}` or any base inferring
to one of those (InferenceError per-base → continue).
`FunctionDef.is_abstract(pass_is_abstract=False)` (scoped_nodes.py:1475-1509): decorated
with `abc.abstractproperty`/`abc.abstractmethod` (first inferred decorator), or the first
body statement is a Raise that `raises_not_implemented()`.

This guard suppresses E1133/E1134/E1135/E1136/E1137/E1138 anywhere lexically inside a
class that is abstract OR merely *named* like one (`*mixin`, `abstract*`, `base*`,
`*base`, case-insensitive).

### 3.6 `is_hashable` (utils.py:2079-2098) — E1143

```python
def is_hashable(node: nodes.NodeNG) -> bool:
    """Return whether any inferred value of `node` is hashable.

    When finding ambiguity, return True.
    """
    try:
        for inferred in node.infer():
            if isinstance(inferred, (nodes.ClassDef, util.UninferableBase)):
                return True
            if not hasattr(inferred, "igetattr"):
                return True
            hash_fn = next(inferred.igetattr("__hash__"))
            if hash_fn.parent is inferred:
                return True
            if getattr(hash_fn, "value", True) is not None:
                return True
        return False
    except astroid.InferenceError:
        return True
```

- Iterates **all** inferred values of the key/element node; returns True on the first
  value that looks hashable; returns False only if every inferred value flunks all four
  tests (so the node must be unambiguously unhashable).
- Tests per inferred value: ClassDef or Uninferable → hashable; objects without an
  `igetattr` attribute (plain non-Instance nodes) → hashable; `__hash__` resolved via
  `igetattr` — if its `.parent is inferred` → hashable (degenerate self-parent case); if
  it is not a Const-None (i.e. `getattr(hash_fn, "value", True) is not None`) → hashable.
  The unhashable signal is precisely `__hash__` inferring to `Const(None)` — which is
  what astroid produces for `list`/`dict`/`set` instances and classes that set
  `__hash__ = None`.
- `next(inferred.igetattr("__hash__"))` can raise `StopIteration` if igetattr yields
  nothing — this is NOT caught and would propagate (potential crash path; in practice
  igetattr raises InferenceError instead of yielding nothing).
- InferenceError anywhere → True (conservative).

### 3.7 `decorated_with`, `decorated_with_property`, `is_overload_stub`

`decorated_with` (utils.py:870-891):

```python
def decorated_with(func, qnames) -> bool:
    decorators = func.decorators.nodes if func.decorators else []
    for decorator_node in decorators:
        if isinstance(decorator_node, nodes.Call):
            decorator_node = decorator_node.func
        try:
            if any(
                i.name in qnames or i.qname() in qnames
                for i in decorator_node.infer()
                if isinstance(i, (nodes.ClassDef, nodes.FunctionDef))
            ):
                return True
        except astroid.InferenceError:
            continue
    return False
```

Matches on bare `name` **or** qualified name. Used for: `contextmanager-decorators`
(E1129), `["contextlib.asynccontextmanager"]` (E1145/E1701), signature-mutators
(visit_call bail), `["typing.overload", "overload"]` (is_overload_stub).

`is_overload_stub` (utils.py:1666-1674):

```python
@lru_cache(maxsize=1024)
def is_overload_stub(node) -> bool:
    decorators = getattr(node, "decorators", None)
    return bool(decorators and decorated_with(node, ["typing.overload", "overload"]))
```

`decorated_with_property` (utils.py:805-815) + `_is_property_decorator` (843-867), used
by E1102's `_check_uninferable_call`:

```python
def decorated_with_property(node: nodes.FunctionDef) -> bool:
    if not node.decorators:
        return False
    for decorator in node.decorators.nodes:
        try:
            if _is_property_decorator(decorator):
                return True
        except astroid.InferenceError:
            pass
    return False

def _is_property_decorator(decorator: nodes.Name) -> bool:
    for inferred in decorator.infer():
        if isinstance(inferred, nodes.ClassDef):
            if inferred.qname() in {"builtins.property", "functools.cached_property"}:
                return True
            for ancestor in inferred.ancestors():
                if ancestor.name == "property" and ancestor.root().name == "builtins":
                    return True
        elif isinstance(inferred, nodes.FunctionDef):
            # If decorator is function, check if it has exactly one return
            # and the return is itself a function decorated with property
            returns: list[nodes.Return] = list(
                inferred._get_return_nodes_skip_functions()
            )
            if len(returns) == 1 and isinstance(
                returns[0].value, (nodes.Name, nodes.Attribute)
            ):
                inferred = safe_infer(returns[0].value)
                if (
                    inferred
                    and isinstance(inferred, objects.Property)
                    and isinstance(inferred.function, nodes.FunctionDef)
                ):
                    return decorated_with_property(inferred.function)
    return False
```

### 3.8 Annotation/type-checking context helpers

`is_postponed_evaluation_enabled` (utils.py:1608-1611): `"annotations" in
node.root().future_imports`.

`is_node_in_type_annotation_context` (utils.py:1614-1637):

```python
def is_node_in_type_annotation_context(node: nodes.NodeNG) -> bool:
    current_node, parent_node = node, node.parent
    while True:
        match parent_node:
            case nodes.AnnAssign(annotation=ann) if ann == current_node:
                return True
            case nodes.Arguments() if current_node in (
                *parent_node.annotations,
                *parent_node.posonlyargs_annotations,
                *parent_node.kwonlyargs_annotations,
                parent_node.varargannotation,
                parent_node.kwargannotation,
            ):
                return True
            case nodes.FunctionDef(returns=ret) if ret == current_node:
                return True
        current_node, parent_node = parent_node, parent_node.parent
        if isinstance(parent_node, nodes.Module):
            return False
```

`in_type_checking_block` (utils.py:1990-2017) — suppresses E1136/E1137/E1138 inside
`if TYPE_CHECKING:` blocks:

```python
def in_type_checking_block(node: nodes.NodeNG) -> bool:
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
            if (
                isinstance(maybe_import_from, nodes.ImportFrom)
                and maybe_import_from.modname == "typing"
            ):
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

Note: it does NOT verify the node is in the `body` (vs `orelse`) of the If.

### 3.9 Misc small helpers

- `is_none` (utils.py:1487-1491): `case None | nodes.Const(value=None) | nodes.Name(value="None")` — note `nodes.Name(value="None")` tests a `value` attribute that Name nodes don't actually have, so that arm never matches a real Name (`Name` has `.name`); effectively None-literal Const or actual `None` only.
- `is_comprehension` (utils.py:1213-1220): isinstance of ListComp/SetComp/DictComp/GeneratorExp.
- `is_builtin_object` (utils.py:286-288): `node and node.root().name == "builtins"`.
- `is_super` (utils.py:270-274): `getattr(node, "name", None) == "super" and node.root().name == "builtins"`.
- `is_error` (utils.py:277-279): function body is exactly one `Raise` statement.
- `infer_all` (utils.py:1413-1422): `list(node.infer(context))`, InferenceError → `[]` (not used by in-scope checks but part of the surface).

---

## 4. astroid surfaces required by these checks

### 4.1 `UninferableBase` / `Uninferable` (astroid/util.py:19-51)

Singleton; falsy (`__bool__` → False); `getattr` on it returns itself for non-dunder
names; calling it returns itself.

### 4.2 `CallContext` (astroid/context.py:164-181)

```python
class CallContext:
    __slots__ = ("args", "callee", "keywords")
    def __init__(self, args, keywords=None, callee=None):
        self.args = args  # Call positional arguments
        if keywords:
            arg_value_pairs = [(arg.arg, arg.value) for arg in keywords]
        else:
            arg_value_pairs = []
        self.keywords = arg_value_pairs
        self.callee = callee
```

`Call.args` is the source-order list of positional argument nodes **including
`Starred` nodes**; `Call.keywords` is the source-order list of `Keyword` nodes where
`kw.arg is None` for `**expr` entries. Convenience properties
(node_classes.py:1727-1735): `Call.starargs = [a for a in self.args if
isinstance(a, Starred)]`, `Call.kwargs = [k for k in self.keywords if k.arg is None]`.

### 4.3 `arguments.CallSite` — FULL specification (astroid/arguments.py:15-309)

Constructor (lines 32-54):

```python
def __init__(self, callcontext, argument_context_map=None, context=None):
    if argument_context_map is None:
        argument_context_map = {}
    self.argument_context_map = argument_context_map
    args = callcontext.args
    keywords = callcontext.keywords
    self.duplicated_keywords: set[str] = set()
    self._unpacked_args = self._unpack_args(args, context=context)
    self._unpacked_kwargs = self._unpack_keywords(keywords, context=context)

    self.positional_arguments = [
        arg for arg in self._unpacked_args if not isinstance(arg, UninferableBase)
    ]
    self.keyword_arguments = {
        key: value
        for key, value in self._unpacked_kwargs.items()
        if not isinstance(value, UninferableBase)
    }
```

`from_call` (56-66): `CallSite(CallContext(call_node.args, call_node.keywords),
context=context or InferenceContext())`.

`has_invalid_arguments` (68-76): `len(self.positional_arguments) !=
len(self._unpacked_args)` — i.e. some unpacked positional slot was Uninferable.
`has_invalid_keywords` (78-86): `len(self.keyword_arguments) !=
len(self._unpacked_kwargs)`.

`_unpack_args` (123-139):

```python
def _unpack_args(self, args, context=None):
    values = []
    context = context or InferenceContext()
    context.extra_context = self.argument_context_map
    for arg in args:
        if isinstance(arg, nodes.Starred):
            inferred = safe_infer(arg.value, context=context)   # astroid safe_infer!
            if isinstance(inferred, UninferableBase):
                values.append(Uninferable)
                continue
            if not hasattr(inferred, "elts"):
                values.append(Uninferable)
                continue
            values.extend(inferred.elts)
        else:
            values.append(arg)
    return values
```

So: `*x` where x infers (unambiguously, astroid-safe_infer) to anything with an `elts`
attribute (List/Tuple/Set...) is flattened into individual element nodes; otherwise it
contributes one Uninferable (note: astroid safe_infer returning **None** also lacks
`elts` → Uninferable). Plain args pass through as their AST nodes.

`_unpack_keywords` (88-121) verbatim (body of the per-keyword loop):

```python
    def _unpack_keywords(self, keywords, context=None):
        values: dict[str | None, InferenceResult] = {}
        context = context or InferenceContext()
        context.extra_context = self.argument_context_map
        for name, value in keywords:
            if name is None:
                # Then it's an unpacking operation (**)
                inferred = safe_infer(value, context=context)
                if not isinstance(inferred, nodes.Dict):
                    # Not something we can work with.
                    values[name] = Uninferable
                    continue

                for dict_key, dict_value in inferred.items:
                    dict_key = safe_infer(dict_key, context=context)
                    if not isinstance(dict_key, nodes.Const):
                        values[name] = Uninferable
                        continue
                    if not isinstance(dict_key.value, str):
                        values[name] = Uninferable
                        continue
                    if dict_key.value in values:
                        # The name is already in the dictionary
                        values[dict_key.value] = Uninferable
                        self.duplicated_keywords.add(dict_key.value)
                        continue
                    values[dict_key.value] = dict_value
            else:
                values[name] = value
        return values
```

Consequences (critical for E1120-E1125/E1132):

- `**expr` where expr does not infer to a literal `nodes.Dict` → entry `values[None] =
  Uninferable` → filtered out of `keyword_arguments` → `has_invalid_keywords()` is True
  → **visit_call bails entirely** (typecheck.py:1490-1492). This is the main reason
  `f(**unknown_dict)` produces no argument errors.
- `**{...}` literal: each key must (astroid-)safe_infer to a `Const` with a `str` value,
  else the `None` slot becomes Uninferable → bail as above.
- `duplicated_keywords` is populated **only** when a later `**{...}` key collides with a
  name already in `values` — i.e. `f(a=1, **{'a': 2})` or `f(**{'a':1}, **{'a':2})`
  flags `'a'`; the reverse order `f(**{'a': 1}, a=2)` silently **overwrites** and flags
  nothing (the explicit keyword is processed after, in source order, via the `else`
  branch which never checks membership).
- `values` is a dict keyed by str (and possibly the `None` key); insertion order =
  source order of keywords with **-unpacked names expanded in dict-literal order.
- `keyword_arguments` preserves that insertion order; visit_call iterates
  `list(call_site.keyword_arguments.keys())`.

`infer_argument` (141-309) is part of CallSite but is **not called by typecheck**; the
visit_call logic re-implements parameter matching itself. (infer_argument is exercised
indirectly through astroid's inference of function bodies; for the port's minimal
surface it matters only insofar as general inference does.) Its full text is in
arguments.py:141-309; key points if needed: keyword args take priority; bound-method
`self`/`cls` resolution from `context.boundnode`; `*args`/`**kwargs` synthesize
Tuple/Dict nodes; defaults via `funcnode.args.default_value(name)`; raises
InferenceError on duplicated keywords, too many positionals without vararg, or no value.

### 4.4 `Arguments` helpers used (node_classes.py:930-990, 1035-1039)

- `find_argname(argname)` → `(index, AssignName)` over `self.arguments` (which is
  posonlyargs + args + vararg-as-AssignName? — `arguments` is the combined list
  posonlyargs + args + [vararg] + kwonlyargs + [kwarg] as built by the rebuilder); first
  match by name; `(None, None)` if absent.
- `default_value(argname)`: kwonly first (`kw_defaults[index]`, None → NoDefault), then
  positional (`defaults[index - (len(args) - len(defaults) - len(kw_defaults))]` over
  `arguments` minus vararg/kwarg names), else NoDefault.
- `is_argument(name)`: vararg name, kwarg name, or find_argname hit.
- `Lambda.argnames()` / `FunctionDef.argnames()` (scoped_nodes.py:972-985, 1283-1296):
  `[elt.name for elt in self.args.arguments]` or `[]` if `arguments` is falsy.

For raw-built builtins (C functions), `args.args is None` — this is the "no argument
information" signal (typecheck.py:1470).

### 4.5 `callable()` / `implicit_parameters()` / `.type`

| object | callable() | implicit_parameters() |
|--------|-----------|----------------------|
| `NodeNG` default (node_ng.py:591-596) | False | (no method) |
| `nodes.Lambda` (scoped_nodes.py:964, 905) | True | 0 |
| `nodes.FunctionDef` (1280, 1412-1413) | True | `1 if self.is_bound() else 0` where `is_bound()` = `self.type in {"method","classmethod"}` (1468-1473) |
| `nodes.ClassDef` (1996-2002, 1918-1919) | True | 1 |
| `bases.Instance` (bases.py:375-380) | `self._proxied.getattr("__call__", class_context=False)` succeeds | (proxied — but `_determine_callable` never gets here) |
| `bases.UnboundMethod` (proxied) | True (proxy → FunctionDef) | 0 (bases.py:454-455) |
| `bases.BoundMethod` (bases.py:546-550) | True | `0 if self.name == "__new__" else 1` |
| `bases.Generator` (bases.py:706-707) | **False** | — |
| `bases.AsyncGenerator` (bases.py:762-763) | **False** | — |

`FunctionDef.type` (scoped_nodes.py:1313-1384, cached_property): `"function"`,
`"method"` (defined in a ClassDef frame), `"classmethod"` (`__new__`,
`__init_subclass__`, `__class_getitem__`, or decorator inference), `"staticmethod"`
(decorator). `Lambda.type` (908-917): `"method"` if first arg named `self` and parent
scope is a ClassDef else `"function"`. Decorator-based detection infers each decorator
node and consults `_infer_decorator_callchain` / subclass checks of
builtins.classmethod/staticmethod.

Inference of an attribute access like `obj.method` yields `BoundMethod`;
`Class.method` yields `UnboundMethod` (verified live: `A.m` → UnboundMethod,
`implicit_parameters()==0`); a bare function name yields `FunctionDef`.

### 4.6 functools.partial brain (brain_functools.py:74-130) + `PartialFunction` (objects.py:277-326)

`functools.partial(f, ...)` Call nodes (detected syntactically by name `partial` or
attribute `functools.partial` — `_looks_like_partial`, brain_functools.py:145-162) are
inferred as `objects.PartialFunction` when ALL hold:

- CallSite has ≥1 positional and (≥2 positionals or some keyword arguments);
- the first positional infers (plain `next(...infer())`) to a `nodes.FunctionDef`
  (not Uninferable);
- every keyword name passed is one of the wrapped function's
  args/posonlyargs/kwonlyargs names (else UseInferenceDefault → no PartialFunction).

`PartialFunction` is a FunctionDef subclass whose `.args` etc. are postinit-copied from
the wrapped function and which carries:

```python
self.filled_args = call.positional_arguments[1:]
self.filled_keywords = call.keyword_arguments
# nested partial flattening:
if isinstance(inferred_wrapped_function, PartialFunction):
    self.filled_args = inferred_wrapped_function.filled_args + self.filled_args
    self.filled_keywords = {**inferred_wrapped_function.filled_keywords, **self.filled_keywords}
self.filled_positionals = len(self.filled_args)
```

visit_call reads these via `getattr(called, "filled_positionals", 0)` /
`getattr(called, "filled_keywords", {})` (typecheck.py:1519-1520). For non-partial
callables the getattr defaults make them no-ops. `PartialFunction.type` resolves through
FunctionDef.type (its parent is the call's parent, not a ClassDef → `"function"`).

### 4.7 `dunder_lookup.lookup` (astroid/interpreter/dunder_lookup.py:41-75)

```python
def lookup(node, name, context=None) -> list:
    if isinstance(node, (nodes.List, nodes.Tuple, nodes.Const, nodes.Dict, nodes.Set)):
        return _builtin_lookup(node, name)            # node.locals.get(name, []) or raise
    if isinstance(node, astroid.Instance):
        return _lookup_in_mro(node, name)             # locals + all ancestors' locals
    if isinstance(node, nodes.ClassDef):
        return _class_lookup(node, name, context)     # metaclass MRO lookup
    raise AttributeInferenceError(...)
```

`_lookup_in_mro` chains `node.locals.get(name, [])` with each
`ancestor.locals.get(name, [])` over `node.ancestors(recurs=True)`; empty → raise.
`_class_lookup`: `node.metaclass()`; None → raise; else `_lookup_in_mro(metaclass, name)`.
Used by E1126 (`__getitem__`/`__setitem__`/`__delitem__` lookup) and by astroid's
unary-op inference (E1130).

### 4.8 `astroid.helpers.object_type` (helpers.py:60-109)

Used by E1131's union-syntax check and by `BadUnaryOperationMessage.__str__`.

```python
def _object_type(node, context=None):
    ...
    for inferred in node.infer(context=context):
        if isinstance(inferred, scoped_nodes.ClassDef):
            metaclass = inferred.metaclass(context=context)
            if metaclass:
                yield metaclass
                continue
            yield builtins.getattr("type")[0]
        elif isinstance(inferred, (scoped_nodes.Lambda, bases.UnboundMethod, scoped_nodes.FunctionDef)):
            yield _function_type(inferred, builtins)
        elif isinstance(inferred, scoped_nodes.Module):
            yield _build_proxy_class("module", builtins)
        elif isinstance(inferred, nodes.Unknown):
            raise InferenceError
        elif isinstance(inferred, util.UninferableBase):
            yield inferred
        elif isinstance(inferred, (bases.Proxy, nodes.Slice, objects.Super)):
            yield inferred._proxied
        else:  # pragma: no cover
            raise AssertionError(...)

def object_type(node, context=None):
    try:
        types = set(_object_type(node, context))
    except InferenceError:
        return util.Uninferable
    if len(types) != 1:
        return util.Uninferable
    return next(iter(types))
```

### 4.9 `Module.fully_defined` (scoped_nodes.py:399-407)

`self.file is not None and self.file.endswith(".py")` — used by E1111
(`function_node.root().fully_defined()`) and `_is_c_extension`.

### 4.10 UnaryOp inference machinery (E1130)

`UNARY_OP_METHOD` (node_classes.py:4249-4254):

```python
UNARY_OP_METHOD = {"+": "__pos__", "-": "__neg__", "~": "__invert__", "not": None}
```

`UnaryOp.type_errors` (node_classes.py:4296-4315):

```python
def type_errors(self, context=None) -> list[util.BadUnaryOperationMessage]:
    bad = []
    try:
        for result in self._infer_unaryop(context=context):
            if result is util.Uninferable:
                raise InferenceError
            if isinstance(result, util.BadUnaryOperationMessage):
                bad.append(result)
    except InferenceError:
        return []
    return bad
```

**If ANY inferred result is Uninferable, the whole list is discarded** (→ no E1130 at
all for that UnaryOp). `_infer_unaryop` (node_classes.py:4326-4388) verbatim:

```python
def _infer_unaryop(self, context=None, **kwargs):
    """Infer what an UnaryOp should return when evaluated."""
    from astroid.nodes import ClassDef

    for operand in self.operand.infer(context):
        try:
            yield operand.infer_unary_op(self.op)
        except TypeError as exc:
            # The operand doesn't support this operation.
            yield util.BadUnaryOperationMessage(operand, self.op, exc)
        except AttributeError as exc:
            meth = UNARY_OP_METHOD[self.op]
            if meth is None:
                # `not node`. Determine node's boolean value and negate its result,
                # unless it is Uninferable...
                bool_value = operand.bool_value()
                if not isinstance(bool_value, util.UninferableBase):
                    yield const_factory(not bool_value)
                else:
                    yield util.Uninferable
            else:
                if not isinstance(operand, (Instance, ClassDef)):
                    # The operation was used on something which doesn't support it.
                    yield util.BadUnaryOperationMessage(operand, self.op, exc)
                    continue

                try:
                    try:
                        methods = dunder_lookup.lookup(operand, meth)
                    except AttributeInferenceError:
                        yield util.BadUnaryOperationMessage(operand, self.op, exc)
                        continue

                    meth = methods[0]
                    inferred = next(meth.infer(context=context), None)
                    if isinstance(inferred, util.UninferableBase) or not inferred.callable():
                        continue

                    context = copy_context(context)
                    context.boundnode = operand
                    context.callcontext = CallContext(args=[], callee=inferred)

                    call_results = inferred.infer_call_result(self, context=context)
                    result = next(call_results, None)
                    if result is None:
                        # Failed to infer, return the same type.
                        yield operand
                    else:
                        yield result
                except AttributeInferenceError as inner_exc:
                    # The unary operation special method was not found.
                    yield util.BadUnaryOperationMessage(operand, self.op, inner_exc)
                except InferenceError:
                    yield util.Uninferable
```

`infer_unary_op` is defined ONLY on Const, Dict, List, Set, Tuple
(node_classes.py:2084, 2361, 3315, 3526, 4069 → protocols.py:48-78) — it applies the
real Python operator to the literal value (`operator.pos/neg/invert/not_`), raising
real `TypeError` for unsupported combos (e.g. `-"a"`). All other operand kinds
(Instance, ClassDef, FunctionDef, Module, ...) raise AttributeError → handled above:
non-Instance/ClassDef operands are immediately Bad; Instance/ClassDef do a
dunder_lookup of `__pos__/__neg__/__invert__` (Instance → MRO; ClassDef → metaclass
MRO), missing → Bad.

`BadUnaryOperationMessage.__str__` (astroid/util.py:83-95):

```python
def __str__(self) -> str:
    if hasattr(self.operand, "name"):
        operand_type = self.operand.name
    else:
        object_type = self._object_type(self.operand)
        if hasattr(object_type, "name"):
            operand_type = object_type.name
        else:
            # Just fallback to as_string
            operand_type = object_type.as_string()
    msg = "bad operand type for unary {}: {}"
    return msg.format(self.op, operand_type)
```

Note `hasattr(operand, "name")` is True for proxied objects (Const proxies the builtin
class — e.g. `-"x"` → operand is Const, `operand.name` → `"str"` via
`Proxy.__getattr__`, bases.py:140-145). For an Instance of class `C` → `"C"`.
`_object_type` returns None if object_type(...) is Uninferable (util.py:76-81) — then
`object_type.as_string()` on None would crash; in practice the operand was inferable.

### 4.11 BaseInstance.getattr (bases.py:243-272) — used by E1129/E1701/protocol checks

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
        raise AttributeInferenceError(...) from exc
    if lookupclass:
        try:
            return values + self._proxied.getattr(name, context, class_context=False)
        except AttributeInferenceError:
            pass
    return values
```

pylint catches `astroid.NotFoundError` which is an alias of
`AttributeInferenceError`.

---

## 5. The checks

### 5.1 E1102 not-callable — `visit_call` → `_check_not_callable` (typecheck.py:1455-1461, 1784-1813)

Entry (visit_call, line 1459-1461): `called = safe_infer(node.func,
compare_constructors=True)` then `self._check_not_callable(node, called)` — runs on
EVERY Call node (visit_call has no `only_required_for_messages`).

```python
def _check_not_callable(self, node, inferred_call) -> None:
    # Handle uninferable calls
    if not inferred_call or inferred_call.callable():
        self._check_uninferable_call(node)
        return

    if not isinstance(inferred_call, astroid.Instance):
        self.add_message("not-callable", node=node, args=node.func.as_string())
        return

    # Don't emit if we can't make sure this object is callable.
    if not has_known_bases(inferred_call):
        return

    if inferred_call.parent and isinstance(inferred_call.scope(), nodes.ClassDef):
        # Ignore descriptor instances
        if "__get__" in inferred_call.locals:
            return
        # NamedTuple instances are callable
        if inferred_call.qname() == "typing.NamedTuple":
            return

    self.add_message("not-callable", node=node, args=node.func.as_string())
```

Flow:

1. `inferred_call` falsy (safe_infer None, or Uninferable which is falsy) OR
   `.callable()` True → run the secondary property check (`_check_uninferable_call`)
   and stop. `callable()` per §4.5: FunctionDef/Lambda/ClassDef/(Un)BoundMethod →
   True (no message); Generator/AsyncGenerator → False; Instance → has `__call__` in
   class MRO (class_context=False).
2. Non-Instance non-callables (e.g. Const? — no: Const IS an Instance subclass;
   realistic hits: `bases.Generator`, `nodes.Module`? Module.callable() is the NodeNG
   default False and Module is not Instance → message). Message args:
   `node.func.as_string()` (exact source text of the callee expression).
3. Instance without `__call__`: bail if unknown bases. Then the descriptor/NamedTuple
   skips: `inferred_call.parent` / `.scope()` / `.locals` / `.qname()` all proxy to the
   instance's class — i.e. *if the class was defined inside a ClassDef body* (a nested
   class → `scope()` of the class is the outer ClassDef) and the class defines
   `__get__` locally → skip (descriptor); class qname `typing.NamedTuple` → skip.
   **Note** `inferred_call.scope()` is the scope of the *class definition*, so the
   descriptor skip only applies to classes nested inside another class body.
4. Otherwise → `not-callable` with `node.func.as_string()`.

Secondary path `_check_uninferable_call` (typecheck.py:1330-1375) — catches
`x.prop()` where `prop` is a property returning a non-callable:

```python
def _check_uninferable_call(self, node: nodes.Call) -> None:
    if not isinstance(node.func, nodes.Attribute):
        return
    expr = node.func.expr
    klass = safe_infer(expr)
    if not isinstance(klass, astroid.Instance):
        return
    try:
        attrs = klass._proxied.getattr(node.func.attrname)
    except astroid.NotFoundError:
        return
    for attr in attrs:
        if not isinstance(attr, nodes.FunctionDef):
            continue
        if decorated_with_property(attr):
            try:
                call_results = list(attr.infer_call_result(node))
            except astroid.InferenceError:
                continue
            if all(isinstance(return_node, util.UninferableBase)
                   for return_node in call_results):
                continue
            if any(return_node.callable() for return_node in call_results):
                continue
            self.add_message("not-callable", node=node, args=node.func.as_string())
```

Bail-outs: callee not an Attribute; LHS doesn't infer to an Instance; attribute not
found on the class. Per attribute in the class-getattr result list: non-FunctionDef →
skip; not decorated with property → skip; InferenceError on infer_call_result → skip;
all returns Uninferable → skip; any return callable → skip. NOTE: no `break` after
`add_message` — multiple property defs could each emit (duplicate messages on the same
node are possible in theory).

E1102 is also influenced upstream by `safe_infer(..., compare_constructors=True)`:
two classes with same pytype (`builtins.type`) but different `__init__` signatures →
None → goes down the `_check_uninferable_call` path (no E1102, and no E112x because
`_determine_callable(None)` raises ValueError).

### 5.2 E1111 assignment-from-no-return / E1128 assignment-from-none — `visit_assign` (typecheck.py:1226-1307)

```python
@only_required_for_messages("assignment-from-no-return", "assignment-from-none",
                            "non-str-assignment-to-dunder-name")
def visit_assign(self, node: nodes.Assign) -> None:
    self._check_assignment_from_function_call(node)
    self._check_dundername_is_string(node)
```

`_check_assignment_from_function_call` (1236-1285) verbatim:

```python
def _check_assignment_from_function_call(self, node: nodes.Assign) -> None:
    if not isinstance(node.value, nodes.Call):
        return

    function_node = safe_infer(node.value.func)
    funcs = (nodes.FunctionDef, astroid.UnboundMethod, astroid.BoundMethod)
    if not isinstance(function_node, funcs):
        return

    # Unwrap to get the actual function node object
    match function_node:
        case astroid.BoundMethod(_proxied=astroid.UnboundMethod(_proxied=p)):
            function_node = p

    # Make sure that it's a valid function that we can analyze.
    # Ordered from less expensive to more expensive checks.
    if (
        not function_node.is_function
        or function_node.decorators
        or self._is_ignored_function(function_node)
    ):
        return

    # Handle builtins such as list.sort() or dict.update()
    if self._is_builtin_no_return(node):
        self.add_message("assignment-from-no-return", node=node, confidence=INFERENCE)
        return

    if not function_node.root().fully_defined():
        return

    return_nodes = list(
        function_node.nodes_of_class(nodes.Return, skip_klass=nodes.FunctionDef)
    )
    if not return_nodes:
        self.add_message("assignment-from-no-return", node=node)
    else:
        for ret_node in return_nodes:
            match ret_node.value:
                case nodes.Const(value=None) | None:
                    pass
                case _:
                    break
        else:
            self.add_message("assignment-from-none", node=node)
```

Details:

- Only `Assign` (not AnnAssign, not AugAssign, not NamedExpr); the value must be
  *directly* a Call.
- `safe_infer(node.value.func)` (plain, no flags) must be FunctionDef /
  UnboundMethod / BoundMethod. The double-proxy unwrap handles
  BoundMethod-of-UnboundMethod.
- `function_node.is_function`: True only for FunctionDef (and proxies of one) —
  Lambdas were already excluded by the isinstance gate, but a (Un)BoundMethod proxying a
  Lambda would fail this test (`Lambda` has no `is_function` attr — would raise
  AttributeError via proxy... actually `Proxy.__getattr__` → `getattr(Lambda)` →
  AttributeError propagates. In practice methods proxy FunctionDefs).
- `function_node.decorators` truthy → bail (decorators may change return).
- `_is_ignored_function` (1287-1296):

```python
return (
    isinstance(function_node, nodes.AsyncFunctionDef)
    or utils.is_error(function_node)          # body is exactly one Raise
    or function_node.is_generator()           # has yield (scoped_nodes.py:1511-1519)
    or function_node.is_abstract(pass_is_abstract=False)
)
```

- `_is_builtin_no_return` (1298-1307):

```python
match node.value:
    case nodes.Call(func=nodes.Attribute(expr=expr, attrname=attr)):
        return (
            bool(inferred := utils.safe_infer(expr))
            and isinstance(inferred, bases.Instance)
            and attr in BUILTINS_IMPLICIT_RETURN_NONE.get(inferred.pytype(), ())
        )
return False
```

with the table (typecheck.py:77-98):

```python
BUILTINS_IMPLICIT_RETURN_NONE = {
    "builtins.dict": {"clear", "update"},
    "builtins.list": {"append", "clear", "extend", "insert", "remove", "reverse", "sort"},
    "builtins.set": {"add", "clear", "difference_update", "discard",
                     "intersection_update", "remove", "symmetric_difference_update",
                     "update"},
}
```

  This path emits **E1111 with INFERENCE confidence** (e.g. `x = mylist.sort()`).
- `function_node.root().fully_defined()` — module must come from a real `.py` file
  (skips C extensions and builtins not in the table).
- Return analysis: collect all `Return` nodes inside the function but not inside nested
  FunctionDefs (`skip_klass=nodes.FunctionDef` — note nested *lambdas*' bodies are
  expressions and contain no Return statements). Zero returns → E1111 (UNDEFINED
  confidence). Otherwise: if EVERY return is bare (`ret_node.value is None`) or
  `return None` literal → E1128. Any other return value → no message (for-else with
  break).

### 5.3 E1120 / E1121 / E1123 / E1124 / E1125 / E1132 — `visit_call` complete walkthrough (typecheck.py:1454-1673)

`visit_call` has NO message gating decorator — it always runs.

#### Step 0: infer the callee, not-callable check

```python
called = safe_infer(node.func, compare_constructors=True)
self._check_not_callable(node, called)
```

#### Step 1: `_determine_callable` (typecheck.py:607-659)

```python
def _determine_callable(callable_obj):
    # Ordering is important, since BoundMethod is a subclass of UnboundMethod,
    # and Function inherits Lambda.
    parameters = 0
    if hasattr(callable_obj, "implicit_parameters"):
        parameters = callable_obj.implicit_parameters()
    match callable_obj:
        case bases.BoundMethod():
            # Bound methods have an extra implicit 'self' argument.
            return callable_obj, parameters, callable_obj.type
        case bases.UnboundMethod():
            return callable_obj, parameters, "unbound method"
        case nodes.FunctionDef():
            return callable_obj, parameters, callable_obj.type
        case nodes.Lambda():
            return callable_obj, parameters, "lambda"
        case nodes.ClassDef():
            # Class instantiation, lookup __new__ instead.
            # If we only find object.__new__, we can safely check __init__
            # instead. If __new__ belongs to builtins, then we look
            # again for __init__ in the locals, since we won't have
            # argument information for the builtin __new__ function.
            try:
                # Use the last definition of __new__.
                new = callable_obj.local_attr("__new__")[-1]
            except astroid.NotFoundError:
                new = None

            from_object = new and new.parent.scope().name == "object"
            from_builtins = new and new.root().name in sys.builtin_module_names

            if not new or from_object or from_builtins:
                try:
                    # Use the last definition of __init__.
                    callable_obj = callable_obj.local_attr("__init__")[-1]
                except astroid.NotFoundError as e:
                    raise ValueError from e
            else:
                callable_obj = new

            if not isinstance(callable_obj, nodes.FunctionDef):
                raise ValueError
            # both have an extra implicit 'cls'/'self' argument.
            return callable_obj, parameters, "constructor"

    raise ValueError
```

- `parameters` (implicit_args) is computed on the ORIGINAL object: ClassDef → 1,
  BoundMethod → 1 (0 for `__new__`), UnboundMethod → 0, FunctionDef → 1 iff
  `type in {"method","classmethod"}` (raw FunctionDef inferred for a name; usually 0),
  Lambda → 0. `None`/Uninferable/Instance → no `implicit_parameters` attr → 0, then no
  match arm → **ValueError**.
- ClassDef resolution: `local_attr("__new__")` looks ONLY at the class's own locals AND
  its ancestors? — `local_attr` is "this class or its parents' locals" (it returns the
  first scope in the MRO chain defining it; `local_attr` from astroid returns
  `self.locals[name]` or searches ancestors). Important: `[-1]` = LAST definition. If
  `__new__` resolves to `object.__new__` (its parent scope name is `"object"`) or comes
  from a builtin module (`new.root().name in sys.builtin_module_names` — `"builtins"`
  is in that tuple) → use `__init__` instead (again `local_attr`, last definition).
  Missing `__init__` → ValueError → **visit_call returns silently**
  (typecheck.py:1463-1468). Non-FunctionDef `__new__`/`__init__` (e.g.
  `__init__ = something`) → ValueError → return.
- `callable_name` (third element) is the string interpolated into all E112x messages:
  `"constructor"` for class calls; `callable_obj.type` for FunctionDef/BoundMethod
  (`"function"`, `"method"`, `"classmethod"`, `"staticmethod"`); `"unbound method"`;
  `"lambda"`.

```python
try:
    called, implicit_args, callable_name = _determine_callable(called)
except ValueError:
    # Any error occurred during determining the function type, most of
    # those errors are handled by different warnings.
    return
```

#### Step 2: builtins / duplicate params bail (typecheck.py:1470-1480)

```python
if called.args.args is None:
    if called.name == "isinstance":
        # Verify whether second argument of isinstance is a valid type
        self._check_isinstance_args(node, callable_name)
    # Built-in functions have no argument information.
    return

if len(called.argnames()) != len(set(called.argnames())):
    # Duplicate parameter name (see duplicate-argument).  We can't really
    # make sense of the function call in this case, so just return.
    return
```

C-implemented callables (raw-built) have `args.args is None` → **no argument checks at
all**, with the single special case `isinstance`. `_check_isinstance_args`
(typecheck.py:1423-1452):

```python
def _check_isinstance_args(self, node: nodes.Call, callable_name: str) -> None:
    if len(node.args) > 2:
        self.add_message("too-many-function-args", node=node,
                         args=(callable_name,), confidence=HIGH)
    elif len(node.args) < 2:
        parameters = ("'_obj'", "'__class_or_tuple'")
        for parameter in parameters[len(node.args):]:
            self.add_message("no-value-for-parameter", node=node,
                             args=(parameter, callable_name), confidence=HIGH)
        return

    second_arg = node.args[1]
    if _is_invalid_isinstance_type(second_arg):
        self.add_message("isinstance-second-argument-not-valid-type",
                         node=node, confidence=INFERENCE)
```

Notes: uses raw `node.args` length (Starred nodes count as one arg each, no unpacking);
the E1120 arg names are pre-quoted strings `'_obj'` / `'__class_or_tuple'` (so the
rendered message is `No value for argument '_obj' in function call`); W1116 is disabled
under `-E`. `isinstance` resolution: the builtin `isinstance` FunctionDef has
`type == "function"` → callable_name `"function"`. (A user-defined `isinstance` with
real args info never reaches this branch.)

#### Step 3: CallSite, E1132, invalid-args bail (typecheck.py:1482-1498)

```python
call_site = arguments.CallSite.from_call(node)

# Warn about duplicated keyword arguments, such as `f=24, **{'f': 24}`
for keyword in call_site.duplicated_keywords:
    self.add_message("repeated-keyword", node=node, args=(keyword,))

if call_site.has_invalid_arguments() or call_site.has_invalid_keywords():
    # Can't make sense of this.
    return

# Has the function signature changed in ways we cannot reliably detect?
if hasattr(called, "decorators") and decorated_with(
    called, self.linter.config.signature_mutators
):
    return
```

- **E1132 ordering hazard**: `duplicated_keywords` is a `set[str]` — when multiple
  keywords are duplicated, emission order is CPython str-hash order
  (PYTHONHASHSEED-dependent). Each duplicate emits one E1132 on the Call node with
  `args=(keyword,)` (`%r` → quoted).
- E1132 is emitted BEFORE the invalid-args bail (so `f(a=1, **{'a': 1, 1: 2})` still
  reports the duplicate even though the rest is abandoned).
- `signature_mutators` default `[]` → `decorated_with` returns False without matching
  anything (it still infers decorator nodes; with empty qnames the `any()` is False).
  A port may treat empty list as a no-op.

#### Step 4: assemble counts (typecheck.py:1500-1536)

```python
num_positional_args = len(call_site.positional_arguments)
keyword_args = list(call_site.keyword_arguments.keys())
overload_function = is_overload_stub(called)

# Determine if we don't have a context for our call and we use variadics.
node_scope = node.scope()
if isinstance(node_scope, (nodes.Lambda, nodes.FunctionDef)):
    has_no_context_positional_variadic = _no_context_variadic_positional(node, node_scope)
    has_no_context_keywords_variadic = _no_context_variadic_keywords(node, node_scope)
else:
    has_no_context_positional_variadic = has_no_context_keywords_variadic = False

# These are coming from the functools.partial implementation in astroid
already_filled_positionals = getattr(called, "filled_positionals", 0)
already_filled_keywords = getattr(called, "filled_keywords", {})

keyword_args += list(already_filled_keywords)
num_positional_args += implicit_args + already_filled_positionals

# Decrement `num_positional_args` by 1 when a function call is assigned to a class attribute
# inside the class where the function is defined.
if (
    isinstance(node.frame(), nodes.ClassDef)
    and isinstance(called, nodes.FunctionDef)
    and called in node.frame().body
    and num_positional_args > 0
    and "builtins.staticmethod" not in called.decoratornames()
):
    num_positional_args -= 1
```

The "no context variadic" workaround (typecheck.py:674-746) — gates E1120 (positional
flavor) and E1125 (keyword flavor):

```python
def _no_context_variadic_keywords(node: nodes.Call, scope: nodes.Lambda) -> bool:
    statement = node.statement()
    variadics = []
    if (
        isinstance(scope, nodes.Lambda) and not isinstance(scope, nodes.FunctionDef)
    ) or isinstance(statement, nodes.With):
        variadics = list(node.keywords or []) + node.kwargs
    elif isinstance(statement, (nodes.Return, nodes.Expr, nodes.Assign)) and isinstance(
        statement.value, nodes.Call
    ):
        call = statement.value
        variadics = list(call.keywords or []) + call.kwargs
    return _no_context_variadic(node, scope.args.kwarg, nodes.Keyword, variadics)

def _no_context_variadic_positional(node: nodes.Call, scope: nodes.Lambda) -> bool:
    variadics = node.starargs + node.kwargs
    return _no_context_variadic(node, scope.args.vararg, nodes.Starred, variadics)

def _no_context_variadic(node, variadic_name, variadic_type, variadics) -> bool:
    """Verify if the given call node has variadic nodes without context....
    Variadic arguments ... are inferred, inherently wrong, by astroid
    as a Tuple, respectively a Dict with empty elements....
    """
    scope = node.scope()
    is_in_lambda_scope = not isinstance(scope, nodes.FunctionDef) and isinstance(
        scope, nodes.Lambda
    )
    statement = node.statement()
    for name in statement.nodes_of_class(nodes.Name):
        if name.name != variadic_name:
            continue
        inferred = safe_infer(name)
        if isinstance(inferred, (nodes.List, nodes.Tuple)):
            length = len(inferred.elts)
        elif isinstance(inferred, nodes.Dict):
            length = len(inferred.items)
        else:
            continue
        if is_in_lambda_scope and isinstance(inferred.parent, nodes.Arguments):
            # The statement of the variadic will be the assignment itself,
            # so we need to go the lambda instead
            inferred_statement = inferred.parent.parent
        else:
            inferred_statement = inferred.statement()
        if not length and isinstance(
            inferred_statement, (nodes.Lambda, nodes.FunctionDef)
        ):
            is_in_starred_context = _has_parent_of_type(node, variadic_type, statement)
            used_as_starred_argument = any(
                variadic.value == name or variadic.value.parent_of(name)
                for variadic in variadics
            )
            if is_in_starred_context or used_as_starred_argument:
                return True
    return False
```

with `_has_parent_of_type` (662-671): walk `node.parent` upward while inside the
statement until a node of `variadic_type` (Keyword/Starred) is found.

Plain-language trigger: the enclosing function has `*args`/`**kwargs`; the call (or its
statement) forwards that very name in a starred/double-starred position; astroid infers
the forwarded variadic as an EMPTY Tuple/List/Dict whose defining statement is a
Lambda/FunctionDef (i.e. the parameter default representation) → set the corresponding
has_no_context_* flag → suppress E1120/E1125 respectively.

The "class attribute assignment" decrement handles:

```python
class A:
    def f(self, x): ...
    g = f(...)   # called is the raw FunctionDef (type 'method', implicit 1)
```

`called in node.frame().body` — identity membership of the FunctionDef statement in the
class body list; `decoratornames()` infers each decorator (skips when staticmethod).

#### Step 5: formal-parameter model (typecheck.py:1538-1563)

```python
# Analyze the list of formal parameters.
args = list(itertools.chain(called.args.posonlyargs or (), called.args.args))
num_mandatory_parameters = len(args) - len(called.args.defaults)
parameters: list[tuple[tuple[str | None, nodes.NodeNG | None], bool]] = []
parameter_name_to_index = {}
for i, arg in enumerate(args):
    name = arg.name
    parameter_name_to_index[name] = i
    if i >= num_mandatory_parameters:
        defval = called.args.defaults[i - num_mandatory_parameters]
    else:
        defval = None
    parameters.append(((name, defval), False))

kwparams = {}
for i, arg in enumerate(called.args.kwonlyargs):
    if isinstance(arg, nodes.Keyword):
        name = arg.arg
    else:
        assert isinstance(arg, nodes.AssignName)
        name = arg.name
    kwparams[name] = [called.args.kw_defaults[i], False]

self._check_argument_order(node, call_site, called, [p[0][0] for p in parameters])
```

- `parameters[i]` = `((name, default_or_None), assigned_bool)`. posonly params are
  folded into the same positional list (they ARE keyed in `parameter_name_to_index`).
- `kwparams[name]` = `[default_node_or_None, assigned_bool]`; `kw_defaults[i]` is None
  for kwonly params without defaults.
- `_check_argument_order` can only emit W1114 (disabled under `-E`); included for
  completeness only — it never emits E messages and mutates nothing.

#### Step 6: positional matching → E1121 (typecheck.py:1565-1580)

```python
# 1. Match the positional arguments.
for i in range(num_positional_args):
    if i < len(parameters):
        parameters[i] = (parameters[i][0], True)
    elif called.args.vararg is not None:
        # The remaining positional arguments get assigned to the *args parameter.
        break
    elif not overload_function:
        # Too many positional arguments.
        self.add_message("too-many-function-args", node=node, args=(callable_name,))
        break
```

At most ONE E1121 per call; suppressed if the callee has `*args` or is an
@overload stub. `args=(callable_name,)` — e.g. "Too many positional arguments for
constructor call".

#### Step 7: keyword matching → W1117 / E1124 / E1123 (typecheck.py:1582-1635)

```python
# 2. Match the keyword arguments.
for keyword in keyword_args:
    # Skip if `keyword` is the same name as a positional-only parameter
    # and a `**kwargs` parameter exists.
    if called.args.kwarg and keyword in [arg.name for arg in called.args.posonlyargs]:
        self.add_message("kwarg-superseded-by-positional-arg", node=node,
                         args=(keyword, f"**{called.args.kwarg}"), confidence=HIGH)
        continue
    if keyword in parameter_name_to_index:
        i = parameter_name_to_index[keyword]
        if parameters[i][1]:
            # Duplicate definition of function parameter.

            # Might be too hard-coded, but this can actually
            # happen when using str.format and `self` is passed
            # by keyword argument, as in `.format(self=self)`.
            # It's perfectly valid to so, so we're just skipping
            # it if that's the case.
            if not (keyword == "self" and called.qname() in STR_FORMAT):
                self.add_message("redundant-keyword-arg", node=node,
                                 args=(keyword, callable_name))
        else:
            parameters[i] = (parameters[i][0], True)
    elif keyword in kwparams:
        if kwparams[keyword][1]:
            # Duplicate definition of function parameter.
            self.add_message("redundant-keyword-arg", node=node,
                             args=(keyword, callable_name))
        else:
            kwparams[keyword][1] = True
    elif called.args.kwarg is not None:
        # The keyword argument gets assigned to the **kwargs parameter.
        pass
    elif isinstance(called, nodes.FunctionDef
          ) and self._keyword_argument_is_in_all_decorator_returns(called, keyword):
        pass
    elif not overload_function:
        # Unexpected keyword argument.
        self.add_message("unexpected-keyword-arg", node=node,
                         args=(keyword, callable_name))
```

- Iteration order of `keyword_args`: call-site keyword insertion order, then
  partial-filled keyword names appended.
- W1117 (`kwarg-superseded-by-positional-arg`) is disabled under `-E` but its
  `continue` still matters: a keyword that names a positional-only parameter when the
  callee has `**kwargs` is *consumed* here, so it neither marks the posonly param
  assigned nor triggers E1123/E1124 — and the posonly param may later trip E1120 if not
  positionally supplied. Replicate the control flow even though the message is
  filtered.
- E1124 (redundant-keyword-arg): keyword targets a positional param already assigned
  (positionally or by earlier keyword) — exempting `.format(self=...)` where
  `called.qname() in {"builtins.str.format"}` (STR_FORMAT, typecheck.py:69); or a
  kwonly param already assigned (only possible via repeated keyword names which Python
  parsing forbids, or partial-filled names colliding with call-site names).
- E1123 (unexpected-keyword-arg): not a known param, no `**kwargs`, not consumed by all
  decorator returns, not an overload stub.

`_keyword_argument_is_in_all_decorator_returns` (typecheck.py:1675-1713):

```python
@staticmethod
def _keyword_argument_is_in_all_decorator_returns(func, keyword) -> bool:
    if not func.decorators:
        return False
    for decorator in func.decorators.nodes:
        inferred = safe_infer(decorator)
        # If we can't infer the decorator we assume it satisfies consumes
        # the keyword, so we don't raise false positives
        if not inferred:
            return True
        # We only check arguments of function decorators
        if not isinstance(inferred, nodes.FunctionDef):
            return False
        for return_value in inferred.infer_call_result(caller=None):
            # infer_call_result() returns nodes.Const.None for None return values
            # so this also catches non-returning decorators
            if not isinstance(return_value, nodes.FunctionDef):
                return False
            # If the return value uses a kwarg the keyword will be consumed
            if return_value.args.kwarg:
                continue
            # Check if the keyword is another type of argument
            if return_value.args.is_argument(keyword):
                continue
            return False
    return True
```

Note the asymmetric conservatism: an UNinferable decorator returns True immediately
(suppresses E1123); an inferable non-FunctionDef decorator returns False (allows
E1123); for function decorators every value of `infer_call_result` must be a
FunctionDef that has `**kwargs` or the keyword as an argument.

#### Step 8: `**kwargs` at the call site (typecheck.py:1637-1646)

```python
# 3. Match the **kwargs, if any.
if node.kwargs:
    for i, [(name, _defval), _assigned] in enumerate(parameters):
        # Assume that *kwargs provides values for all remaining
        # unassigned named parameters.
        if name is not None:
            parameters[i] = (parameters[i][0], True)
        else:
            # **kwargs can't assign to tuples.
            pass
```

If the call has any `**expr` (remember: to reach here it must have inferred to a literal
Dict with str-Const keys), ALL named positional/posonly parameters are marked assigned
— but **kwonly params are NOT marked** here. However, the **-unpacked names already
flowed into `keyword_args` individually (from `_unpack_keywords`), so kwonly params
matching unpacked names were marked in step 7. The blanket marking covers names NOT
present in the literal dict too — i.e. `f(**{'a': 1})` suppresses E1120 for every
positional parameter, not just `a`.

#### Step 9: E1120 + E1125 emission (typecheck.py:1648-1673)

```python
# Check that any parameters without a default have been assigned values.
for [(name, defval), assigned] in parameters:
    if (defval is None) and not assigned:
        display_name = "<tuple>" if name is None else repr(name)
        if not has_no_context_positional_variadic and not overload_function:
            self.add_message("no-value-for-parameter", node=node,
                             args=(display_name, callable_name))

for name, val in kwparams.items():
    defval, assigned = val
    if (
        defval is None
        and not assigned
        and not has_no_context_keywords_variadic
        and not overload_function
    ):
        self.add_message("missing-kwoa", node=node,
                         args=(name, callable_name), confidence=INFERENCE)
```

- One E1120 per unassigned mandatory positional param, in parameter order;
  `display_name` is `repr(name)` → message shows quoted name (`No value for argument
  'x' in function call`). The `"<tuple>"` branch is a py2 relic (tuple params).
- One E1125 per unassigned mandatory kwonly param, in kwonly declaration order
  (`kwparams` dict preserves insertion order), INFERENCE confidence; `%r` of name.
- Note E1120 args pass `display_name` already-repr'd through a `%s` slot, while E1125
  passes the raw name through `%r` — net effect: both render quoted.

#### Worked consequences (bug-for-bug behaviors to preserve)

- `f(*unknown)`: `_unpack_args` → Uninferable → `has_invalid_arguments()` → bail; NO
  E112x.
- `f(**unknown)`: bail via `has_invalid_keywords()`.
- Calling an Instance with `__call__`: `_determine_callable` ValueError → only
  E1102-relevant checks happen; argument counts of `__call__` are never verified.
- Calling a class without analyzable `__init__`/`__new__` (e.g. builtin `dict()`):
  builtin path → `args.args is None` → return (except isinstance).
- `@overload` stubs: E1121/E1123/E1120/E1125 suppressed; E1124/E1132 NOT suppressed.
- functools.partial objects: filled positionals shift `num_positional_args`; filled
  keywords join `keyword_args` (can cause E1124-style redundancy and consume kwonly
  params).
- Implicit-first-arg handling: `implicit_args` from `_determine_callable`; bound
  methods add 1; constructors add 1 (ClassDef); `Class.method(...)` adds 0
  (UnboundMethod).

### 5.4 E1126 invalid-sequence-index / E1127 invalid-slice-index / E1144 invalid-slice-step

Entry: `visit_subscript` (typecheck.py:2147-2148) calls
`self._check_invalid_sequence_index(node)` first, for ALL Subscript nodes (the method
is gated by `only_required_for_messages("unsubscriptable-object", ...,
"invalid-sequence-index", "invalid-slice-index", "invalid-slice-step")`,
typecheck.py:2138-2146).

`_check_invalid_sequence_index` (typecheck.py:1715-1782) verbatim:

```python
def _check_invalid_sequence_index(self, subscript: nodes.Subscript) -> None:
    # Look for index operations where the parent is a sequence type.
    # If the types can be determined, only allow indices to be int,
    # slice or instances with __index__.
    parent_type = safe_infer(subscript.value)
    if not (
        isinstance(parent_type, (nodes.ClassDef, astroid.Instance))
        and has_known_bases(parent_type)
    ):
        return None

    # Determine what method on the parent this index will use
    if subscript.ctx is astroid.Context.Store:
        methodname = "__setitem__"
    elif subscript.ctx is astroid.Context.Del:
        methodname = "__delitem__"
    else:
        methodname = "__getitem__"

    # Check if this instance's __getitem__, __setitem__, or __delitem__, as
    # appropriate to the statement, is implemented in a builtin sequence
    # type. This way we catch subclasses of sequence types but skip classes
    # that override __getitem__ and which may allow non-integer indices.
    try:
        methods = astroid.interpreter.dunder_lookup.lookup(parent_type, methodname)
        if isinstance(methods, util.UninferableBase):
            return None
        itemmethod = methods[0]
    except (astroid.AttributeInferenceError, IndexError):
        return None
    if not (
        isinstance(itemmethod, nodes.FunctionDef)
        and itemmethod.root().name == "builtins"
        and itemmethod.parent
        and itemmethod.parent.frame().name in SEQUENCE_TYPES
    ):
        return None

    index_type = safe_infer(subscript.slice)
    if index_type is None or isinstance(index_type, util.UninferableBase):
        return None
    match index_type:
        case nodes.Const(value=int()):
            # Constants must be of type int
            return None
        case astroid.Instance():
            # Instance values must be int, slice, or have an __index__ method
            if index_type.pytype() in {"builtins.int", "builtins.slice"}:
                return None
            try:
                index_type.getattr("__index__")
                return None
            except astroid.NotFoundError:
                pass
        case nodes.Slice():
            # A slice can be present here after inferring the index node,
            # which could be a `slice(...)` call for instance.
            return self._check_invalid_slice_index(index_type)

    # Anything else is an error
    self.add_message("invalid-sequence-index", node=subscript)
    return None
```

with `SEQUENCE_TYPES` (typecheck.py:416-426):

```python
SEQUENCE_TYPES = {"str", "unicode", "list", "tuple", "bytearray",
                  "xrange", "range", "bytes", "memoryview"}
```

Guards/conservatism:

- container must safe_infer to ClassDef or Instance with known bases;
- the relevant dunder (chosen by `subscript.ctx`: Load→`__getitem__`,
  Store→`__setitem__`, Del→`__delitem__`) must resolve via `dunder_lookup` to a
  FunctionDef defined in module `builtins` whose enclosing frame is named one of
  SEQUENCE_TYPES. Any user override of the dunder kills the check.
- index must safe_infer; Const int (NB: `bool` matches `int()` pattern — `x[True]`
  passes); Instance of pytype builtins.int/builtins.slice; Instance with `__index__`
  via instance-getattr; an inferred `nodes.Slice` recurses into the slice-component
  check (see below) and E1126 itself is NOT emitted.
- everything else (e.g. Const str/float/None, List, Dict, ClassDef, FunctionDef...) →
  E1126 on the **Subscript node**, no args.

`_check_invalid_slice_index` (typecheck.py:1815-1879) verbatim:

```python
def _check_invalid_slice_index(self, node: nodes.Slice) -> None:
    # Check the type of each part of the slice
    invalid_slices_nodes: list[nodes.NodeNG] = []
    for index in (node.lower, node.upper, node.step):
        if index is None:
            continue

        match index_type := safe_infer(index):
            case _ if not index_type:
                continue
            case nodes.Const(value=int() | None):
                # Constants must be of type int or None
                continue
            case astroid.Instance():
                # Instance values must be of type int, None or an object
                # with __index__
                if index_type.pytype() in {"builtins.int", "builtins.NoneType"}:
                    continue

                try:
                    index_type.getattr("__index__")
                    return
                except astroid.NotFoundError:
                    pass
        invalid_slices_nodes.append(index)

    invalid_slice_step = (
        node.step and isinstance(node.step, nodes.Const) and node.step.value == 0
    )

    if not (invalid_slices_nodes or invalid_slice_step):
        return

    # Anything else is an error, unless the object that is indexed
    # is a custom object, which knows how to handle this kind of slices
    parent = node.parent
    if isinstance(parent, nodes.Subscript):
        inferred = safe_infer(parent.value)
        if inferred is None or isinstance(inferred, util.UninferableBase):
            # Don't know what this is
            return
        known_objects = (nodes.List, nodes.Dict, nodes.Tuple, objects.FrozenSet, nodes.Set)
        if not (
            isinstance(inferred, known_objects)
            or (isinstance(inferred, nodes.Const)
                and inferred.pytype() in {"builtins.str", "builtins.bytes"})
            or (isinstance(inferred, astroid.bases.Instance)
                and inferred.pytype() == "builtins.range")
        ):
            # Might be an instance that knows how to handle this slice object
            return
    for snode in invalid_slices_nodes:
        self.add_message("invalid-slice-index", node=snode)
    if invalid_slice_step:
        self.add_message("invalid-slice-step", node=node.step, confidence=HIGH)
```

Subtleties:

- The `index_type.getattr("__index__")` SUCCESS path does `return` — bailing out of the
  **entire function** (all three components plus the step check are abandoned), unlike
  the per-component `continue`s.
- `case _ if not index_type` catches both safe_infer→None and Uninferable (falsy).
- The step check `node.step.value == 0` uses `==`, so `False == 0` is True — a literal
  `x[::False]` would flag invalid-slice-step (Const bool). It also requires the step to
  be a literal Const directly in the AST (no inference).
- The "custom object" gate: only applies when `node.parent` is a Subscript (for
  inferred-slice recursion from `_check_invalid_sequence_index`, the Slice node's
  parent might not be a Subscript; e.g. for a literal `a[1:2]` the parent IS the
  subscript). If the indexed object safe_infers to anything other than literal
  List/Dict/Tuple/FrozenSet/Set, str/bytes Const, or a range instance → **no messages**.
- E1127 is emitted once per offending component, at the component node; E1144 at the
  step node with HIGH confidence.
- NOTE: `_check_invalid_slice_index` is only reachable through
  `_check_invalid_sequence_index` (which already restricted to builtin sequence types) —
  wait, no: it is reached for (a) the inferred-Slice case there, and that is the ONLY
  caller in pylint 4.0.5. A literal slice `a[1:2:0]` reaches it because
  `safe_infer(subscript.slice)` on a literal slice infers a `nodes.Slice` node →
  `case nodes.Slice()` → recursion. So E1127/E1144 inherit ALL the sequence-type guards
  of §5.4's first half.

### 5.5 E1129 not-context-manager / E1145 async-context-manager-with-regular-with — `visit_with` (typecheck.py:1881-1958)

```python
@only_required_for_messages("not-context-manager", "async-context-manager-with-regular-with")
def visit_with(self, node: nodes.With) -> None:
    for ctx_mgr, _ in node.items:
        context = astroid.context.InferenceContext()
        match inferred := safe_infer(ctx_mgr, context=context):
            case _ if not inferred:
                continue
            case bases.Generator():
                # Check if we are dealing with a function decorated
                # with contextlib.contextmanager.
                if decorated_with(inferred.parent, self.linter.config.contextmanager_decorators):
                    continue
                # Check if it's an AsyncGenerator decorated with asynccontextmanager
                if isinstance(inferred, bases.AsyncGenerator):
                    async_decorators = ["contextlib.asynccontextmanager"]
                    if decorated_with(inferred.parent, async_decorators):
                        self.add_message(
                            "async-context-manager-with-regular-with",
                            node=node, args=(inferred.parent.name,),
                            confidence=INFERENCE,
                        )
                        continue
                # ... walk all the inferred statements for the given *ctx_mgr* and
                # if you find one function scope which is decorated, consider it to
                # be the real manager and give up, otherwise emit not-context-manager.
                for inferred_path, _ in context.path:
                    if not inferred_path:
                        continue
                    if isinstance(inferred_path, nodes.Call):
                        scope = safe_infer(inferred_path.func)
                    else:
                        scope = inferred_path.scope()
                    if not isinstance(scope, nodes.FunctionDef):
                        continue
                    if decorated_with(scope, self.linter.config.contextmanager_decorators):
                        break
                else:
                    self.add_message("not-context-manager", node=node, args=(inferred.name,))
            case _:
                try:
                    inferred.getattr("__enter__")
                    inferred.getattr("__exit__")
                except astroid.NotFoundError:
                    if isinstance(inferred, astroid.Instance):
                        # If we do not know the bases of this class, just skip it.
                        if not has_known_bases(inferred):
                            continue
                        # Just ignore mixin classes.
                        if ("not-context-manager"
                                in self.linter.config.ignored_checks_for_mixins):
                            if inferred.name[-5:].lower() == "mixin":
                                continue
                    self.add_message("not-context-manager", node=node, args=(inferred.name,))
```

Per with-item (`ctx_mgr` is the context expression, before `as`):

1. `safe_infer` with a FRESH InferenceContext (kept to inspect `context.path` later).
   None/Uninferable → skip item.
2. **Generator branch** (calling a generator function in `with`): if the generator's
   defining function (`inferred.parent`) is decorated with any of
   `contextmanager-decorators` (default `["contextlib.contextmanager"]`,
   name-or-qname match) → fine. If it's an `AsyncGenerator` whose function is decorated
   `contextlib.asynccontextmanager` → **E1145** with `args=(inferred.parent.name,)`
   (the function name), INFERENCE. Otherwise scan the inference path: for each
   `(inferred_path, lookupname)` tuple in `context.path` (a **set** — iteration order
   nondeterministic but only existence matters), determine a scope (for Call nodes:
   safe_infer of the callee; else `inferred_path.scope()`); if any such scope is a
   FunctionDef decorated with a contextmanager decorator → give up (no message); else
   **E1129** with `args=(inferred.name,)` — for a Generator, `.name` proxies to the
   builtin class → the literal string `"generator"` (verified:
   `Context manager 'generator' doesn't implement __enter__ and __exit__.`); for an
   AsyncGenerator → `"async_generator"`.
3. **Everything else**: `inferred.getattr("__enter__")` AND `getattr("__exit__")` must
   both succeed (Instance getattr = instance attrs + class MRO; ClassDef.getattr = MRO
   incl. metaclass attrs; Module.getattr = module scope). On NotFoundError: if it is an
   Instance with unknown bases → skip; if `"not-context-manager"` is in
   `ignored-checks-for-mixins` (default: yes) and the **class name ends with "mixin"
   case-insensitively** (`inferred.name[-5:].lower() == "mixin"` — NOT the
   mixin-class-rgx!) → skip; else **E1129** `args=(inferred.name,)` (class name for
   instances — `.name` proxies to `_proxied.name`; function name for FunctionDef used
   directly as a context manager; module name for modules; for a Const e.g. int →
   `"int"`).

Note `__enter__` found but `__exit__` missing (or vice versa) → same NotFoundError
path. Both lookups must succeed.

### 5.6 E1130 invalid-unary-operand-type — `visit_unaryop` (typecheck.py:1960-1965)

```python
@only_required_for_messages("invalid-unary-operand-type")
def visit_unaryop(self, node: nodes.UnaryOp) -> None:
    """Detect TypeErrors for unary operands."""
    for error in node.type_errors():
        # Let the error customize its output.
        self.add_message("invalid-unary-operand-type", args=str(error), node=node)
```

All logic lives in astroid (§4.10). Summary of emission conditions:

- For each inferred value of `node.operand` (full multi-value inference, not
  safe_infer):
  - literal (Const/List/Tuple/Set/Dict): apply the real Python unary operator to the
    literal value; TypeError → one Bad message. (`not` is `operator.not_` — never a
    TypeError; `-`/`+`/`~` on str/bytes/None/list/dict/set etc. → TypeError.)
  - other node kinds: `not` → bool negation (never Bad); `+`/`-`/`~`:
    - operand not Instance/ClassDef (FunctionDef, Module, Lambda, Generator...) → Bad.
    - Instance/ClassDef: dunder_lookup `__pos__/__neg__/__invert__` (Instance: class
      MRO; ClassDef: metaclass MRO); AttributeInferenceError → Bad; found-but-
      uninferable/non-callable method → silently skipped (no Bad, no result);
      InferenceError during the call → Uninferable.
- `type_errors()` returns `[]` (→ NO messages) if **any** result is the Uninferable
  singleton — e.g. an operand union containing one uninferable branch suppresses real
  errors from the other branch.
- one message per Bad result; message text = `str(BadUnaryOperationMessage)` =
  `"bad operand type for unary {op}: {operand_type}"` with operand_type =
  `operand.name` if present (proxied class name for literals/instances: `"int"`,
  `"MyClass"`; function NAME for FunctionDef operands — note: the function's own name,
  not "function"!), else `object_type(operand).name`, else `as_string()`.
- Reported on the UnaryOp node, `args=str(error)` (single string through the `"%s"`
  template).

### 5.7 E1131 unsupported-binary-operation — `visit_binop` (typecheck.py:1967-2066)

The generic binop type-error check is **disabled** (methods renamed `_visit_binop` /
`_visit_augassign`, typecheck.py:2068-2092, with comment "This check was disabled ...
due to false positives several years ago"). The ONLY live path:

```python
@only_required_for_messages("unsupported-binary-operation")
def visit_binop(self, node: nodes.BinOp) -> None:
    if node.op == "|":
        self._detect_unsupported_alternative_union_syntax(node)
```

`_detect_unsupported_alternative_union_syntax` (1972-2012):

```python
def _detect_unsupported_alternative_union_syntax(self, node: nodes.BinOp) -> None:
    """Detect if unsupported alternative Union syntax (PEP 604) was used."""
    if self._py310_plus:  # 310+ supports the new syntax
        return
    ...
```

**With the ground-truth runtime (py-version defaults to (3,12)), `_py310_plus` is True
and this returns immediately → E1131 can never fire under the target invocation**
(unless a config file sets py-version < 3.10 — the harness uses an empty rcfile, so it
cannot). For completeness, the sub-3.10 logic:

- If `node.parent` is AnnAssign/Arguments/FunctionDef (TYPE_ANNOTATION_NODES_TYPES,
  typecheck.py:72-76) and postponed evaluation is NOT enabled →
  `_check_unsupported_alternative_union_syntax(node)`.
- If `node.parent` is Assign/Call/Keyword/Dict/Tuple/Set/List/BinOp: allowed only when
  postponed evaluation is on AND some ancestor (walk to Module) is a type-annotation
  node; else `_check_unsupported_alternative_union_syntax(node)`.

`_check_unsupported_alternative_union_syntax` (2043-2066): computes
`astroid.helpers.object_type` of both operands; `_recursive_search_for_classdef_type`
(2030-2041) checks whether the operand's *type* is a ClassDef lacking
`__or__`/`__ror__` (getattr NotFoundError → "is a type without |" → True); a found
overload that is only `builtins.type.__or__/__ror__` while `_py310_plus` is False is
treated via `VERSION_COMPATIBLE_OVERLOAD_SENTINEL` (typecheck.py:101-105, 2014-2028) →
return without message. If either side "is a type" → message
`unsupported-binary-operation` with args = the literal string
`"unsupported operand type(s) for |"`, node=BinOp, INFERENCE confidence.

### 5.8 E1133 not-an-iterable / E1134 not-a-mapping — `IterableChecker` (typecheck.py:2243-2350)

```python
def _check_iterable(self, node: nodes.NodeNG, check_async: bool = False) -> None:
    if is_inside_abstract_class(node):
        return
    inferred = safe_infer(node)
    if not inferred or is_comprehension(inferred):
        return
    if not is_iterable(inferred, check_async=check_async):
        self.add_message("not-an-iterable", args=node.as_string(), node=node)

def _check_mapping(self, node: nodes.NodeNG) -> None:
    if is_inside_abstract_class(node):
        return
    if isinstance(node, nodes.DictComp):
        return
    inferred = safe_infer(node)
    if inferred is None or isinstance(inferred, util.UninferableBase):
        return
    if not is_mapping(inferred):
        self.add_message("not-a-mapping", args=node.as_string(), node=node)
```

Visitors (each gated on its message):

```python
@only_required_for_messages("not-an-iterable")
def visit_for(self, node: nodes.For) -> None:
    self._check_iterable(node.iter)

@only_required_for_messages("not-an-iterable")
def visit_asyncfor(self, node: nodes.AsyncFor) -> None:
    self._check_iterable(node.iter, check_async=True)

@only_required_for_messages("not-an-iterable")
def visit_yieldfrom(self, node: nodes.YieldFrom) -> None:
    if self._is_asyncio_coroutine(node.value):
        return
    self._check_iterable(node.value)

@only_required_for_messages("not-an-iterable", "not-a-mapping")
def visit_call(self, node: nodes.Call) -> None:
    for stararg in node.starargs:
        self._check_iterable(stararg.value)
    for kwarg in node.kwargs:
        self._check_mapping(kwarg.value)

@only_required_for_messages("not-an-iterable")
def visit_listcomp(self, node: nodes.ListComp) -> None:
    for gen in node.generators:
        self._check_iterable(gen.iter, check_async=gen.is_async)
# visit_dictcomp / visit_setcomp / visit_generatorexp identical over node.generators
```

`_is_asyncio_coroutine` (2272-2289): node.value is a Call whose func safe_infers to a
FunctionDef having a decorator that safe_infers to a FunctionDef with qname
`"asyncio.coroutines.coroutine"` → skip yield-from check.

Guards: `is_inside_abstract_class` on the **AST node** (lexical position);
`is_comprehension(inferred)` — when inference returns a comprehension node itself
(generator expressions infer to themselves) → skip; safe_infer None/Uninferable → skip.
`is_iterable`/`is_mapping` per §3.4 (`__iter__` or `__getitem__`; async: `__aiter__`;
mapping: `__getitem__` AND `keys`). Message args: `node.as_string()` (exact source
re-rendering of the offending expression); node = that expression.

### 5.9 E1135 unsupported-membership-test — `visit_compare` (typecheck.py:2094-2114)

```python
def _check_membership_test(self, node: nodes.NodeNG) -> None:
    if is_inside_abstract_class(node):
        return
    if is_comprehension(node):
        return
    inferred = safe_infer(node)
    if inferred is None or isinstance(inferred, util.UninferableBase):
        return
    if not supports_membership_test(inferred):
        self.add_message("unsupported-membership-test",
                         args=node.as_string(), node=node)

@only_required_for_messages("unsupported-membership-test")
def visit_compare(self, node: nodes.Compare) -> None:
    if len(node.ops) != 1:
        return
    op, right = node.ops[0]
    if op in {"in", "not in"}:
        self._check_membership_test(right)
```

- Chained comparisons (`a in b in c`) are skipped entirely (`len(node.ops) != 1`).
- The `is_comprehension(node)` test here is on the **AST node itself** (a literal
  comprehension as the right operand is fine — it's iterable anyway).
- `supports_membership_test` = `__contains__` via protocol machinery OR iterable
  (`__iter__`/`__getitem__`). Reported on the right operand with its `as_string()`.

### 5.10 E1136 unsubscriptable-object / E1137 unsupported-assignment-operation / E1138 unsupported-delete-operation — `visit_subscript` (typecheck.py:2138-2201)

```python
@only_required_for_messages(
    "unsubscriptable-object", "unsupported-assignment-operation",
    "unsupported-delete-operation", "unhashable-member",
    "invalid-sequence-index", "invalid-slice-index", "invalid-slice-step",
)
def visit_subscript(self, node: nodes.Subscript) -> None:
    self._check_invalid_sequence_index(node)

    supported_protocol: Callable[[Any, Any], bool] | None = None
    match node.value:
        case nodes.ListComp() | nodes.DictComp():
            return

        case nodes.Dict():
            # Assert dict key is hashable
            if not is_hashable(node.slice):
                self.add_message(
                    "unhashable-member",
                    node=node.value,
                    args=(node.slice.as_string(), "key", "dict"),
                    confidence=INFERENCE,
                )

    match node.ctx:
        case astroid.Context.Load:
            supported_protocol = supports_getitem
            msg = "unsubscriptable-object"
        case astroid.Context.Store:
            supported_protocol = supports_setitem
            msg = "unsupported-assignment-operation"
        case astroid.Context.Del:
            supported_protocol = supports_delitem
            msg = "unsupported-delete-operation"

    if isinstance(node.value, nodes.SetComp):
        self.add_message(msg, args=node.value.as_string(), node=node.value)
        return

    if is_inside_abstract_class(node):
        return

    inferred = safe_infer(node.value)

    if inferred is None or isinstance(inferred, util.UninferableBase):
        return

    if getattr(inferred, "decorators", None):
        first_decorator = astroid.util.safe_infer(inferred.decorators.nodes[0])
        if isinstance(first_decorator, nodes.ClassDef):
            inferred = first_decorator.instantiate_class()
        else:
            return  # It would be better to handle function
            # decorators, but let's start slow.

    if (
        supported_protocol
        and not supported_protocol(inferred, node)
        and not utils.in_type_checking_block(node)
    ):
        self.add_message(msg, args=node.value.as_string(), node=node.value)
```

Ordered behavior:

1. `_check_invalid_sequence_index` runs first (E1126/E1127/E1144, §5.4) — both checks
   can fire for the same subscript.
2. `node.value` is a ListComp or DictComp literal → return (they ARE subscriptable? no
   — they aren't, but pylint skips them).
3. `node.value` is a literal Dict → E1143 if the subscript key (`node.slice`) is not
   `is_hashable` (§3.6). **Reported at `node.value` (the Dict literal!), args =
   `(node.slice.as_string(), "key", "dict")`, INFERENCE.** Then continue with protocol
   checks.
4. Protocol/message chosen by `node.ctx`: Load→supports_getitem/E1136,
   Store→supports_setitem/E1137, Del→supports_delitem/E1138.
5. SetComp literal subscripted → message unconditionally (set comprehensions are never
   subscriptable), args/node = `node.value`.
6. `is_inside_abstract_class(node)` → return (§3.5).
7. `safe_infer(node.value)` None/Uninferable → return.
8. **Decorated inferred values**: if the inferred object has truthy `.decorators`
   (FunctionDefs/ClassDefs — e.g. subscripting a decorated class or function), infer
   the FIRST decorator with **astroid's** `safe_infer`; if it is a ClassDef
   (class-as-decorator) replace `inferred` with `first_decorator.instantiate_class()`
   (an Instance of the decorator class); any other decorator (function decorators,
   uninferable) → **return, no message** (conservative).
9. Final: not supported AND not `in_type_checking_block(node)` (§3.8) → message,
   `args=node.value.as_string()`, `node=node.value`.

`supports_getitem` extras for ClassDef values (§3.4): `__class_getitem__` (covers PEP
585 builtin generics, typing generics via brains), postponed-annotations
type-annotation-context allowance, metaclass `__getitem__`.

### 5.11 E1139 invalid-metaclass — `visit_classdef` (typecheck.py:1026-1057)

```python
@only_required_for_messages("invalid-metaclass")
def visit_classdef(self, node: nodes.ClassDef) -> None:
    def _metaclass_name(metaclass) -> str | None:
        # pylint: disable=unidiomatic-typecheck
        if isinstance(metaclass, (nodes.ClassDef, nodes.FunctionDef)):
            return metaclass.name
        if type(metaclass) is bases.Instance:
            # Really do mean type, not isinstance, since subclasses of bases.Instance
            # like Const or Dict should use metaclass.as_string below.
            return str(metaclass)
        return metaclass.as_string()

    metaclass = node.declared_metaclass()
    if not metaclass:
        return

    if isinstance(metaclass, nodes.FunctionDef):
        # Try to infer the result.
        metaclass = _infer_from_metaclass_constructor(node, metaclass)
        if not metaclass:
            # Don't do anything if we cannot infer the result.
            return

    if isinstance(metaclass, nodes.ClassDef):
        if _is_invalid_metaclass(metaclass):
            self.add_message("invalid-metaclass", node=node,
                             args=(_metaclass_name(metaclass),))
    else:
        self.add_message("invalid-metaclass", node=node,
                         args=(_metaclass_name(metaclass),))
```

- `node.declared_metaclass()` (astroid): infers the `metaclass=` keyword of the class
  statement, returning the first non-Uninferable result (None if absent/uninferable).
- Metaclass given as a FunctionDef (factory): `_infer_from_metaclass_constructor`
  (typecheck.py:757-795):

```python
def _infer_from_metaclass_constructor(cls, func):
    context = astroid.context.InferenceContext()
    class_bases = nodes.List()
    class_bases.postinit(elts=cls.bases)
    attrs = nodes.Dict(lineno=0, col_offset=0, parent=None, end_lineno=0, end_col_offset=0)
    local_names = [(name, values[-1]) for name, values in cls.locals.items()]
    attrs.postinit(local_names)
    builder_args = nodes.Tuple()
    builder_args.postinit([cls.name, class_bases, attrs])
    context.callcontext = astroid.context.CallContext(builder_args)
    try:
        inferred = next(func.infer_call_result(func, context), None)
    except astroid.InferenceError:
        return None
    return inferred or None
```

  (Quirks preserved verbatim: `cls.name` — a plain `str` — is placed in the Tuple's
  elts; the CallContext receives the Tuple node itself as `args`, not a list.
  `cls.locals` iteration order = insertion order = source definition order.
  `inferred or None` maps Uninferable→None.) Failure → no message (bail).
- `_is_invalid_metaclass` (typecheck.py:749-754):

```python
def _is_invalid_metaclass(metaclass: nodes.ClassDef) -> bool:
    try:
        mro = metaclass.mro()
    except (astroid.DuplicateBasesError, astroid.InconsistentMroError):
        return True
    return not any(is_builtin_object(cls) and cls.name == "type" for cls in mro)
```

  i.e. a ClassDef metaclass is valid iff builtins `type` appears in its MRO; broken MRO
  → invalid. NOTE: other `MroError` subclasses (e.g. base not a class) are NOT caught
  and would propagate (ASTWalker catches/prints/re-raises — astroid raises
  `MroError` variants only DuplicateBases/InconsistentMro here in practice).
- Non-ClassDef inferred metaclass (Const, Instance, Lambda...) → message directly.
- args: `(_metaclass_name(metaclass),)` through `%r` — for ClassDef/FunctionDef the
  bare name (rendered `'Name'`); for a plain `bases.Instance` exactly (`type(...) is`)
  → `str(instance)` = `"Instance of module.Class"`; otherwise `as_string()` of the node
  (e.g. `"1"` for Const 1).
- Node: the ClassDef (position-aware → reported at the `class X` span).

### 5.12 E1141 dict-iter-missing-items — `TypeChecker.visit_for` (typecheck.py:2203-2225)

```python
@only_required_for_messages("dict-items-missing-iter")   # <-- typo, see §1.2
def visit_for(self, node: nodes.For) -> None:
    if not (isinstance(node.target, nodes.Tuple) and len(node.target.elts) == 2):
        # target is not a tuple of two elements
        return

    iterable = node.iter
    if not isinstance(iterable, nodes.Name):
        # it's not a bare variable
        return

    inferred = safe_infer(iterable)
    if not inferred:
        return
    if not isinstance(inferred, nodes.Dict):
        # the iterable is not a dict
        return

    if all(isinstance(i[0], nodes.Tuple) for i in inferred.items):
        # if all keys are tuples
        return

    self.add_message("dict-iter-missing-items", node=node)
```

Trigger: `for a, b in d:` where the target is a 2-element Tuple, the iterable is a bare
Name, safe_infer yields a literal `nodes.Dict`, and NOT all of its literal keys are
Tuple nodes. (Empty dict: `all(...)` over empty → True → no message.) Message has no
args; node = the For statement. Applies only to `For` (not AsyncFor — `visit_for` on
TypeChecker is not aliased; note `IterableChecker` has its own separate `visit_for`).

### 5.13 E1142 await-outside-async — `visit_await` (typecheck.py:2227-2240)

```python
@only_required_for_messages("await-outside-async")
def visit_await(self, node: nodes.Await) -> None:
    self._check_await_outside_coroutine(node)

def _check_await_outside_coroutine(self, node: nodes.Await) -> None:
    node_scope = node.scope()
    while not isinstance(node_scope, nodes.Module):
        match node_scope:
            case nodes.AsyncFunctionDef():
                return
            case nodes.FunctionDef() | nodes.Lambda():
                break
        node_scope = node_scope.parent.scope()
    self.add_message("await-outside-async", node=node)
```

Walk the scope chain from `node.scope()`: AsyncFunctionDef → OK (return);
plain FunctionDef or Lambda → STOP and emit; comprehension scopes (ComprehensionScope
matches neither case — AsyncFunctionDef is checked first since it's a FunctionDef
subclass) → climb to `parent.scope()`; reaching Module → emit. (So `await` inside a
comprehension inside an async def is fine; `await` at module level or in a sync def /
lambda → E1142 at the Await node, no args.)

### 5.14 E1143 unhashable-member — `visit_dict` / `visit_set` (typecheck.py:2116-2136) + subscript variant (§5.10 step 3)

```python
@only_required_for_messages("unhashable-member")
def visit_dict(self, node: nodes.Dict) -> None:
    for k, _ in node.items:
        if not is_hashable(k):
            self.add_message("unhashable-member", node=k,
                             args=(k.as_string(), "key", "dict"),
                             confidence=INFERENCE)

@only_required_for_messages("unhashable-member")
def visit_set(self, node: nodes.Set) -> None:
    for element in node.elts:
        if not is_hashable(element):
            self.add_message("unhashable-member", node=element,
                             args=(element.as_string(), "member", "set"),
                             confidence=INFERENCE)
```

`is_hashable` per §3.6 (the unhashable signal = `__hash__` infers to Const None on
EVERY inferred value). One message per offending key/element, at that node, args
`(as_string, "key"|"member", "dict"|"set")`. Dict-literal-subscript variant in §5.10
(node = the Dict literal). `{**x}` unpack entries appear in `node.items` with a
DictUnpack key node — `is_hashable` on it: inference of DictUnpack raises/returns
nothing standard → InferenceError → True → no message (conservative).

### 5.15 E1701 not-async-context-manager — `AsyncChecker.visit_asyncwith` (async_checker.py:56-93)

Config capture in `open()` (44-46): `self._mixin_class_rgx =
self.linter.config.mixin_class_rgx`; `self._async_generators =
["contextlib.asynccontextmanager"]`.

```python
@checker_utils.only_required_for_messages("not-async-context-manager")
def visit_asyncwith(self, node: nodes.AsyncWith) -> None:
    for ctx_mgr, _ in node.items:
        match inferred := checker_utils.safe_infer(ctx_mgr):
            case _ if not inferred:
                continue
            case nodes.AsyncFunctionDef():
                # Check if we are dealing with a function decorated
                # with contextlib.asynccontextmanager.
                if decorated_with(inferred, self._async_generators):
                    continue
            case astroid.bases.AsyncGenerator():
                if decorated_with(inferred.parent, self._async_generators):
                    continue
            case _:
                try:
                    inferred.getattr("__aenter__")
                    inferred.getattr("__aexit__")
                except astroid.exceptions.NotFoundError:
                    if isinstance(inferred, astroid.Instance):
                        # If we do not know the bases of this class, just skip it.
                        if not checker_utils.has_known_bases(inferred):
                            continue
                        # Ignore mixin classes if they match the rgx option.
                        if (
                            "not-async-context-manager"
                            in self.linter.config.ignored_checks_for_mixins
                            and self._mixin_class_rgx.match(inferred.name)
                        ):
                            continue
                else:
                    continue
        self.add_message("not-async-context-manager", node=node, args=(inferred.name,))
```

Control-flow points (replicate exactly):

- `safe_infer` with no context. Falsy → skip item.
- AsyncFunctionDef inferred (using the function object itself, NOT a call) decorated
  with asynccontextmanager → skip; otherwise **falls through the match → message**.
- AsyncGenerator instance (result of calling an async generator function): decorated →
  skip; else fall through → message (args: `inferred.name` = `"async_generator"`).
- default case: both `__aenter__` AND `__aexit__` must getattr-resolve; on success →
  `continue` (else-clause of try). On NotFoundError: Instance with unknown bases →
  skip; Instance whose class name matches **mixin-class-rgx** (default `.*[Mm]ixin`,
  `re.match` = anchored at start) while `"not-async-context-manager"` is in
  `ignored-checks-for-mixins` (default: it is) → skip; otherwise fall through →
  message.
- Message: `"Async context manager '%s' doesn't implement __aenter__ and __aexit__."`
  with `args=(inferred.name,)`, node = the AsyncWith statement node, UNDEFINED
  confidence.

E1700 (sibling, for completeness — async_checker.py:48-54): for each `Yield` node in an
AsyncFunctionDef whose `scope() is node`, emit only when
`sys.version_info[:2] == (3,5)` or the child is a `YieldFrom` — on the ground-truth
3.12 runtime this means: only `yield from` inside `async def` (which is a SyntaxError
anyway, so it would have died at E0001) — effectively never fires.

---

## 6. Iteration-order / determinism notes

1. **E1132 emission order**: `call_site.duplicated_keywords` is a `set[str]`; multiple
   duplicates on one call emit in str-hash order (PYTHONHASHSEED-dependent across
   runs). Same line/col, differing args order.
2. `visit_with`'s `context.path` is a `set[tuple[NodeNG, str|None]]`
   (context.py:59) — iterated to *search* for a decorated scope; order-independent
   outcome (existence), but inference side effects (safe_infer of Call funcs) occur in
   nondeterministic order (harmless: safe_infer is pure + cached).
3. `keyword_args` order: `call_site.keyword_arguments` dict insertion order (source
   order with ** expansion in dict-literal order) + partial `filled_keywords` order —
   deterministic.
4. `kwparams` (E1125 emission order): kwonly declaration order — deterministic.
5. `_infer_from_metaclass_constructor`: `cls.locals.items()` insertion order —
   deterministic per source.
6. pylint `safe_infer` lru_cache (1024) and `is_overload_stub`/`class_is_abstract`
   caches (1024) persist across modules in a run; eviction can in principle alter
   nothing observable (pure functions), but `has_known_bases` memoizes on the node
   (`_all_bases_known`) permanently.

## 7. Minimal astroid inference surface needed (per check)

- **E1102**: `node.func.infer()` (multi-value via pylint safe_infer +
  compare_constructors → ClassDef `local_attr("__init__")` comparisons),
  `.callable()` (Instance: class getattr `__call__` class_context=False),
  `has_known_bases` (infer class bases), Instance `.parent/.scope()/.locals/.qname()`
  via proxy; secondary path: safe_infer of attribute LHS, class `getattr(attrname)`,
  `decorated_with_property` (decorator inference incl. `objects.Property`),
  `attr.infer_call_result(node)`.
- **E1111/E1128**: safe_infer of callee; method proxy unwrap; `.decorators`,
  `is_generator()` (yield scan), `is_abstract()` (decorator inference),
  `root().fully_defined()`; `nodes_of_class(Return, skip_klass=FunctionDef)`;
  builtin table needs `safe_infer(expr).pytype()` for the receiver.
- **E1120/21/23/24/25/32**: safe_infer(func, compare_constructors); class
  `local_attr("__new__"/"__init__")`; `argnames()`; CallSite construction = astroid
  safe_infer of every `*`/`**` argument + dict-literal key inference;
  `is_overload_stub`/`decorated_with` (decorator inference);
  `_no_context_variadic`: safe_infer of forwarded variadic Names; partial brain
  (CallSite + FunctionDef inference at `functools.partial(...)` call sites);
  `decoratornames()` for the class-attribute decrement;
  `_keyword_argument_is_in_all_decorator_returns`: decorator safe_infer +
  `infer_call_result(caller=None)`.
- **E1126/27/44**: safe_infer of container + index; `has_known_bases`;
  `dunder_lookup.lookup` (instance MRO locals / metaclass MRO); `pytype()`;
  instance `getattr("__index__")`.
- **E1129/45**: safe_infer of context expr (with live InferenceContext.path);
  Generator/AsyncGenerator detection; `decorated_with`; `getattr("__enter__"/"__exit__")`
  on Instance/ClassDef/Module; `has_known_bases`.
- **E1130**: full multi-value `operand.infer()`; literal `infer_unary_op` (real Python
  op on the literal); dunder_lookup of `__pos__/__neg__/__invert__`; method
  `infer()` + `infer_call_result`; `helpers.object_type` for message text.
- **E1133/34/35**: safe_infer of the expression; protocol getattr lookups
  (`__iter__`, `__aiter__`, `__getitem__`, `__contains__`, `keys`) on
  Instance/ClassDef-metaclass; `has_dynamic_getattr`; `has_known_bases`;
  `metaclass()` resolution; for-yieldfrom: decorator qname inference.
- **E1136/37/38**: as above plus `__class_getitem__`/`__setitem__`/`__delitem__`,
  `instantiate_class()` on decorator ClassDef, `in_type_checking_block` (lookup +
  safe_infer of TYPE_CHECKING), future_imports, annotation-context walk.
- **E1139**: `declared_metaclass()`; `metaclass.mro()`; `infer_call_result` of factory
  functions; `as_string()`.
- **E1141**: safe_infer of a Name to a literal Dict.
- **E1142**: scope chain only — NO inference.
- **E1143**: full `node.infer()` of keys/elements; `igetattr("__hash__")` on inferred
  instances.
- **E1701**: safe_infer; `decorated_with`; `getattr("__aenter__"/"__aexit__")`;
  `has_known_bases`; mixin-class-rgx match.
