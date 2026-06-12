# 09 — checkers/base/: remaining W/C/R messages (exact spec, pylint 4.0.5 / astroid 4.0.4)

Scope: every message owned by the seven checker classes in
`pylint/checkers/base/` that is NOT already specified for `-E` mode in
`reference/notes/08-other-checkers.md` (which covers basic_error_checker's
E-messages E0100-E0118, and basic_checker's E0111/E0119). Covered here:

- `basic_checker.py` (BasicChecker): W0101 W0102 W0104 W0105 W0106 W0108
  W0109 W0122 W0123 W0124 W0125 W0126 W0127 W0128 W0129 W0130 W0131 W0133
  W0134 W0150 W0199
- `basic_error_checker.py` (BasicErrorChecker) non-E leftovers: W0120 W0136
  W0137
- `pass_checker.py` (PassChecker): W0107
- `comparison_checker.py` (ComparisonChecker): C0121 C0123 R0123 R0124 R0133
  W0143 W0177
- `name_checker/checker.py` (NameChecker): C0103 C0104 C0105 C0131 C0132
- `docstring_checker.py` (DocStringChecker): C0112 C0114 C0115 C0116
- `function_checker.py` (FunctionChecker): W0135

All file:line cites refer to `reference/pylint/pylint/...` at tag v4.0.5 and
`reference/astroid/astroid/...` at v4.0.4. Code quoted verbatim where load
bearing. Empirical claims below marked [verified] were run against the pinned
`.venv-pylint` (pylint 4.0.5 / astroid 4.0.4 / CPython 3.12.12,
PYTHONHASHSEED=0).

Conventions (same as notes/09-variables-imports-classes-wc.md):

- Confidence objects (HIGH/INFERENCE/INFERENCE_FAILURE) affect nothing in
  default output (`--confidence` default empty → no filtering), but matter
  for NameChecker's `_is_multi_naming_match` (§5.8) and must be carried for
  byte-identical `--confidence` runs.
- Default-enabled: EVERY message in this document is enabled by default in
  full-pylint mode — none of the msgs dicts in these seven files carry
  `"default_enabled": False`. Per-message option dicts that DO exist:
  - C0104 `old_names: [("C0102", "blacklisted-name")]` (name_checker/checker.py:179-184)
  - C0112 `old_names: [("W0132", "old-empty-docstring")]` (docstring_checker.py:50)
  - C0114/C0115/C0116 `old_names: [("C0111", "missing-docstring")]`
    (docstring_checker.py:57,64,72)
  - C0123 `old_names: [("W0154", "old-unidiomatic-typecheck")]` (comparison_checker.py:48)
  - E0106 `maxversion: (3, 3)` (basic_error_checker.py:202) → `may_be_emitted`
    False on py3.12; msgs.rs has it with `enabled: false`; the E0106 emission
    code in visit_functiondef is dead on our runtime (`returns` generator is
    consumed only in the `__init__` branch anyway).
  - E0118 `minversion: (3, 6)` → may_be_emitted True on 3.12 (E-mode, see 08).
  The `enabled` flag in `crates/pycheckers/src/msgs.rs` is the `-E` flag set
  only; all messages here have `enabled: false` there. Cross-checked: every
  msgid/symbol/template in this doc is present verbatim in msgs.rs (lines
  15-26, 76, 80, 87, 210-212, 271-298).
- Report position: via `PyLinter._add_one_message` (pylint/lint/pylinter.py:1195-1233):
  if `node.position` is not None (only FunctionDef/ClassDef get keyword-anchored
  `position`), `line = line or node.position.lineno`, col/end likewise; else
  `line = line or node.fromlineno`, `col_offset = col_offset or node.col_offset`,
  `end_lineno = end_lineno or node.end_lineno`, `end_col_offset = ... or
  node.end_col_offset`. Note the `or` semantics: an explicit `line=` argument
  wins for the line but col/end still come from the node. Final tuple uses
  `line or 1` and `col_offset or 0` (pylinter.py:1280-1284) — Module messages
  (fromlineno 0) print as line 1 col 0. [verified: C0114 at `1:0`]
- `%`-interpolation: `msg = template % args` (pylinter.py:1252-1254). Several
  checks pass a bare string as args (works for single-`%s` templates).
- Emission order = output order. TextReporter writes each message as
  `add_message` fires; there is NO sorting by line. Several checks here emit
  out of line order (see §8).

================================================================================
# 0. Checker classes, registration, walker dispatch
================================================================================

`pylint/checkers/base/__init__.py:43-50`:

```python
def register(linter: PyLinter) -> None:
    linter.register_checker(BasicErrorChecker(linter))
    linter.register_checker(BasicChecker(linter))
    linter.register_checker(NameChecker(linter))
    linter.register_checker(DocStringChecker(linter))
    linter.register_checker(PassChecker(linter))
    linter.register_checker(ComparisonChecker(linter))
    linter.register_checker(FunctionChecker(linter))
```

ALL seven inherit `_BasicChecker` with `name = "basic"`
(basic_checker.py:27-32). They are therefore all subject to the same-name
prepared-checker ordering quirk (notes/02); the empirical callback order per
node type is in the harness order dump — within one checker, callbacks are
collected in `dir(checker)` (alphabetical) order (ast_walker.py:48).

Walker gating (pylint/utils/ast_walker.py:37-40): a `visit_*`/`leave_*`
method decorated with `@utils.only_required_for_messages(...)` (which sets
`func.checks_msgs`, checkers/utils.py:479-500) is registered only if ANY
listed message `is_message_enabled` at config level. Methods WITHOUT the
decorator are ALWAYS registered. Undecorated methods in this file set:

- `BasicChecker.visit_module`, `visit_classdef` (stats only),
  `visit_try`/`leave_try` (maintains `_trys` stack AND emits W0134),
- `NameChecker.leave_module` (multi-naming-group flush),
- `DocStringChecker.open` etc. (open/close are not walker callbacks).

`visit_asyncfunctiondef = visit_functiondef` aliases exist in BasicChecker
(:590), NameChecker (:406), DocStringChecker (:148); FunctionChecker defines
a separate decorated `visit_asyncfunctiondef` (:33-35). The walker dispatches
on `astroid.__class__.__name__.lower()` (ast_walker.py:77) so
AsyncFunctionDef nodes ONLY fire `*_asyncfunctiondef` events (AsyncFunctionDef
subclasses FunctionDef, scoped_nodes.py:1695, but dispatch is by exact class
name).

Statement counting: `ASTWalker.walk` increments `self.nbstatements` for every
node with `is_statement` (ast_walker.py:84-85); this becomes
`stats.statement` and is the denominator of the score formula. The
`linter.stats.node_count` / `undocumented` / `bad_names` counters incremented
by BasicChecker/DocStringChecker/NameChecker feed ONLY the optional reports
(RP0101, off by default with `reports=no`) — they never affect message output
or the score.

Exception behavior: any exception in a callback propagates out of the walk
after printing a traceback to stderr (ast_walker.py:93-102 `raise`); pylint
then turns it into F0002 astroid-error at module level. Relevant because one
check (W0177 `_is_float_nan`, §4.7) calls `node.inferred()` catching only
AttributeError.

`open()` hooks (called once per run, before any module):
- `BasicChecker.open` (basic_checker.py:276-281): `py_version =
  self.linter.config.py_version` (default `sys.version_info[:2]` =
  `(3, 12)`, pylint/lint/base_options.py:356-364) → `self._py38_plus = py_version
  >= (3, 8)` → True on our runtime (kills the pre-3.8 arm of E0111);
  `self._trys = []`; `stats.reset_node_count()`.
- `NameChecker.open` (name_checker/checker.py:296-310): builds name-group map,
  naming-rule regexes/hints, compiles good/bad-names-rgxs (§5.3).
- `DocStringChecker.open` (docstring_checker.py:101-102): resets undocumented
  stats.

================================================================================
# 1. BasicChecker (basic_checker.py:101-962)
================================================================================

Message table (basic_checker.py:115-268), templates verbatim:

| id | symbol | template |
|----|--------|----------|
| W0101 | unreachable | `Unreachable code` |
| W0102 | dangerous-default-value | `Dangerous default value %s as argument` |
| W0104 | pointless-statement | `Statement seems to have no effect` |
| W0105 | pointless-string-statement | `String statement has no effect` |
| W0106 | expression-not-assigned | `Expression "%s" is assigned to nothing` |
| W0108 | unnecessary-lambda | `Lambda may not be necessary` |
| W0109 | duplicate-key | `Duplicate key %r in dictionary` |
| W0122 | exec-used | `Use of exec` |
| W0123 | eval-used | `Use of eval` |
| W0124 | confusing-with-statement | `Following "as" with another context manager looks like a tuple.` |
| W0125 | using-constant-test | `Using a conditional statement with a constant value` |
| W0126 | missing-parentheses-for-call-in-test | `Using a conditional statement with potentially wrong function or method call due to missing parentheses` |
| W0127 | self-assigning-variable | `Assigning the same variable %r to itself` |
| W0128 | redeclared-assigned-name | `Redeclared variable %r in assignment` |
| W0129 | assert-on-string-literal | `Assert statement has a string literal as its first argument. The assert will %s fail.` |
| W0130 | duplicate-value | `Duplicate value %r in set` |
| W0131 | named-expr-without-context | `Named expression used without context` |
| W0133 | pointless-exception-statement | `Exception statement has no effect` |
| W0134 | return-in-finally | `'return' shadowed by the 'finally' clause.` |
| W0150 | lost-exception | `%s statement in finally block may swallow exception` |
| W0199 | assert-on-tuple | `Assert called on a populated tuple. Did you mean 'assert x,y'?` |
| E0111 | bad-reversed-sequence | (E-mode, spec in notes/08 §2) |
| E0119 | misplaced-format-function | (E-mode, spec in notes/08 §2) |

Reports: `reports = (("RP0101", "Statistics by type", report_by_type_stats),)`
(:270) — only with `--reports=y`, out of scope for message output.

--------------------------------------------------------------------------------
## 1.1 W0101 unreachable — `Unreachable code` (no args)
--------------------------------------------------------------------------------

Trigger sites:
- `visit_return` (:632-643) → `_check_unreachable(node)` (HIGH)
- `visit_continue` (:645-650) → `_check_unreachable(node)` (HIGH)
- `visit_break` (:652-664) → `_check_unreachable(node)` (HIGH)
- `visit_raise` (:666-671) → `_check_unreachable(node)` (HIGH)
- `visit_call` (:696-699): `if utils.is_terminating_func(node):
  self._check_unreachable(node, confidence=INFERENCE)`

`_check_unreachable` (:767-787) pseudocode:

```
unreachable_statement = node.next_sibling()        # Statement sibling via
                                                   # parent.child_sequence (astroid
                                                   # _base_nodes.py:57-68); for a Call
                                                   # node (not a statement) NodeNG.next_sibling
                                                   # delegates to parent (node_ng.py:380-386)
                                                   # → the Expr stmt's next sibling.
if unreachable_statement is None: return
if isinstance(node, Return) and isinstance(unreachable_statement, Expr) \
        and isinstance(unreachable_statement.value, Yield):
    # empty-generator idiom: `return` followed by bare `yield` — skip the yield
    unreachable_statement = unreachable_statement.next_sibling()
    if unreachable_statement is None: return       # [verified: return/yield at fn end → no W0101]
add_message("unreachable", node=unreachable_statement, confidence=confidence)
```

- Report node = the unreachable statement itself (NOT the return/raise).
- The Yield-skip applies only when the trigger is a `Return` node (not Raise,
  not terminating call) and the next sibling is exactly `Expr(Yield)`
  (YieldFrom is a different class → not skipped).
- `is_terminating_func` (checkers/utils.py:2211-2253), verbatim conditions:
  - `node.func` must be Attribute or Name; `node.parent` must not be a Lambda.
  - iterate `node.func.infer()` (full inference, InferenceError/StopIteration
    caught → False):
    - any inferred with `qname()` in `TERMINATING_FUNCS_QNAMES = frozenset({
      "_sitebuiltins.Quitter", "sys.exit", "posix._exit", "nt._exit",
      "unittest.case.TestCase.fail"})` (utils.py:240-248) → True.
      (`exit`/`quit` builtins infer to `_sitebuiltins.Quitter` instances —
      hasattr(inferred,"qname") via Instance proxy.)
    - else unwrap `BoundMethod(_proxied=UnboundMethod(_proxied=p)) → p`; if
      inferred is a FunctionDef (and, when AsyncFunctionDef, only if
      `node.parent` is Await) whose `returns` is a **Name** node (Attribute
      annotations like `t.NoReturn` do NOT count) and `safe_infer(returns)`
      has qname in TYPING_NEVER ∪ TYPING_NORETURN → True.
- Iteration note: one message per trigger node; a function ending
  `sys.exit(1)` followed by 2 statements yields ONE W0101 (on the first
  following statement).

--------------------------------------------------------------------------------
## 1.2 W0102 dangerous-default-value — `Dangerous default value %s as argument`
--------------------------------------------------------------------------------

`visit_functiondef`/`visit_asyncfunctiondef` (:579-590) — decorated
`@only_required_for_messages("dangerous-default-value")`; also increments
`stats.node_count["method"|"function"]` keyed on `node.is_method()`
(astroid scoped_nodes.py:1435-1446: `self.type != "function" and parent
frame is ClassDef`).

`_check_dangerous_default` (:592-630):

```
DEFAULT_ARGUMENT_SYMBOLS = {                       # basic_checker.py:40-57
  "builtins.set": "set()", "builtins.dict": "{}", "builtins.list": "[]",
  "collections.deque": "collections.deque()",
  "collections.ChainMap": "collections.ChainMap()",
  "collections.Counter": "collections.Counter()",
  "collections.OrderedDict": "collections.OrderedDict()",
  "collections.defaultdict": "collections.defaultdict()",
  "collections.UserDict": "collections.UserDict()",
  "collections.UserList": "collections.UserList()",
}
is_iterable(n) = isinstance(n, (nodes.List, nodes.Set, nodes.Dict))   # NOT Tuple

defaults = (node.args.defaults or []) + (node.args.kw_defaults or [])
for default in defaults:                      # kw_defaults has None for
    if not default: continue                  # non-defaulted kwonly args
    value = next(default.infer())             # FIRST result only; on
                                              # InferenceError → continue
                                              # (NOT safe_infer)
    if isinstance(value, astroid.Instance) and value.qname() in DEFAULT_ARGUMENT_SYMBOLS:
        if value is default:                  # literal [] / {} / set-display
            msg = DEFAULT_ARGUMENT_SYMBOLS[value.qname()]
        elif isinstance(value, astroid.Instance) or is_iterable(value):  # always True here
            if is_iterable(default):          # default is a display node but inferred
                msg = value.pytype()          #   to a different node (e.g. [*a])
            elif isinstance(default, nodes.Call):
                msg = f"{value.name}() ({value.qname()})"
            else:                             # Name/Attribute/BinOp/... default
                msg = f"{default.as_string()} ({value.qname()})"
        else:                                 # dead code (guarded by outer isinstance)
            msg = f"{default.as_string()} ({DEFAULT_ARGUMENT_SYMBOLS[value.qname()]})"
        add_message("dangerous-default-value", node=node, args=(msg,))
```

Key astroid facts: `nodes.List/Set/Dict/Const` inherit `bases.Instance`
(BaseContainer/Dict/Const class defs in astroid node_classes.py), and
`Instance.qname()` proxies to `_proxied.qname()` — so a literal `[]` IS an
`astroid.Instance` with qname `builtins.list`. Tuples are immune (no
`builtins.tuple` key).

[verified] message args:
- `def f(x=[])` → `Dangerous default value [] as argument`
- `def f(x=dict())` → `Dangerous default value dict() (builtins.dict) as argument`
- `def f(x=collections.deque())` → `Dangerous default value deque() (collections.deque) as argument`
  (`value.name` = class name, unqualified)
- `Y = []; def f(x=Y)` → `Dangerous default value Y (builtins.list) as argument`
- `def f(x={'a': 1}, *, y=[1])` → two messages, positional defaults first,
  then kw_defaults, all reported on the FunctionDef node (keyword-anchored
  `position` → line of `def`, col of `def`).

One message per offending default; no dedup.

--------------------------------------------------------------------------------
## 1.3 W0104/W0105/W0106/W0131/W0133 — visit_expr (:422-494)
--------------------------------------------------------------------------------

Decorator: `@only_required_for_messages("pointless-statement",
"pointless-exception-statement", "pointless-string-statement",
"expression-not-assigned", "named-expr-without-context")` — i.e. one shared
callback; enabling ANY of the five runs all five checks (each message still
individually gated at add_message time).

Exact flow for `visit_expr(node: nodes.Expr)`, `expr = node.value`:

1. **String statement** (:432-452): if `isinstance(expr, nodes.Const) and
   isinstance(expr.value, str)`:
   - PEP-257 attribute-docstring exemption: `scope = expr.scope()`; if scope
     is ClassDef, Module, or FunctionDef **named `__init__`** (any other
     FunctionDef falls through to emit):
     ```
     sibling = expr.previous_sibling()      # NodeNG.previous_sibling →
                                            # parent(Expr).previous_sibling()
                                            # → previous statement in block
     if sibling is not None and sibling.scope() is scope and \
        isinstance(sibling, (nodes.Assign, nodes.AnnAssign, nodes.TypeAlias)):
         return                             # exempt
     ```
   - else `add_message("pointless-string-statement", node=node)` (node =
     the Expr statement; default confidence = UNDEFINED). `return`.
   - [verified] `X = 5` followed by `"""attr doc."""` at module level → no
     message. Note f-strings are JoinedStr, not Const → fall through to
     step 3/4.

2. **Exception statement W0133** (:455-470): if `isinstance(expr, nodes.Call)`:
   ```
   name = ""
   if isinstance(expr.func, nodes.Name): name = expr.func.name
   elif isinstance(expr.func, nodes.Attribute): name = expr.func.attrname
   # perf heuristic (issue #8073): infer only Uppercase-initial names
   inferred = utils.safe_infer(expr) if name[:1].isupper() else None
   if isinstance(inferred, objects.ExceptionInstance):
       add_message("pointless-exception-statement", node=node, confidence=INFERENCE)
   return            # ALL bare calls return here — never pointless-statement
   ```
   `objects.ExceptionInstance` is the astroid wrapper produced when a class
   inheriting BaseException is called. Lambdas/Subscript funcs → name "" →
   no inference → return.

3. **Skip list** (:478-486): return silently when
   - `isinstance(expr, (nodes.Yield, nodes.Await))`, or
   - `isinstance(node.parent, (nodes.Try, nodes.TryStar)) and
     node.parent.body == [node]` (the Expr is the UNIQUE statement of a
     try body), or
   - `isinstance(expr, nodes.Const) and expr.value is Ellipsis` (`...`
     statement).

4. **NamedExpr W0131** (:487-488): `isinstance(expr, nodes.NamedExpr)` →
   `add_message("named-expr-without-context", node=node, confidence=HIGH)`.
   (Reached only for a top-level parenthesized `(x := 5)` statement —
   NamedExpr used inside if/while/comprehension is not an Expr statement.)

5. **W0106 vs W0104** (:489-494):
   ```
   elif any(expr.nodes_of_class(nodes.Call)):     # ANY Call anywhere in subtree
       add_message("expression-not-assigned", node=node, args=expr.as_string())
   else:
       add_message("pointless-statement", node=node)
   ```
   W0106 args = `expr.as_string()` (astroid round-trip string of the value
   expression, not the whole statement). Note: a bare Call was already
   consumed by step 2, so W0106 fires for e.g. `x == f()`, `foo.bar() != 2`,
   `[f()]`; W0104 for `1 + 1`, `x == 5`, `foo.bar`, lists/dicts without
   calls. [verified: `x == 5` and `foo.bar` → W0104.]

--------------------------------------------------------------------------------
## 1.4 W0108 unnecessary-lambda — `Lambda may not be necessary` (no args)
--------------------------------------------------------------------------------

`visit_lambda` (:522-577), decorated for "unnecessary-lambda". Bail-outs in
order; ALL must pass:

1. `node.args.defaults` non-empty → return (can't compare defaults).
2. `node.body` not a `nodes.Call` → return.
3. `match call.func: case nodes.Attribute(expr=nodes.Call()): return`
   (chained call like `lambda x: foo().method(x)`).
4. kwarg/keywords correspondence:
   ```
   if node.args.kwarg:                      # lambda has **kw
       if _has_variadic_argument(call.keywords, node.args.kwarg): return
   elif call.keywords: return               # call uses keywords, lambda has no **kw
   ```
   `_has_variadic_argument(args, variadic_name)` (:512-520) verbatim:
   ```python
   return not args or any(
       (isinstance(a.value, nodes.Name) and a.value.name != variadic_name)
       or not isinstance(a.value, nodes.Name)
       for a in args
   )
   ```
   i.e. with `**kw` present, the call must have ≥1 keyword and EVERY
   keyword's value must be `Name(kw)` — note this (buggily) accepts
   named keywords `f(a=kw)` too, since it only inspects `.value`.
5. vararg/starargs correspondence (same shape):
   ```
   if node.args.vararg:
       if _has_variadic_argument(call.starargs, node.args.vararg): return
   elif call.starargs: return
   ```
   `Call.starargs` = `[a for a in self.args if isinstance(a, Starred)]`
   (astroid node_classes.py:1727-1730).
6. Ordinary-args correspondence:
   ```
   ordinary_args = list(node.args.args)
   new_call_args = list(self._filter_vararg(node, call.args))
   # _filter_vararg (:496-510) verbatim: a Starred arg is yielded ONLY if
   # its value is a Name DIFFERENT from node.args.vararg; Starred with
   # value == Name(vararg) OR with a non-Name value (`*[1]`, `*f()`) is
   # DROPPED. (Unreachable nuance: step 5 already bailed unless every
   # Starred in call.args is Name(vararg) with the lambda having *vararg,
   # so in practice _filter_vararg only ever drops Name(vararg) stars.)
   # Non-Starred args are always yielded.
   if len(ordinary_args) != len(new_call_args): return
   for arg, passed_arg in zip(ordinary_args, new_call_args):
       if not isinstance(passed_arg, nodes.Name): return
       if arg.name != passed_arg.name: return
   ```
   Positional-only/keyword-only lambda params are not in `args.args`; a
   lambda can't have them syntactically except posonly (`lambda x, /: ...`)
   — posonlyargs would NOT be counted, causing length mismatch → return.
7. Func-uses-param check (:573-575):
   ```
   for name in call.func.nodes_of_class(nodes.Name):
       if name.lookup(name.name)[0] is node: return
   # e.g. lambda foo: (func1 if foo else func2)(foo)
   ```
   `lookup()[0]` is the scope where the name resolves; if the call's FUNC
   expression references a lambda parameter, bail.

Emit: `add_message("unnecessary-lambda", line=node.fromlineno, node=node)`
(:577). Lambda has no `position` → col_offset = lambda's col; explicit line
is redundant with fromlineno. [verified: `g = lambda: f()` → W0108 at the
lambda's position, col 4.]

--------------------------------------------------------------------------------
## 1.5 W0109 duplicate-key — `Duplicate key %r in dictionary`
--------------------------------------------------------------------------------

`visit_dict` (:724-738), decorated:

```
keys = set()
for k, _ in node.items:
    match k:
        case nodes.Const():     key = k.value
        case nodes.Attribute(): key = k.as_string()
        case _: continue
    if key in keys:
        add_message("duplicate-key", node=node, args=key)
    keys.add(key)
```

- Python set semantics apply: `True`/`1`/`1.0` collide ({True:1, 1:2} →
  `Duplicate key 1 in dictionary`, %r of the SECOND occurrence's value).
  [verified] Must replicate CPython cross-type numeric/bool equality+hash.
- `%r` formatting = CPython repr: strings quoted (`'a'`), floats `1.0`,
  bools `True`, bytes `b'x'`.
- Attribute keys compared by `as_string()` (e.g. `a.b` twice → duplicate;
  args is then the string `"a.b"` → rendered `'a.b'` by %r).
- Const vs Attribute can't collide (string `"a.b"` == as_string `"a.b"`
  CAN collide — both are str keys in the same set; replicate).
- Report node = the Dict node; one message per duplicate occurrence (a key
  appearing 3× → 2 messages).
- `**spread` items: astroid represents dict-unpacking keys as... the key
  node for `**d` is a `DictUnpack` placeholder — not Const/Attribute →
  skipped by `case _`.

--------------------------------------------------------------------------------
## 1.6 W0130 duplicate-value — `Duplicate value %r in set`
--------------------------------------------------------------------------------

`visit_set` (:740-753), decorated. Same shape as W0109 but Consts only:

```
values = set()
for v in node.elts:
    if isinstance(v, nodes.Const): value = v.value
    else: continue
    if value in values:
        add_message("duplicate-value", node=node, args=value, confidence=HIGH)
    values.add(value)
```

[verified] `{1, 1.0}` → `Duplicate value 1.0 in set` (arg is the SECOND
occurrence's value, so the repr shows `1.0` not `1`). Confidence HIGH.

--------------------------------------------------------------------------------
## 1.7 W0122 exec-used / W0123 eval-used — visit_call (:696-712)
--------------------------------------------------------------------------------

`visit_call` decorated for ("eval-used", "exec-used",
"bad-reversed-sequence", "misplaced-format-function", "unreachable"). After
the unreachable and E0119 checks:

```
if isinstance(node.func, nodes.Name):
    name = node.func.name
    if not (name in node.frame() or name in node.root()):
        # i.e. name NOT shadowed in the enclosing frame's locals nor module
        # globals — `in` is LocalsDictNodeNG.__contains__ (locals dict)
        match name:
            case "exec":     add_message("exec-used", node=node)
            case "reversed": self._check_reversed(node)   # E0111, notes/08
            case "eval":     add_message("eval-used", node=node)
```

- No args, default confidence. Report node = the Call.
- Shadowing test: `def exec(...)` in the same frame or module-level
  `exec = something` suppresses. NOTE `name in node.frame()` — frame of the
  call site (function frame if inside a function); intermediate closure
  scopes are not consulted.
- `eval.__call__()` or `builtins.eval()` (Attribute func) are NOT caught.

--------------------------------------------------------------------------------
## 1.8 W0124 confusing-with-statement
--------------------------------------------------------------------------------

`visit_with` (:872-886), decorated:

```
pairs = node.items                      # list[(context_expr, optional_vars)]
if pairs:
    for prev_pair, pair in itertools.pairwise(pairs):
        if isinstance(prev_pair[1], nodes.AssignName) and (
            pair[1] is None and not isinstance(pair[0], nodes.Call)
        ):
            add_message("confusing-with-statement", node=node)
```

i.e. for each adjacent pair of with-items: previous item has a NAME binding
(`as x` — Tuple/List/Attribute bindings don't count) AND current item has NO
binding AND current context manager expression is not a Call. Example:
`with ctx() as a, b:` → emitted. `with ctx() as a, open(f):` → not.
One message per matching adjacent pair, all on the With node. The docstring
mentions a line-number check, but NO line check exists in the code.

--------------------------------------------------------------------------------
## 1.9 W0125 using-constant-test / W0126 missing-parentheses-for-call-in-test
--------------------------------------------------------------------------------

Entry points (all three decorated with both messages):
- `visit_if(node)` → `_check_using_constant_test(node, node.test)` (:286-287)
- `visit_ifexp(node)` → same (:292-293)
- `visit_comprehension(node)` → for each `if_test` in `node.ifs`:
  `_check_using_constant_test(node, if_test)` (:298-301)

`_check_using_constant_test(node, test)` (:303-380):

```
const_nodes = (Module, GeneratorExp, Lambda, FunctionDef, ClassDef,
               bases.Generator, astroid.UnboundMethod, astroid.BoundMethod,
               Module)                      # Module listed twice — verbatim
structs = (Dict, Tuple, Set, List)
except_nodes = (Call, BinOp, BoolOp, UnaryOp, Subscript)

inferred = None
emit = isinstance(test, (Const, *structs, *const_nodes))
maybe_generator_call = None
if not isinstance(test, except_nodes):
    inferred = utils.safe_infer(test)
    if isinstance(inferred, util.UninferableBase) and isinstance(test, nodes.Name):
        emit, maybe_generator_call = _name_holds_generator(test)
elif isinstance(test, nodes.Call):
    maybe_generator_call = test

if maybe_generator_call:
    inferred_call = safe_infer(maybe_generator_call.func)
    if isinstance(inferred_call, nodes.FunctionDef):
        # all returns must be GeneratorExp; empty return-set → None → no emit
        all_returns_were_generator = None
        for return_node in inferred_call._get_return_nodes_skip_functions():
            if not isinstance(return_node.value, nodes.GeneratorExp):
                all_returns_were_generator = False; break
            all_returns_were_generator = True
        if all_returns_were_generator:
            add_message("using-constant-test", node=node, confidence=INFERENCE)
            return                         # NOTE: node = the If/IfExp/Comprehension
if emit:
    add_message("using-constant-test", node=test, confidence=INFERENCE)
elif isinstance(inferred, const_nodes):
    call_inferred = None
    try:
        if isinstance(inferred, (nodes.FunctionDef, nodes.Lambda)):
            call_inferred = list(inferred.infer_call_result(node))
    except astroid.InferenceError:
        call_inferred = None
    if call_inferred:
        add_message("missing-parentheses-for-call-in-test", node=test,
                    confidence=INFERENCE)
    add_message("using-constant-test", node=test, confidence=INFERENCE)
```

Notes:
- Syntactic emit: test is literally a Const, container display, lambda, or
  (impossible syntactically) FunctionDef etc. Report node = `test`.
- `except_nodes` (Call/BinOp/BoolOp/UnaryOp/Subscript) suppress the
  inference path entirely — EXCEPT a Call goes down the
  generator-call route.
- Inferred emit: `safe_infer(test)` returns FunctionDef/Lambda/ClassDef/
  Module/Generator/(Un)BoundMethod → W0126 first (only if the
  FunctionDef/Lambda's `infer_call_result(node)` yields ≥1 result without
  InferenceError), then W0125, both on `test`.
- W0126 can NEVER fire without W0125 directly after it (same node/line).
- `FunctionDef.infer_call_result` no-return semantics (astroid
  scoped_nodes.py:1555-1635), decisive for W0126 [all verified]:
  - `is_generator()` → yields a `bases.Generator` → W0126 fires
    (`if gen_func:` → W0126+W0125).
  - no Return nodes (`_get_return_nodes_skip_functions` empty) and
    `self.body` non-empty (astroid 4 strips the docstring into `doc_node`,
    so "body" = real statements): `is_abstract(pass_is_abstract=True,
    any_raise_is_abstract=True)` → yields Uninferable, else yields
    `Const(None)` — either way ≥1 result → W0126 fires.
  - no Return nodes and EMPTY body (docstring-only function) → raises
    `InferenceError("The function does not have any return statements")`
    (scoped_nodes.py:1627) → caught → W0125 only.
  - special case [verified on pinned venv]: a FunctionDef literally named
    `with_metaclass` with exactly 1 positional arg + *vararg, tested bare
    (`if with_metaclass:`) takes the metaclass-hack path
    (scoped_nodes.py:1573-1614); `caller` here is the If/IfExp/Comprehension
    node, which has no `.args` → `isinstance(caller.args, Arguments)`
    (scoped_nodes.py:1584) raises AttributeError → NOT caught by the
    `except astroid.InferenceError` in _check_using_constant_test →
    propagates out of the walk → traceback on stderr + F0002 astroid-error
    for the module at 1:0. Messages emitted BEFORE the crash in walk order
    still print; the rest of the module's walk is aborted. Probe:
    `def with_metaclass(meta, *bases): return 1` + `if with_metaclass:` →
    C0114, C0116, 2×W0613, then F0002. Replicate.
  - `Lambda.infer_call_result` (scoped_nodes.py:987-993) = body inference;
    InferenceError there → W0125 only.
- `_name_holds_generator(test)` (:382-410):
  ```
  lookup_result = test.frame().lookup(test.name)
  if not lookup_result: return (False, None)      # always truthy in practice
  maybe_generator_assigned = (isinstance(an.parent.value, GeneratorExp)
      for an in lookup_result[1] if isinstance(an.parent, nodes.Assign))
  first_item = next(maybe_generator_assigned, None)
  if first_item is not None:
      if all(chain((first_item,), maybe_generator_assigned)): emit = True
      elif (len(lookup_result[1]) == 1
            and isinstance(lookup_result[1][0].parent, nodes.Assign)
            and isinstance(lookup_result[1][0].parent.value, nodes.Call)):
          maybe_generator_call = lookup_result[1][0].parent.value
  ```
  Only reached when `safe_infer(test)` returned Uninferable and test is a
  Name. `frame().lookup` = astroid scope lookup (notes/07 §7).
  Caution: the elif arm requires the single assignment to be BOTH
  `parent.value` GeneratorExp-free AND a Call — i.e. `x = f()`, then `if x:`
  where f only `return (... for ...)` → W0125 on the If node.
- Comprehension entry: message node for the generator-call arm is the
  Comprehension node (per-`if` checks but message points at the
  comprehension clause); for the other arms it is the if-test expression.

--------------------------------------------------------------------------------
## 1.10 W0127 self-assigning-variable — `Assigning the same variable %r to itself`
--------------------------------------------------------------------------------

`visit_assign` (:953-958) decorated for ("self-assigning-variable",
"redeclared-assigned-name"); calls `_check_self_assigning_variable(node)`
then `_check_redeclared_assign_name(node.targets)`.

`_check_self_assigning_variable` (:888-928):

```
scope = node.scope(); scope_locals = scope.locals
rhs_names = []
targets = node.targets
if isinstance(targets[0], nodes.Tuple):
    if len(targets) != 1: return            # a, b = c, d = ... → bail
    targets = targets[0].elts
    if len(targets) == 1: return            # (x,) = x unpacking → bail
match node.value:
    case nodes.Name():
        if len(targets) != 1: return
        rhs_names = [node.value]
    case nodes.Tuple():
        rhs_count = len(node.value.elts)
        if len(targets) != rhs_count or rhs_count == 1: return
        rhs_names = node.value.elts
    # any other RHS → rhs_names stays [] → zip() empty → nothing
for target, lhs_name in zip(targets, rhs_names):
    if not isinstance(lhs_name, nodes.Name): continue
    if not isinstance(target, nodes.AssignName): continue
    if isinstance(scope, nodes.ClassDef) and target.name in scope_locals:
        continue        # class-level X = X pattern (expose module attr) exempt
    if target.name == lhs_name.name:
        add_message("self-assigning-variable", args=(target.name,), node=target)
```

- Report node = the AssignName target (not the Assign).
- Hits: `x = x`; `x, y = x, y` (two messages); chain `x = x = 1` is `targets
  = [x, x]`, value Const → no rhs_names → nothing (that's W0128's job? no —
  see below, W0128 only inspects Tuple targets; `x = x = 1` emits NOTHING).
- ClassDef exemption quirk: `target.name in scope_locals` is always True for
  the target itself (it IS a local of the class)… so ALL class-level
  `X = X` are exempt.

--------------------------------------------------------------------------------
## 1.11 W0128 redeclared-assigned-name — `Redeclared variable %r in assignment`
--------------------------------------------------------------------------------

Entry: `visit_assign` (targets list) and `visit_for` (`[node.target]`,
:960-962, decorated for "redeclared-assigned-name" only).

`_check_redeclared_assign_name(targets)` (:930-951):

```
dummy_variables_rgx = self.linter.config.dummy_variables_rgx
   # variables-checker option, default regex (variables.py:1250-1257):
   #   _+$|(_[a-zA-Z0-9_]*[a-zA-Z0-9]+?$)|dummy|^ignored_|^unused_
for target in targets:
    if not isinstance(target, nodes.Tuple): continue   # ONLY tuple targets
    found_names = []
    for element in target.elts:
        if isinstance(element, nodes.Tuple):
            self._check_redeclared_assign_name([element])     # recurse
        elif isinstance(element, nodes.AssignName) and element.name != "_":
            if dummy_variables_rgx and dummy_variables_rgx.match(element.name):
                return            # NOTE: returns from the WHOLE function,
                                  # aborting remaining elements AND targets
            found_names.append(element.name)
    names = collections.Counter(found_names)
    for name, count in names.most_common():   # count desc, insertion order ties
        if count > 1:
            add_message("redeclared-assigned-name", args=(name,), node=target)
```

- Triggers: `v, v = 1, 2` / `for v, v in ...`. List targets (`[a, a] = ...`)
  do NOT trigger (Tuple check only). Starred elements ignored.
- `_` always skipped; any name matching dummy rgx ABORTS the whole check
  (conservatism bail-out — note `.match` = prefix anchor, so `dummyfoo`
  matches via the `dummy` alternative).
- Report node = the Tuple target; one message per name with count>1.

--------------------------------------------------------------------------------
## 1.12 W0129 assert-on-string-literal / W0199 assert-on-tuple
--------------------------------------------------------------------------------

`visit_assert` (:714-722), decorated for both:

```
match node.test:
    case nodes.Tuple(elts=elts) if len(elts) > 0:
        add_message("assert-on-tuple", node=node, confidence=HIGH)
    case nodes.Const(value=str() as val):
        when = "never" if val else "always"
        add_message("assert-on-string-literal", node=node, args=(when,))
```

- W0199: only a NON-EMPTY tuple display as the test (`assert (x, y)`); the
  empty tuple `assert ()` emits nothing. With a message
  (`assert (x, y), "msg"`) — test is still the Tuple → fires.
- W0129: only a plain str Const (bytes don't match `str()` pattern;
  f-strings are JoinedStr → no). args `("never",)` for truthy strings,
  `("always",)` for `""`. [verified both renderings.]
- Report node = the Assert statement.

--------------------------------------------------------------------------------
## 1.13 W0134 return-in-finally + W0150 lost-exception
--------------------------------------------------------------------------------

`visit_try` (:755-761) — UNDECORATED (always runs):

```
self._trys.append(node)
for final_node in node.finalbody:
    for return_node in final_node.nodes_of_class(nodes.Return):
        add_message("return-in-finally", node=return_node, confidence=HIGH)
leave_try: self._trys.pop()
```

- W0134: every Return ANYWHERE inside the finalbody subtree — `nodes_of_class`
  has no skip_klass, so a `return` inside a nested `def` in the finally
  block IS flagged (bug; replicate).
- TryStar (`except*`) has class name `trystar` → `visit_try` not dispatched →
  no W0134, no `_trys` push for TryStar. (astroid keeps a separate TryStar
  class, node_classes.py:3916.)
- Emission order quirk: W0134 fires when the WALKER REACHES the Try node, so
  it precedes any message produced by statements inside the try body even if
  those have smaller line numbers than the return. Within the same return
  statement, W0134 precedes W0150. [verified: same line, W0134 then W0150.]

W0150 — `visit_return` (:632-643) and `visit_break` (:652-664) call
`_check_not_in_finally(node, "return", (nodes.FunctionDef,))` /
`(node, "break", (nodes.For, nodes.While))` after `_check_unreachable`.
(`visit_return` decorated ("unreachable", "lost-exception");
`visit_break` same; `visit_continue`/`visit_raise` only "unreachable".)

`_check_not_in_finally` (:789-812):

```
if not self._trys: return            # fast path: not inside any Try at all
_parent = node.parent; _node = node
while _parent and not isinstance(_parent, breaker_classes):
    if hasattr(_parent, "finalbody") and _node in _parent.finalbody:
        add_message("lost-exception", node=node, args=node_name)
        return
    _node = _parent; _parent = _node.parent
```

- args: `"return"` or `"break"` → `return statement in finally block may
  swallow exception`.
- breaker classes: for `return` the walk stops at the enclosing FunctionDef
  (a return in a finally of an OUTER function isn't possible anyway); for
  `break`, stops at For/While — so `for: try/finally: while: break` is NOT
  flagged (break belongs to inner while; walk hits While first).
- `hasattr(_parent, "finalbody")` matches Try AND TryStar — but the
  `self._trys` gate only counts Try nodes, so a break/return inside a
  TryStar-finally with no enclosing plain Try emits nothing (replicate).
- Report node = the Return/Break statement.

--------------------------------------------------------------------------------
## 1.14 Stats bookkeeping in BasicChecker
--------------------------------------------------------------------------------

- `visit_module` (:412-414): `stats.node_count["module"] += 1`.
- `visit_classdef` (:416-420): `stats.node_count["klass"] += 1`.
- `visit_functiondef`: `node_count["method"|"function"] += 1`.
These three callbacks are registered unconditionally (visit_module/
visit_classdef undecorated; visit_functiondef decorated but counts happen
whenever the callback runs, i.e. when dangerous-default-value enabled).
None of this affects message output or score.

================================================================================
# 2. BasicErrorChecker non-E leftovers (basic_error_checker.py)
================================================================================

E0100-E0118 specified in notes/08 §1. Non-E messages owned here:

| id | symbol | template |
|----|--------|----------|
| W0120 | useless-else-on-loop | `Else clause on loop without a break statement, remove the else and de-indent all the code inside it` |
| W0136 | continue-in-finally | `'continue' discouraged inside 'finally' clause` |
| W0137 | break-in-finally | `'break' discouraged inside 'finally' clause` |

--------------------------------------------------------------------------------
## 2.1 W0120 useless-else-on-loop
--------------------------------------------------------------------------------

`visit_for` (:444-446) / `visit_while` (:448-450), both decorated
`@only_required_for_messages("useless-else-on-loop")` →
`_check_else_on_loop(node)` (:541-551):

```
if node.orelse and not _loop_exits_early(node):
    add_message("useless-else-on-loop", node=node,
                line=node.orelse[0].lineno - 1)
```

Position: explicit `line` = first-else-statement's lineno minus 1 (the
comment admits this is approximate — it equals the `else:` line only when
`else:` is directly above); col_offset/end_* come from the LOOP node
(For/While have no `.position`) → col = loop's col_offset, end_lineno = end
of the whole loop. [verified: `for...else` body at line 20 → message
`19:0`.]

`_loop_exits_early(loop)` (:47-67):

```
loop_nodes = (For, While); definition_nodes = (FunctionDef, ClassDef)
inner_loop_nodes = [n for n in loop.nodes_of_class(loop_nodes,
                    skip_klass=definition_nodes) if n != loop]
return any(n for n in loop.nodes_of_class(Break, skip_klass=definition_nodes)
           if _get_break_loop_node(n) not in inner_loop_nodes)
```

`_get_break_loop_node(break_node)` (:26-44): walk parents upward while the
parent is not a For/While OR the current node is in `parent.orelse`; returns
the loop owning the break (or None past the root). So: a break belonging to
the inspected loop itself (or to anything that is not one of its INNER
loops) counts as "exits early". Breaks inside nested function/class defs are
excluded by skip_klass. A break in an inner loop does NOT save the outer
loop's else.

--------------------------------------------------------------------------------
## 2.2 W0136 continue-in-finally / W0137 break-in-finally
--------------------------------------------------------------------------------

Emitted from `_check_in_loop(node, name)` (:553-577) which is primarily the
E0103 not-in-loop check (notes/08 §1). Walk `node.node_ancestors()` from the
nearest parent outward:

```
for parent in node.node_ancestors():
    if isinstance(parent, (For, While)):
        if node not in parent.orelse: return         # found owning loop, done
    if isinstance(parent, (ClassDef, FunctionDef)): break
    if isinstance(parent, nodes.Try) and node in parent.finalbody \
            and isinstance(node, nodes.Continue):
        add_message("continue-in-finally", node=node)
    if isinstance(parent, nodes.Try) and node in parent.finalbody \
            and isinstance(node, nodes.Break):
        add_message("break-in-finally", node=node)
self.add_message("not-in-loop", node=node, args=node_name)   # loop never found
```

Subtleties:
- `node in parent.finalbody` is a DIRECT-member test: only fires when the
  continue/break statement is an immediate child of the finalbody block
  (e.g. `finally: if x: continue` does NOT emit W0136 — the If is the
  member, not the Continue; the ancestor scan tests `node in finalbody`
  against the original node only).
- Order: ancestors scanned inner→outer; a `while: try: finally: break`
  emits W0137 first (Try parent reached) then finds the While → returns, so
  W0137 emits WITHOUT not-in-loop. If no loop exists, BOTH W0136/W0137 and
  E0103 emit.
- TryStar not matched (isinstance Try only) → no W0136/W0137 in `except*`
  finallys.
- visit_continue decorated ("not-in-loop", "continue-in-finally");
  visit_break ("not-in-loop", "break-in-finally") (:436-442).
- No args; node = the Continue/Break.
- Note: since Python 3.8 `continue` in finally is legal syntax; 3.14 makes it
  a SyntaxWarning — pylint emits the lint regardless.

================================================================================
# 3. PassChecker — W0107 unnecessary-pass (pass_checker.py:11-29)
================================================================================

Template: `Unnecessary pass statement`, no args.

```python
@utils.only_required_for_messages("unnecessary-pass")
def visit_pass(self, node: nodes.Pass) -> None:
    if len(node.parent.child_sequence(node)) > 1 or (
        isinstance(node.parent, (nodes.ClassDef, nodes.FunctionDef))
        and node.parent.doc_node
    ):
        self.add_message("unnecessary-pass", node=node)
```

- `child_sequence` (astroid node_ng.py:325-349): returns the body/orelse/
  finalbody/handlers list that contains the pass. So: pass is unnecessary
  iff its block has >1 statements, OR its DIRECT parent is a ClassDef/
  FunctionDef that has a docstring (`doc_node` is not part of `body` in
  astroid 4, so `def f(): """d"""; pass` has body == [Pass] but doc_node
  set → emitted). [verified]
- AsyncFunctionDef subclasses FunctionDef → isinstance passes.
- A pass directly inside If/For/While/Try blocks with siblings → emitted;
  a lone pass in an except handler → not.
- Report node = the Pass statement.

================================================================================
# 4. ComparisonChecker (comparison_checker.py:25-352)
================================================================================

Message table (:34-82):

| id | symbol | template |
|----|--------|----------|
| C0121 | singleton-comparison | `Comparison %s should be %s` |
| C0123 | unidiomatic-typecheck | `Use isinstance() rather than type() for a typecheck.` |
| R0123 | literal-comparison | `In '%s', use '%s' when comparing constant literals not '%s' ('%s')` |
| R0124 | comparison-with-itself | `Redundant comparison - %s` |
| R0133 | comparison-of-constants | `Comparison between constants: "%s %s %s" has a constant value` |
| W0143 | comparison-with-callable | `Comparing against a callable, did you omit the parenthesis?` |
| W0177 | nan-comparison | `Comparison %s should be %s` |

Single entry point `visit_compare` (:287-319), decorated with ALL seven
symbols:

```
self._check_callable_comparison(node)      # W0143
self._check_logical_tautology(node)        # R0124
self._check_unidiomatic_typecheck(node)    # C0123
self._check_constants_comparison(node)     # R0133
if len(node.ops) != 1: return              # chained comparisons stop here
left = node.left; operator, right = node.ops[0]
if operator in {"==", "!="}:
    self._check_singleton_comparison(left, right, node,
                                     checking_for_absence=operator == "!=")
if operator in {"==", "!=", "is", "is not"}:
    self._check_nan_comparison(left, right, node,
                               checking_for_absence=operator in {"!=", "is not"})
if operator in {"is", "is not"}:
    self._check_literal_comparison(right, node)
```

Emission order within one Compare node: W0143 → R0124 → C0123 → R0133 →
C0121 → W0177 → R0123. The first four run even for chained comparisons but
inspect only `node.ops[0]`. All report node = the Compare node.

Module constants (:14-17):
```
LITERAL_NODE_TYPES = (Const, Dict, List, Set)
COMPARISON_OPERATORS = frozenset(("==", "!=", "<", ">", "<=", ">="))
TYPECHECK_COMPARISON_OPERATORS = frozenset(("is", "is not", "==", "!="))
TYPE_QNAME = "builtins.type"
```

--------------------------------------------------------------------------------
## 4.1 W0143 comparison-with-callable (no args)
--------------------------------------------------------------------------------

`_check_callable_comparison` (:264-285):

```
operator = node.ops[0][0]
if operator not in COMPARISON_OPERATORS: return      # is/is not/in excluded
bare_callables = (nodes.FunctionDef, astroid.BoundMethod)
left, right = node.left, node.ops[0][1]
count = 0
for operand in (left, right):
    inferred = utils.safe_infer(operand)
    if (isinstance(inferred, bare_callables)
            and "typing._SpecialForm" not in inferred.decoratornames()
            and not any(isinstance(x, nodes.Raise) for x in inferred.body)):
        count += 1
if count == 1: add_message("comparison-with-callable", node=node)
```

- Exactly ONE side infers to a bare FunctionDef/BoundMethod (both sides
  callable → 0 messages, deliberate).
- Exemptions: functions decorated `typing._SpecialForm` (typing constants),
  and functions whose TOP-LEVEL body contains a Raise statement (
  `inferred.body` direct children only).
- `decoratornames()` can raise? It catches InferenceError internally and
  yields qnames; safe.

--------------------------------------------------------------------------------
## 4.2 R0124 comparison-with-itself — `Redundant comparison - %s`
--------------------------------------------------------------------------------

`_check_logical_tautology` (:219-244):

```
left_operand = node.left; right_operand = node.ops[0][1]
operator = node.ops[0][0]
if isinstance(left_operand, Const) and isinstance(right_operand, Const):
    left_operand = left_operand.value; right_operand = right_operand.value
elif isinstance(left_operand, Name) and isinstance(right_operand, Name):
    left_operand = left_operand.name; right_operand = right_operand.name
if left_operand == right_operand:
    suggestion = f"{left_operand} {operator} {right_operand}"
    add_message("comparison-with-itself", node=node, args=(suggestion,))
```

- Mixed Const/Name pairs compare NODE OBJECTS with `==` (NodeNG identity) →
  never equal → no message.
- Const values use PYTHON equality: `1 == 1.0` → emitted; the suggestion
  f-string uses `str(value)` so `'a' == 'a'` renders `Redundant comparison -
  a == a` (NO quotes) and `1 == 1.0` renders `1 == 1.0` (original reprs
  preserved per-side). [verified both]
- ANY operator (including `in`, `is`) qualifies — `x in x` → emitted.
- Fires alongside R0133 for Const pairs (R0124 first).

--------------------------------------------------------------------------------
## 4.3 C0123 unidiomatic-typecheck (no args)
--------------------------------------------------------------------------------

`_check_unidiomatic_typecheck` (:321-329):

```
operator, right = node.ops[0]
if operator in TYPECHECK_COMPARISON_OPERATORS:        # is/is not/==/!=
    left = node.left
    if _is_one_arg_pos_call(left):
        self._check_type_x_is_y(node, left, right)
    elif isinstance(left, nodes.Name) and _is_one_arg_pos_call(right):
        self._check_type_x_is_y(node, left=right, right=left)   # Y == type(x) swap
```

`_is_one_arg_pos_call(call)` (:20-22): `isinstance(call, Call) and
len(call.args) == 1 and not call.keywords`.

`_check_type_x_is_y` (:331-352):

```
left_func = utils.safe_infer(left.func)
if not (isinstance(left_func, ClassDef) and left_func.qname() == "builtins.type"):
    return
if _is_one_arg_pos_call(right):
    right_func = safe_infer(right.func)
    if isinstance(right_func, ClassDef) and right_func.qname() == "builtins.type":
        # type(x) == type(a)
        right_arg = safe_infer(right.args[0])
        if not isinstance(right_arg, LITERAL_NODE_TYPES):   # Const/Dict/List/Set
            return     # type(x) == type(y) with non-literal arg → exempt
add_message("unidiomatic-typecheck", node=node)
```

- `type(x) == Y` for any RHS → message; `type(x) == type([])` → message;
  `type(x) == type(y)` → exempt. The swapped form requires LHS to be a bare
  Name (`y == type(x)` fires, `a.b == type(x)` does not).
- Note `type(x, b, d) == Y` (3-arg type) fails one-arg test → exempt.

--------------------------------------------------------------------------------
## 4.4 R0133 comparison-of-constants — `Comparison between constants: "%s %s %s" has a constant value`
--------------------------------------------------------------------------------

`_check_constants_comparison` (:246-262): `node.left` is Const AND
`node.ops[0][1]` is Const → args `(left.as_string(), operator,
right.as_string())`, confidence HIGH. `as_string()` of a Const = repr-like
round trip (`'a'`, `1`, `True`). Fires for any operator. [verified:
`'a' == 'a'` → `Comparison between constants: "'a' == 'a'" has a constant
value`]. Chained comparisons: only first pair inspected, fires even when
`len(ops) > 1`.

--------------------------------------------------------------------------------
## 4.5 C0121 singleton-comparison — `Comparison %s should be %s`
--------------------------------------------------------------------------------

Only `==`/`!=`; `checking_for_absence = (operator == "!=")`.
`_check_singleton_comparison` (:84-137):

```
if utils.is_singleton_const(left):  singleton, other = left.value, right
elif utils.is_singleton_const(right): singleton, other = right.value, left
else: return
# is_singleton_const (utils.py:2205-2208): Const whose value IS one of
# SINGLETON_VALUES = {True, False, None} (identity, so 1/0/'' don't match)

singleton_comparison_example = {False: "'{} is {}'", True: "'{} is not {}'"}
if singleton in {True, False}:
    suggestion_template = "{} if checking for the singleton value {}, or {} if testing for {}"
    truthiness_example = {False: "not {}", True: "{}"}
    truthiness_phrase = {True: "truthiness", False: "falsiness"}
    checking_truthiness = singleton is not checking_for_absence
    suggestion = suggestion_template.format(
        singleton_comparison_example[checking_for_absence].format(
            left.as_string(), right.as_string()),
        singleton,
        ("'bool({})'" if not utils.is_test_condition(root_node) and checking_truthiness
         else "'{}'").format(
            truthiness_example[checking_truthiness].format(other.as_string())),
        truthiness_phrase[checking_truthiness])
else:   # None
    suggestion = singleton_comparison_example[checking_for_absence].format(
        left.as_string(), right.as_string())
add_message("singleton-comparison", node=root_node,
            args=(f"'{root_node.as_string()}'", suggestion))
```

`utils.is_test_condition(node)` (utils.py:1708-1718): parent is
While/If/IfExp/Assert and node is (or is inside) `parent.test`; or parent is
Comprehension and node in `parent.ifs`; or parent is a `bool(...)` call
containing node.

[verified] renderings:
- `if x == True:` → `Comparison 'x == True' should be 'x is True' if
  checking for the singleton value True, or 'x' if testing for truthiness`
- `if x != False:` → `... should be 'x is not False' if checking for the
  singleton value False, or 'x' if testing for truthiness`
- `x == True` NOT in a test condition → third slot becomes `'bool(x)'`.
- `x == None` → `Comparison 'x == None' should be 'x is None'`.
- `x != True` → checking_truthiness = False → `... or 'not x' if testing
  for falsiness` (and bool() never applies when checking falsiness).
- Both sides singleton (`True == False`): LEFT branch wins (left is
  singleton → other = right).

--------------------------------------------------------------------------------
## 4.6 R0123 literal-comparison
--------------------------------------------------------------------------------

Only for `is`/`is not`, and only the RIGHT operand is examined
(`_check_literal_comparison(right, node)`, :183-217):

```
match literal:
    case Const(value=bool() | None): return     # True/False/None fine with is
    case Const(value=bytes() | str() | int() | float()): pass
    case List() | Dict() | Set(): pass          # display literals
    case _: return                              # Tuple displays NOT flagged
incorrect_node_str = node.as_string()
if "is not" in incorrect_node_str:
    equal_or_not_equal = "!="; is_or_is_not = "is not"
else:
    equal_or_not_equal = "=="; is_or_is_not = "is"
fixed_node_str = incorrect_node_str.replace(is_or_is_not, equal_or_not_equal)
add_message("literal-comparison", node=node, confidence=HIGH,
            args=(incorrect_node_str, equal_or_not_equal, is_or_is_not, fixed_node_str))
```

- [verified] `y is "str"` → `In 'y is 'str'', use '==' when comparing
  constant literals not 'is' ('y == 'str'')`.
- The `"is not" in as_string()` test is TEXTUAL — a comparison like
  `x is "is not funny"` contains "is not" in its string → treated as
  is-not, and `.replace` replaces the FIRST occurrence(s) — `str.replace`
  with default count replaces ALL occurrences: `x is y is [1]`… (chained is
  excluded by len(ops)!=1 gate). But the literal text inside a string
  operand can be corrupted in fixed_node_str. Replicate exactly.
- LEFT literal (`1 is x`) is NOT flagged (right-side check only).
- complex Consts (`1j`) not in the allow list → exempt.

--------------------------------------------------------------------------------
## 4.7 W0177 nan-comparison — `Comparison %s should be %s`
--------------------------------------------------------------------------------

Operators `==`, `!=`, `is`, `is not`; absence = `!=`/`is not`.
`_check_nan_comparison` (:139-181):

```
_is_float_nan(node):
    try:
        match node:
            case Call(args=[Const(value=str() as value)]) if value.lower() == "nan":
                return node.inferred()[0].pytype() == "builtins.float"
        return False
    except AttributeError:
        return False
_is_numpy_nan(node):
    match node:
        case Attribute(attrname="NaN", expr=Name(name=name)):
            return name in {"numpy", "np"}
    return False
_is_nan = _is_float_nan or _is_numpy_nan

nan_left = _is_nan(left)
if not nan_left and not _is_nan(right): return
absence_text = "not " if checking_for_absence else ""
suggestion = f"'{absence_text}math.isnan({right.as_string()})'"  if nan_left
        else f"'{absence_text}math.isnan({left.as_string()})'"
add_message("nan-comparison", node=root_node,
            args=(f"'{root_node.as_string()}'", suggestion))
```

- `_is_float_nan` matches ANY one-string-arg call whose arg lowercases to
  "nan" and which infers (first result of full `node.inferred()`) to a
  float — i.e. `float("nan")`, `float("NaN")`. CAUTION: `node.inferred()`
  may raise InferenceError (only AttributeError is caught) → propagates →
  module becomes F0002 (astroid-error). In practice the Call pattern rarely
  errors; replicate the crash semantics or note divergence.
- `numpy.NaN` / `np.NaN` matched purely syntactically (attrname is
  case-sensitive: `np.nan` does NOT match — pylint 4.0.5 only knows the
  removed-in-numpy-2 spelling `NaN`).
- Example: `x == float('nan')` → `Comparison 'x == float('nan')' should be
  'math.isnan(x)'`; `x != np.NaN` → `... should be 'not math.isnan(x)'`.

================================================================================
# 5. NameChecker (name_checker/checker.py, naming_style.py)
================================================================================

Message table (checker.py:168-207):

| id | symbol | template |
|----|--------|----------|
| C0103 | invalid-name | `%s name "%s" doesn't conform to %s` |
| C0104 | disallowed-name | `Disallowed name "%s"` (old_names C0102 blacklisted-name) |
| C0105 | typevar-name-incorrect-variance | `Type variable name does not reflect variance%s` |
| C0131 | typevar-double-variance | `TypeVar cannot be both covariant and contravariant` |
| C0132 | typevar-name-mismatch | `TypeVar name "%s" does not match assigned variable name "%s"` |

--------------------------------------------------------------------------------
## 5.1 Options (checker.py:209-285 + naming_style.py:146-187)
--------------------------------------------------------------------------------

Base options `_options` (defaults):
- `good-names`: csv, default `("i", "j", "k", "ex", "Run", "_")`
- `good-names-rgxs`: regexp_csv, default `""` (empty → list `[]`)
- `bad-names`: csv, default `("foo", "bar", "baz", "toto", "tutu", "tata")`
- `bad-names-rgxs`: regexp_csv, default `""`
- `name-group`: csv, default `()` — colon-delimited sets, e.g.
  `function:method` makes both share group `group_function:method`
- `include-naming-hint`: yn, default `False`
- `property-classes`: csv, default `("abc.abstractproperty",)`

Generated naming options (`_create_naming_options`, naming_style.py:146-187):
for each name_type in `sorted(KNOWN_NAME_TYPES)` —
`{argument, attr, class, class_attribute, class_const, const, function,
inlinevar, method, module, paramspec, typealias, typevar, typevartuple,
variable}` (15 types; sorted order matters only for --help):
- if type in KNOWN_NAME_TYPES_WITH_STYLE (all except typevar/paramspec/
  typevartuple/typealias): `<type-hyphened>-naming-style`, choice of
  `["snake_case", "camelCase", "PascalCase", "UPPER_CASE", "any"]`, default
  per DEFAULT_NAMING_STYLES (naming_style.py:121-133):
  ```
  module: snake_case      const: UPPER_CASE     class: PascalCase
  function: snake_case    method: snake_case    attr: snake_case
  argument: snake_case    variable: snake_case  class_attribute: any
  class_const: UPPER_CASE inlinevar: any
  ```
- always: `<type-hyphened>-rgx`, type regexp, default `None`
  (`class_const` → `class-const-rgx` etc.).

--------------------------------------------------------------------------------
## 5.2 Naming style regexes (naming_style.py:14-103) — verbatim
--------------------------------------------------------------------------------

`NamingStyle.get_regex(name_type)` maps:
module→MOD_NAME_RGX, const→CONST_NAME_RGX, class→CLASS_NAME_RGX,
function/method/attr/argument/variable→DEFAULT_NAME_RGX,
class_attribute→CLASS_ATTRIBUTE_RGX, class_const→CONST_NAME_RGX,
inlinevar→COMP_VAR_RGX.

ALL patterns are used with `re.match` (prefix-anchored at start; `$` inside
patterns anchors the end). Unicode `re` semantics (`\W`, `\d` are
unicode-aware) — the Rust port must use unicode character classes.

SnakeCaseStyle:
```
CLASS_NAME_RGX      = [^\W\dA-Z][^\WA-Z]*$
MOD_NAME_RGX        = [^\W\dA-Z][^\WA-Z]*$
CONST_NAME_RGX      = ([^\W\dA-Z][^\WA-Z]*|__.*__)$
COMP_VAR_RGX        = CLASS_NAME_RGX
DEFAULT_NAME_RGX    = ([^\W\dA-Z][^\WA-Z]*|_[^\WA-Z]*|__[^\WA-Z\d_][^\WA-Z]+__)$
CLASS_ATTRIBUTE_RGX = ([^\W\dA-Z][^\WA-Z]*|__.*__)$
```
CamelCaseStyle:
```
CLASS_NAME_RGX      = [^\W\dA-Z][^\W_]*$
MOD_NAME_RGX        = [^\W\dA-Z][^\W_]*$
CONST_NAME_RGX      = ([^\W\dA-Z][^\W_]*|__.*__)$
COMP_VAR_RGX        = MOD_NAME_RGX
DEFAULT_NAME_RGX    = (?:__)?([^\W\dA-Z][^\W_]*|__[^\W\dA-Z_]\w+__)$
CLASS_ATTRIBUTE_RGX = ([^\W\dA-Z][^\W_]*|__.*__)$
```
PascalCaseStyle:
```
CLASS_NAME_RGX      = [^\W\da-z][^\W_]*$
MOD_NAME_RGX        = CLASS_NAME_RGX
CONST_NAME_RGX      = ([^\W\da-z][^\W_]*|__.*__)$
COMP_VAR_RGX        = CLASS_NAME_RGX
DEFAULT_NAME_RGX    = ([^\W\da-z][^\W_]*|__[^\W\dA-Z_]\w+__)$
CLASS_ATTRIBUTE_RGX = [^\W\da-z][^\W_]*$
```
UpperCaseStyle:
```
CLASS_NAME_RGX      = [^\W\da-z][^\Wa-z]*$
MOD_NAME_RGX        = CLASS_NAME_RGX
CONST_NAME_RGX      = ([^\W\da-z][^\Wa-z]*|__.*__)$
COMP_VAR_RGX        = CLASS_NAME_RGX
DEFAULT_NAME_RGX    = ([^\W\da-z][^\Wa-z]*|__[^\W\dA-Z_]\w+__)$
CLASS_ATTRIBUTE_RGX = [^\W\da-z][^\Wa-z]*$
```
AnyStyle: every regex is `.*` (always matches — except note `.` does not
match `\n`; names can't contain `\n`, irrelevant).

Default no-style patterns (checker.py:41-50) — used for the four
style-less types; "naming_style_name" for hints is the literal string
`"predefined"`:
```
typevar      = ^_{0,2}(?!T[A-Z])(?:[A-Z]+|(?:[A-Z]+[a-z]+)+(?:T)?(?<!Type))(?:_co(?:ntra)?)?$
paramspec    = ^_{0,2}(?:[A-Z]+|(?:[A-Z]+[a-z]+)+(?:P)?(?<!Type))$
typevartuple = ^_{0,2}(?:[A-Z]+|(?:[A-Z]+[a-z]+)+(?:Ts)?(?<!Type))$
typealias    = ^_{0,2}(?!T[A-Z]|Type)[A-Z]+[a-z0-9]+(?:[A-Z][a-z0-9]+)*$
```
(lookahead/lookbehind — Rust `regex` crate can't do these; port needs
fancy-regex or hand-rolled.)

Effective defaults under default config (what the corpora exercise):
- module: snake_case MOD — `[^\W\dA-Z][^\WA-Z]*$`
- const & class_const: UPPER_CASE CONST — `([^\W\da-z][^\Wa-z]*|__.*__)$`
- class: PascalCase CLASS — `[^\W\da-z][^\W_]*$`
- function/method/attr/argument/variable: snake_case DEFAULT —
  `([^\W\dA-Z][^\WA-Z]*|_[^\WA-Z]*|__[^\WA-Z\d_][^\WA-Z]+__)$`
- class_attribute & inlinevar: AnyStyle (`.*`) → never fail.

--------------------------------------------------------------------------------
## 5.3 open() — rule construction (checker.py:296-338)
--------------------------------------------------------------------------------

```
stats.reset_bad_names()
for group in config.name_group:                  # e.g. "function:method"
    for name_type in group.split(":"):
        self._name_group[name_type] = f"group_{group}"
regexps, hints = self._create_naming_rules()
# per name_type: style regex (or DEFAULT_PATTERNS for the 4 special types),
#   overridden by <type>_rgx when not None.
# hint: f"{custom_regex.pattern!r} pattern" if custom else
#       f"{naming_style_name} naming style"   ("predefined naming style" for
#       the 4 special types without custom rgx)
self._good_names_rgxs_compiled = [re.compile(r) for r in config.good_names_rgxs]
self._bad_names_rgxs_compiled  = [re.compile(r) for r in config.bad_names_rgxs]
```

Default hints: `"snake_case naming style"`, `"UPPER_CASE naming style"`,
`"PascalCase naming style"`, `"any naming style"`, `"predefined naming
style"`.

--------------------------------------------------------------------------------
## 5.4 _check_name — the funnel (checker.py:642-686)
--------------------------------------------------------------------------------

`_check_name(node_type, name, node, confidence=HIGH, disallowed_check_only=False)`:

```
def _should_exempt_from_invalid_name(node):
    if node_type == "variable":
        inferred = utils.safe_infer(node)        # infer the AssignName itself
        if isinstance(inferred, nodes.ClassDef): return True
    return False

if self._name_allowed_by_regex(name): return
    # name in config.good_names OR any(good_names_rgxs.match(name))
    # NOTE: early return also skips C0104 AND the typevar checks below.
if self._name_disallowed_by_regex(name):
    # name in config.bad_names OR any(bad_names_rgxs.match(name))
    stats.increase_bad_name(node_type, 1)
    add_message("disallowed-name", node=node, args=name, confidence=HIGH)
    return
regexp = self._name_regexps[node_type]; match = regexp.match(name)
if _is_multi_naming_match(match, node_type, confidence):     # §5.8
    name_group = self._name_group.get(node_type, node_type)
    self._bad_names.setdefault(name_group, {}) \
        .setdefault(match.lastgroup, []).append((node, node_type, name, confidence))
if match is None and not disallowed_check_only \
        and not _should_exempt_from_invalid_name(node):
    self._raise_name_warning(None, node, node_type, name, confidence)
if node_type == "typevar":
    self._check_typevar(name, node)              # C0105/C0131/C0132, §5.10
```

`_raise_name_warning(prevalent_group, node, node_type, name, confidence,
warning="invalid-name")` (:605-630):

```
type_label = constants.HUMAN_READABLE_TYPES[node_type]
    # constants.py:64-81: file→file, module→module, const→constant,
    # class→class, function→function, method→method, attr→attribute,
    # argument→argument, variable→variable, class_attribute→"class attribute",
    # class_const→"class constant", inlinevar→"inline iteration",
    # typevar→"type variable", paramspec→"parameter specification variable",
    # typevartuple→"type variable tuple", typealias→"type alias"
hint = self._name_hints[node_type]
if prevalent_group:
    hint = f"the `{prevalent_group}` group in the {hint}"
if config.include_naming_hint:                  # default False
    hint += f" ({self._name_regexps[node_type].pattern!r} pattern)"
args = (type_label.capitalize(), name, hint)    # capitalize() lowercases rest;
                                                # labels are all-lowercase → safe
add_message("invalid-name", node=node, args=args, confidence=confidence)
stats.increase_bad_name(node_type, 1)
```

(The function signature has a `warning="invalid-name"` parameter with an
else-arm building 2-tuple args for other warnings, but BOTH call sites in
4.0.5 — checker.py:370 and :682 — use the default, so the else-arm is dead
code; the 3-tuple path above is the only live one.)

Rendered example: `Constant name "a" doesn't conform to UPPER_CASE naming
style` / `Class constant name "FinalThing" doesn't conform to UPPER_CASE
naming style` / `Attribute name "AttrBad" doesn't conform to snake_case
naming style`. [verified]

--------------------------------------------------------------------------------
## 5.5 Visit sites
--------------------------------------------------------------------------------

### visit_module (:340-343) — decorated ("disallowed-name", "invalid-name")
```
self._check_name("module", node.name.split(".")[-1], node)
self._bad_names = {}                 # reset AFTER the module-name check
```
Module name = last dotted component of the astroid module name (package
`__init__` modules check the PACKAGE basename). Message at line 1 col 0
(Module fromlineno 0 → `line or 1`).

### visit_classdef (:372-379) — decorated same
```
self._check_name("class", node.name, node)
for attr, anodes in node.instance_attrs.items():     # insertion order =
    if not any(node.instance_attr_ancestors(attr)) \           # build order
            and not utils.is_assign_name_annotated_with(anodes[0], "Final"):
        self._check_name("attr", attr, anodes[0])
```
- `instance_attrs`: `self.X = ...` AssignAttr nodes collected at build time
  (delayed_assattr); `anodes[0]` = first assignment → report node.
- `instance_attr_ancestors(attr)` (astroid scoped_nodes.py:2234-2246):
  ancestors (MRO walk) defining attr in THEIR instance_attrs → if any,
  exempt (attribute inherited).
- `is_assign_name_annotated_with(node, "Final")` (utils.py:1748-1762):
  parent is AnnAssign whose annotation (or `annotation.value` if Subscript)
  is `Name("Final")` or `Attribute(attrname="Final")`. For an AssignAttr
  whose parent is AnnAssign (`self.x: Final = 1`) this works identically.
- Emission order: attr messages fire at ClassDef visit time — BEFORE
  messages from the class's methods (e.g. method invalid-name at a smaller
  line can come AFTER the attr message at a bigger line). [verified §t5: line
  6 attr before line 4 method]

### visit_functiondef / visit_asyncfunctiondef (:381-406) — decorated same
```
confidence = HIGH
if node.is_method():
    if utils.overrides_a_method(node.parent.frame(), node.name):
        return            # overridden method: skips name check AND arg checks
    confidence = INFERENCE if utils.has_known_bases(node.parent.frame())
                 else INFERENCE_FAILURE
self._check_name(_determine_function_name_type(node, config), node.name,
                 node, confidence)
args = node.args.args
if args is not None:
    self._recursive_check_names(args)     # each arg → _check_name("argument",
                                          #   arg.name, arg) — checker.py:597-600
```
- `overrides_a_method(class_node, name)` (utils.py:468-477): any ancestor
  (skipping ones literally named `object`) with `name in ancestor` and
  `ancestor[name]` a FunctionDef. [verified: override exempts BOTH the
  method name and its argument names.]
- `has_known_bases` (utils.py:1466-1484): recursively all bases safe_infer
  to ClassDef (≠ self); memoized on `klass._all_bases_known`.
- ONLY `node.args.args` is checked — posonlyargs and kwonlyargs are NEVER
  name-checked [verified §t5: PosBad/KwBad silent, NormBad flagged].
  (`args.args` is never None for def; None only for some lambda shells.)
- `_determine_function_name_type(node, config)` (:115-149):
  ```
  property_classes = {"builtins.property"} ∪ config.property_classes
  property_names = {last component of each config.property_classes entry}
  if not node.is_method(): return "function"
  if is_property_setter(node) or is_property_deleter(node): return "attr"
     # utils.py:828-836 → _is_property_kind: any decorator that is an
     # Attribute with attrname == "setter"/"deleter" (PURELY syntactic)
  for decorator in node.decorators.nodes if node.decorators else []:
      if isinstance(decorator, nodes.Name) or (
          isinstance(decorator, nodes.Attribute)
          and decorator.attrname in property_names):
          inferred = safe_infer(decorator)
          if inferred and hasattr(inferred, "qname")
             and inferred.qname() in property_classes:
              return "attr"
  return "method"
  ```
  Note: ANY Name decorator is inference-tested against property classes
  (so `@property` → builtins.property → "attr"); Attribute decorators only
  when attrname matches a configured property short-name
  (`abstractproperty` by default).

### visit_assignname (:408-565) — decorated ("disallowed-name",
"invalid-name", "typevar-name-incorrect-variance", "typevar-double-variance",
"typevar-name-mismatch")

```
frame = node.frame(); assign_type = node.assign_type()
   # AssignName.assign_type = parent.assign_type() recursively
   # (ParentAssignNode, astroid _base_nodes.py:122-127); Assign/AnnAssign/
   # AugAssign/Delete/ExceptHandler/For/With/TypeAlias/TypeVar/ParamSpec/
   # TypeVarTuple/NamedExpr/MatchMapping/MatchStar/MatchAs/Arguments(?) and
   # Comprehension (node_classes.py:1983) return self.

if isinstance(assign_type, nodes.Comprehension):
    _check_name("inlinevar", node.name, node)            # default AnyStyle
elif isinstance(assign_type, nodes.TypeVar):             # PEP 695 `def f[T]`
    _check_name("typevar", node.name, node)
elif isinstance(assign_type, nodes.ParamSpec):
    _check_name("paramspec", node.name, node)
elif isinstance(assign_type, nodes.TypeVarTuple):
    _check_name("typevartuple", node.name, node)
elif isinstance(assign_type, nodes.TypeAlias):           # `type X = ...`
    _check_name("typealias", node.name, node)

elif isinstance(frame, nodes.Module):                    # ===== module scope
    if isinstance(assign_type, nodes.AnnAssign) and \
            self._assigns_typealias(assign_type.annotation):
        _check_name("typealias", node.name, node)        # `X: TypeAlias = ...`
    elif isinstance(assign_type, (nodes.Assign, nodes.AnnAssign)):
        inferred_assign_type = safe_infer(assign_type.value) if assign_type.value else None

        if isinstance(node.parent, nodes.Assign):        # single-name target
            if typevar_node_type := self._assigns_typevar(assign_type.value):
                _check_name(typevar_node_type, assign_type.targets[0].name, node)
                return
            if self._assigns_typealias(assign_type.value):
                _check_name("typealias", assign_type.targets[0].name, node)
                return

        if (isinstance(node.parent, nodes.Tuple)
                and isinstance(assign_type.value, nodes.Tuple)
                and node.parent.elts.index(node) < len(assign_type.value.elts)):
            assigner = assign_type.value.elts[node.parent.elts.index(node)]
            if typevar_node_type := self._assigns_typevar(assigner):
                _check_name(typevar_node_type,
                            assign_type.targets[0].elts[idx].name, node); return
            if self._assigns_typealias(assigner):
                _check_name("typealias",
                            assign_type.targets[0].elts[idx].name, node); return
            # NEITHER matched → fall out of the if/elif chain entirely:
            # tuple-unpacked module names with literal-tuple RHS get NO
            # const/variable check AT ALL.  [verified: `x, y = 1, 2` silent]
        elif inferred_assign_type in (None, util.Uninferable):
            return                                       # conservatism bail
        elif self._should_check_class_regex(inferred_assign_type):
            _check_name("class", node.name, node)        # X = SomeClass alias
        elif (not (redefines_import := _redefines_import(node))
              and not isinstance(inferred_assign_type, (nodes.FunctionDef, nodes.Lambda))
              and not utils.is_reassigned_before_current(node, node.name)
              and not utils.is_reassigned_after_current(node, node.name)
              and not utils.get_node_first_ancestor_of_type(node, (nodes.For, nodes.While))):
            # single-assignment, not loop-bound, not a func alias → CONST
            if not self._meets_exception_for_non_consts(inferred_assign_type, node.name):
                _check_name("const", node.name, node)
        else:                                            # VARIABLE path
            node_type = "variable"
            iattrs = tuple(node.frame().igetattr(node.name))
            if (util.Uninferable in iattrs
                    and self._name_regexps["const"].match(node.name) is not None):
                return                                   # ambiguous + const-shaped → bail
            attrs = tuple(node.frame().getattr(node.name))
            if len(attrs) > 1 and all(astroid.are_exclusive(*combo)
                    for combo in itertools.combinations(attrs, 2)):
                node_type = "const"                      # exclusive branches → const
            if not self._meets_exception_for_non_consts(inferred_assign_type, node.name):
                _check_name(node_type, node.name, node,
                            disallowed_check_only=redefines_import)

elif isinstance(frame, nodes.FunctionDef):               # ===== function scope
    if node.name in frame and node.name not in frame.argnames():
        # `in frame` = in frame.locals (globals-declared names are NOT
        # in function locals); argnames covers all arg kinds
        if not _redefines_import(node):
            if isinstance(assign_type, nodes.AnnAssign) and \
                    self._assigns_typealias(assign_type.annotation):
                _check_name("typealias", node.name, node)
            else:
                _check_name("variable", node.name, node)

elif isinstance(frame, nodes.ClassDef) and \
        not any(frame.local_attr_ancestors(node.name)):  # ===== class scope
    # local_attr_ancestors: ancestors defining the name in class locals →
    # inherited class attrs exempt
    if utils.is_assign_name_annotated_with_class_var_typing_name(node, "Final"):
        _check_name("class_const", node.name, node)      # X: ClassVar[Final[...]]
    elif utils.is_assign_name_annotated_with(node, "Final"):
        if frame.is_dataclass:                           # set by astroid brain
            _check_name("class_attribute", node.name, node)
        else:
            _check_name("class_const", node.name, node)  # X: Final = ...
    elif utils.is_enum_member(node):
        _check_name("class_const", node.name, node)
    else:
        _check_name("class_attribute", node.name, node)  # default AnyStyle → no-op
# Lambda frames: NO branch matches → unchecked.
```

Helper specifics:

- `_assigns_typevar(value)` (:688-698): value is Call AND
  `safe_infer(value.func)` is ClassDef whose qname is in TYPE_VAR_QNAMES
  (:53-66): typevar={typing.TypeVar, typing_extensions.TypeVar},
  paramspec={typing.ParamSpec, typing_extensions.ParamSpec},
  typevartuple={typing.TypeVarTuple, typing_extensions.TypeVarTuple} →
  returns the type key, else None.
- `_assigns_typealias(node)` (:700-718): `safe_infer(node)`:
  - ClassDef or bases.UnionType: qname == "typing.TypeAlias" → True; qname
    in {".Union", "builtins.Union", "builtins.UnionType"} → True UNLESS
    `node.parent` is AnnAssign (annotation usage, not alias);
  - FunctionDef with qname "typing.TypeAlias" (pre-3.12 typing) → True.
- `_should_check_class_regex(inferred)` (:575-595): inferred is ClassDef →
  True; inferred is bases.Instance whose `mro()` names intersect
  {"EnumMeta", "TypedDict"} → True (note: `.mro()` here is called on the
  INSTANCE — proxies to the class); inferred is FunctionDef with qname
  "typing.Annotated" → True; else False.
- `_meets_exception_for_non_consts(inferred, name)` (:567-573): inferred is
  Const → False; else True iff VARIABLE regex matches name. I.e. a
  non-Const-valued module name in snake_case is exempt from the const check
  ([verified: `lst = []` silent; `a = 5` flagged]).
- `_redefines_import(node)` (:93-112): walk up to the node whose parent is
  an ExceptHandler; handler must catch ImportError
  (`utils.error_of_type(handler, ImportError)`, utils.py:778-802 →
  `handler.catch({"ImportError"})`); then scan `handler.parent.parent`
  (the Try) — actually `current.parent.parent` = Try — for
  Import/ImportFrom nodes whose name-or-alias equals node.name. True →
  except-ImportError fallback assignment: const/variable checks reduced to
  `disallowed_check_only=True` (C0104 still possible) at module scope, or
  fully skipped at function scope.
- `is_reassigned_before_current` / `after` (utils.py:1900-1912 →
  `_is_reassigned_relative_to_current`, :1876-1898): scan
  `node.scope().nodes_of_class((AssignName, ClassDef, FunctionDef))` for
  same `.name` with `lineno <`/`> node.lineno` and in the same scope
  (`_is_node_in_same_scope`, :1868-1873: for ClassDef/FunctionDef compare
  `candidate.parent.scope()`, else `candidate.scope()`). NOTE comparisons
  are by LINE NUMBER, two assignments on one line (`x = 1; x = 2`) are
  invisible to each other.
- `get_node_first_ancestor_of_type` (utils.py:1963-1969): nearest ancestor
  isinstance For/While (the whole ancestor chain, so a module-level
  assignment inside `if:` inside `for:` IS loop-bound → variable path).
- `igetattr`/`getattr` on the Module frame: full inference; `are_exclusive`
  (astroid node_classes.py:116-186, spec in notes/05 §13.9). The
  all-pairwise-exclusive upgrade to "const" handles
  `if cond: X = 1 else: X = 2`.
- `is_assign_name_annotated_with_class_var_typing_name` (utils.py:1765-1780):
  annotation is ClassVar[...]; unwrap subscript slice (twice if needed) and
  match Name/Attribute == "Final".
- `is_enum_member(node)` (utils.py:2342-2358): frame is ClassDef,
  `frame.is_subtype_of("enum.Enum")` (qname equality on self or ancestors,
  scoped_nodes.py:2004-2015), frame's module is not `enum` itself; then
  `members = frame.locals.get("__members__")`; None → False; True iff
  node.name in the member-name list of `members[0].items` (astroid's enum
  brain synthesizes `__members__` as a Dict).

Confidence: every `_check_name` call from visit_assignname uses default
HIGH.

--------------------------------------------------------------------------------
## 5.6 C0104 disallowed-name
--------------------------------------------------------------------------------

Emitted inside `_check_name` (§5.4) for ANY name type when the name is in
`bad-names` (default foo/bar/baz/toto/tutu/tata) or matches any
`bad-names-rgxs` pattern — UNLESS the name is good-listed first
(good-names/good-names-rgxs win). args = the bare name; confidence HIGH;
report node = same node the invalid-name would use. Template renders e.g.
`Disallowed name "foo"`. `disallowed_check_only=True` (redefined-import
case) still emits C0104 but never C0103.

--------------------------------------------------------------------------------
## 5.7 C0103 invalid-name — gating recap
--------------------------------------------------------------------------------

A name N of type T produces C0103 iff (in order):
1. N ∉ good-names and no good-names-rgxs match;
2. N ∉ bad-names and no bad-names-rgxs match (else C0104 instead);
3. `regexps[T].match(N)` is None;
4. not disallowed_check_only;
5. not (T == "variable" and safe_infer(node) is a ClassDef).

Under default config, class_attribute and inlinevar can NEVER fail
(AnyStyle), so class-body plain assignments and comprehension variables are
silent unless bad-listed.

--------------------------------------------------------------------------------
## 5.8 Multi-naming-group machinery (leave_module, :345-370)
--------------------------------------------------------------------------------

`_is_multi_naming_match(match, node_type, confidence)` (:156-164): match is
not None AND `match.lastgroup` is not None AND lastgroup not in
`EXEMPT_NAME_CATEGORIES = {"exempt", "ignore"}` AND not (node_type ==
"method" and confidence == INFERENCE_FAILURE).

DEFAULT REGEXES HAVE NO NAMED GROUPS → `match.lastgroup` is None → the whole
machinery is a NO-OP under default config. It activates only when the user
supplies custom `<type>-rgx` with named groups (e.g.
`(?P<snake>[a-z_]+)|(?P<camel>[a-z][A-Za-z]+)`).

`leave_module` (undecorated, always runs):

```
for all_groups in self._bad_names.values():      # keyed by name-group
    if len(all_groups) < 2: continue             # need ≥2 distinct lastgroups
    groups = defaultdict(list); min_warnings = sys.maxsize
    prevalent_group, _ = max(all_groups.items(), key=lambda it: len(it[1]))
        # ties: first-encountered wins (max keeps first maximal; dict order =
        # insertion order of lastgroup keys)
    for group in all_groups.values():
        groups[len(group)].append(group)
        min_warnings = min(len(group), min_warnings)
    if len(groups[min_warnings]) > 1:
        by_line = sorted(groups[min_warnings],
            key=lambda g: min(w[0].lineno for w in g if w[0].lineno is not None))
        warnings = itertools.chain(*by_line[1:])  # spare the first-by-line
    else:
        warnings = groups[min_warnings][0]
    for args in warnings:
        self._raise_name_warning(prevalent_group, *args)
        # hint becomes: f"the `{prevalent_group}` group in the {hint}"
```

`self._bad_names` is reset in visit_module AFTER the module-name check
(:343) — i.e. per-module accumulation; module-name matches recorded into the
previous module's dict are discarded.

--------------------------------------------------------------------------------
## 5.9 C0105/C0131/C0132 — _check_typevar (:720-809)
--------------------------------------------------------------------------------

Called from `_check_name` ONLY when node_type == "typevar" (i.e. for
`T = TypeVar(...)` assignments routed via `_assigns_typevar` and PEP 695
`def f[T]`/`class C[T]` type params) and only when the name survived BOTH
early returns in §5.4 — good-listed names AND bad-listed names (the
disallowed-name branch `return`s before the typevar check) skip C0105/
C0131/C0132 entirely.

```
variance = invariant
match node.parent:
    case nodes.Assign():
        keywords = node.assign_type().value.keywords
        args     = node.assign_type().value.args
    case nodes.Tuple():
        idx = node.parent.elts.index(node)
        keywords = node.assign_type().value.elts[idx].keywords
        args     = node.assign_type().value.elts[idx].args
    case _:                       # PEP 695 type parameter
        keywords = (); args = (); variance = inferred

name_arg = None
for kw in keywords:
    if variance == double_variant: pass
    elif kw.arg == "covariant" and kw.value.value:
        variance = covariant if variance != contravariant else double_variant
    elif kw.arg == "contravariant" and kw.value.value:
        variance = contravariant if variance != covariant else double_variant
    if kw.arg == "name" and isinstance(kw.value, nodes.Const):
        name_arg = kw.value.value
if name_arg is None and args and isinstance(args[0], nodes.Const):
    name_arg = args[0].value

match variance:
    case inferred: pass                       # PEP 695: no variance checks
    case double_variant:
        add_message("typevar-double-variance", node=node, confidence=INFERENCE)
        add_message("typevar-name-incorrect-variance", node=node,
                    args=("",), confidence=INFERENCE)
    case covariant if not name.endswith("_co"):
        suggest = f"{re.sub('_contra$', '', name)}_co"
        add_message("typevar-name-incorrect-variance", node=node, confidence=INFERENCE,
            args=(f'. "{name}" is covariant, use "{suggest}" instead'))
    case contravariant if not name.endswith("_contra"):
        suggest = f"{re.sub('_co$', '', name)}_contra"
        add_message(..., args=(f'. "{name}" is contravariant, use "{suggest}" instead'))
    case invariant if name.endswith(("_co", "_contra")):
        suggest = re.sub("_contra$|_co$", "", name)
        add_message(..., args=(f'. "{name}" is invariant, use "{suggest}" instead'))

if name_arg is not None and name_arg != name:
    add_message("typevar-name-mismatch", node=node,
                args=(name_arg, name), confidence=INFERENCE)
```

- `kw.value.value` truthiness: `covariant=True` → Const(True).value; a
  non-Const expr (`covariant=cond`) → Const? no — kw.value.value would
  AttributeError for non-Const... astroid Keyword.value is the expr node;
  `.value` on a Name raises AttributeError → propagates (F0002). In practice
  TypeVar kwargs are literal bools.
- C0105 rendering: double-variant arg "" → `Type variable name does not
  reflect variance`; covariant case → `Type variable name does not reflect
  variance. "T" is covariant, use "T_co" instead`.
- C0132: `TypeVar name "X" does not match assigned variable name "T"`
  (name_arg first, variable name second).
- Note `_check_name` was invoked with the TARGET name from
  `assign_type.targets[0]...` but `_check_typevar` re-reads `name` from that
  same parameter and the call/keywords from `node.assign_type().value` — for
  the Tuple case indexing by `node.parent.elts.index(node)`.

================================================================================
# 6. DocStringChecker (docstring_checker.py:43-203)
================================================================================

| id | symbol | template |
|----|--------|----------|
| C0112 | empty-docstring | `Empty %s docstring` |
| C0114 | missing-module-docstring | `Missing module docstring` |
| C0115 | missing-class-docstring | `Missing class docstring` |
| C0116 | missing-function-docstring | `Missing function or method docstring` |

Options (:75-99):
- `no-docstring-rgx`: regexp, default `re.compile("^_")` (NO_REQUIRED_DOC_RGX,
  :25) — names matching are FULLY exempt (both missing AND empty checks)
  for classes and functions; NOT applied to modules.
- `docstring-min-length`: int, default `-1` — bodies shorter than this many
  lines are exempt from the MISSING check (never from empty); -1 disables.

Visit sites:

### visit_module (:104-106) — decorated ("missing-module-docstring",
"empty-docstring") → `_check_docstring("module", node)`. No name gate.

### visit_classdef (:108-111) — decorated ("missing-class-docstring",
"empty-docstring"):
```
if config.no_docstring_rgx.match(node.name) is None:
    self._check_docstring("class", node)
```
Default `^_` → `_Private` classes exempt. [verified]

### visit_functiondef / visit_asyncfunctiondef (:113-148) — decorated
("missing-function-docstring", "empty-docstring"):
```
if config.no_docstring_rgx.match(node.name) is None:
    # → ALL dunders (__init__ etc.) and _private functions exempt by default;
    #   there is NO separate __init__ docstring-inheritance rule in 4.0.5.
    ftype = "method" if node.is_method() else "function"
    if is_property_setter(node) or is_property_deleter(node) \
            or is_overload_stub(node):
        return
        # is_overload_stub (utils.py:1666-1675): decorated_with(node,
        #   ["typing.overload", "overload"]) — inference-based.
    if isinstance(node.parent.frame(), nodes.ClassDef):
        overridden = False
        confidence = INFERENCE if utils.has_known_bases(node.parent.frame())
                     else INFERENCE_FAILURE
        for ancestor in node.parent.frame().ancestors():
            if ancestor.qname() == "builtins.object": continue
            if node.name in ancestor and isinstance(ancestor[node.name],
                                                    nodes.FunctionDef):
                overridden = True; break
        self._check_docstring(ftype, node, report_missing=not overridden,
                              confidence=confidence)
        # overridden methods: NO missing-function-docstring, but the
        # empty-docstring branch still applies (report_missing only gates
        # the missing path).
    elif isinstance(node.parent.frame(), nodes.Module):
        self._check_docstring(ftype, node)          # HIGH
    else:
        return        # NESTED functions (frame = FunctionDef/Lambda):
                      # never checked at all. [verified]
```

### _check_docstring(node_type, node, report_missing=True, confidence=HIGH)
(:150-203):

```
docstring = node.doc_node.value if node.doc_node else None
if docstring is None:
    docstring = _infer_dunder_doc_attribute(node)
    # (:28-40) node["__doc__"] → locals lookup (first binding), KeyError →
    # None; safe_infer it; Const → str(docstring.value) (non-str consts
    # stringified, e.g. __doc__ = 5 → "5" → counts as docstring).

if docstring is None:
    if not report_missing: return
    lines = utils.get_node_last_lineno(node) - node.lineno
    # get_node_last_lineno (utils.py:1584-1605): recursive last stmt lineno,
    # priority finalbody > orelse > handlers > body > node.lineno.
    # Module.lineno = 0 → lines = lineno of last top-level stmt.
    if node_type == "module" and not lines:
        return                       # empty module → no C0114
    max_lines = config.docstring_min_length
    if node_type != "module" and max_lines > -1 and lines < max_lines:
        return
    stats.undocumented["klass" if node_type == "class" else node_type] += 1
    match node.body:                 # str.format() heuristic
        case [nodes.Expr(value=nodes.Call() as value), *_]:
            match utils.safe_infer(value.func):
                case astroid.BoundMethod(
                        bound=astroid.Instance(name="str" | "unicode" | "bytes")):
                    return           # first stmt is "...".format(...) → treat
                                     # as docstring-ish, no message
    message = {"module": "missing-module-docstring",
               "class": "missing-class-docstring"}.get(node_type,
               "missing-function-docstring")
    add_message(message, node=node, confidence=confidence)
elif not docstring.strip():
    stats.undocumented[...] += 1
    add_message("empty-docstring", node=node, args=(node_type,),
                confidence=confidence)
```

- C0112 args render as `Empty module docstring` / `Empty class docstring` /
  `Empty function docstring` / `Empty method docstring`. [verified method]
- Report node = Module (line 1 col 0) / ClassDef / FunctionDef (keyword-
  anchored position).
- `docstring-min-length` uses `lines = last_lineno - node.lineno` — for a
  one-line `def f(): return 1` lines = 0; a def spanning to line+3 → 3.
- The astroid.BoundMethod match: `bound=` attribute is the instance the
  method is bound to; e.g. `def f(): "x {}".format(1)` — wait, that body's
  first stmt is Expr(Call) only when the docstring position holds a CALL
  (so the doc_node is None); safe_infer of `"x {}".format` → BoundMethod
  bound to a str Instance (Const proxies) → exempt.

================================================================================
# 7. FunctionChecker — W0135 contextmanager-generator-missing-cleanup
  (function_checker.py:17-149)
================================================================================

Template: `The context used in function %r will not be exited.`

`visit_functiondef` / `visit_asyncfunctiondef` (both decorated) →
`_check_contextmanager_generator_missing_cleanup(node)` (:37-77):

```
with_nodes = list(node.nodes_of_class(nodes.With))
if not with_nodes: return
yield_nodes = list(chain.from_iterable(
    wn.nodes_of_class(nodes.Yield) for wn in with_nodes))
if not yield_nodes: return                  # need a yield INSIDE a with
for with_node in with_nodes:
    for call, held in with_node.items:
        if held is None: continue           # `with ctx():` (no `as`) skipped
        inferred_node = getattr(utils.safe_infer(call), "parent", None)
        # safe_infer of the context expr; for a generator-returning call the
        # result is a bases.Generator whose .parent is the FunctionDef
        if not isinstance(inferred_node, nodes.FunctionDef): continue
        if self._node_fails_contextmanager_cleanup(inferred_node, yield_nodes):
            add_message("contextmanager-generator-missing-cleanup",
                        node=with_node, args=(node.name,))
```

`_node_fails_contextmanager_cleanup(node /*the CM function*/, yield_nodes
/*yields of the CALLER inside withs*/)` (:79-149):

```
# 1. if ANY caller-yield is bare or yields a Const → False (no message)
if any(y.value is None or isinstance(y.value, nodes.Const)
       for y in yield_nodes): return False
# 2. single-yield-is-last-statement check ON THE CM FUNCTION:
yield_nodes = list(node.nodes_of_class(nodes.Yield))   # REBOUND to CM's yields
if len(yield_nodes) == 1:
    n = yield_nodes[0].parent
    while n is not node:
        if n.next_sibling() is not None: break
        n = n.parent
    else: return False        # yield is the last statement → no cleanup needed
# 3. Try blocks containing a yield, inside the CM function:
try_with_yield_nodes = [t for t in node.nodes_of_class(nodes.Try)
                        if any(t.nodes_of_class(nodes.Yield))]
if not try_with_yield_nodes: return True         # cleanup code, no try → FAIL
if all(t.finalbody for t in try_with_yield_nodes): return False
if all(check_handles_generator_exceptions(t) for t in try_with_yield_nodes):
    # handler.type None (bare except) OR safe_infer(handler.type).qname() in
    # {"builtins.GeneratorExit", "builtins.Exception"} for SOME handler of
    # EVERY yield-holding try
    return False
return True
```

Report node = the With statement in the CALLING generator; args = the
CALLING function's name (%r → quoted). Emitted once per (with_node, item)
pair that fails — a `with a() as x, b() as y:` can emit twice.

================================================================================
# 8. Iteration-order / emission-order dependencies (summary)
================================================================================

1. Messages stream in WALK order, not line order. Specific inversions to
   reproduce:
   - W0134 fires at the Try node visit, before messages from inside the try
     body (its reported line is the return's, later than try's).
   - NameChecker attr checks (instance_attrs) fire at ClassDef visit, before
     per-method messages, with lines pointing into method bodies. [verified]
   - NameChecker `leave_module` group warnings flush at module end (custom
     multi-group configs only).
2. Within one node type, callbacks run in prepared-checker order (notes/02
   empirical dump) — e.g. for visit_compare only ComparisonChecker; for
   visit_functiondef: BasicErrorChecker → BasicChecker → NameChecker →
   DocStringChecker → FunctionChecker (subject to the same-name registration
   quirk; verify against the order dump).
3. Within ComparisonChecker.visit_compare: W0143 → R0124 → C0123 → R0133 →
   C0121 → W0177 → R0123 for the same Compare node.
4. `visit_dict`/`visit_set` duplicate scans preserve source order; the
   reported arg is the CURRENT (second+) occurrence's value.
5. W0128 `Counter.most_common()` — count-descending, insertion-ordered ties
   (pure-Python dict semantics; PYTHONHASHSEED-independent).
6. `_check_dangerous_default` iterates positional defaults then kw_defaults.
7. NameChecker module-scope checks `igetattr`/`getattr`/`are_exclusive` —
   inference caching (lru_cache on infer_all, maxsize 512; has_known_bases
   memoized on the class node) is per-process in pylint; results must be
   deterministic in the port regardless.

================================================================================
# 9. Conservatism bail-outs (consolidated checklist)
================================================================================

- W0101: no message when no next sibling; return+bare-yield-at-end skip.
- W0102: `next(default.infer())` InferenceError → skip that default; qname
  must be in the 10-entry table (tuples/frozensets/custom classes immune).
- W0104/W0106: bare Call statements never produce W0104 (early return in
  exception branch); unique-stmt-of-try-body, yield/await, Ellipsis skips.
- W0105: attribute-docstring exemption (prev sibling Assign/AnnAssign/
  TypeAlias in same scope; scope must be Class/Module/`__init__`).
- W0108: defaults present, non-Call body, chained attr-call, kwarg/vararg
  mismatch, length/name mismatch, func-uses-param → all silent.
- W0125/W0126: Call/BinOp/BoolOp/UnaryOp/Subscript tests skip inference;
  safe_infer ambiguity → None → silent; generator-call path requires ALL
  returns to be GeneratorExp and ≥1 return.
- W0127: multi-target, 1-elt tuple, non-Name RHS elements, class-scope →
  silent.
- W0128: dummy-rgx match ABORTS whole statement check; `_` skipped; only
  Tuple targets.
- W0133: name must start uppercase (perf heuristic) AND safe_infer to
  ExceptionInstance.
- W0143: both-sides-callable (count==2) silent; typing._SpecialForm and
  raise-in-body functions exempt.
- W0150: `self._trys` empty (incl. TryStar-only nesting) → silent; breaker
  class reached first → silent.
- C0103 module-scope: safe_infer(value) None/Uninferable → silent;
  tuple-target with tuple RHS falls through unchecked; Uninferable in
  igetattr + const-shaped name → silent; non-Const value + variable-regex
  match → silent; FunctionDef/Lambda alias → variable path (not const);
  redefines-import → C0104 only.
- C0103 function/method: overridden method → fully silent (args too);
  posonly/kwonly args never checked; global-declared names skipped
  (`node.name in frame` fails).
- C0105/C0131/C0132: good-listed names skip; PEP 695 params skip variance.
- C0115/C0116: `^_` name gate; property setter/deleter (syntactic) and
  overload stubs (inferred) exempt; nested functions exempt; overridden
  methods exempt from missing (not empty); min-length gate;
  str-format-first-statement heuristic; `__doc__` dunder fallback.
- C0114: empty module exempt (`lines == 0`).
- W0135: bare/Const caller-yields; CM yield-is-last-stmt; all-try-finally;
  all-try-handles-GeneratorExit/Exception/bare → silent.

================================================================================
# 10. Port checklist (Rust-side notes)
================================================================================

- Regexes: name checker needs Python-`re`-equivalent semantics — the typevar
  family uses (?!...) (?<!...) lookarounds → fancy-regex; `[^\W\dA-Z]`-class
  unicode behavior must match Python's str.isalnum-based `\w`.
  `str.capitalize()` for the type label (first char upper, rest lower).
- `%` formatting: `%r` must reproduce CPython repr for str (quote choice:
  prefers single quotes, switches to double when the string contains a
  single quote and no double), bool, int, float (repr shortest-roundtrip),
  bytes.
- Python value equality for W0109/W0130 key/value sets (bool==int==float,
  str, bytes, tuple consts are not collected — only Const values, which in
  pyast are scalar consts; tuples appear as Tuple nodes → skipped).
- `as_string()` (astroid round-trip) is needed for: W0106 args, C0121 args
  (both whole-Compare and operand strings), R0123 args (+ textual
  `.replace`), R0133 args, W0177 suggestions, W0102 Name defaults, W0109
  Attribute keys. Reuse the pyast as_string port (notes/08 §basics).
- Confidence values to store: HIGH default; INFERENCE for W0101-via-call,
  W0125/W0126, W0133, C0105/C0131/C0132, method-name C0103 (known bases),
  DocString method checks; INFERENCE_FAILURE for unknown-bases method
  C0103/C011x; HIGH explicit for W0124?-no (default), W0129?-no (default),
  W0130, W0131, W0134, W0199, R0123, R0133, C0104, nonlocal-without-binding.
- Stats: skip (reports off; score uses only message-category counts +
  walker statement count).
- Exit-code bits: W→4, R→8, C→16 via MSG_TYPES_STATUS on DISPLAYED messages
  (pylinter.py:1245 `self.msg_status |= MSG_TYPES_STATUS[...]`).

================================================================================
# 11. Open questions
================================================================================

1. W0177 `_is_float_nan` calls `node.inferred()` catching only
   AttributeError — an InferenceError there crashes the module walk into
   F0002. Need a corpus probe to see if any target hits this (likely not);
   decide whether to replicate the crash or treat as unreachable.
2. `_check_typevar` reads `kw.value.value` — non-Const variance kwargs
   (`covariant=FLAG`) raise AttributeError → F0002. Same decision needed.
3. The exact interleaving of these seven "basic" checkers' callbacks with
   other checkers (variables, refactoring, …) for shared node types comes
   from the empirical prepared-order dump (notes/02); re-verify
   visit_functiondef order BasicErrorChecker→BasicChecker→NameChecker→
   DocStringChecker→FunctionChecker against `harness` order dump before
   relying on §8.2.
4. `_name_holds_generator` uses `test.frame().lookup` — confirm pyinfer's
   scope_lookup returns the (scope, stmts) pair with identical stmt
   ordering for the all()/len==1 logic.
5. NameChecker module-scope variable path calls `Module.igetattr(name)`
   which can raise InferenceError if the name is missing from locals
   (shouldn't happen for an AssignName in that module, but `del X` after
   assignment removes it from locals? astroid keeps AssignName+DelName in
   locals — believed safe). Probe with a `X = 1; del X` corpus case.
6. `frame.is_dataclass` (Final-in-dataclass → class_attribute) is set by
   astroid's dataclass brain — pyast must expose this flag.
7. Comprehension nodes have no lineno of their own in some astroid builds;
   W0125's comprehension/generator-call arm reports on the Comprehension
   node — verify pyast's fromlineno fallback (first-child chain) matches
   astroid for that node before trusting positions.
