# 08 — All remaining in-scope checkers (exact spec, pylint 4.0.5 / astroid 4.0.4)

Scope: every E/F-category message still in scope for `pylint . -E` excluding E0110, E0401,
E0611, E1101. One section per checker. All paths are relative to
`reference/pylint/pylint/` unless prefixed with `astroid/` (then relative to
`reference/astroid/astroid/`).

Conventions used in this doc:
- "report at NODE" means `add_message(..., node=NODE)`. Per
  `lint/pylinter.py:1212-1230`, when a `node` is passed and no explicit
  `line`/`col_offset`: if `node.position` is set (astroid sets `position` for
  FunctionDef/AsyncFunctionDef/ClassDef to the span of the `def`/`class` keyword + name),
  the message uses `node.position.{lineno,col_offset,end_lineno,end_col_offset}`;
  otherwise `node.fromlineno`, `node.col_offset`, `node.end_lineno`, `node.end_col_offset`.
  Explicitly passed `line`/`col_offset`/end values override (note: the check is `if not line`,
  so a passed value of `0` is treated as missing).
- Confidence default is `UNDEFINED` unless stated (`pylinter.py:1306-1307`).
- "safe_infer(x)" = `checkers/utils.py:1348-1410`, see §0.1.

---------------------------------------------------------------------------------------

# 0. Shared helper functions (checkers/utils.py)

## 0.1 safe_infer — utils.py:1348-1410
Returns the single inferred value of a node, or None on failure/ambiguity. This is THE
conservatism primitive: nearly every checker bails out when it returns None.

```
def safe_infer(node, context=None, *, compare_constants=False, compare_constructors=False):
    inferred_types = set()
    try:
        infer_gen = node.infer(context); value = next(infer_gen)
    except InferenceError: return None
    if not isinstance(value, UninferableBase):
        inferred_types.add(_get_python_type_of_node(value))   # value.pytype() or None
    try:
        for inferred in infer_gen:
            if _get_python_type_of_node(inferred) not in inferred_types: return None
            if compare_constants and both Const and values differ: return None
            if both FunctionDef and function_arguments_are_ambiguous(...): return None
            if compare_constructors and both ClassDef and constructors ambiguous: return None
    except InferenceError: return None
    except StopIteration: return value
    return value if len(inferred_types) <= 1 else None
```
NOTE: it CAN return `Uninferable` (UninferableBase instance) — when the *first* result is
Uninferable and remaining results all have `pytype() is None` in `inferred_types`
(Uninferable contributes nothing to `inferred_types` since the first value is skipped only
in the add). Callers therefore often check `isinstance(x, util.UninferableBase)` separately.
It is `@lru_cache(maxsize=1024)` (utils.py:1347).

## 0.2 infer_all — utils.py:1413-1422
`list(node.infer(context))`, returning `[]` on `InferenceError`. `@lru_cache(maxsize=512)`.

## 0.3 has_known_bases — utils.py:1466-1484
True iff every base of a class infers (via safe_infer) to a ClassDef that is not the class
itself, recursively. Caches result on the node as `_all_bases_known`.

## 0.4 inherit_from_std_ex — utils.py:766-775
```
ancestors = node.ancestors() if hasattr(node, "ancestors") else []
return any(anc.name in {"Exception","BaseException"}
           and anc.root().name == EXCEPTIONS_MODULE   # "builtins"
           for anc in chain([node], ancestors))
```
i.e. the node itself or any ancestor is named Exception/BaseException defined in builtins.

## 0.5 is_none — utils.py:1487-1491
True for `None` (python None), `Const(value=None)`, or `Name(value="None")` — NB the last
arm is a match against a `value` attribute on Name, which Name doesn't have, so in
practice it matches None-literal Consts and python None only.

## 0.6 is_overload_stub — utils.py:1666-1674
`bool(node.decorators and decorated_with(node, ["typing.overload", "overload"]))`.
`@lru_cache(maxsize=1024)`.

## 0.7 decorated_with — utils.py:870-891
For each decorator node (unwrap `Call` to its `.func`), `decorator_node.infer()`; True if
any inferred ClassDef/FunctionDef has `.name in qnames or .qname() in qnames`.
`InferenceError` per-decorator → continue. (So plain-name match works without imports.)

## 0.8 is_registered_in_singledispatch_function — utils.py:1515-1546
True if `node` has a decorator of shape `@f.register` / `@f.register(...)` where
`next(f.infer())` is a FunctionDef decorated with `functools.singledispatch` or
`singledispatch.singledispatch` (via decorated_with). InferenceError → continue.

## 0.9 find_inferred_fn_from_register — utils.py:1549-1565
Same decorator-shape extraction; returns the FunctionDef inferred from `func.expr` via
safe_infer, else None.

## 0.10 is_registered_in_singledispatchmethod_function — utils.py:1568-1581
For each decorator, `find_inferred_fn_from_register(decorator)`; if found, return
`decorated_with(func_def, ("functools.singledispatchmethod","singledispatch.singledispatchmethod"))`.
NOTE: returns on the FIRST decorator that yields a func_def (no continue).

## 0.11 get_argument_from_call — utils.py:717-744
Returns `call_node.args[position]` if in range, else searches `call_node.keywords` for
`arg.arg == keyword`; raises `NoSuchArgumentError` if neither found, `ValueError` if both
position and keyword are None.

## 0.12 node_frame_class — utils.py:677-699
Climb `node.frame()` then `klass.parent.frame()` until a ClassDef is reached; None if a
parentless frame is hit first. Returns the class wrapping a method node.

## 0.13 unimplemented_abstract_methods — utils.py:945-994 (`@lru_cache(maxsize=1024)` :944)
Walk `reversed(node.mro())` (ResolveError → return {}); for every local value `obj` of each
ancestor: if AssignName, safe_infer it (None → delete from visited, continue; non-FunctionDef
→ delete from visited). If a FunctionDef: `abstract = is_abstract_cb(inferred)`; abstract →
`visited[obj.name] = inferred`; not abstract and name in visited → delete. Default
`is_abstract_cb` = decorated_with(qnames=ABC_METHODS) where ABC_METHODS =
{"abc.abstractproperty","abc.abstractmethod","abc.abstractclassmethod","abc.abstractstaticmethod"}.

## 0.14 find_except_wrapper_node_in_scope — utils.py:1011-1025
Walk `node.node_ancestors()`: a `LocalsDictNodeNG` (function/class/module scope boundary)
→ None; an `ExceptHandler` → return it.

## 0.15 is_subclass_of — utils.py:1647-1663
Both args must be ClassDef. For each `child.ancestors()`, `astroid.helpers.is_subtype(ancestor, parent)`
(qname-based type identity); `_NonDeducibleTypeHierarchy` → continue. True on first match.

## 0.16 are_exclusive — astroid/nodes/node_classes.py:116-190
True iff two statements are on mutually exclusive control-flow branches: index stmt1's
ancestor chain; climb stmt2's ancestors until a common ancestor; if the common ancestor is
an `If` (and exceptions arg is None): exclusive iff the two children are in different
attributes and neither is the `test`. If a `Try`: exclusive for body-vs-handler (when the
handler catches the given exceptions), else-vs-handler combinations.

## 0.17 SPECIAL_METHODS_PARAMS / PYMETHODS — utils.py:78-193
`_SPECIAL_METHODS_PARAMS` maps expected-arg-count → tuple of dunder names:
- `None` (variadic, never checked): `__new__ __init__ __call__ __init_subclass__`
- `0`: `__del__ __repr__ __str__ __bytes__ __hash__ __bool__ __dir__ __len__ __length_hint__
  __iter__ __reversed__ __neg__ __pos__ __abs__ __invert__ __complex__ __int__ __float__
  __index__ __trunc__ __floor__ __ceil__ __enter__ __aenter__ __getnewargs_ex__
  __getnewargs__ __getstate__ __reduce__ __copy__ __unicode__ __nonzero__ __await__
  __aiter__ __anext__ __fspath__ __subclasses__`
- `1`: `__format__ __lt__ __le__ __eq__ __ne__ __gt__ __ge__ __getattr__ __getattribute__
  __delattr__ __delete__ __instancecheck__ __subclasscheck__ __getitem__ __missing__
  __delitem__ __contains__ __add__ __sub__ __mul__ __truediv__ __floordiv__ __rfloordiv__
  __mod__ __divmod__ __lshift__ __rshift__ __and__ __xor__ __or__ __radd__ __rsub__ __rmul__
  __rtruediv__ __rmod__ __rdivmod__ __rpow__ __rlshift__ __rrshift__ __rand__ __rxor__
  __ror__ __iadd__ __isub__ __imul__ __itruediv__ __ifloordiv__ __imod__ __ilshift__
  __irshift__ __iand__ __ixor__ __ior__ __ipow__ __setstate__ __reduce_ex__ __deepcopy__
  __cmp__ __matmul__ __rmatmul__ __imatmul__ __div__`
- `2`: `__setattr__ __get__ __set__ __setitem__ __set_name__`
- `3`: `__exit__ __aexit__`
- `(0, 1)`: `__round__`
- `(1, 2)`: `__pow__`
`SPECIAL_METHODS_PARAMS` inverts to name→count; `PYMETHODS = set(SPECIAL_METHODS_PARAMS)`.

## 0.18 is_function_body_ellipsis — utils.py:1925-1930
True iff body is exactly `[Expr(value=Const(value=Ellipsis))]`.

## 0.19 is_class_attr — utils.py:2257-2262
`klass.getattr(name)` succeeds (NotFoundError → False).

## 0.20 Format-string parsers (used by strings.py and logging.py)

### parse_format_string — utils.py:518-591  (old-style `%` formatting)
Returns `(keys: set[str], num_args: int, key_types: dict[str,str], pos_types: list[str])`.
Raises `IncompleteFormatString` (utils.py:504) or `UnsupportedFormatCharacter(index)`
(utils.py:508-515; `.index` is the index of the offending char in the format string).

Exact algorithm (mirror this precisely):
```
i = 0
while i < len(s):
    char = s[i]
    if char == "%":
        i, char = next_char(i)            # next_char: i+=1; if i == len(s): raise IncompleteFormatString
        key = None
        if char == "(":                   # mapping key, with nested-paren balancing
            depth = 1
            i, char = next_char(i); key_start = i
            while depth != 0:
                if char == "(": depth += 1
                elif char == ")": depth -= 1
                i, char = next_char(i)
            key = s[key_start : i-1]
        while char in "#0- +": i, char = next_char(i)            # conversion flags
        if char == "*": num_args += 1; i, char = next_char(i)    # min field width "*"
        else:
            while char in string.digits: i, char = next_char(i)
        if char == ".":                                          # precision
            i, char = next_char(i)
            if char == "*": num_args += 1; i, char = next_char(i)
            else:
                while char in string.digits: i, char = next_char(i)
        if char in "hlL": i, char = next_char(i)                 # length modifier
        flags = "diouxXeEfFgGcrs%a"                              # legal conversion types
        if char not in flags: raise UnsupportedFormatCharacter(i)
        if key:  keys.add(key); key_types[key] = char
        elif char != "%": num_args += 1; pos_types.append(char)
    i += 1
```
Subtleties: `%(name)*d` adds to BOTH keys and num_args (mixed → E1302 path);
`%%` consumes no argument; the key may contain parens (`%(a(b))s` key = "a(b)").

### collect_string_fields — utils.py:603-634  (PEP 3101)
Wraps `string.Formatter().parse(format_string)`; yields each field `name`
(None for literal-only chunks is skipped via `if all(item is None for item in result[1:])`),
then recursively yields fields of the nested `format_spec`. On `ValueError`:
if message starts with `"cannot switch from manual"` → yield `""` then `"1"`
(forces format-combined-specification); else raise `IncompleteFormatString(format_string)`.
NOTE on CPython 3.12: `Formatter.parse` itself raises this ValueError lazily during
iteration when `{}` and `{0}` are mixed, so the special branch IS taken.

### split_format_field_names — utils.py:594-600
`_string.formatter_field_name_split(format_string)`; ValueError → IncompleteFormatString.
Returns `(keyname, iterator of (is_attribute: bool, specifier))` — e.g. `"0.a[1]"` →
`(0, [(True,'a'), (False,1)])`.

### parse_format_method_string — utils.py:637-663
```
keyword_arguments = []; implicit_pos_args_cnt = 0; explicit_pos_args = set()
for name in collect_string_fields(format_string):
    if name and str(name).isdigit(): explicit_pos_args.add(str(name))
    elif name:
        keyname, fielditerator = split_format_field_names(name)
        if isinstance(keyname, numbers.Number): explicit_pos_args.add(str(keyname))
        keyword_arguments.append((keyname, list(fielditerator)))   # ValueError → IncompleteFormatString
    else: implicit_pos_args_cnt += 1
return keyword_arguments, implicit_pos_args_cnt, len(explicit_pos_args)
```
So `"{0.attr}"` produces keyword_arguments entry with keyname=0 (an int) AND counts in
explicit_pos_args; `"{[0]}"` produces keyname="" with an index specifier; `"{}"` counts in
implicit_pos_args_cnt.

---------------------------------------------------------------------------------------

# 1. checkers/base/basic_error_checker.py — BasicErrorChecker

Checker name "basic" (inherits _BasicChecker, basic_checker.py:26-32). Message defs at
basic_error_checker.py:164-277.

Module constants:
- `ABC_METACLASSES = {"_py_abc.ABCMeta", "abc.ABCMeta"}` :20 (E0110, out of scope)
- `REDEFINABLE_METHODS = frozenset(("__module__",))` :22
- `FORWARD_REF_QNAME = {"typing.ForwardRef", "annotationlib.ForwardRef"}` :23

## E0100 init-is-generator — "__init__ method is a generator" (no args)
visit_functiondef / visit_asyncfunctiondef (:333-367, alias :367).
```
if node.is_method() and node.name == "__init__":          # :346
    if node.is_generator():                                # :347 (astroid: any Yield in body, not in nested frame; YieldFrom counts)
        add_message("init-is-generator", node=node)        # :348  → reported at FunctionDef (uses node.position)
```
`is_method()` (astroid) = parent chain puts the function directly in a ClassDef (via
`node.type` computation honoring staticmethod/classmethod decorators; for E0100 only the
"is in a class" aspect matters since name is `__init__`).

## E0101 return-in-init — "Explicit return in __init__" (no args)
Same visit, else-branch of E0100 (:349-353):
```
returns = node.nodes_of_class(nodes.Return, skip_klass=(FunctionDef, ClassDef))  # :343-345
else:                                # not a generator
    values = [r.value for r in returns]
    if any(v for v in values if not utils.is_none(v)):     # :352 — any Return with non-None value
        add_message("return-in-init", node=node)           # :353 at FunctionDef
```
NOTE skip_klass excludes returns in nested defs. `return` bare and `return None` are fine.

## E0102 function-redefined — "%s already defined line %s"
args = `(redeftype, defined_self.fromlineno)` where redeftype ∈ {"class","method","function"}.
Emitted from:
- visit_classdef (:279-281): `self._check_redefinition("class", node)`
- visit_functiondef (:336-340): only when `not redefined_by_decorator(node) and not
  utils.is_registered_in_singledispatch_function(node)`; redeftype =
  `"method" if node.is_method() else "function"`.

`redefined_by_decorator` (:79-95): True iff any decorator is an `Attribute` whose
`expr` has attribute `name` equal to the function's own name (e.g. `@x.setter` on def x).

`_check_redefinition(redeftype, node)` (:579-647) — quoted verbatim because every branch
matters:
```python
parent_frame = node.parent.frame()
# Ignore function stubs created for type information
redefinitions = [
    i for i in parent_frame.locals[node.name]
    if not (isinstance(i.parent, nodes.AnnAssign) and i.parent.simple)
]
defined_self = next(
    (local for local in redefinitions if not utils.is_overload_stub(local)),
    node,
)
if defined_self is not node and not astroid.are_exclusive(node, defined_self):
    if (isinstance(parent_frame, nodes.ClassDef)
            and node.name in REDEFINABLE_METHODS):
        return
    if _is_singledispatchmethod_registration(node):
        return
    if utils.is_overload_stub(node):
        return
    if isinstance(node.parent, nodes.If):
        match node.parent.test:
            case nodes.UnaryOp(op="not", operand=nodes.Name(name=name)) if name == node.name:
                return                       # "if not <func>:" guard
            case nodes.Compare(left=nodes.Name(name=name),
                               ops=[["is", nodes.Const(value=None)]]) if name == node.name:
                return                       # "if <func> is None:" guard
    try:
        redefinition_index = redefinitions.index(node)
    except ValueError:
        pass
    else:
        for redefinition in redefinitions[:redefinition_index]:
            inferred = utils.safe_infer(redefinition)
            if (inferred and isinstance(inferred, astroid.Instance)
                    and inferred.qname() in FORWARD_REF_QNAME):
                return                       # earlier binding was a typing.ForwardRef
    self.add_message("function-redefined", node=node,
                     args=(redeftype, defined_self.fromlineno))
```
Reported at the redefining ClassDef/FunctionDef (node.position). Note `defined_self` is the
FIRST binding in `parent_frame.locals[name]` that is not an overload stub (AnnAssign-simple
stubs filtered out); if ALL are overload stubs, `defined_self` defaults to `node` itself →
no message. `are_exclusive` (§0.16) exempts if/else branches.
`_is_singledispatchmethod_registration` (:138-160): any decorator of shape
`@f.register`/`@f.register(...)` where safe_infer(f) is a (Async)FunctionDef having a
decorator whose safe_infer has qname `functools.singledispatchmethod` (:115-135).

## E0103 not-in-loop — "%r not properly in loop", args = node_name ("break"/"continue")
visit_continue (:436-438) → `_check_in_loop(node, "continue")`;
visit_break (:440-442) → `_check_in_loop(node, "break")`.
`_check_in_loop` (:553-577):
```
for parent in node.node_ancestors():
    if isinstance(parent, (For, While)):
        if node not in parent.orelse: return          # properly in a loop → no message
    if isinstance(parent, (ClassDef, FunctionDef)): break   # scope boundary → fall through to message
    if isinstance(parent, Try) and node in parent.finalbody and isinstance(node, Continue):
        add_message("continue-in-finally", node=node)        # W0136 (out of scope)
    if isinstance(parent, Try) and node in parent.finalbody and isinstance(node, Break):
        add_message("break-in-finally", node=node)           # W0137 (out of scope)
add_message("not-in-loop", node=node, args=node_name)        # :577, at Break/Continue node
```
Note `node in parent.orelse` only matches if the break/continue is a DIRECT child of the
loop's else-suite; nested statements inside the orelse are not detected here (the walk
continues; node was reassigned? No — `node` stays the original; so `break` nested deeper
inside `for...else:` (e.g. inside an `if` in the orelse) passes the `node not in
parent.orelse` test and returns: no message. Only a direct `break` in orelse keeps walking).
Both W0136/W0137 are emitted during the walk even when a loop is eventually found.

## E0104 return-outside-function — "Return outside function" (no args)
visit_return (:423-426):
```
if not isinstance(node.frame(), nodes.FunctionDef):
    add_message("return-outside-function", node=node)   # at the Return statement
```
(AsyncFunctionDef subclasses FunctionDef in astroid → OK in async defs.)

## E0105 yield-outside-function — "Yield outside function" (no args)
visit_yield (:428-430) and visit_yieldfrom (:432-434) → `_check_yield_outside_func` (:537-539):
```
if not isinstance(node.frame(), (nodes.FunctionDef, nodes.Lambda)):
    add_message("yield-outside-function", node=node)    # at the Yield/YieldFrom expr node
```

## E0106 return-arg-in-generator — has `{"maxversion": (3, 3)}` (:202) → NEVER registered
on Python 3.12. Skip entirely.

## E0107 nonexistent-operator — "Use of the non-existent %s operator", args = node.op*2
visit_unaryop (:452-461):
```
if (node.op in "+-"
        and isinstance(node.operand, nodes.UnaryOp)
        and node.operand.op == node.op
        and node.col_offset + 1 == node.operand.col_offset):   # adjacency: "++x" not "+ +x" / "+(+x)"
    add_message("nonexistent-operator", node=node, args=node.op * 2)   # at outer UnaryOp
```
NOTE `node.op in "+-"` is a substring test on the string "+-" (ops "+" and "-" only;
also matches "+-"? no — op is always a single char for UnaryOp ("+","-","not","~")).

## E0108 duplicate-argument-name — "Duplicate argument name %r in function definition"
visit_functiondef (:354-365):
```
arg_clusters = {}
for arg in node.args.arguments:        # astroid Arguments.arguments = posonlyargs + args + vararg? NO:
    # .arguments property = posonlyargs + args + [vararg-as-AssignName if present? no] ...
    if arg.name in arg_clusters:
        add_message("duplicate-argument-name", node=arg, args=(arg.name,), confidence=HIGH)
    else: arg_clusters[arg.name] = arg
```
Reported at the duplicate AssignName arg node (2nd and later occurrences), confidence HIGH.
astroid `Arguments.arguments` = `posonlyargs + args + vararg_node + kwonlyargs + kwarg_node`
(vararg/kwarg included as AssignName nodes when present) — duplicates across all of these
count. (Python itself raises SyntaxError for duplicates within a def, so in practice this
fires only on AST built from sources Python accepts? No — `def f(a, *, a)` is also a
SyntaxError; this check matters for code parsed without compile-time validation.)

## E0112 too-many-star-expressions — "More than one starred expression in assignment"
visit_assign (:296-304):
```
match node.targets[0]:
    case nodes.Starred(): add_message("invalid-star-assignment-target", node=node)   # E0113
    case nodes.Tuple():
        if self._too_many_starred_for_tuple(target): add_message("too-many-star-expressions", node=node)
```
`_too_many_starred_for_tuple` (:283-291): iterate `assign_tuple.itered()`; on the FIRST
nested Tuple element, recurse and RETURN its result (ignoring any remaining elements —
bug-for-bug: `*a, (x, y), *b = ...` → counts only inside `(x, y)` → no message);
count Starred elements; return count > 1. Reported at the Assign node, no args.
Only `targets[0]` is examined (chained assignment `a = *b, *c = d` checks only `a`).

## E0113 invalid-star-assignment-target — "Starred assignment target must be in a list or tuple"
See above (:298-300): `*a = b` (Assign whose first target is a Starred). At Assign, no args.

## E0114 star-needs-assignment-target — "Can use starred expression only in assignment target"
visit_starred (:306-322):
```
if isinstance(node.parent, nodes.Call): return                 # f(*args)
if isinstance(node.parent, (List, Tuple, Set, Dict)): return   # PEP 448 literal unpacking
stmt = node.statement()
if not isinstance(stmt, nodes.Assign): return                  # any other context: bail
if stmt.value is node or stmt.value.parent_of(node):           # Starred on the RHS
    add_message("star-needs-assignment-target", node=node)     # at the Starred node
```
(Python usually SyntaxErrors these; reachable for e.g. `a = *b` — actually that is a
SyntaxError too; reachable in `for` targets? statement() not Assign → bail. Practical
trigger: `a = (*b)`? SyntaxError. This rarely fires on valid 3.12 source.)

## E0115 nonlocal-and-global — "Name %r is nonlocal and global"
visit_functiondef → `_check_nonlocal_and_global` (:395-421):
```
nonlocals = set of all names in Nonlocal nodes n with n.scope() is node
if not nonlocals: return
global_vars = same for Global nodes
for name in nonlocals & global_vars:
    add_message("nonlocal-and-global", args=(name,), node=node)   # at the FunctionDef
```
(Python 3.12 compiles `nonlocal x; global x` to SyntaxError — fires only via AST-level
analysis of both statements in one function scope, which IS a SyntaxError... pylint still
implements it because the astroid parse may succeed where compile fails? astroid uses the
compiler's parser, so this triggers when the two statements name the same var in the same
function — that's the SyntaxError "name 'x' is nonlocal and global". With ast.parse this
does NOT raise (it's a symtable error), so astroid does parse it. ✔ reachable.)

## E0117 nonlocal-without-binding — "nonlocal name %s found without binding"
visit_nonlocal (:485-488): for each name in node.names → `_check_nonlocal_without_binding`
(:463-483):
```
current_scope = node.scope()
while current_scope.parent is not None:
    if not isinstance(current_scope, (ClassDef, FunctionDef)):
        add_message("nonlocal-without-binding", args=(name,), node=node); return
        # ^ e.g. scope is a comprehension/lambda? (Lambda can't contain nonlocal) — generator exp scope
    if current_scope is node.scope() or name not in current_scope.locals:
        current_scope = current_scope.parent.scope(); continue
    return                                      # found a binding in an enclosing function/class
if not isinstance(current_scope, nodes.FunctionDef):     # reached module (or top-level non-func)
    add_message("nonlocal-without-binding", args=(name,), node=node, confidence=HIGH)
```
Reported at the Nonlocal statement. args is the name (note format `%s` not `%r`).
Subtlety: the scope containing the `nonlocal` itself is skipped (a local binding in the
same scope doesn't count); ClassDef scopes in between are traversed and their locals DO
satisfy the search (bug-for-bug: pylint accepts a class-level binding even though CPython
would not). First message (mid-walk) has default UNDEFINED confidence, final has HIGH.

## E0118 used-prior-global-declaration — "Name %r is used prior to global declaration"
visit_functiondef → `_check_name_used_prior_global` (:369-393):
```
scope_globals = {name: global_stmt for each Global child of node (any depth)
                 for name in child.names if child.scope() is node}
if not scope_globals: return
for name_node in node.nodes_of_class(nodes.Name):
    if name_node.scope() is not node: continue
    g = scope_globals.get(name_node.name)
    if g and g.fromlineno and g.fromlineno > name_node.fromlineno:
        add_message("used-prior-global-declaration", node=name_node, args=(name,))
```
Reported at the offending Name node (load context only — nodes.Name, not AssignName).
NOTE dict comprehension keeps the LAST Global statement for a name if several.
Message def has `{"minversion": (3, 6)}` (:261) — always active on 3.12.

## E0119 misplaced-format-function — see §2 (lives in basic_checker.py).

---------------------------------------------------------------------------------------

# 2. checkers/base/basic_checker.py — BasicChecker (E0111, E0119 only)

Constants (basic_checker.py:35-37):
```
REVERSED_PROTOCOL_METHOD = "__reversed__"
SEQUENCE_PROTOCOL_METHODS = ("__getitem__", "__len__")
REVERSED_METHODS = (SEQUENCE_PROTOCOL_METHODS, (REVERSED_PROTOCOL_METHOD,))
```
`open()` (:276-281) sets `self._py38_plus = config.py_version >= (3, 8)` — True by default
(py-version defaults to the running interpreter version).

## visit_call dispatch (:689-712)
```
@only_required_for_messages("eval-used","exec-used","bad-reversed-sequence",
                            "misplaced-format-function","unreachable")
def visit_call(node):
    if utils.is_terminating_func(node): self._check_unreachable(node, confidence=INFERENCE)
    self._check_misplaced_format_function(node)                     # E0119, every Call
    if isinstance(node.func, nodes.Name):
        name = node.func.name
        if not (name in node.frame() or name in node.root()):       # only builtins (not shadowed locally/globally)
            match name:
                case "exec": ...                                    # W (out of scope)
                case "reversed": self._check_reversed(node)         # E0111
                case "eval": ...
```
`name in node.frame()` uses astroid `__contains__` on locals of the enclosing frame;
`name in node.root()` checks module locals — i.e. a module-level `def reversed(...)`
suppresses the check.

## E0119 misplaced-format-function — "format function is not called on str" (no args)
`_check_misplaced_format_function` (:673-687):
```
if not isinstance(call_node.func, nodes.Attribute): return
if call_node.func.attrname != "format": return
expr = utils.safe_infer(call_node.func.expr)
if isinstance(expr, util.UninferableBase): return        # bail on Uninferable
if not expr:                                             # safe_infer returned None
    match call_node.func.expr:
        case nodes.Call(func=nodes.Name(name="print")):
            add_message("misplaced-format-function", node=call_node)
```
i.e. ONLY fires for the literal pattern `print(...).format(...)` and only when inference of
`print(...)` failed (returns None) — if inference succeeds (it infers Const(None) from the
print stub), no message... in practice astroid CAN infer print() → Const(None), making
`expr` truthy?? `Const(None)` is truthy as a node object, so `not expr` is False → no
message. BUT pylint's own tests expect this to fire; astroid's builtins brain doesn't
provide an infer_call_result for print → safe_infer(print(...)) returns None.
Reported at the Call node, confidence UNDEFINED.

## E0111 bad-reversed-sequence — "The first reversed() argument is not a sequence" (no args)
`_check_reversed` (:814-870), reported at the Call node:
```
try: argument = safe_infer(get_argument_from_call(node, position=0))
except NoSuchArgumentError: pass                       # reversed() with no positional arg → nothing
else:
    match argument:
        case util.UninferableBase(): return            # bail
        case None:                                     # inference failed
            if isinstance(node.args[0], nodes.Call):   # maybe reversed(iter(...))
                try: func = next(node.args[0].func.infer())
                except InferenceError: return
                if getattr(func, "name", None) == "iter" and utils.is_builtin_object(func):
                    add_message("bad-reversed-sequence", node=node)
            return
        case nodes.List() | nodes.Tuple(): return      # ok
        case astroid.Instance() if not self._py38_plus:
            ... dict-subclass special case, dead on py>=3.8 default ...
    if hasattr(argument, "getattr"):
        for methods in REVERSED_METHODS:               # (("__getitem__","__len__"), ("__reversed__",))
            for meth in methods:
                try: argument.getattr(meth)
                except astroid.NotFoundError: break     # this group fails
            else: break                                 # all of group found → ok, stop
        else:
            add_message("bad-reversed-sequence", node=node)   # no group fully satisfied
    else:
        add_message("bad-reversed-sequence", node=node)       # inferred object has no getattr at all
```
NOTE only `position=0` positional arg is considered (get_argument_from_call with no keyword).
`is_builtin_object(n)` (utils.py:286-288) = `n.root().name == "builtins"`.
The protocol test is performed on whatever safe_infer returned — Instance, ClassDef, Const,
etc. A Const str HAS __getitem__/__len__ via its proxied class → ok. A ClassDef passed
directly (reversed(SomeClass)) checks attributes on the class — class-level methods exist →
NOT flagged (bug-for-bug). Generators (bases.Generator) have getattr but lack all three →
flagged.

---------------------------------------------------------------------------------------

# 3. checkers/classes/class_checker.py — ClassChecker

Checker name "classes"; msgs at class_checker.py:493-734. Config that matters here:
- `valid-classmethod-first-arg` default `("cls",)` (:797-805)
- `valid-metaclass-classmethod-first-arg` default `("mcs",)` (:807-815)
(other options affect only out-of-scope W/C messages).
State: `self._accessed = ScopeAccessMap()` and `self._first_attrs: list[str|None]` (:847-850).
`open()` sets `_py38_plus` (:852-855).

ScopeAccessMap (:742-760): `set_accessed(node)` appends Attribute/AssignAttr nodes per
(class-frame, attrname); `accessed(scope)` returns dict attr → [access nodes].

Access recording:
- visit_attribute (:1680-1697): `self._check_super_without_brackets(node)`; then
  `if self._uses_mandatory_method_param(node): self._accessed.set_accessed(node); return`.
- visit_assignattr (:1714-1723): if `assign_type()` is AugAssign and
  `_uses_mandatory_method_param(node)` → set_accessed; then `_check_in_slots(node)` (E0237)
  and `_check_invalid_class_object(node)` (E0243).
- `_uses_mandatory_method_param` (:2359-2366) → `_is_mandatory_method_param(node.expr)`
  (:2368-2388): True iff node.expr is a Name equal to the current method's first parameter
  name (top of `_first_attrs` stack, maintained by `_check_first_arg_for_type` push (:2100)
  and leave_functiondef pop (:1668-1676; pops only when `node.is_method() and
  node.args.args is not None`); when the stack is empty, falls back to the closest
  enclosing FunctionDef's first positional arg if that function `is_bound()`).
  Static methods set `_first_attrs[-1] = None` (:2111) → accesses in them are not recorded.

## E0211 no-method-argument / E0213 no-self-argument
visit_functiondef (:1266-1281) for every method calls
`_check_first_arg_for_type(node, klass.type == "metaclass")` (:2079-2155):
```
if node.args.args is None: return                       # builtins / unknown args — bail
first_arg = posonlyargs[0].name if posonlyargs else (argnames()[0] if args.args else None)
self._first_attrs.append(first_arg)
first = self._first_attrs[-1]
if node.type == "staticmethod":
    if first_arg == "self" or first_arg in valid_classmethod_first_arg
            or first_arg in valid_metaclass_classmethod_first_arg:
        add_message("bad-staticmethod-argument", args=first, node=node); return    # W0211 (out of scope)
    self._first_attrs[-1] = None
elif "builtins.staticmethod" in node.decoratornames(): return    # decorator aliased to staticmethod
elif not (node.args.args or node.args.posonlyargs or node.args.vararg or node.args.kwarg):
    add_message("no-method-argument", node=node, args=node.name)      # E0211 "Method %r has no argument"
elif metaclass:
    ... C0203/C0204 (out of scope) ...
elif node.type == "classmethod" or node.name == "__class_getitem__":
    ... C0202 (out of scope) ...
elif first != "self":
    add_message("no-self-argument", node=node, args=node.name)        # E0213
```
Both at the FunctionDef (node.position). Note E0211: keyword-only args alone do NOT count
(`def m(*, a)` → E0211? args.args empty, posonlyargs empty, no vararg/kwarg → E0211 fires).
E0213 only for regular instance methods of non-metaclass classes whose first positional
(or posonly) arg isn't literally "self" (kwonly-only case is caught by E0211 first).
`node.type` is astroid's method-type computed from decorators
(staticmethod/classmethod/builtins lookups + special names: `__new__`, `__init_subclass__`,
`__class_getitem__` are implicitly classmethod in astroid for `type` computation —
careful: astroid sets `type == "classmethod"` for `__new__`, `__init_subclass__`; pylint
additionally treats `__class_getitem__` via the explicit name check :2145).

## E0202 method-hidden — "An attribute defined in %s line %s hides this method"
visit_functiondef (:1266-1357) tail; runs for every method that wasn't returned out earlier
(the `__init__` early-return at :1279-1281 means `__init__` is never checked).
Decorator exemptions (:1298-1331):
```
if node.decorators:
    for decorator in node.decorators.nodes:
        match decorator:
            case nodes.Attribute(attrname="getter"|"setter"|"deleter"): return
            case nodes.Name():
                if decorator.name in ALLOWED_PROPERTIES: return
                # ALLOWED_PROPERTIES = {"bultins.property", "functools.cached_property"} (:52)
                #   NB typo "bultins.property" is in the source — a bare Name never matches
                #   a dotted string anyway, so this arm effectively never returns.
            case nodes.Attribute():
                if self._check_functools_or_not(decorator): return
                # (:1507-1523) attrname == "cached_property" and decorator.expr is a Name
                # whose lookup resolves to an Import/ImportFrom with "functools" in names
        inferred = safe_infer(decorator)
        if not inferred: return                                  # bail: uninferable decorator
        if isinstance(inferred, nodes.FunctionDef):
            try: inferred = next(inferred.infer_call_result(inferred))
            except InferenceError: return
        try:
            if isinstance(inferred, (astroid.Instance, nodes.ClassDef)) \
                    and inferred.getattr("__get__") and inferred.getattr("__set__"):
                return                                           # data descriptor decorator
        except astroid.AttributeInferenceError: pass
```
Main check (:1333-1357):
```
try:
    overridden = klass.instance_attr(node.name)[0]     # first instance-attr assignment of same name
    overridden_frame = overridden.frame()
    match overridden_frame:
        case nodes.FunctionDef(type="method"):
            overridden_frame = overridden_frame.parent.frame()    # the class owning that method
    if not (isinstance(overridden_frame, nodes.ClassDef)
            and klass.is_subtype_of(overridden_frame.qname())):
        return                          # assignment isn't in this class or an ancestor → ignore
    for ancestor in klass.ancestors():
        if node.name in ancestor.instance_attrs and is_attr_private(node.name):
            return                      # private name set by an ancestor: name-mangled, not ours
        for obj in ancestor.lookup(node.name)[1]:
            if isinstance(obj, nodes.FunctionDef):
                return                  # an ancestor also defines it as a method → its fault
    args = (overridden.root().name, overridden.fromlineno)
    self.add_message("method-hidden", args=args, node=node)
except astroid.NotFoundError:
    pass                                # no instance attr with this name → no message
```
args = (module name of the assignment, line of the assignment). Reported at the method
FunctionDef. `klass.instance_attrs` is populated by astroid for `self.X = ...` assignments
in any method of the class and its ancestors? — `instance_attr` (astroid) searches the
class AND its ancestors' instance_attrs.
`is_attr_private` (utils.py:709-714): regex `^_{2,10}.*[^_]+_?$`.

## E0203 access-member-before-definition — "Access to member %r before its definition line %s"
leave_classdef (:1045-1059) → `_check_attribute_defined_outside_init` (:1192-1206) →
`if cnode.type != "metaclass": self._check_accessed_members(cnode, accessed)` where
accessed = self._accessed.accessed(cnode).
NOTE the mixin early-return at :1194-1202 applies (default `ignored_checks_for_mixins`
does NOT include "attribute-defined-outside-init"? It does — the default for
ignored-checks-for-mixins includes several; but for -E mode the relevant part is only that
this guard can skip the whole function for classes matching mixin rgx `.*[Mm]ixin` IF
"attribute-defined-outside-init" is in ignored_checks_for_mixins (it IS in the default
list, defined in pylint/lint/base_options.py). So: classes named *Mixin are fully exempt
from E0203 as well (bug-for-bug).
`_check_accessed_members` (:2017-2077):
```
excs = ("AttributeError", "Exception", "BaseException")
for attr, nodes_lst in accessed.items():
    try: node.local_attr(attr); continue            # class attribute exists → fine
    except NotFoundError: pass
    try: next(node.instance_attr_ancestors(attr)); continue   # ancestor defines it as inst attr → fine
    except StopIteration: pass
    try: defstmts = node.instance_attr(attr)
    except NotFoundError: pass                       # never defined → no E0203 (typecheck handles it)
    else:
        defstmts = [stmt for stmt in defstmts if stmt not in nodes_lst]   # drop AugAssign self-accesses
        if not defstmts: continue
        scope = defstmts[0].scope()
        defstmts = [stmt for i, stmt in enumerate(defstmts)
                    if i == 0 or stmt.scope() is not scope]   # 1 per first scope
        if len(defstmts) == 1:
            defstmt = defstmts[0]; frame = defstmt.frame(); lno = defstmt.fromlineno
            for _node in nodes_lst:
                if (_node.frame() is frame and _node.fromlineno < lno
                        and not astroid.are_exclusive(_node.statement(), defstmt, excs)):
                    add_message("access-member-before-definition",
                                node=_node, args=(attr, lno))
```
Reported at the accessing Attribute node; args = (attrname, definition line).
Conservatism: any of (class attr exists / ancestor instance attr / >1 defining scope /
exclusive branches / try-except AttributeError|Exception|BaseException) suppresses.
Only accesses recorded via `self.<attr>` (mandatory first param) in non-static methods are
candidates (see access recording above; plus AugAssign assign-attrs).

## F0202 method-check-failed — "Unable to check methods signature (%s / %s)"
`_check_signature` (:2272-2286): called from visit_functiondef override loop (:1283-1296)
with (node, parent_function, klass):
```
if not (isinstance(method1, FunctionDef) and isinstance(refmethod, FunctionDef)):
    add_message("method-check-failed", args=(method1, refmethod), node=method1); return
```
args are the node objects themselves (rendered via %s → their repr). In practice
unreachable from visit_functiondef because the caller already filters
`isinstance(parent_function, nodes.FunctionDef)` (:1292) and node is a FunctionDef —
keep as dead-code parity or implement trivially.

## E0236 invalid-slots-object — "Invalid object %r in __slots__, must contain only non empty strings"
visit_classdef → `_check_slots` (:1547-1582):
```
if "__slots__" not in node.locals: return
try: inferred_slots = tuple(node.ilookup("__slots__"))    # infer all values bound to __slots__
except InferenceError: return
for slots in inferred_slots:
    if isinstance(slots, UninferableBase): continue
    if not is_iterable(slots) and not is_comprehension(slots):
        add_message("invalid-slots", node=node); continue            # E0238
    if isinstance(slots, nodes.Const):
        add_message("single-string-used-for-slots", node=node); continue   # C0205 (out of scope)
    if not hasattr(slots, "itered"): continue                        # bail (e.g. deque)
    values = [item[0] for item in slots.items] if isinstance(slots, nodes.Dict) else slots.itered()
    if isinstance(values, UninferableBase): continue
    for elt in values:
        try: self._check_slots_elt(elt, node)
        except InferenceError: continue
    self._check_redefined_slots(node, slots, values)                 # W0244 (out of scope)
```
`_check_slots_elt` (:1638-1666):
```
for inferred in elt.infer():
    match inferred:
        case UninferableBase(): continue
        case nodes.Const(value=str() as value) if value: pass        # non-empty string ok
        case _:
            add_message("invalid-slots-object", args=elt.as_string(), node=elt,
                        confidence=INFERENCE)
            continue
    # E0242 below runs only for the non-empty-str case
```
Reported at the slot element node; args = source text of the element. Note empty string
Const → invalid-slots-object.

## E0242 class-variable-slots-conflict — "Value %r in slots conflicts with class variable"
Continuation of `_check_slots_elt` (:1656-1666), for each inferred non-empty string slot:
```
match class_variable := node.locals.get(inferred.value):
    case [nodes.NodeNG(parent=nodes.AnnAssign(value=None))]:
        return                                  # single bare annotation → no conflict, STOP (returns!)
    case _ if class_variable:
        add_message("class-variable-slots-conflict", args=(inferred.value,), node=elt)
```
args = the slot string; reported at the slot element node. Conflicts include methods and
properties (anything in class locals). NB the AnnAssign arm `return`s out of the whole
element check (not just continue) — bug-for-bug.

## E0237 assigning-non-slot — "Assigning to attribute %r not defined in class slots"
visit_assignattr (:1714-1723) → `_check_in_slots(node)` (:1751-1824):
```
inferred = safe_infer(node.expr)                     # the object being assigned to
if not isinstance(inferred, astroid.Instance): return        # BAIL (classes, modules, unknown)
klass = inferred._proxied
if not has_known_bases(klass): return                        # BAIL
if "__slots__" not in klass.locals: return                   # class itself must declare slots
if any(base.locals.get("__setattr__") for base in klass.mro()
       if base.qname() != "builtins.object"):
    return                                                   # custom __setattr__ → BAIL
if any(base.qname() == "typing.Generic" for base in klass.mro()):
    # invalidate astroid's cached slots() result for Generic-based classes
    cache = getattr(klass, "__cache", None)
    if cache and cache.get(klass.slots) is not None: del cache[klass.slots]
slots = klass.slots()                                # astroid: merged Const slot list over MRO,
if slots is None: return                             #   None if any ancestor lacks __slots__ etc.
if any("__slots__" not in ancestor.locals and ancestor.name not in ("Generic","object")
       for ancestor in klass.ancestors()):
    return                                           # any slot-less ancestor → instances get __dict__
if not any(slot.value == node.attrname for slot in slots):
    if not any(slot.value == "__dict__" for slot in slots):
        if _is_attribute_property(node.attrname, klass): return     # (:449-476) property → BAIL
        if node.attrname != "__class__" and utils.is_class_attr(node.attrname, klass):
            return                                                  # class attr exists → BAIL
        if node.attrname in klass.locals:
            for local_name in klass.locals.get(node.attrname):
                if isinstance(local_name.statement(), nodes.AnnAssign) \
                        and not local_name.statement().value:
                    return                                          # bare annotation → BAIL
            if _has_data_descriptor(klass, node.attrname): return   # (:397-413) __get__+__set__
        if node.attrname == "__class__" and _has_same_layout_slots(slots, node.parent.value):
            return                                                  # (:479-490) same-layout class swap
        add_message("assigning-non-slot", args=(node.attrname,), node=node,
                    confidence=INFERENCE)
```
args = the attribute name; reported at the AssignAttr node, confidence INFERENCE.
`_is_attribute_property(name, klass)` (:449-476): klass.getattr(name) succeeds and any
attr infers (first result) to a FunctionDef decorated with property (decorated_with_property,
utils.py:805-815 — inference-based incl. property subclasses and functools.cached_property
via `_is_property_decorator` utils.py:843-867) or whose pytype() == "builtins.property".
`_has_data_descriptor` (:397-413): any attribute value infers to an Instance with both
`__get__` and `__set__`; InferenceError → True (conservative no-emit).
`_has_same_layout_slots(slots, assigned_value)` (:479-490): `next(assigned_value.infer())`
is a ClassDef whose slots() zip_longest-match the current slots by .value.

## E0238 invalid-slots — "Invalid __slots__ object"
See `_check_slots` above (:1559-1561): an inferred `__slots__` value that is neither
iterable (utils.is_iterable, §below) nor a comprehension → message at the ClassDef, no args.
`is_iterable` (utils.py:1304-1309) → `_supports_protocol(value, _supports_iteration_protocol)`
(:1275-1301): ClassDef → True only if unknown bases or metaclass supports iter; Instance →
True if unknown bases / dynamic getattr / has `__iter__` or `__getitem__`;
ComprehensionScope → True; Proxy of instance → protocol check. A Const str IS an Instance
proxy with __getitem__ → iterable → falls to the Const check (C0205).

## E0239 inherit-non-class — "Inheriting %r, which is not a class."
visit_classdef → `_check_proper_bases` (:995-1022):
```
for base in node.bases:
    ancestor = safe_infer(base)
    if not ancestor: continue                                    # bail per-base
    if isinstance(ancestor, astroid.Instance) and (
            ancestor.is_subtype_of("builtins.type") or ancestor.is_subtype_of(".Protocol")):
        continue                                                 # e.g. TypeVar instances / Protocol
    if not isinstance(ancestor, nodes.ClassDef) or _is_invalid_base_class(ancestor):
        add_message("inherit-non-class", args=base.as_string(), node=node)
    if isinstance(ancestor, nodes.ClassDef) and ancestor.is_subtype_of("enum.Enum"):
        self._check_enum_base(node, ancestor)                    # E0244, below
    if ancestor.name == "object":
        add_message("useless-object-inheritance", ...)           # R0205 (out of scope)
```
`_is_invalid_base_class` (:393-394): name in {"bool","range","slice","memoryview"} and
is_builtin_object. args = base expression source text; at the ClassDef.
`Instance.is_subtype_of(".Protocol")` — note leading dot: matches qname suffix for typing
Protocol stubs whose root module name is "" in some astroid builds.

## E0240 inconsistent-mro / E0241 duplicate-bases
visit_classdef → `_check_consistent_mro` (:928-935):
```
try: node.mro()
except astroid.InconsistentMroError: add_message("inconsistent-mro", args=node.name, node=node)
except astroid.DuplicateBasesError:  add_message("duplicate-bases", args=node.name, node=node)
```
Messages: "Inconsistent method resolution order for class %r" / "Duplicate bases for class %r",
args = class name, at the ClassDef. astroid's `mro()` implements C3 linearization;
DuplicateBasesError raised when the same ClassDef object appears twice in bases (after
inference); unresolvable bases raise neither (MroError subclasses are the two caught).

## E0243 invalid-class-object — "Invalid assignment to '__class__'. Should be a class definition but got a '%s'"
visit_assignattr → `_check_invalid_class_object` (:1725-1749):
```
if node.attrname != "__class__": return
if isinstance(node.parent, nodes.Tuple):              # unpacking assignment: (a.__class__, x) = ...
    class_index = index of elt with attrname "__class__" in node.parent.elts
    inferred = safe_infer(node.parent.parent.value.elts[class_index])
    #          ^ assumes RHS is a literal tuple with matching arity; AttributeError risk accepted upstream?
else:
    inferred = safe_infer(node.parent.value)           # parent is Assign (or AnnAssign)
match inferred:
    case nodes.ClassDef() | util.UninferableBase() | None: return    # bail on ok/unknown
add_message("invalid-class-object", node=node, args=inferred.__class__.__name__,
            confidence=INFERENCE)
```
args = the astroid node class name of the inferred value (e.g. "Const", "FunctionDef").
Reported at the AssignAttr. Conservatism: uninferable or None → silent.

## E0244 invalid-enum-extension — 'Extending inherited Enum class "%s"'
`_check_enum_base` (:937-954), called per enum.Enum-derived base from _check_proper_bases:
```
match ancestor.getattr("__members__"):
    case [nodes.Dict(items=items), *_] if items:       # enum brain synthesizes __members__ Dict
        for _, name_node in items:
            if all(isinstance(item.parent, nodes.AnnAssign) and item.parent.value is None
                   for item in ancestor.getattr(name_node.name)):
                continue                                # annotation-only members don't count
            add_message("invalid-enum-extension", args=ancestor.name, node=node,
                        confidence=INFERENCE)
            break
```
args = the ancestor enum's name; at the (subclassing) ClassDef. The rest of
`_check_enum_base` (:956-993) is W0213 implicit-flag-alias (out of scope).
Note: `ancestor.getattr("__members__")` raising NotFoundError would propagate?? It's inside
match — `getattr` raises AttributeInferenceError if missing; astroid's enum brain always
adds `__members__` to Enum subclasses, and non-enum ancestors aren't passed here. The call
is guarded by `ancestor.is_subtype_of("enum.Enum")`.

## E0245 declare-non-slot — "No such name %r in __slots__"
visit_classdef → `_check_declare_non_slot` (:886-926):
```
if not self._has_valid_slots(node): return            # (:1525-1545) __slots__ exists locally,
                                                      # all inferred values iterable, non-Const, itered-able
slot_names = self._get_classdef_slots_names(node)     # (:1584-1598)
if not slot_names: return                             # empty __slots__ → likely MI helper, bail
if "__dict__" in slot_names: return
for base in node.bases:
    ancestor = safe_infer(base)
    if not isinstance(ancestor, nodes.ClassDef): continue
    if not self._has_valid_slots(ancestor): return    # any base without valid __slots__ → bail
    for slot_name in self._get_classdef_slots_names(ancestor):
        if slot_name == "__dict__": return
        slot_names.append(slot_name)
for child in node.body:
    match child:
        case nodes.AnnAssign(target=nodes.AssignName(name=name), value=None) if name not in slot_names:
            add_message("declare-non-slot", args=child.target.name, node=child.target,
                        confidence=INFERENCE)
```
Reported at the AssignName target of the annotation-only class-body statement.
`_get_classdef_slots_names`: for each inferred `__slots__` value, take Dict keys or
itered() elements, then `_get_slots_names` (:1600-1610): Const → its value; otherwise
safe_infer the element and use its `.value` if str. NOTE only direct bases are examined
(not transitive ancestors) — bug-for-bug.

## also in visit_functiondef: per-method E-codes interplay
Order inside visit_functiondef (:1266-1357): not-a-method → return; useless-super (W);
property-with-parameters (R); `_check_first_arg_for_type` (E0211/E0213); `__init__` →
`_check_init` (W0231/W0233) then RETURN (so __init__ skips signature + method-hidden);
override loop (W0221/W0222/W0236/W0239 + F0202); decorator exemptions; E0202.

---------------------------------------------------------------------------------------

# 4. checkers/classes/special_methods_checker.py — SpecialMethodsChecker (E0301-E0313)

Checker name "classes"; msgs at special_methods_checker.py:62-143.

## Entry — visit_functiondef / visit_asyncfunctiondef (:164-195)
```
if not node.is_method(): return
inferred = _safe_infer_call_result(node, node)        # :30-53
if (inferred and node.name in self._protocol_map and not is_function_body_ellipsis(node)):
    self._protocol_map[node.name](node, inferred)
if node.name in PYMETHODS:
    self._check_unexpected_method_signature(node)     # E0302
```
`_safe_infer_call_result` (:30-53): `node.infer_call_result(caller=node)`; returns the
single inferred return value; None on InferenceError, no values, ambiguity (a second yield
from the generator), or InferenceError while checking for a second value. THE bail-out:
un-inferable return → none of the return-type checks fire.
`is_function_body_ellipsis` exempts `def __str__(self): ...` stubs.
protocol_map (:147-162): `__iter__ __len__ __bool__ __index__ __repr__ __str__ __bytes__
__hash__ __length_hint__ __format__ __getnewargs__ __getnewargs_ex__`.

For generators/async: `infer_call_result` on a generator function returns a
`bases.Generator` object — `__iter__` implemented as a generator passes `_is_iterator`.

## Type predicates (:247-320) — used by all return checks
```
_is_wrapped_type(node, T): isinstance(node, bases.Instance) and node.name == T
                           and not isinstance(node, nodes.Const)
_is_int:   wrapped "int"   or Const with isinstance(value, int)     # NB bool is int subclass!
_is_str:   wrapped "str"   or Const str
_is_bool:  wrapped "bool"  or Const bool
_is_bytes: wrapped "bytes" or Const bytes
_is_tuple: wrapped "tuple" or Const tuple
_is_dict:  wrapped "dict"  or Const dict
_is_iterator(node):
    Generator → True; ComprehensionScope → True;
    bases.Instance → True iff node.local_attr("__next__") succeeds
        # local_attr searches the instance's class AND ancestors' locals
    nodes.ClassDef → True iff its metaclass() is a ClassDef with local "__next__"
    else False
```
NOTE `_is_int(Const(True))` is True (bool ⊂ int) → `def __index__: return True` NOT flagged.
`_is_bool` only accepts exact bool → `__bool__` returning 1 → flagged.

## E0301 non-iterator-returned — "__iter__ returns non-iterator" (no args)
`_check_iter` (:322-324): `if not _is_iterator(inferred): add_message(...)` at FunctionDef.

## E0302 unexpected-special-method-signature —
"The special method %r expects %s param(s), %d %s given"
`_check_unexpected_method_signature` (:197-245):
```
expected_params = SPECIAL_METHODS_PARAMS[node.name]
if expected_params is None: return                       # __init__ etc: variadic
if not node.args.args and not node.args.vararg: return   # no params at all → E0211's job
if decorated_with(node, ["builtins.staticmethod"]): all_args = node.args.args
else: all_args = node.args.args[1:]                      # drop self
mandatory = len(all_args) - len(node.args.defaults)
optional  = len(node.args.defaults)
current_params = mandatory + optional
if isinstance(expected_params, tuple):                   # __round__ (0,1) / __pow__ (1,2)
    emit = mandatory not in expected_params
    expected_params = f"between {expected_params[0]} or {expected_params[1]}"
else:
    rest = expected_params - mandatory
    if rest == 0: emit = False
    elif rest < 0: emit = True                           # too many mandatory
    elif rest > 0: emit = not ((optional - rest) >= 0 or node.args.vararg)
if emit:
    verb = "was" if current_params <= 1 else "were"
    add_message("unexpected-special-method-signature",
                args=(node.name, expected_params, current_params, verb), node=node)
```
args = (dunder name, expected count (int or "between X or Y" str), actual int, "was"/"were").
At the FunctionDef. NOTE posonlyargs/kwonlyargs are NOT counted at all (bug-for-bug);
`node.args.defaults` length is subtracted from all_args even though defaults may belong to
self-less count — exact mirror required.

## E0303 invalid-length-returned — "__len__ does not return non-negative integer"
`_check_len` (:326-330): not _is_int → emit; elif Const and value < 0 → emit. At FunctionDef.
(Const True/False pass _is_int; bool is never < 0 → not flagged.)

## E0304 invalid-bool-returned — "__bool__ does not return bool"
`_check_bool` (:332-334): not _is_bool → emit.

## E0305 invalid-index-returned — "__index__ does not return int"
`_check_index` (:336-338): not _is_int → emit.

## E0306 invalid-repr-returned / E0307 invalid-str-returned / E0311 invalid-format-returned
`_check_repr` (:340-342), `_check_str` (:344-346), `_check_format` (:364-366):
not _is_str → emit.

## E0308 invalid-bytes-returned — `_check_bytes` (:348-350): not _is_bytes → emit.
## E0309 invalid-hash-returned — `_check_hash` (:352-354): not _is_int → emit.
## E0310 invalid-length-hint-returned — `_check_length_hint` (:356-362): like __len__
   (non-int → emit, negative Const int → emit).
## E0312 invalid-getnewargs-returned — `_check_getnewargs` (:368-372): not _is_tuple → emit.
## E0313 invalid-getnewargs-ex-returned — `_check_getnewargs_ex` (:374-403):
```
if not _is_tuple(inferred): emit; return
if not isinstance(inferred, nodes.Tuple): return        # wrapped tuple instance: can't see elts → bail
found_error = False
if len(inferred.elts) != 2: found_error = True
else:
    for arg, check in ((elts[0], _is_tuple), (elts[1], _is_dict)):
        if isinstance(arg, (nodes.Call, nodes.Name)): arg = safe_infer(arg)
        if arg and not isinstance(arg, UninferableBase):
            if not check(arg): found_error = True; break
if found_error: emit
```
All special-method messages have no args and are reported at the method's FunctionDef.

---------------------------------------------------------------------------------------

# 5. checkers/exceptions.py — ExceptionsChecker (E0701-E0712)

Checker name "exceptions" (:287); msgs at exceptions.py:62-180. Option
`overgeneral-exceptions` default `("builtins.BaseException","builtins.Exception")`
(:289-298) — only used by W0718/W0719 (out of scope).
`open()` (:301-303) computes `self._builtin_exceptions` = set of names of all builtins
members that are BaseException subclasses (via inspect on the RUNNING interpreter,
:28-33) — for the Rust port this must be the fixed py3.12 builtin exception name set.

Helpers:
- `_annotated_unpack_infer(stmt, context)` (:36-54): if stmt is a List/Tuple node, yield
  `(elt, safe_infer(elt))` for each element with a non-None non-Uninferable inference
  (others silently skipped); otherwise yield `(stmt, inferred)` for every result of
  `stmt.infer(context)` that isn't Uninferable. May raise InferenceError (callers catch).
- `_is_raising(body)` (:57-59): any direct child of the list is a Raise node.

## visit_raise (:305-331)
```
if node.exc is None: self._check_misplaced_bare_raise(node); return     # E0704 path
if node.cause is None: self._check_raise_missing_from(node)             # W0707 (out)
else: self._check_bad_exception_cause(node)                             # E0705
expr = node.exc
ExceptionRaiseRefVisitor(self, node).visit(expr)        # E0711 / W0719 / W0715 on raw AST
inferred = utils.safe_infer(expr)
if inferred is None or isinstance(inferred, util.UninferableBase): return   # BAIL
ExceptionRaiseLeafVisitor(self, node).visit(inferred)   # E0702 / E0710 on inferred value
```
Visitor dispatch (:190-196): method `visit_<classname.lower()>` else `visit_default`.

## E0704 misplaced-bare-raise — "The raise statement is not inside an except clause" (no args)
`_check_misplaced_bare_raise` (:333-352):
```
scope = node.scope()
if isinstance(scope, FunctionDef) and scope.is_method() and scope.name == "__exit__": return
current = node
ignores = (ExceptHandler, FunctionDef)
while current and not isinstance(current.parent, ignores):
    current = current.parent
if not (current and isinstance(current.parent, (ExceptHandler,))):
    add_message("misplaced-bare-raise", node=node, confidence=HIGH)
```
The walk stops at the first ancestor whose PARENT is an ExceptHandler or FunctionDef; if
that parent is the FunctionDef (or we ran off the tree), emit. So: bare raise anywhere
lexically nested under an except block (any depth, same function) is fine; bare raise in a
finally/else/try-body, at module level, or in a nested function defined inside an except
→ flagged. Reported at the Raise, confidence HIGH.

## E0705 bad-exception-cause — "Exception cause set to something which is not an exception, nor None"
`_check_bad_exception_cause` (:354-369) (raise X from CAUSE):
```
cause = utils.safe_infer(node.cause)
if cause is None or isinstance(cause, UninferableBase): return     # BAIL
if isinstance(cause, nodes.Const):
    if cause.value is not None: emit(confidence=INFERENCE)
elif not isinstance(cause, nodes.ClassDef) and not utils.inherit_from_std_ex(cause):
    emit(confidence=INFERENCE)
```
No args; reported at the Raise. NOTE any ClassDef cause is accepted (even a non-exception
class — bug-for-bug), and instances are accepted iff inherit_from_std_ex.

## E0711 notimplemented-raised — "NotImplemented raised - should raise NotImplementedError"
ExceptionRaiseRefVisitor.visit_name (:205-209): `if node.name == "NotImplemented"` → emit
at the Raise node, confidence HIGH, no args. Also reached via visit_call (:229-231) when
`raise NotImplemented(...)` (Call func is a Name → visit_name on it). Pure name match, no
inference.

## E0702 raising-bad-type — "Raising %s while only classes or instances are allowed"
ExceptionRaiseLeafVisitor on the safe_infer'ed value (:240-281):
- `visit_const` (:243-249): args = `node.value.__class__.__name__` (python type name, e.g.
  "int", "str", "NoneType"), confidence INFERENCE.
- `visit_tuple` (:266-272): args = "tuple", confidence INFERENCE.
- `visit_default` (:274-281): args = `getattr(node, "name", node.__class__.__name__)`,
  confidence INFERENCE. (e.g. inferred Module → its name; FunctionDef → function name? NO:
  FunctionDef raises are... a FunctionDef has .name so args = func name. A Lambda →
  "Lambda".)
All at the Raise node.
Dispatch names: instances of exceptions hit `visit_instance`/`visit_exceptioninstance`
(:251-256) → delegate to visit_classdef on `instance._proxied`.

## E0710 raising-non-exception — "Raising a class which doesn't inherit from BaseException"
ExceptionRaiseLeafVisitor.visit_classdef (:258-264) (also via instances, see above):
```
if not utils.inherit_from_std_ex(node) and utils.has_known_bases(node):
    add_message("raising-non-exception", node=self._node, confidence=INFERENCE)
```
No args, at the Raise. Conservatism: unknown bases → silent.

## E0712 catching-non-exception — "Catching an exception which doesn't inherit from Exception: %s"
visit_try (:576-651) iterates handlers; for handlers with a non-None, non-BoolOp type:
```
try: exceptions = list(_annotated_unpack_infer(handler.type))
except InferenceError: continue                         # BAIL per-handler
for part, exception in exceptions:
    if isinstance(exception, astroid.Instance) and utils.inherit_from_std_ex(exception):
        exception = exception._proxied                  # exception INSTANCE in except → treat as its class
    self._check_catching_non_exception(handler, exception, part)
    ...
```
`_check_catching_non_exception(handler, exc, part)` (:417-476):
```
if isinstance(exc, nodes.Tuple):                        # an inferred value that is itself a tuple
    inferred = [safe_infer(elt) for elt in exc.elts]
    if any(isinstance(n, UninferableBase) for n in inferred): return   # BAIL: unknown component
    if all(n and (inherit_from_std_ex(n)
                  or (isinstance(n, ClassDef) and not has_known_bases(n)))
           for n in inferred):
        return                                           # every element ok/unknown-bases → fine
    # else fall through to the non-ClassDef branch below (Tuple is not ClassDef) → EMIT
if not isinstance(exc, nodes.ClassDef):
    match exc:
        case nodes.Const(value=None):
            if (isinstance(handler.type, nodes.Const) and handler.type.value is None) \
                    or handler.type.parent_of(exc):
                emit                                     # literal None or None-element written in handler
            # else: inferred None from elsewhere → SILENT (redefinition guard)
        case _:
            emit
    return
if not inherit_from_std_ex(exc) and exc.name not in self._builtin_exceptions:
    if has_known_bases(exc):
        emit
```
emit = `add_message("catching-non-exception", node=handler.type, args=(X,))` where X =
`part.as_string()` in the non-ClassDef branches and `exc.name` in the ClassDef branch.
Reported at the handler's type expression node.
Note the `exc.name not in self._builtin_exceptions` escape: a class merely NAMED like a
builtin exception (e.g. user class `OSError`) is never flagged.

## E0701 bad-except-order — "Bad except clauses order (%s)"
Two emission sites in visit_try:
(a) bare except not last (:582-592):
```
if handler.type is None:
    if not _is_raising(handler.body): add_message("bare-except", ...)   # W0702 (out)
    if index < nb_handlers - 1:
        msg = "empty except clause should always appear last"
        add_message("bad-except-order", node=node, args=msg, confidence=HIGH)
        # NOTE reported at the Try node
```
(b) handler catching an ancestor of a previously caught class (:615-632):
```
# (within the per-handler loop, after _check_catching_non_exception)
if not isinstance(exception, nodes.ClassDef): continue
exc_ancestors = [a for a in exception.ancestors() if isinstance(a, nodes.ClassDef)]
for previous_exc in exceptions_classes:
    if previous_exc in exc_ancestors:
        msg = f"{previous_exc.name} is an ancestor class of {exception.name}"
        add_message("bad-except-order", node=handler.type, args=msg, confidence=INFERENCE)
...
exceptions_classes += [exc for _, exc in exceptions]    # :651 accumulate ALL inferred values
```
"More specific handler after more general" → for EACH earlier-caught class that is an
ancestor of the current class, one message. Identity comparison `previous_exc in
exc_ancestors` is object identity of ClassDef nodes (ancestors() yields the same objects).
Reported at handler.type, args = the sentence string. Also `visit_trystar` (:563-574)
delegates to visit_try → same checks for `except*`.
NOTE `exceptions_classes` accumulates inferred values including instances-promoted-to-class
only inside the loop variable, not the list (list gets raw `exc` from `_annotated_unpack_infer`
BEFORE the Instance→_proxied promotion? No: promotion happens to local `exception`, and
:651 appends `exc for _, exc in exceptions` — the UN-promoted values. So an earlier
`except SomeInstance` stores the Instance object, which will never be `in exc_ancestors`
→ no E0701 from instance handlers. Bug-for-bug.)

Also W0705 duplicate-except (:643-649) and W0718, W0711 binary-op-exception (BoolOp type,
:594-600) are out of scope (W-category).

---------------------------------------------------------------------------------------

# 6. checkers/strings.py — StringFormatChecker (E13xx)

Checker name "string"; msgs at strings.py:68-197.
`OTHER_NODES = (Const, List, Lambda, FunctionDef, ListComp, SetComp, GeneratorExp)` (:199-207).

## visit_binop — %-formatting (:251-405). Decorated only_required_for_messages(...E13xx...).
```
if node.op != "%": return
left, args = node.left, node.right
if not (isinstance(left, nodes.Const) and isinstance(left.value, str)): return   # literal LHS only
try:
    required_keys, required_num_args, required_key_types, required_arg_types = \
        utils.parse_format_string(left.value)
except UnsupportedFormatCharacter as exc:
    formatted = format_string[exc.index]
    add_message("bad-format-character", node=node,
                args=(formatted, ord(formatted), exc.index)); return       # E1300
except IncompleteFormatString:
    add_message("truncated-format-string", node=node); return              # E1301
if not required_keys and not required_num_args:
    add_message("format-string-without-interpolation", node=node); return  # W1310 (out)
if required_keys and required_num_args:
    add_message("mixed-format-string", node=node)                          # E1302 (no return!)
elif required_keys:
    # ---- named specifiers ----
    if isinstance(args, nodes.Dict):
        keys = set(); unknown_keys = False
        for k, _ in args.items:
            if isinstance(k, nodes.Const):
                if isinstance(k.value, str): keys.add(k.value)
                else: add_message("bad-format-string-key", node=node, args=k.value)  # W1300 (out)
            else: unknown_keys = True
        if not unknown_keys:
            for key in required_keys:
                if key not in keys:
                    add_message("missing-format-string-key", node=node, args=key)    # E1304
        for key in keys:
            if key not in required_keys:
                add_message("unused-format-string-key", node=node, args=key)         # W1301 (out)
        for key, arg in args.items:                       # E1307 per dict entry
            if not isinstance(key, nodes.Const): continue
            format_type = required_key_types.get(key.value, None)
            arg_type = utils.safe_infer(arg)
            if (format_type is not None and arg_type
                    and not isinstance(arg_type, UninferableBase)
                    and not arg_matches_format_type(arg_type, format_type)):
                add_message("bad-string-format-type", node=node,
                            args=(arg_type.pytype(), format_type))                   # E1307
    elif isinstance(args, (OTHER_NODES, nodes.Tuple)):
        add_message("format-needs-mapping", node=node, args=type(args).__name__)     # E1303
    # else: Name/expression RHS → could be a mapping → SILENT
else:
    # ---- unnamed specifiers: count args ----
    args_elts = []
    if isinstance(args, nodes.Tuple):
        rhs_tuple = utils.safe_infer(args)
        num_args = None
        if isinstance(rhs_tuple, nodes.BaseContainer):
            args_elts = rhs_tuple.elts; num_args = len(args_elts)
    elif isinstance(args, (OTHER_NODES, (nodes.Dict, nodes.DictComp))):
        args_elts = [args]; num_args = 1                      # single non-tuple value = 1 arg
    elif isinstance(args, nodes.Name):
        inferred = utils.safe_infer(args)
        if isinstance(inferred, nodes.Tuple): args_elts = inferred.elts; num_args = len(...)
        elif isinstance(inferred, nodes.Const): args_elts = [inferred]; num_args = 1
        else: num_args = None                                  # BAIL
    else: num_args = None                                      # arbitrary expression → BAIL
    if num_args is not None:
        if num_args > required_num_args: add_message("too-many-format-args", node=node)   # E1305
        elif num_args < required_num_args: add_message("too-few-format-args", node=node)  # E1306
        for arg, format_type in zip(args_elts, required_arg_types):
            if not arg: continue
            arg_type = utils.safe_infer(arg)
            if (arg_type and not isinstance(arg_type, UninferableBase)
                    and not arg_matches_format_type(arg_type, format_type)):
                add_message("bad-string-format-type", node=node,
                            args=(arg_type.pytype(), format_type))                          # E1307
```
All reported at the BinOp node.
- E1300 args = (char, ord(char), index) — template "Unsupported format character %r (%#02x) at index %d".
- E1303 args = the python AST-node class name of the RHS ("Tuple", "Const", "ListComp", ...).
- E1304 args = missing key string.
- E1307 args = (pytype string e.g. "builtins.str", conversion char).

`arg_matches_format_type` (:223-239):
```
if format_type in "sr": return True            # %s/%r accept anything (NB "" in "sr" never queried)
if isinstance(arg_type, astroid.Instance):
    match arg_type.pytype():
        case "builtins.str":   return format_type == "c"
        case "builtins.float": return format_type in "deEfFgGn%"
        case "builtins.int":   return True
    return False                                # other instances mismatch any non-s/r type
return True                                     # non-Instance (ClassDef, Module...) → accept
```
NOTE Const nodes ARE Instances (Const subclasses Instance via bases) — pytype on Const str
→ "builtins.str". `%d" % "x"` → mismatch → E1307. `%c` accepts str and int.

## visit_call — bound-method checks (:419-437). NO only_required_for_messages decorator
(runs always).
```
match func := utils.safe_infer(node.func):
    case astroid.BoundMethod(bound=astroid.Instance(name="str"|"unicode"|"bytes" as bound_name)):
        if func.name in {"strip", "lstrip", "rstrip"} and node.args:
            arg = utils.safe_infer(node.args[0])
            if not (isinstance(arg, nodes.Const) and isinstance(arg.value, str)): return
            if len(arg.value) != len(set(arg.value)):
                add_message("bad-str-strip-call", node=node, args=(bound_name, func.name))  # E1310
        elif func.name == "format":
            self._check_new_format(node, func)
```

### E1310 bad-str-strip-call — "Suspicious argument in %s.%s call"
args = (bound type name "str"/"bytes"/"unicode", method name). At the Call node. Trigger:
the first positional argument infers to a Const str with at least one duplicate character.
Conservatism: non-str / uninferable arg → silent; keyword-only call (no node.args) → silent.
NOTE on bytes instances: `b" x ".strip(b"x ")` — arg infers to Const bytes, `isinstance(value,str)`
False → silent (bug-for-bug: bytes strip args never flagged despite matching bound).

### _check_new_format — .format() checks (:452-534)
```
if isinstance(node.func, nodes.Attribute) and not isinstance(node.func.expr, nodes.Const):
    return                                  # only literal "...".format(...) — no vars
if node.starargs or node.kwargs: return     # *args/**kwargs present → BAIL
try: strnode = next(func.bound.infer())
except InferenceError: return
if not (isinstance(strnode, nodes.Const) and isinstance(strnode.value, str)): return
try: call_site = arguments.CallSite.from_call(node)
except InferenceError: return
try: fields, num_args, manual_pos = utils.parse_format_method_string(strnode.value)
except IncompleteFormatString:
    add_message("bad-format-string", node=node); return       # W1302 (out of scope)
positional_arguments = call_site.positional_arguments
named_arguments = call_site.keyword_arguments
named_fields = {field[0] for field in fields if isinstance(field[0], str)}
if num_args and manual_pos:
    add_message("format-combined-specification", node=node); return   # W1305 (out)
check_args = False
num_args += sum(1 for field in named_fields if not field)   # "{[0]}" counts as positional
if named_fields:
    for field in named_fields:
        if field and field not in named_arguments:
            add_message("missing-format-argument-key", node=node, args=(field,))   # W1303 (out)
    for field in named_arguments:
        if field not in named_fields:
            add_message("unused-format-string-argument", node=node, args=(field,)) # W1304 (out)
    num_args = num_args or manual_pos
    if positional_arguments or num_args:
        empty = not all(field for field in named_fields)
        if named_arguments or empty: check_args = True
else:
    check_args = True
if check_args:
    num_args = num_args or manual_pos
    if not num_args:
        add_message("format-string-without-interpolation", node=node); return      # W1310 (out)
    if len(positional_arguments) > num_args:
        add_message("too-many-format-args", node=node)        # E1305
    elif len(positional_arguments) < num_args:
        add_message("too-few-format-args", node=node)         # E1306
self._detect_vacuous_formatting(node, positional_arguments)   # W1308 (out)
self._check_new_format_specifiers(node, fields, named_arguments)   # W1306/W1307 (out)
```
In-scope here: E1305/E1306 (same message ids as %-formatting) at the Call node, no args.
`CallSite.from_call` (astroid arguments.py) resolves positional/keyword arguments.
W1306 missing-format-attribute / W1307 invalid-format-index in `_check_new_format_specifiers`
(:537-636) are W-category → out of scope (document presence only).

---------------------------------------------------------------------------------------

# 7. checkers/logging.py — LoggingChecker (E1200, E1201, E1205, E1206)

Checker name "logging"; msgs at logging.py:25-89. Options (:134-156):
- `logging-modules`, csv, default `("logging",)`
- `logging-format-style`, choice old|new, default `"old"`

State per module — visit_module (:158-173):
```
self._logging_names = set()                       # names bound to logging modules in THIS module
logging_mods = config.logging_modules             # ("logging",)
self._format_style = config.logging_format_style  # "old"
self._logging_modules = set(logging_mods)
self._from_imports = {}                           # "a.b" → {"a": "b"} for dotted entries
for logging_mod in logging_mods:
    parts = logging_mod.rsplit(".", 1)
    if len(parts) > 1: self._from_imports[parts[0]] = parts[1]
```
visit_importfrom (:175-183): if `node.modname` in _from_imports, for each (module, as_name)
in node.names with module == the configured tail → add `as_name or module` to
_logging_names. (With default config, _from_imports is empty → no-op.)
visit_import (:185-189): for each (module, as_name) with module in _logging_modules →
add `as_name or module` (so `import logging`, `import logging as log` both tracked).

## visit_call (:191-220)
```
def is_logging_name():
    match node.func:
        case nodes.Attribute(expr=nodes.Name(name=name)): return name in self._logging_names
    return False
def is_logger_class():
    for inferred in infer_all(node.func):
        if isinstance(inferred, astroid.BoundMethod):
            parent = inferred._proxied.parent
            if isinstance(parent, nodes.ClassDef) and (
                    parent.qname() == "logging.Logger"
                    or any(a.qname() == "logging.Logger" for a in parent.ancestors())):
                return True, inferred._proxied.name
    return False, None
if is_logging_name(): name = node.func.attrname
else:
    result, name = is_logger_class()
    if not result: return
self._check_log_method(node, name)
```
So both `logging.warning(...)` (module attr via tracked name) and `self.logger.warning(...)`
(BoundMethod whose owner class is/inherits logging.Logger) are checked; `name` is the
method name.

## _check_log_method (:222-269)
```
if name == "log":
    if node.starargs or node.kwargs or len(node.args) < 2: return    # BAIL
    format_pos = 1
elif name in CHECKED_CONVENIENCE_FUNCTIONS:   # {"critical","debug","error","exception","fatal","info","warn","warning"} (:92-101)
    if node.starargs or node.kwargs or not node.args: return         # BAIL
    format_pos = 0
else: return
match format_arg := node.args[format_pos]:
    case nodes.BinOp(): ...logging-not-lazy W1201 (out of scope)...
    case nodes.Call(): self._check_call_func(format_arg)             # W1202 (out)
    case nodes.Const(): self._check_format_string(node, format_pos)  # E1200/E1201/E1205/E1206
    case nodes.JoinedStr(): ...W1203 (out)...
```
NOTE `node.starargs`/`node.kwargs` are astroid Call properties (any Starred in args /
any kwarg with arg=None).

## _check_format_string (:322-375) — the E-codes
```
num_args = _count_supplied_tokens(node.args[format_pos + 1:])
    # (:391-404) = count of post-format args that are NOT nodes.Keyword
    # (keywords like exc_info=, extra= are in node.keywords, not args — Keyword filter is
    #  for safety) — lazy % args: logging.info("%s %s", a, b) → num_args = 2
format_string = node.args[format_pos].value
required_num_args = 0
if isinstance(format_string, bytes): format_string = format_string.decode()
if isinstance(format_string, str):
    try:
        if self._format_style == "old":
            keyword_args, required_num_args, _, _ = utils.parse_format_string(format_string)
            if keyword_args: return          # named specifiers (%(x)s) → out of scope, BAIL
        elif self._format_style == "new":
            keyword_arguments, implicit_pos_args, explicit_pos_args = \
                utils.parse_format_method_string(format_string)
            keyword_args_cnt = len({k for k, _ in keyword_arguments if not isinstance(k, int)})
            required_num_args = keyword_args_cnt + implicit_pos_args + explicit_pos_args
    except utils.UnsupportedFormatCharacter as ex:
        if num_args > 0:                     # only when arguments are actually supplied
            char = format_string[ex.index]
            add_message("logging-unsupported-format", node=node,
                        args=(char, ord(char), ex.index))                  # E1200
        return
    except utils.IncompleteFormatString:
        add_message("logging-format-truncated", node=node); return         # E1201
if num_args > required_num_args:
    add_message("logging-too-many-args", node=node, confidence=HIGH)       # E1205
elif num_args < required_num_args:
    add_message("logging-too-few-args", node=node)                         # E1206
```
All at the Call node.
- E1200 "Unsupported logging format character %r (%#02x) at index %d", args=(char, ord, idx).
  Only if at least one format argument supplied. NOTE: E1201 has no such num_args guard.
- E1201 no args. E1205 no args (confidence HIGH). E1206 no args.
- A non-str non-bytes Const (e.g. `logging.info(42)`): skips parsing, required_num_args=0
  → E1205 iff extra positional args present.
- Default format style "old": `{}` placeholders are NOT counted → `logging.info("{}", x)`
  → E1205 too-many-args (required 0, supplied 1). Bug-for-bug.

---------------------------------------------------------------------------------------

# 8. checkers/async_checker.py — AsyncChecker (E1700, E1701)

Checker name "async"; msgs at async_checker.py:27-42. `open()` (:44-46):
`self._mixin_class_rgx = config.mixin_class_rgx` (default regex `.*[Mm]ixin`),
`self._async_generators = ["contextlib.asynccontextmanager"]`.

## E1700 yield-inside-async-function — "Yield inside async function" (no args)
visit_asyncfunctiondef (:48-54):
```
for child in node.nodes_of_class(nodes.Yield):
    if child.scope() is node and (sys.version_info[:2] == (3, 5)
                                  or isinstance(child, nodes.YieldFrom)):
        add_message("yield-inside-async-function", node=child)
```
On Python 3.12 host: only `yield from` (YieldFrom subclasses Yield) directly in the async
function's own scope. Plain `yield` is legal (async generators) → not flagged. Reported at
the YieldFrom node. NOTE this checks the RUNNING interpreter (`sys.version_info`), not
py-version config — on the 3.12 reference runtime the condition is YieldFrom-only.

## E1701 not-async-context-manager —
"Async context manager '%s' doesn't implement __aenter__ and __aexit__."
visit_asyncwith (:56-93): for each `(ctx_mgr, _)` in node.items:
```
match inferred := checker_utils.safe_infer(ctx_mgr):
    case _ if not inferred: continue                       # BAIL per-item (None)
    case nodes.AsyncFunctionDef():
        if decorated_with(inferred, ["contextlib.asynccontextmanager"]): continue
        # else falls THROUGH to emit (an async function object isn't a ctx mgr)
    case astroid.bases.AsyncGenerator():
        if decorated_with(inferred.parent, ["contextlib.asynccontextmanager"]): continue
    case _:
        try:
            inferred.getattr("__aenter__"); inferred.getattr("__aexit__")
        except astroid.exceptions.NotFoundError:
            if isinstance(inferred, astroid.Instance):
                if not checker_utils.has_known_bases(inferred): continue     # BAIL unknown bases
                if ("not-async-context-manager" in config.ignored_checks_for_mixins
                        and self._mixin_class_rgx.match(inferred.name)):
                    continue                                                 # mixin exemption
            # non-Instance or known-bases Instance → fall through to emit
        else: continue                                       # both attrs found → OK
add_message("not-async-context-manager", node=node, args=(inferred.name,))
```
Reported at the AsyncWith node; args = inferred.name (the inferred object's name attribute
— Instance name = class name; AsyncGenerator name = "async_generator"; AsyncFunctionDef
name = function name). NOTE `Uninferable` is falsy → `not inferred` continue covers it.
Default `ignored_checks_for_mixins` (base_options) includes "not-async-context-manager",
so classes matching `.*[Mm]ixin` are exempt by default.

---------------------------------------------------------------------------------------

# 9. checkers/unicode.py — UnicodeChecker (E2501-E2515) — RAW-BYTES checker

`BaseRawFileChecker`: runs `process_module(node)` once per module, reading the raw byte
stream (`node.stream()`), NOT the AST. Checker name "unicode_checker"; msgs at
unicode.py:322-373 (E2501, E2502, C2503 (out of scope), E2510-E2515 generated from
BAD_CHARS).

## Data tables
BIDI_UNICODE (:37-55): U+202A, U+202B, U+202C, U+202D, U+202E, U+2066, U+2067, U+2068,
U+2069, U+200F. (U+200E deliberately excluded.)

BAD_CHARS (:80-137) — `_BadChar(name, unescaped, escaped, code, help)`:
| code  | name             | char  | escaped  |
|-------|------------------|-------|----------|
| E2510 | backspace        | \x08  | `\b`     |
| E2511 | carriage-return  | \x0D  | `\r`     |
| E2512 | sub              | \x1A  | `\x1A`   |
| E2513 | esc              | \x1B  | `\x1B`   |
| E2514 | nul              | \x00  | `\0`     |
| E2515 | zero-width-space | ​| `​` |
Message id symbol = `invalid-character-<name>` (:74-76); message TEXT (the msg template) =
`f'Invalid unescaped character {name}, use "{escaped}" instead.'` (:67-72) — NO % args.
`BAD_ASCII_SEARCH_DICT = {char.unescaped: char}` (:138).

UNICODE_BOMS (:193-201): utf-8→BOM_UTF8(EF BB BF), utf-16→BOM_UTF16 (native, ambiguous),
utf-32, utf-16le(FF FE), utf-16be(FE FF), utf-32le(FF FE 00 00), utf-32be(00 00 FE FF).
BOM_SORTED_TO_CODEC (:202-206): ordered mapping checked in order utf-32le, utf-32be,
utf-8, utf-16le, utf-16be (longest BOM first so FF FE 00 00 wins over FF FE).
`_normalize_codec_name` (:213-215): regex `utf[ -]?(8|16|32)[ -]?(le|be|)?(sig)?`
(IGNORECASE) replaced with `utf-\1\2`, lowercased — e.g. "UTF8" → "utf-8", "utf-8-sig" →
"utf-8", "UTF-16LE" → "utf-16le".
`_byte_to_str_length` (:233-240): utf-32* → 4, utf-16* → 2, else 1.

## Codec detection — `_determine_codec(stream)` (:419-458)
```
try:
    codec, lines = tokenize.detect_encoding(stream.readline)   # PEP 263: BOM + coding: comments
    codec_definition_line = len(lines) or 1     # lines is [] when UTF-8 BOM found → line 1
except SyntaxError as e:                        # detect_encoding raises on bogus coding decl
    stream.seek(0)
    try:
        codec = extract_codec_from_bom(stream.readline())     # (:279-297) startswith over BOM_SORTED_TO_CODEC
        codec_definition_line = 1
    except ValueError as ve:
        raise e from ve                          # no BOM → re-raise original SyntaxError
return _normalize_codec_name(codec), codec_definition_line
```
`detect_encoding` semantics (CPython): default "utf-8"; checks BOM_UTF8; reads up to 2
lines for `coding[:=]\s*([-\w.]+)` comment; raises SyntaxError for unknown/conflicting
encodings. If the file starts with a UTF-16/32 BOM, the first "line" is binary junk —
detect_encoding usually succeeds returning utf-8 (no coding comment found) UNLESS the
junk forms an invalid coding declaration; the except-SyntaxError path covers files where
detect_encoding chokes (e.g. non-UTF-8 bytes in the first two lines... it does NOT decode
eagerly; it only raises if a coding comment names an unknown codec or BOM/comment
conflict). In practice: UTF-16/32-BOM files reach E2501 only via the SyntaxError fallback
OR via a `coding: utf-16` comment. (Conservative port: replicate exactly.)
`process_module` does not catch the re-raised SyntaxError → pylint surfaces it as a crash
guard upstream (astroid already failed to parse such files before checkers run; raw
checkers run from the same stream that parsed OK, so this path is largely theoretical).

## E2501 invalid-unicode-codec — "UTF-16 and UTF-32 aren't backward compatible. Use UTF-8 instead"
`_check_codec(codec, line)` (:460-475):
```
if codec != "utf-8":
    msg = "bad-file-encoding"                       # C2503 (out of scope for -E)
    if codec.startswith(("utf-16", "utf-32")):      # _is_invalid_codec (:375-377)
        msg = "invalid-unicode-codec"
    add_message(msg, line=codec_definition_line, end_lineno=codec_definition_line,
                confidence=HIGH, col_offset=None, end_col_offset=None)
```
No args; line = line of the codec declaration (1 for BOM).

## process_module (:518-533)
```
with node.stream() as stream:
    codec, codec_line = self._determine_codec(stream)
    self._check_codec(codec, codec_line)
    stream.seek(0)
    for lineno, line in enumerate(_fix_utf16_32_line_stream(stream, codec), start=1):
        if lineno == 1: line = _remove_bom(line, codec)
        self._check_bidi_chars(line, lineno, codec)
        self._check_invalid_chars(line, lineno, codec)
```
`_fix_utf16_32_line_stream` (:249-276): for non-utf-16/32 codecs, yields the raw stream's
lines (Python splits byte-streams on b"\n"). For utf-16/32, re-splits the whole content on
the ENCODED newline (e.g. b"\n\x00" for utf-16le), keeping the newline attached.
`_remove_bom` (:218-225): strips the codec's BOM from line 1 if codec in UNICODE_BOMS.

## E2510-E2515 — `_check_invalid_chars(line, lineno, codec)` (:477-490)
```
matches = self._find_line_matches(line, codec)
for col, char in matches.items():
    add_message(char.human_code(), line=lineno, end_lineno=lineno, confidence=HIGH,
                col_offset=col + 1, end_col_offset=col + len(char.unescaped) + 1)
```
NOTE col_offset is the 0-based column PLUS ONE (intentional; marks the char after?
bug-for-bug: +1).
`_find_line_matches` (:383-417):
```
try:
    line_search = line.decode(codec, errors="strict")        # decode to str for char-accurate cols
    return _map_positions_to_result(line_search, BAD_ASCII_SEARCH_DICT, "\n")
except UnicodeDecodeError:
    # fall back to byte search; cols approximated by byte//byte_str_length
    search_dict_byte = {encode-without-bom(char.unescaped, codec): char  for char in BAD_CHARS
                        if encodable (suppress UnicodeDecodeError)}
    return _map_positions_to_result(line, search_dict_byte,
                                    encoded "\n", byte_str_length=_byte_to_str_length(codec))
```
`_map_positions_to_result(line, search_dict, new_line, byte_str_length=1)` (:156-190):
```
result = {}
for search_for, char in search_dict.items():
    if search_for not in line: continue
    if char.unescaped == "\r" and line.endswith(new_line):
        ignore_pos = len(line) - 2 * byte_str_length         # the \r of a \r\n line ending
    else: ignore_pos = None
    start = 0; pos = line.find(search_for, start)
    while pos > 0:                                           # NOTE: pos > 0, NOT >= 0!
        if pos != ignore_pos:
            col = int(pos / byte_str_length)
            result[col] = char
        start = pos + 1; pos = line.find(search_for, start)
return result
```
BUGS preserved bug-for-bug: (1) a bad char at column 0 is NEVER reported (`while pos > 0`);
(2) Windows `\r\n` line endings exempt only the final `\r`; (3) dict keyed by col — two
different bad chars at the same col keep the latter.

## E2502 bidirectional-unicode — `_check_bidi_chars(line, lineno, codec)` (:492-516)
```
if not codec.startswith("utf"): return            # _is_unicode (:379-381)
for dangerous in BIDI_UNICODE:
    if _cached_encode_search(dangerous, codec) in line:     # encoded without BOM (:243-246,228-230)
        add_message("bidirectional-unicode", line=lineno, end_lineno=lineno,
                    col_offset=0, end_col_offset=_line_length(line, codec),
                    confidence=HIGH)
        break                                      # once per line max
```
No args. `_line_length` (:141-153): decode (BOM removed, errors="replace"), strip one
trailing "\n" then one trailing "\r", return char length.

---------------------------------------------------------------------------------------

# 10. checkers/match_statements_checker.py — MatchStatementChecker (E1901-E1904)

Checker name "match_statements"; msgs at match_statements_checker.py:41-79.
`MATCH_CLASS_SELF_NAMES` (:24-36) = {"builtins.bool","builtins.bytearray","builtins.bytes",
"builtins.dict","builtins.float","builtins.frozenset","builtins.int","builtins.list",
"builtins.set","builtins.str","builtins.tuple"}.

## E1901 bare-name-capture-pattern —
"The name capture `case %s` makes the remaining patterns unreachable. Use a dotted name
(for example an enum) to fix this."
visit_match (:102-122):
```
for idx, case in enumerate(node.cases):
    match case:
        case nodes.MatchCase(pattern=nodes.MatchAs(pattern=None,
                                                   name=nodes.AssignName(name=name)),
                             guard=None) if idx < len(node.cases) - 1:
            add_message("bare-name-capture-pattern", node=case.pattern, args=(name,),
                        confidence=HIGH)
```
Fires for every non-last `case x:` (bare capture, no guard). `case _:` — astroid
represents the wildcard as MatchAs with name=None → AssignName pattern fails → not flagged.
Reported at the MatchAs pattern node; args = captured name.

## E1902 invalid-match-args-definition — "`__match_args__` must be a tuple of strings." (no args)
visit_assignname (:81-100):
```
if (node.name == "__match_args__"
        and isinstance(node.frame(), nodes.ClassDef)        # class body assignment
        and isinstance(node.parent, nodes.Assign)           # plain assignment (not AnnAssign/For...)
        and not (isinstance(node.parent.value, nodes.Tuple)
                 and all(isinstance(el, nodes.Const) and isinstance(el.value, str)
                         for el in node.parent.value.elts))):
    add_message("invalid-match-args-definition", node=node.parent.value, args=(),
                confidence=HIGH)
```
Reported at the assigned VALUE node. args=() (template has no placeholders). Purely
syntactic: `__match_args__ = ["a"]` (list) → flagged; `= ("a", "b")` ok; `= ()` ok.

## get_match_args_for_class (:144-166) — shared by E1903/E1904
```
inferred = safe_infer(node)                      # node = MatchClass.cls expression
if not isinstance(inferred, nodes.ClassDef): return None        # BAIL
try: match_args = inferred.getattr("__match_args__")
except NotFoundError:
    return ["<self>"] if inferred.qname() in MATCH_CLASS_SELF_NAMES else None   # BAIL
match match_args:
    case [nodes.AssignName(parent=nodes.Assign(value=nodes.Tuple(elts=elts))), *_] \
            if all(isinstance(el, Const) and isinstance(el.value, str) for el in elts):
        return [el.value for el in elts]
    case _: return None                          # dataclass-synthesized or odd defs → BAIL
```
NOTE astroid's dataclass brain ADDS a synthesized `__match_args__` AssignName whose parent
Assign value is a Tuple of Consts → dataclasses DO resolve. getattr returns ancestors'
definitions too (first match wins).

## E1903 too-many-positional-sub-patterns — "%s expects %d positional sub-patterns (given %d)"
visit_matchclass (:183-226):
```
attrs = set(); dups = set()
if node.patterns and (match_args := get_match_args_for_class(node.cls)) is not None:
    if len(node.patterns) > len(match_args):
        add_message("too-many-positional-sub-patterns", node=node,
                    args=(node.cls.as_string(), len(match_args), len(node.patterns)),
                    confidence=INFERENCE)
        return
    ... R1906 match-class-positional-attributes (out of scope) ...
    for i in range(len(node.patterns)):
        name = match_args[i]
        self.check_duplicate_sub_patterns(name, node, attrs=attrs, dups=dups)   # E1904
for kw_name in node.kwd_attrs:
    self.check_duplicate_sub_patterns(kw_name, node, attrs=attrs, dups=dups)    # E1904
```
args = (source text of the class expr, allowed count, given count). At the MatchClass node.
Builtin self-matching classes report len(match_args)==1 ("<self>").

## E1904 multiple-class-sub-patterns — "Multiple sub-patterns for attribute %s"
`check_duplicate_sub_patterns` (:168-181):
```
if name in attrs and name not in dups:
    dups.add(name)
    add_message("multiple-class-sub-patterns", node=node, args=(name,), confidence=INFERENCE)
else: attrs.add(name)
```
Covers positional-vs-positional (same __match_args__ name twice — impossible), positional
name colliding with a keyword attr (`case Pt(1, x=2)` where __match_args__=("x","y")), and
keyword-vs-keyword duplicates — note pure `case C(x=1, x=2)` is a SyntaxError, so the real
trigger is positional+keyword overlap. One message per attr name (dups set). At the
MatchClass node; args = attribute name. If get_match_args_for_class returned None,
positional names aren't known → only kwd_attrs duplicates are checkable (none possible) →
silent: conservative.

---------------------------------------------------------------------------------------

# 11. checkers/dataclass_checker.py — DataclassChecker (E3701)

Checker name "dataclass"; the only message is E3701 (:46-52):
"Invalid usage of field(), %s". `DATACLASS_MODULES` (astroid/brain/brain_dataclasses.py:38-40)
= frozenset({"dataclasses", "marshmallow_dataclass", "pydantic.dataclasses"}).

visit_call (:54-56) → `_check_invalid_field_call` (:58-102):
```
if not isinstance(node.func, (nodes.Name, nodes.Attribute)): return
if not _check_name_or_attrname_eq_to(node.func, "field"): return
    # (:26-34) Name → str(node.name)=="field"; Attribute → str(node.attrname)=="field"
inferred_func = utils.safe_infer(node.func)
if not (isinstance(inferred_func, nodes.FunctionDef)
        and inferred_func.root().name in DATACLASS_MODULES): return     # BAIL: not dataclasses.field
scope_node = node.parent
while scope_node and not isinstance(scope_node, (nodes.ClassDef, nodes.Call)):
    scope_node = scope_node.parent
if isinstance(scope_node, nodes.Call):
    self._check_invalid_field_call_within_call(node, scope_node); return
if not (scope_node and scope_node.is_dataclass):
    add_message("invalid-field-call", node=node,
        args=("it should be used within a dataclass or the make_dataclass() function.",),
        confidence=INFERENCE)
    return
if not (isinstance(node.parent, nodes.AnnAssign) and node == node.parent.value):
    add_message("invalid-field-call", node=node,
        args=("it should be the value of an assignment within a dataclass.",),
        confidence=INFERENCE)
```
`scope_node.is_dataclass` is an astroid ClassDef attribute set by the dataclass brain
(True when decorated with a recognized @dataclass).
`_check_invalid_field_call_within_call` (:104-125): if the enclosing Call's func is a
Name/AssignName named "make_dataclass" AND safe_infer(scope_node.func) is a FunctionDef
rooted in a DATACLASS_MODULES module → ok; else emit the first message variant.
All at the field() Call node; args is the explanatory sentence (one of the two strings).
NOTE the ancestor walk stops at the NEAREST Call or ClassDef — `field()` nested in any call
inside a dataclass body (e.g. `x: int = foo(field())`) hits the Call branch → flagged
unless that call is make_dataclass.

---------------------------------------------------------------------------------------

# 12. checkers/modified_iterating_checker.py — ModifiedIterationChecker (E4702, E4703)

Checker name "modified_iteration"; msgs at modified_iterating_checker.py:30-50
(W4701 list variant is W-category → OUT of -E scope; E4702 dict, E4703 set are in).
`_LIST_MODIFIER_METHODS = {"append","remove"}`; `_SET_MODIFIER_METHODS =
{"add","clear","discard","pop","remove"}` (:18-19).

visit_for (:54-60): `iter_obj = node.iter`; for each direct body statement,
`_modified_iterating_check_on_node_and_children(body_node, iter_obj)` (:62-68) which
checks the node then recurses into ALL children (so nested statements are covered; the
`for` node's own iter/target are not).

`_modified_iterating_check(node, iter_obj)` (:70-98):
```
msg_id = None
if isinstance(node, nodes.Delete) and any(self._deleted_iteration_target_cond(t, iter_obj)
                                          for t in node.targets):
    match utils.safe_infer(iter_obj):
        case nodes.List(): msg_id = "modified-iterating-list"
        case nodes.Dict(): msg_id = "modified-iterating-dict"
        case nodes.Set():  msg_id = "modified-iterating-set"
elif not isinstance(iter_obj, (nodes.Name, nodes.Attribute)): pass    # BAIL for f(x) etc.
elif self._modified_iterating_list_cond(node, iter_obj): msg_id = "modified-iterating-list"
elif self._modified_iterating_dict_cond(node, iter_obj): msg_id = "modified-iterating-dict"
elif self._modified_iterating_set_cond(node, iter_obj):  msg_id = "modified-iterating-set"
if msg_id:
    add_message(msg_id, node=node, args=(iter_obj.repr_name(),), confidence=INFERENCE)
```
args = `iter_obj.repr_name()` (Name → name; Attribute → attrname). Reported at the
modifying statement node (Expr/Assign/Delete).

Delete branch — `_deleted_iteration_target_cond(node, iter_obj)` (:180-194): node must be a
DelName; `iter_obj.parent` must be the For with an AssignName/BaseContainer target; True if
the deleted name equals any name in `find_assigned_names_recursive(iter_obj.parent.target)`
(utils.py:2051-2061). I.e. `for x in lst: del x`?? — no: deleting the LOOP TARGET name.
Then msg chosen by the inferred type of iter_obj. (NB: `del x` where x is the loop variable
— this models `del` of dict keys via loop var only for the Delete-of-target form.)

E4703 set / list condition — `_is_node_expr_that_calls_attribute_name` (:100-105): node is
`Expr(value=Call(func=Attribute(expr=Name())))` (a statement-level `name.method(...)`).
`_modified_iterating_set_cond` (:167-178):
```
infer_val = safe_infer(node.value.func.expr)
if not isinstance(infer_val, nodes.Set): return False
return (infer_val == safe_infer(iter_obj)                 # same inferred Set node (astroid Node __eq__ is identity)
        and node.value.func.expr.name == iter_obj_name    # same source name (Name.name / Attribute.attrname)
        and node.value.func.attrname in _SET_MODIFIER_METHODS)
```
(`_common_cond_list_set` :107-120 implements the first two conjuncts.)

E4702 dict condition — `_modified_iterating_dict_cond` (:142-165):
```
node must be Assign(targets=[Subscript(value=Name()), *_])         # d[k] = v   (:122-127)
# exemption: writing the SAME key currently being iterated:
if (isinstance(iter_obj, nodes.Name)
        and iter_obj.name == node.targets[0].value.name
        and isinstance(iter_obj.parent.target, nodes.AssignName)
        and isinstance(node.targets[0].slice, nodes.Name)
        and iter_obj.parent.target.name == node.targets[0].slice.name):
    return False                                                    # for k in d: d[k] = ...
infer_val = safe_infer(node.targets[0].value)
if not isinstance(infer_val, nodes.Dict): return False
if infer_val != safe_infer(iter_obj): return False
iter_obj_name = iter_obj.attrname if Attribute else iter_obj.name
return node.targets[0].value.name == iter_obj_name
```
NOTE only subscript-ASSIGNMENT is detected for dicts (adding keys); `d.pop(k)` etc. are NOT
(no dict-method condition exists). `del d[k]` is a Delete of a DelAttr/Subscript → the
Delete branch requires DelName targets → not flagged. Bug-for-bug.
NOTE the exemption requires iter_obj.parent to be the For node — `for k in d:` where
iter_obj is the Name `d`. When iterating `d.keys()` iter_obj is a Call → top-level bail.
Messages:
- E4702: "Iterated dict '%s' is being modified inside for loop body, iterate through a copy of it instead."
- E4703: "Iterated set '%s' is being modified inside for loop body, iterate through a copy of it instead."

---------------------------------------------------------------------------------------

# 13. checkers/stdlib.py — StdlibChecker (E1507, E1519, E1520)

Checker name "stdlib". Constants: `OS_ENVIRON = "os._Environ"` (:32),
`ENV_GETTERS = ("os.getenv",)` (:33).

## Dispatch — visit_call (:690-721)
```
for inferred in utils.infer_all(node.func):
    if isinstance(inferred, util.UninferableBase): continue
    ...
    elif isinstance(inferred, nodes.FunctionDef):
        name = inferred.qname()
        ...
        elif name in ENV_GETTERS:                 # "os.getenv" ONLY (not os.environ.get)
            self._check_env_function(node, inferred)
        ...
```

## E1507 invalid-envvar-value — "%s does not support %s type argument"
`_check_env_function(node, infer)` (:922-959):
```
env_name_kwarg = "key"; env_value_kwarg = "default"
kwargs = {kw.arg: kw.value for kw in node.keywords} if node.keywords else None
env_name_arg = node.args[0] if node.args else (kwargs["key"] if kwargs and "key" in kwargs else None)
if env_name_arg:
    self._check_invalid_envvar_value(node=node, message="invalid-envvar-value",
        call_arg=utils.safe_infer(env_name_arg), infer=infer, allow_none=False)
env_value_arg = node.args[1] if len(node.args) == 2 else (kwargs["default"] if ... else None)
if env_value_arg:
    self._check_invalid_envvar_value(node=node, message="invalid-envvar-default",   # W1508 (out)
        call_arg=utils.safe_infer(env_value_arg), infer=infer, allow_none=True)
```
`_check_invalid_envvar_value` (:961-985):
```
if call_arg is None or isinstance(call_arg, UninferableBase): return     # BAIL
name = infer.qname()                       # "os.getenv"
if isinstance(call_arg, nodes.Const):
    emit = False
    match call_arg.value:
        case None: emit = not allow_none   # os.getenv(None) → E1507
        case str(): pass                   # ok
        case _: emit = True                # int, bytes, bool(≠str)...
    if emit: add_message(message, node=node, args=(name, call_arg.pytype()))
else:
    add_message(message, node=node, args=(name, call_arg.pytype()))      # any non-Const inferred:
    # Dict/List/Tuple/FunctionDef/ClassDef/Instance... ALWAYS flagged (bug-for-bug —
    # an inferred Instance of str via f-string Const IS Const; JoinedStr infers to Const str
    # when static; an uninferable name returns None above)
```
args = ("os.getenv", pytype of the inferred arg e.g. "builtins.int"); at the Call node.
NOTE: `match str():` arm matches bool? No — bool matches `case _` (bool is not str).
A Const bytes → pytype "builtins.bytes" → flagged.

## E1519 singledispatch-method / E1520 singledispatchmethod-function
Messages (:538-551), no args:
- E1519: "singledispatch decorator should not be used with methods, use singledispatchmethod instead."
- E1520: "singledispatchmethod decorator should not be used with functions, use singledispatch instead."
visit_functiondef (:746-750): `if node.decorators:` → if parent is ClassDef →
`_check_lru_cache_decorators` (W, out); ALWAYS → `_check_dispatch_decorators(node)`.
`_check_dispatch_decorators` (:794-820):
```
decorators_map = {}
for decorator in node.decorators.nodes:
    if isinstance(decorator, nodes.Name) and decorator.name:
        decorators_map[decorator.name] = (decorator, HIGH)
        # plain @singledispatch / @singledispatchmethod by NAME — no inference!
    elif utils.is_registered_in_singledispatch_function(node):        # §0.8 — note: checks the FUNCTION,
        decorators_map["singledispatch"] = (decorator, INFERENCE)     # not this decorator specifically
    elif utils.is_registered_in_singledispatchmethod_function(node):  # §0.10
        decorators_map["singledispatchmethod"] = (decorator, INFERENCE)
if node.is_method():
    if "singledispatch" in decorators_map:
        add_message("singledispatch-method", node=decorators_map["singledispatch"][0],
                    confidence=decorators_map["singledispatch"][1])           # E1519
elif "singledispatchmethod" in decorators_map:
    add_message("singledispatchmethod-function", node=decorators_map["singledispatchmethod"][0],
                confidence=decorators_map["singledispatchmethod"][1])         # E1520
```
Reported at the offending DECORATOR node. So: a method decorated `@singledispatch` (by
name, HIGH) or registered via `@f.register` where f is singledispatch-decorated (INFERENCE)
→ E1519. A non-method registered into a singledispatchmethod → E1520. NB the Name branch
keys by ARBITRARY decorator name — a method decorated with any function named
"singledispatch" triggers E1519 with HIGH confidence regardless of origin (bug-for-bug).

---------------------------------------------------------------------------------------

# 14. checkers/method_args.py — MethodArgsChecker (E3102)

Checker name "method_args"; msgs at method_args.py:30-45. E3102:
"`%s()` got some positional-only arguments passed as keyword arguments: %s".

visit_call (:68-73) → `_check_positional_only_arguments_expected(node)` (:101-125):
```
inferred_func = utils.safe_infer(node.func)
while isinstance(inferred_func, (astroid.BoundMethod, astroid.UnboundMethod)):
    inferred_func = inferred_func._proxied                     # unwrap to FunctionDef
if not (isinstance(inferred_func, nodes.FunctionDef) and inferred_func.args.posonlyargs):
    return                                                     # BAIL: uninferable or no posonly
if inferred_func.args.kwarg: return                            # **kwargs absorbs keywords → BAIL
pos_args = [a.name for a in inferred_func.args.posonlyargs]
kws = [k.arg for k in node.keywords if k.arg in pos_args]
if not kws: return
add_message("positional-only-arguments-expected", node=node,
            args=(node.func.as_string(), ", ".join(f"'{k}'" for k in kws)),
            confidence=INFERENCE)
```
args = (call target source text, comma-joined quoted keyword names in call order).
At the Call node. NOTE no special-casing of `self` — for a bound method call
`obj.m(x=1)` where `def m(self, x, /)` → "self" isn't passed as kw, `x` is → flagged.

---------------------------------------------------------------------------------------

# 15. checkers/variables.py — E0643 potential-index-error

Message (variables.py:489-494): "Invalid index for iterable length" /
"potential-index-error", no args.

visit_subscript (:3458-3461): `inferred_slice = utils.safe_infer(node.slice)` then
`_check_potential_index_error(node, inferred_slice)` (:3476-3496):
```
if not (isinstance(inferred_slice, nodes.Const) and isinstance(inferred_slice.value, int)):
    return                                          # only literal-int (or inferable-int) indexes
if isinstance(node.value, (nodes.Tuple, nodes.List)):       # subscript ON A LITERAL only
    if self._inferred_iterable_length(node.value) < inferred_slice.value + 1:
        add_message("potential-index-error", node=node, confidence=INFERENCE)
    return
```
`_inferred_iterable_length(iterable)` (:3463-3474): count elts; for a Starred elt, if
safe_infer(elt.value) is a BaseContainer add len(its elts) else add 1.
Reported at the Subscript node. NOTE: negative indexes: `(1,2)[-3]` → -3+1 = -2 < 2 →
condition `2 < -2` False → NOT flagged (negative indexes never flagged). True positives
only for `("a","b")[2]`-style direct literal subscripts. `True` as index: isinstance(True,
int) → length < 2 check (bool-as-int, bug-for-bug). visit_subscript has no
only_required_for_messages guard (it's part of VariablesChecker's unconditioned visits).

---------------------------------------------------------------------------------------

# 16. checkers/imports.py — E0402 relative-beyond-top-level

Message (imports.py:234-239): "Attempted relative import beyond top-level package", no args.

Emission — `_get_imported_module(importnode, modname)` (:1023-1031):
```
try:
    return importnode.do_import_module(modname)
except astroid.TooManyLevelsError:
    if _ignore_import_failure(importnode, modname, self._ignored_modules): return None
    self.add_message("relative-beyond-top-level", node=importnode)
```
Called from visit_import for each imported name (:541) and from visit_importfrom with
`basename = node.modname` (:555-556). Reported at the Import/ImportFrom node.
NOTE for `import a.b` (no level) TooManyLevelsError can't occur; the real trigger is
ImportFrom with `level > 0`.

The actual depth computation — astroid `Module.relative_to_absolute_name(modname, level)`
(astroid/nodes/scoped_nodes/scoped_nodes.py:477-523), invoked via `do_import_module`
(astroid/nodes/_base_nodes.py:148-172) → `Module.import_module` (scoped_nodes.py:460-475):
```
if self.absolute_import_activated() and level is None: return modname
if level:
    if self.package:                       # module is a package __init__
        level = level - 1
        package_name = self.name.rsplit(".", level)[0]
        # NB rsplit(...,0) returns [whole string] → package_name = self.name for level 1
    elif (self.path and not os.path.exists(dirname(self.path[0]) + "/__init__.py")
          and os.path.exists(dirname(self.path[0]) + "/" + modname.split(".")[0])):
        level = level - 1
        package_name = ""                  # script next to the target package
    else:
        package_name = self.name.rsplit(".", level)[0]
    if level and self.name.count(".") < level:
        raise TooManyLevelsError(level=level, name=self.name)   # ← E0402 trigger
elif self.package: package_name = self.name
else: package_name = self.name.rsplit(".", 1)[0]
if package_name:
    return f"{package_name}.{modname}" if modname else package_name
return modname
```
So the EXACT E0402 condition: a relative import with level L, where (after decrementing L
by 1 if the importing module is a package `__init__`, or via the script-sibling branch)
the ADJUSTED level is still nonzero and `self.name.count(".") < adjusted_level`. The
module's dotted name (as computed by pylint's module discovery relative to sys.path roots)
is the depth source: a top-level module `foo` (0 dots) doing `from .. import x` (level 2 →
adjusted 2, or 1 if package) → 0 < 2 → E0402.
NOTE `self.name.rsplit(".", level)[0]` for non-package modules uses the UN-decremented
level — `from . import x` in module `pkg.mod` → package_name "pkg", level stays 1, check
`1 and 1 < 1` False → fine.

Suppression — `_ignore_import_failure(node, modname, ignored_modules)` (:140-155):
```
if is_module_ignored(modname, ignored_modules): return True
    # utils.py:2190-2202 — modname or any dotted prefix equal to OR fnmatch-ing an entry
    # of config.ignored-modules (default: empty tuple)
if in_type_checking_block(node): return True            # utils.py:1990-2017 (TYPE_CHECKING If)
if isinstance(node.parent, nodes.If) and is_sys_guard(node.parent): return True
    # utils.py:1845-1865 — sys.version_info compare / six.PY2/PY3 test
return node_ignores_exception(node, ImportError)        # utils.py:1148-1159 —
    # enclosing try/except ImportError (handler catches it) or contextlib.suppress(ImportError)
```
For E0402 the message is NOT gated on is_message_enabled("import-error") (that gate exists
only in the AstroidBuildingError branch for E0401). Also note `TooManyLevelsError` is
raised before any filesystem import attempt, so --ignored-modules matching is against the
RELATIVE modname (e.g. "x" for `from ...x import y`).

---------------------------------------------------------------------------------------

# 17. Registration / plumbing notes

- All these checkers register via module-level `register(linter)` functions; in -E mode
  message filtering happens via `only_required_for_messages` decorators (visit method is
  skipped entirely if none of the listed messages are enabled) — EXCEPT
  StringFormatChecker.visit_call (strings.py:419) and VariablesChecker.visit_subscript
  (variables.py:3458) and ClassChecker.visit_functiondef/visit_attribute (no decorator —
  always run, they gate internally).
- E0102/E0107/etc. of BasicErrorChecker: note visit_functiondef's decorator
  (basic_error_checker.py:324-332) does NOT list "nonexistent-operator" etc.; each visit's
  decorator list is authoritative for whether the visit runs in -E with disables applied.
- F0202 is F-category ("fatal") — enabled under -E.
- E0106 (maxversion 3.3), and the `not self._py38_plus` arm of E0111 are dead on the
  pinned 3.12 runtime.
- Messages W0120/W0136/W0137/W1201-W1310/C0205/R0205... appearing in the same visit
  methods are OUT of scope (disabled in -E) but their control flow (e.g. `continue` after
  C0205 single-string-used-for-slots in _check_slots) still shapes in-scope behavior —
  mirror the control flow, not the emission.

# Open questions / port risks

1. exceptions.py `_builtin_exceptions()` introspects the RUNNING interpreter's builtins
   (exceptions.py:28-33). The Rust port needs the frozen CPython 3.12.12 builtin-exception
   name list (66 names incl. ExceptionGroup, BaseExceptionGroup, EncodingWarning, etc.).
2. strings.py E1310 pattern matches BoundMethod via `astroid.BoundMethod(bound=Instance(
   name=...))` — requires astroid-faithful bound-method inference for str literals.
3. unicode.py depends on CPython `tokenize.detect_encoding` semantics (incl. SyntaxError
   cases for bad coding declarations); replicate PEP 263 exactly, including the
   "lines empty when UTF-8-BOM" → codec_definition_line = 1 rule.
4. `is_message_enabled` interactions: `_helper_string` (logging.py:271-286) queries a
   nonexistent message id "logging-fstring-formatting" — only affects W-message args (out
   of scope) but shows pylint tolerates unknown ids there.
5. ClassChecker mixin exemptions hinge on the DEFAULT value of `ignored-checks-for-mixins`
   (defined in checkers/typecheck.py:877-888: ["no-member", "not-async-context-manager",
   "not-context-manager", "attribute-defined-outside-init"]) and `mixin-class-rgx`
   (typecheck.py:856-862, default ".*[Mm]ixin") — affects E0203 (via the
   attribute-defined-outside-init guard in class_checker.py:1192-1202, which skips
   `_check_accessed_members` too for matching class names) and E1701.
6. E0202's ALLOWED_PROPERTIES contains the typo "bultins.property" (class_checker.py:52);
   keep it: a decorator literally named `property` (bare Name) is NOT exempted by that arm
   (it's exempted later via the safe_infer + __get__/__set__ data-descriptor path, since
   builtins.property has both).
7. `_map_positions_to_result` `while pos > 0` skips column-0 bad chars — intentional to
   replicate (unicode.py:182).
8. astroid behaviors load-bearing here: `Module.locals` ordering, `ClassDef.instance_attrs`
   population, `ilookup("__slots__")`, `ClassDef.slots()`, `mro()` C3 + Inconsistent/
   DuplicateBases errors, `infer_call_result`, enum/dataclass brains (`__members__`,
   `is_dataclass`, synthesized `__match_args__`).
