# 09 — Design metrics (R0901–R0917) and duplicate-code (R0801)

Port-ready spec for two checkers, extracted from the pinned sources:

- `reference/pylint/pylint/checkers/design_analysis.py` (705 lines) — checker
  name `design`, class `MisdesignChecker`, messages R0901–R0904, R0911–R0917.
- `reference/pylint/pylint/checkers/symilar.py` (932 lines) — checker name
  `similarities`, class `SimilaritiesChecker` (multiple-inheritance wrapper
  around the standalone `Symilar` engine), message R0801 + report RP0801.
  (In pylint <4 this file was `similarities.py`/`similar.py`; pylint 4.0
  renamed it to `symilar.py`. Algorithm is the line-hashing one.)

Everything below was read from the pinned pylint 4.0.5 / astroid 4.0.4 trees;
all "empirically confirmed" claims were run against
`.venv-pylint/bin/pylint` with `PYTHONHASHSEED=0` on 2026-06-12.

Both checkers are *default-enabled* in vanilla pylint: none of their message
tuples carry a `default_enabled: False` flag or version bounds, so
`may_be_emitted()` is always true and every message starts enabled unless the
user disables it. NOTE: `crates/pycheckers/src/msgs.rs` currently has
`enabled: false` for R0801 and R0901–R0917 (lines 219–230) — that flag was
generated for the **-E harness config** (flags.txt disable list), not for
vanilla full-pylint defaults. For full-pylint mode all 12 messages are
default-ON.

Message inventory owned by this note (cross-checked against msgs.rs — all
present, templates byte-identical):

| msgid | symbol | template | confidence emitted |
|-------|--------|----------|--------------------|
| R0801 | duplicate-code | `Similar lines in %s files\n%s` | UNDEFINED |
| R0901 | too-many-ancestors | `Too many ancestors (%s/%s)` | UNDEFINED |
| R0902 | too-many-instance-attributes | `Too many instance attributes (%s/%s)` | UNDEFINED |
| R0903 | too-few-public-methods | `Too few public methods (%s/%s)` | UNDEFINED |
| R0904 | too-many-public-methods | `Too many public methods (%s/%s)` | UNDEFINED |
| R0911 | too-many-return-statements | `Too many return statements (%s/%s)` | UNDEFINED |
| R0912 | too-many-branches | `Too many branches (%s/%s)` | UNDEFINED |
| R0913 | too-many-arguments | `Too many arguments (%s/%s)` | UNDEFINED |
| R0914 | too-many-locals | `Too many local variables (%s/%s)` | UNDEFINED |
| R0915 | too-many-statements | `Too many statements (%s/%s)` | UNDEFINED |
| R0916 | too-many-boolean-expressions | `Too many boolean expressions in if statement (%s/%s)` | UNDEFINED |
| R0917 | too-many-positional-arguments | `Too many positional arguments (%s/%s)` | **HIGH** |

All are 'R' category → each displayed message sets exit bit 8
(`MSG_TYPES_STATUS['R'] = 8`) and increments `stats.refactor` (feeds the
score formula's `refactor` term). No `old_names` on any of them.

---------------------------------------------------------------------------
# PART A — MisdesignChecker (design_analysis.py)
---------------------------------------------------------------------------

## A.1 Checker identity and config options

`MisdesignChecker(BaseChecker)`, `name = "design"`, registered via module
`register()` (design_analysis.py:704-705). Pure AST checker: no
process_module/process_tokens, no close() side effects (inherits the no-op
`BaseChecker.close`, base_checker.py:219), no reports.

Options tuple (design_analysis.py:301-425) — these are the only config knobs
that gate behavior; defaults verbatim:

| option | default | type | used by |
|--------|---------|------|---------|
| `max-args` | 5 | int | R0913 |
| `max-positional-arguments` | 5 | int | R0917 |
| `max-locals` | 15 | int | R0914 |
| `max-returns` | 6 | int | R0911 |
| `max-branches` | 12 | int | R0912 |
| `max-statements` | 50 | int | R0915 |
| `max-parents` | 7 | int | R0901 |
| `ignored-parents` | `()` | csv (tuple of strings) | R0901 |
| `max-attributes` | 7 | int | R0902 |
| `min-public-methods` | 2 | int | R0903 |
| `max-public-methods` | 20 | int | R0904 |
| `max-bool-expr` | 5 | int | R0916 |
| `exclude-too-few-public-methods` | `[]` | regexp_csv (list of compiled `re.Pattern`) | R0903 |

CROSS-CHECKER CONFIG READ: `visit_functiondef` reads
`self.linter.config.ignored_argument_names` (design_analysis.py:547), an
option *owned by the variables checker*:
`variables.py:50: IGNORED_ARGUMENT_NAMES = re.compile("_.*|^ignored_|^unused_")`,
registered at variables.py:1298-1305 with `"type": "regexp"`. The default is
a **compiled regex**, never None, and exists in config even if the variables
checker is disabled (options are registered at checker registration, not
preparation). Matching uses `.match(arg.name)` (anchored at start).

## A.2 Walker integration — THE critical part for arbitrary --disable lists

The checker is prepared at all only if ≥1 of its 11 messages is enabled
(pylinter.py:588-598 `prepare_checkers`: `messages = {msg for msg in
checker.msgs if self.is_message_enabled(msg)}`; it has no reports).

`ASTWalker.add_checker` (ast_walker.py:42-69) registers each `visit_*` /
`leave_*` method only if `_is_method_enabled` (ast_walker.py:37-40): a method
decorated with `only_required_for_messages(...)` (utils.py:480-501, which
just sets `func.checks_msgs = messages`) is registered iff **any** listed
message is enabled (`is_message_enabled` resolves symbols, so this is the
config-level state). Undecorated methods are always registered. Then:

```python
# ast_walker.py:64-69
visit_default = getattr(checker, "visit_default", None)
if visit_default:
    for cls in nodes.ALL_NODE_CLASSES:
        cid = cls.__name__.lower()
        if cid not in vcids:
            visits[cid].append(visit_default)
```

i.e. `visit_default` is attached to every astroid node class **for which this
checker did not register a specific (enabled) visit method**. MisdesignChecker
is the only stock checker with a `visit_default`
(design_analysis.py:640-645):

```python
def visit_default(self, node: nodes.NodeNG) -> None:
    if node.is_statement:
        self._inc_all_stmts(1)
```

Method gating map (decorators quoted from source):

- `visit_classdef` (design_analysis.py:447-452): gated on
  `{"too-many-ancestors","too-many-instance-attributes",
  "too-few-public-methods","too-many-public-methods"}` (R0901,R0902,R0903,R0904).
- `leave_classdef` (483): gated on `{"too-few-public-methods",
  "too-many-public-methods"}` (R0903,R0904) — subset of visit's set, so
  leave registered ⇒ visit registered.
- `visit_functiondef` / `visit_asyncfunctiondef` (529-537, alias at 596 —
  same function object, same `checks_msgs`): gated on
  `{"too-many-return-statements","too-many-branches","too-many-arguments",
  "too-many-locals","too-many-positional-arguments","too-many-statements",
  "keyword-arg-before-vararg"}`. **NOTE the stray `"keyword-arg-before-vararg"`
  (W1113)** — that message is owned by typecheck.py:382-385 and never emitted
  here, but `--disable=all --enable=keyword-arg-before-vararg` still registers
  design's visit_functiondef (affecting R0915 accounting; harmless since no
  design message can be emitted then, but the stmt/return stacks still run).
- `leave_functiondef` / `leave_asyncfunctiondef` (598-604, alias 632): gated
  on `{"too-many-return-statements","too-many-branches","too-many-arguments",
  "too-many-locals","too-many-statements"}` (R0911,R0912,R0913,R0914,R0915 —
  NOT R0917, NOT W1113). Subset of visit's set ⇒ never pops an empty stack.
- `visit_if` (657): gated on `{"too-many-boolean-expressions",
  "too-many-branches"}` (R0916, R0912).
- UNGATED (always registered when checker prepared): `visit_return` (634),
  `visit_default` (640), `visit_try` (647), `visit_while` (685),
  `visit_for` (= visit_while, alias at 692), `visit_match` (694).

### A.2.1 Statement-counting coupling (bug-for-bug requirement)

Because `visit_default` only fires for node classes without a registered
specific visitor, **the R0915 statement count depends on which other design
messages are enabled**:

- If R0912 and R0916 are both disabled → `visit_if` unregistered → If nodes
  fall through to `visit_default` → an `if/else` counts **1** statement
  instead of `branches` (1, or 2 with a non-elif else).
  EMPIRICALLY CONFIRMED: `def g(x): if x: a=1 else: a=2; return a` with
  `--max-statements=2`:
  - `--enable=too-many-statements,too-many-branches` → `R0915: Too many
    statements (5/2)` (1 initial + 2 if-branches + 2 assigns)
  - `--enable=too-many-statements` only → `(4/2)` (1 + 1 + 2).
- If R0901–R0904 are all disabled → `visit_classdef` unregistered →
  ClassDef statements inside a function fall to `visit_default` (+1);
  when registered, a ClassDef contributes **0** statements.
- Same for FunctionDef/AsyncFunctionDef: when `visit_functiondef` is
  registered, a nested `def` contributes 0 statements to enclosing frames
  (the new frame starts at 1 for itself, see A.9); when unregistered
  (none of its 7 gate messages enabled — then no design message except
  R0916/R0901-R0904 can fire anyway), it would contribute +1 via default.

Per-node statement contribution table (checker prepared, all design messages
enabled — the common case):

| node class | handler | stmts added to all open frames | branches |
|------------|---------|-------------------------------|----------|
| FunctionDef/AsyncFunctionDef | visit_functiondef | 0 (pushes new frame initialized to **1**) | — |
| ClassDef | visit_classdef | 0 | — |
| Return | visit_return | **0** | — (increments returns) |
| If | visit_if | `branches` (1; 2 if orelse non-elif) | same |
| Try | visit_try | `branches` = len(handlers)+bool(orelse)+bool(finalbody) | same |
| TryStar (`except*`) | **no handler → visit_default** | 1 | 0 |
| While | visit_while | **0** | 1 (+1 if orelse) |
| For | visit_for (=visit_while) | **0** | 1 (+1 if orelse) |
| AsyncFor | **no handler → visit_default** (cid `asyncfor` ≠ `for`) | 1 | 0 |
| Match | visit_match | 1 | len(cases) |
| ExceptHandler | visit_default (is_statement=True!) | 1 | 0 |
| With/AsyncWith, Assign, AnnAssign, AugAssign, Expr, Assert, Raise, Pass, Break, Continue, Delete, Global, Nonlocal, Import, ImportFrom, TypeAlias | visit_default | 1 each | 0 |
| everything else (expressions, Arguments, Decorators, MatchCase, …) | visit_default but `is_statement` False | 0 | 0 |

astroid statement classes = subclasses of `_base_nodes.Statement`
(_base_nodes.py:48-54, `is_statement = True`; default False at
node_ng.py:59): Assert, Assign, AnnAssign, AugAssign, Break, Continue,
Delete, Expr, Global, If, Import, ImportFrom (via ImportNode,
_base_nodes.py:129), Nonlocal, Pass, Raise, Return, Try, TryStar, TypeAlias,
While, Match, For (+AsyncFor), With (+AsyncWith), **ExceptHandler**
(node_classes.py:2584-2586), FunctionDef (+AsyncFunctionDef)
(scoped_nodes.py:1072-1077), ClassDef (scoped_nodes.py:1807-1808).
Module is NOT a statement.

EMPIRICALLY CONFIRMED double-count: `try:/a=1/except ValueError:/a=2/
finally:/a=3` in a function = **7** statements with --max-statements=1 →
`(7/1)`: 1 (frame init) + 2 (visit_try: 1 handler + finalbody) + 1
(ExceptHandler via visit_default) + 3 assigns.

NOTE `nodes.ALL_NODE_CLASSES` (astroid/nodes/__init__.py) includes
non-concrete entries (NodeNG, Pattern, ComprehensionScope, LocalsDictNodeNG,
BaseContainer, EmptyNode, EvaluatedObject, Unknown, DictUnpack, const_factory
…). Registration by lowercase class name is harmless for them; for the port
only the table above matters.

### A.2.2 _inc_all_stmts is stack-wide (nested-function leakage)

```python
# design_analysis.py:443-445
def _inc_all_stmts(self, amount: int) -> None:
    for i, _ in enumerate(self._stmts):
        self._stmts[i] += amount
```

`self._stmts` is a stack of counters, one per *currently open*
FunctionDef/AsyncFunctionDef (pushed at visit, popped at leave). Every
statement increments **all** open frames, so statements inside a nested
function also count toward the enclosing function's R0915. By contrast
`_branches` is keyed by `node.scope()` (design_analysis.py:699-701) so
branch counts do NOT leak outward (an If inside a nested def accrues to the
nested def only; an If directly in a class body accrues to the ClassDef key,
which is never reported; module-level Ifs accrue to Module, never reported).
`_returns` is also per-frame only (`self._returns[-1] += 1`).

## A.3 open() state (design_analysis.py:433-441)

```python
self.linter.stats.reset_node_count()   # resets node_count{function,klass,method,module}=0
self._returns = []
self._branches = defaultdict(int)      # keyed by scope node
self._stmts = []
self._exclude_too_few_public_methods = self.linter.config.exclude_too_few_public_methods
```

`reset_node_count` (linterstats.py:290-292) zeroes counters that the basic
checker increments; only relevant to reports/--verbose, not to message
output. State persists across modules within a run (the stacks naturally
empty at module end since walk is balanced).

## A.4 R0901 too-many-ancestors — visit_classdef (453-465)

```python
parents = _get_parents(node,
    STDLIB_CLASSES_IGNORE_ANCESTOR.union(self.linter.config.ignored_parents))
nb_parents = len(parents)
if nb_parents > self.linter.config.max_parents:           # strict >, default 7
    self.add_message("too-many-ancestors", node=node,
                     args=(nb_parents, self.linter.config.max_parents))
```

`_get_parents_iter(node, ignored_parents)` (design_analysis.py:246-279):
explicit work-list DFS over `ancestors(recurs=False)`:

```python
parents: set[ClassDef] = set()
to_explore = list(node.ancestors(recurs=False))
while to_explore:
    parent = to_explore.pop()              # LIFO
    if parent.qname() in ignored_parents:
        continue                            # skip it AND don't explore its bases
    if parent not in parents:               # identity/equality on ClassDef nodes (default object identity hash)
        yield parent
        parents.add(parent)
        to_explore.extend(parent.ancestors(recurs=False))
```

Count = number of **unique ClassDef nodes** reachable through non-ignored
parents. An ignored class is excluded *and* its ancestors are unreachable
through it (but still counted if reachable via another base). Iteration
order is irrelevant to the count (set semantics); only `len` is used.

`ClassDef.ancestors(recurs=False)` (astroid scoped_nodes.py:2167-2211):
- `yielded = {self}`; if `not self.bases and qname != "builtins.object"`:
  yield the builtins `object` ClassDef and return (so a bare `class A:` has
  exactly one direct ancestor: object).
- else for each base expression in source order: `stmt.infer(context)`;
  non-ClassDef results: `bases.Instance` → unwrap `._proxied`, anything else
  skipped; `baseobj.hide` skipped; dedup via `yielded`. `InferenceError` on a
  base → that base silently contributes nothing (conservatism bail-out).
  With `recurs=False` only the direct (inferred) bases are yielded.
- Note: with recurs=False, `object` is NOT implicitly added when bases exist
  but is normally reached transitively by `_get_parents_iter` exploring up.

`STDLIB_CLASSES_IGNORE_ANCESTOR` (design_analysis.py:100-181): frozenset of
qnames — builtins.object/tuple/dict/list/set, **"bulitins.frozenset"
(TYPO IN PYLINT — builtins.frozenset is therefore NOT ignored; keep the
typo)**, 9 `collections.*` (ChainMap, Counter, OrderedDict, UserDict,
UserList, UserString, defaultdict, deque, namedtuple), 22
`_collections_abc.*` (Awaitable, Coroutine, AsyncIterable, AsyncIterator,
AsyncGenerator, Hashable, Iterable, Iterator, Generator, Reversible, Sized,
Container, Collection, Set, MutableSet, Mapping, MutableMapping,
MappingView, KeysView, ItemsView, ValuesView, Sequence, MutableSequence,
ByteString), 40 `typing.*` (Tuple, List, Dict, Set, FrozenSet, Deque,
DefaultDict, OrderedDict, Counter, ChainMap, Awaitable, Coroutine,
AsyncIterable, AsyncIterator, AsyncGenerator, Iterable, Iterator, Generator,
Reversible, Container, Collection, AbstractSet, MutableSet, Mapping,
MutableMapping, Sequence, MutableSequence, ByteString, MappingView,
KeysView, ItemsView, ValuesView, ContextManager, AsyncContextManager,
Hashable, Sized, NamedTuple, TypedDict), plus
`typing_extensions.TypedDict`. Read the source block verbatim when porting —
the exact strings matter (e.g. `_collections_abc.` prefix, NOT
`collections.abc.`, because qname comes from the real defining module).

`ignored-parents` (default `()`) is a csv of **qualified names** unioned in.

Message: node=ClassDef → position = astroid `position` (the
`class Name` keyword span; line = position.lineno, col = position.col_offset
per pylinter.py:1212-1221). args `(nb_parents, max_parents)`.

## A.5 R0902 too-many-instance-attributes — visit_classdef (467-481)

```python
root = node.root()
filtered_attrs = [k for (k, v) in node.instance_attrs.items() if v[0].root() is root]
if len(filtered_attrs) > self.linter.config.max_attributes:   # strict >, default 7
    self.add_message("too-many-instance-attributes", node=node,
                     args=(len(filtered_attrs), self.linter.config.max_attributes))
```

- `instance_attrs` is the astroid dict `attr name -> [AssignAttr nodes]`,
  populated at build time by `AstroidBuilder.delayed_assattr`
  (astroid builder.py:248-284): for every delayed `self.x = ...`-style
  AssignAttr, infer `node.expr`; for `Instance`/`ExceptionInstance` results
  (narrow `type(...) in {bases.Instance, objects.ExceptionInstance}` check)
  unwrap `._proxied` and append to that ClassDef's `instance_attrs[attrname]`
  (subject to `_can_assign_attr`); functions get `instance_attrs`; other
  scoped nodes get `locals`. Inference can later MUTATE instance_attrs with
  cross-module entries (astroid bug #2273), hence the filter:
- the filter keeps an attribute name iff the FIRST recorded assignment
  node's root module **is** (identity) the class's own module. Count =
  number of surviving names. Insertion order irrelevant (len only).
- pyinfer note: prylint must reproduce whichever instance_attrs population
  its builder does; the `v[0].root() is root` filter then makes most
  inference-time pollution invisible, but an attribute whose *first* entry
  is foreign is dropped entirely (order within the list matters).

Message: node=ClassDef, args `(len(filtered_attrs), max_attributes)`.

## A.6 R0904 / R0903 — leave_classdef (483-527)

Runs at leave (after the subtree walk). Order of checks matters:

1. `my_methods = sum(1 for method in node.mymethods() if not
   method.name.startswith("_"))`. If `my_methods >
   config.max_public_methods` (strict >, default 20) → emit
   **too-many-public-methods** args `(my_methods, max_public_methods)`,
   node=ClassDef. (Despite the docstring saying "less than", R0904 uses ONLY
   methods defined in this class body, not ancestors.)
2. R0903 exclusion #1 (design_analysis.py:505-511): if `node.type == "class"`
   and `exclude-too-few-public-methods` non-empty: for every
   `ancestor in node.ancestors()` (full recursive ancestors), if any
   configured pattern `.match(ancestor.qname())` → **return** (skips R0903
   only; R0904 already emitted). Default config `[]` → skipped.
3. R0903 exclusion #2 (515-516): `if node.type != "class" or
   _is_exempt_from_public_methods(node): return`.
4. `all_methods = _count_methods_in_class(node)`; if `all_methods <
   config.min_public_methods` (strict <, default 2) → emit
   **too-few-public-methods** args `(all_methods, min_public_methods)`,
   node=ClassDef.

### astroid primitives

`ClassDef.mymethods()` (astroid scoped_nodes.py:2606-2614): iterates
`self.values()` and yields members that are `FunctionDef` instances
(AsyncFunctionDef subclasses FunctionDef → included; Lambda assigned to a
name is NOT a FunctionDef → excluded). `values()`
(mixin.py:169-178) is `[self.locals[key][0] for key in locals.keys()]` —
**only the first binding of each name is examined**: if a name is first
bound to a non-function (`x = 1` then `def x():`), it is not counted; a name
re-defined N times counts once. Order = locals insertion order (source
order, with delayed import resort quirks — irrelevant here, only counts).

`ClassDef.methods()` (2592-2604): chain of `self` + `self.ancestors()`
(recursive, inference-based, dedup'd), yielding each `mymethods()` member
whose **name** hasn't been seen yet (first definition wins, subclass
shadows ancestor).

`ClassDef.type` (property → `_class_type`, scoped_nodes.py:1750-1785,
cached in `klass._type`):
- `"metaclass"` if `_is_metaclass(klass)` (1714-1747: name == "type", or any
  inferred base (recursively) is/derives a metaclass; bases inferred to
  `Instance` → returns False ("not abstract"); InferenceError → continue);
- elif `klass.name.endswith("Exception")` → `"exception"` (NAME-based!);
- else recurse over `ancestors(recurs=False)`: first base whose
  `_class_type` ≠ "class" propagates its `base.type` (with a guard that a
  "metaclass" result doesn't propagate unless klass itself can be one);
  ancestor-qname loop guard returns "class".
So `class Foo(Exception)` → "exception" via base name `Exception`;
`class Bar(type)` → "metaclass". Only `type == "class"` proceeds to R0903.

### _count_methods_in_class (design_analysis.py:236-243)

```python
all_methods = sum(1 for method in node.methods() if not method.name.startswith("_"))
for method in node.mymethods():
    if SPECIAL_OBJ.search(method.name) and method.name != "__init__":
        all_methods += 1
```

`SPECIAL_OBJ = re.compile("^_{2}[a-z]+_{2}$")` (design_analysis.py:90) —
exactly two leading and trailing underscores around one-or-more lowercase
ascii letters (`__init__`, `__str__`; NOT `__init2__`, NOT `__INIT__`).
So: public (non-underscore) methods incl. inherited + own dunders except
`__init__`.

### _is_exempt_from_public_methods (design_analysis.py:184-219)

- For each `ancestor in node.ancestors()` (recursive): exempt if
  `is_enum(ancestor)` — utils.py:1744-1745: `node.name == "Enum" and
  node.root().name == "enum"` (so anything deriving enum.Enum, incl.
  IntEnum chains, since Enum itself appears among recursive ancestors) — or
  if `ancestor.qname() in ("typing.NamedTuple", "typing.TypedDict",
  "typing_extensions.TypedDict")`.
- Decorator-based dataclass/attrs exemption: `if not node.decorators:
  return False`. `root_locals = set(node.root().locals)` (module-level
  names). For each decorator node (unwrap `Call.func`), match
  `Name(name=X)` or `Attribute(attrname=X)` (anything else skipped):
  - X ∈ `{"dataclass", "attrs"}` AND (root_locals ∩ {"dataclass","attrs"}
    non-empty OR `"dataclasses"` ∈ root_locals) → exempt.
  - X ∈ `{"define", "frozen"}` AND (root_locals ∩ {"define","frozen"}
    non-empty OR `"attrs"` ∈ root_locals) → exempt.
  Pure name heuristics — no inference. E.g. `@attr.s` does NOT exempt
  (attrname "s"); `@dataclasses.dataclass` exempts iff `dataclasses`
  imported at module level (it is, by definition of the attribute access…
  unless imported inside a function).

## A.7 R0913 / R0917 / R0914 — visit_functiondef (538-594)

Runs for FunctionDef and AsyncFunctionDef. Verbatim core:

```python
self._returns.append(0)
args = node.args.args + node.args.posonlyargs + node.args.kwonlyargs
pos_args = node.args.args + node.args.posonlyargs
ignored_argument_names = self.linter.config.ignored_argument_names
if args is not None:                       # always True (lists); else-branch dead code
    ignored_args_num = 0
    if ignored_argument_names:
        ignored_pos_args_num = sum(1 for arg in pos_args
                                   if ignored_argument_names.match(arg.name))
        ignored_kwonly_args_num = sum(1 for arg in node.args.kwonlyargs
                                      if ignored_argument_names.match(arg.name))
        ignored_args_num = ignored_pos_args_num + ignored_kwonly_args_num
    argnum = len(args) - ignored_args_num
    if argnum > self.linter.config.max_args:
        self.add_message("too-many-arguments", node=node,
                         args=(len(args), self.linter.config.max_args))
    pos_args_count = len(args) - len(node.args.kwonlyargs) - ignored_pos_args_num
    if pos_args_count > self.linter.config.max_positional_arguments:
        self.add_message("too-many-positional-arguments", node=node,
            args=(pos_args_count, self.linter.config.max_positional_arguments),
            confidence=HIGH)
else:
    ignored_args_num = 0
locnum = len(node.locals) - ignored_args_num
if "_" in node.locals:
    locnum -= 1
if locnum > self.linter.config.max_locals:
    self.add_message("too-many-locals", node=node,
                     args=(locnum, self.linter.config.max_locals))
self._stmts.append(1)
```

Counting rules:
- "arguments" = normal + positional-only + keyword-only. `*args` and
  `**kwargs` are NOT counted (not in those three lists). `self`/`cls` ARE
  counted (no bound-method exemption — EMPIRICALLY CONFIRMED:
  `def m(self,a,b,c,d,e)` → `R0913 (6/5)` and `R0917 (6/5)`).
- ignored args: names matching `ignored-argument-names` (default
  `_.*|^ignored_|^unused_`, `.match` = prefix-anchored) are subtracted from
  the *threshold comparison* for R0913 — but **the reported count is
  `len(args)`, NOT `argnum`** (design_analysis.py:566 — bug-for-bug: with
  ignored args present, the displayed numerator includes them while the
  trigger test excludes them; you can get `(9/5)` only when
  `9 - ignored > 5`).
- R0917: `pos_args_count = len(args) - len(kwonlyargs) - ignored_pos_args_num`
  (= non-ignored positional-capable args) compared and reported with the
  SAME value (no len/argnum mismatch here). confidence=HIGH (matters only
  for `--confidence` filtering; the default `confidence` config value is
  empty → no filtering. is_message_enabled checks
  `confidence.name not in config.confidence` only when that list is
  non-empty — message_state_handler.py:334-335 guards with
  `if confidence and ...` where UNDEFINED is falsy-named member; in 4.0.5
  config default is all confidence levels, so no effect by default).
- R0914 locals: `len(node.locals)` of the FunctionDef scope. astroid
  FunctionDef.locals contains: every parameter name **including vararg and
  kwarg** (vararg/kwarg set at rebuilder.py:592-597 mapping to the
  Arguments node; plain/posonly/kwonly args via `visit_arg` → AssignName →
  `_save_assignment` → scope set_local), plus every name assigned in the
  function body (Assign/AnnAssign(with value or not — AnnAssign target
  AssignName always recorded)/AugAssign/For targets/With-as/except-as
  (name kept after scope ends)/walrus at function depth), nested
  `def`/`class` names, imports, `global`-declared names are routed to
  module locals NOT function locals (rebuilder.py:494-501), comprehension
  targets do NOT count (own scope in py3), lambda params don't count
  (own scope), and **PEP 695 type params DO count** (EMPIRICALLY CONFIRMED
  on astroid 4.0.4: `def f[T](a, b): x = 1; return x` →
  `f.locals == ['T', 'a', 'b', 'x']`).
  Names only `del`-ed or only read are not locals; DelName *is* recorded
  via _save_assignment (a `del x` after assignment doesn't change the key
  count; `del` of a never-assigned name still creates the key).
- subtract `ignored_args_num` (note: computed from pos+kwonly args only —
  a `**kwargs` named `_foo` is in locals but never in ignored_args_num);
  subtract 1 more if literally `"_"` is a local.
- Both R0913/R0917/R0914 trigger only on strict `>`.

Position for all function messages: node=FunctionDef → astroid position
(`def name` span; e.g. `2:4` for a method indented 4 — confirmed above).

## A.8 R0911 too-many-return-statements

`visit_return` (634-638, ungated): `if not self._returns: return` (return
outside function — module-level Return is a syntax error normally, guard is
for robustness) else `self._returns[-1] += 1`. Counts every `Return` node in
the function's own frame (nested functions have their own frame). `yield`
does NOT count (despite max-returns help text saying "return / yield").
Return in finally/except/loops all count equally.

`leave_functiondef` (605-615): `returns = self._returns.pop(); if returns >
config.max_returns` (strict >, default 6) → emit with args
`(returns, max_returns)`, node=FunctionDef.

## A.9 R0912 too-many-branches / R0915 too-many-statements

Branch increments (all keyed by `node.scope()` via `_inc_branch`,
design_analysis.py:699-701):
- `visit_try` (647-655): `branches = len(node.handlers) + bool(orelse) +
  bool(finalbody)`; also `_inc_all_stmts(branches)`.
- `visit_if` (658-668): `_check_boolean_expressions(node)` first; then
  `branches = 1`; `if node.orelse and not (len(node.orelse) == 1 and
  isinstance(node.orelse[0], nodes.If)): branches += 1` (elif chains: each
  If node counts its own 1; a real `else` adds 1); `_inc_branch`;
  `_inc_all_stmts(branches)`.
- `visit_while` / `visit_for` (685-692): `branches = 1 + bool(orelse)`;
  `_inc_branch` only — NO stmt increment.
- `visit_match` (694-697): `_inc_all_stmts(1)`; `_inc_branch(node,
  len(node.cases))`.
- TryStar / AsyncFor: no handler → no branches (visit_default counts 1 stmt).

`leave_functiondef` (616-622): `branches = self._branches[node]` —
defaultdict read; keyed by the FunctionDef itself (an If directly in the
function body has `scope()` == the function). `> max_branches` (default 12)
→ args `(branches, max_branches)`.

Statements: frame pushed `self._stmts.append(1)` at visit (the function
"costs" 1 in its own frame); `_inc_all_stmts` adds to every open frame (see
A.2.2 for the full per-node table and coupling). At leave:
`stmts = self._stmts.pop(); if stmts > config.max_statements` (default 50)
→ args `(stmts, max_statements)`.

Emission order within leave_functiondef: R0911, then R0912, then R0915 —
all node=FunctionDef, so they share a position; ordering matters for
byte-identical output. visit-time messages (R0913, R0917, R0914 — in that
order) are emitted at visit, i.e. BEFORE any messages produced inside the
function body, and leave-time ones AFTER. Note walker callback order across
checkers per prepared-checker order was already extracted in earlier notes;
within one callback the source order above is definitive.

## A.10 R0916 too-many-boolean-expressions

`_check_boolean_expressions` (670-683), called from visit_if only (so only
`if`/`elif` tests count — NOT while-tests, NOT ternaries, NOT assert):

```python
condition = node.test
if not isinstance(condition, nodes.BoolOp): return
nb_bool_expr = _count_boolean_expressions(condition)
if nb_bool_expr > self.linter.config.max_bool_expr:    # strict >, default 5
    self.add_message("too-many-boolean-expressions", node=condition,
                     args=(nb_bool_expr, self.linter.config.max_bool_expr))
```

`_count_boolean_expressions` (222-233): recursive over
`bool_op.get_children()` (= its `values`): each child that is itself a
BoolOp recurses, every other child counts 1. `a and (b or c or (d and e))`
→ 5. A parenthesized BoolOp child of a different op (`a and not (b or c)`)
is a UnaryOp child → counts 1.
**Report node = the BoolOp condition**, not the If: position is the
condition's span (BoolOp has no `position` → fromlineno/col_offset/
end_lineno/end_col_offset per pylinter.py:1222-1230).

---------------------------------------------------------------------------
# PART B — SimilaritiesChecker / Symilar (symilar.py)
---------------------------------------------------------------------------

## B.1 Identity, options, preparation

`SimilaritiesChecker(BaseRawFileChecker, Symilar)` (symilar.py:741),
`name = "similarities"`, msgs = {R0801} (718-726), reports =
`(("RP0801", "Duplication", report_similarities),)` (803). Registered via
`register()` (877-878).

Options (symilar.py:756-802):

| option | default | type |
|--------|---------|------|
| `min-similarity-lines` | 4 (`DEFAULT_MIN_SIMILARITY_LINE`, line 57) | int |
| `ignore-comments` | **True** | yn |
| `ignore-docstrings` | **True** | yn |
| `ignore-imports` | **True** | yn |
| `ignore-signatures` | **True** | yn |

(The standalone `symilar` CLI defaults all four ignore-flags to False —
store_true args, Run() at 881-928 — but the *checker* defaults are True.)

`__init__` (805-814) calls `Symilar.__init__` with the linter config values;
`Symilar.__init__` (338-357) detects pylint mode via
`isinstance(self, BaseChecker)` and sets `self.namespace =
self.linter.config` then re-assigns the five values onto it (no-op writes
back into the config namespace); `self.linesets = []`.

Preparation: prepare_checkers (pylinter.py:588-598) first calls
`disable_reporters()` when `config.reports` is falsy (the default,
`--reports=n`), which disables RP0801 — so with default reports the checker
is prepared **iff R0801 is enabled**. With `--reports=y` and R0801 disabled,
the checker IS still prepared (linesets collected, close() runs, stats
computed) but `add_message("R0801", ...)` is filtered at emission time and
only the RP0801 table is printed. `--disable=RP0801` /
`--disable=all` paths: `_get_messages_to_set` handles `rp*` ids by calling
`disable_report` (message_state_handler.py:126-132); `disable("all")`
expands only message categories, NOT reports.

If `min-similarity-lines` is 0: the standalone `run()` short-circuits
(symilar.py:392-396) but the **checker's close() has no such guard** —
`hash_lineset` with min_common_lines=0 → `shifted_lines = []` →
`zip(*[])` = zip() → one infinite... actually `zip()` with no args yields
nothing → no chunks → no matches → no messages. (Edge: don't special-case.)

## B.2 Lifecycle

- `open()` (816-819): `self.linesets = []`;
  `self.linter.stats.reset_duplicated_lines()` (linterstats.py:276-279 →
  nb_duplicated_lines=0, percent_duplicated_lines=0.0).
- `process_module(node)` (821-839): runs once per linted module, inside
  `_check_astroid_module`'s raw-checker loop (pylinter.py:1100-1101) —
  i.e. AFTER pragma processing (`process_tokens`, line 1096) and BEFORE the
  AST walk (1105); only for `node.pure_python`; skipped entirely when
  `self._ignore_file` (file-level `# pylint: skip-file`). Body:
  `with node.stream() as stream: self.append_stream(self.linter.current_name,
  stream, node.file_encoding)`. (The current_name-None DeprecationWarning
  branch is unreachable in normal runs.) `Module.stream()` returns
  `io.BytesIO(file_bytes)` or `open(file, 'rb')` (astroid
  scoped_nodes.py:287-296), i.e. a binary stream; `file_encoding` is set by
  `AstroidBuilder.file_build` → `_post_build` (astroid builder.py:163) from
  `open_source_file`'s detected encoding (PEP263/BOM, default utf-8).
- `append_stream(streamid, stream, encoding)` (359-390): binary stream →
  `decoding_stream(stream, encoding).readlines` (pylint utils.py:139-149:
  `codecs.getreader(encoding or sys.getdefaultencoding())(stream, "strict")`,
  LookupError → default encoding reader). `try: lines = readlines() except
  UnicodeDecodeError: lines = []` (conservatism bail-out: undecodable file
  contributes an empty LineSet — but still appears in `total` as 0 lines).
  Appends `LineSet(streamid, lines, <4 ignore flags>,
  line_enabled_callback=self.linter._is_one_message_enabled)` (the bound
  method exists since SimilaritiesChecker has a linter). NOTE streamid =
  **module dotted name** (`pkg.mod`), not a path — that's what appears in
  `==pkg.mod:[i:j]` lines.
- `close()` (841-860): computes and emits — detailed in B.7. Called from
  `_astroid_module_checker` context exit (pylinter.py:993-996) after ALL
  files are linted, in `reversed(prepare_checkers())` order; see B.8.

The per-run lineset list order = module processing order = FileItem order
(single job). This order determines pair enumeration and tie-breaking.

## B.3 stripped_lines — line normalization (symilar.py:566-657)

Input: the file's decoded lines (with trailing `\n`). Produces
`list[LineSpecifs(line_number: 0-based int, text: str)]` — only non-empty
stripped lines, in order.

Phase 1 — AST-derived ignore set (only if ignore_imports or
ignore_signatures): `tree = astroid.parse("".join(lines))` — a FRESH parse
of the same text (file already parsed fine, so this should succeed; a
failure here would propagate → `_lint_file` wraps in AstroidError →
F0002 astroid-error for the file — never observed in corpora).
- ignore_imports: for every `Import`/`ImportFrom` node anywhere in the tree
  (`tree.nodes_of_class`), add `range(node.lineno, (node.end_lineno or
  node.lineno) + 1)` — 1-based, inclusive of the whole multi-line import.
- ignore_signatures: `_get_functions` (598-616) recursively collects
  FunctionDef/AsyncFunctionDef by walking **only `tree.body` of
  Module/ClassDef/FunctionDef/AsyncFunctionDef** — functions nested inside
  `if`/`try`/`with` blocks are NOT collected (their signatures stay).
  For each collected func add `range(func.lineno, func.body[0].lineno if
  func.body else func.tolineno + 1)` — from the `def` line (astroid
  fromlineno = def keyword line; decorators NOT included) up to but NOT
  including the first body statement's line. Multi-line signatures fully
  covered; a same-line body (`def f(): return 1`) → empty range → nothing
  ignored.

Phase 2 — per line, `for lineno, line in enumerate(lines, start=1)`, in
this exact order:
1. pragma callback: `if line_enabled_callback is not None and not
   line_enabled_callback("R0801", lineno): continue` — the line is dropped
   from the stripped collection entirely (lines around it become adjacent
   in stripped space — windows can span a disabled region!). This is
   `_is_one_message_enabled` (message_state_handler.py:279-313) evaluated
   against the CURRENT file's FileState: per-line pragma states first,
   KeyError → past-EOF fallback / `self._msgs_state.get("R0801", True)`
   config state. This is the mechanism by which `# pylint:
   disable=duplicate-code` pragmas (file- or block-scoped) remove a file's
   lines from similarity computation. (The close()-time emission check
   can't see per-file pragmas — see B.7.)
2. `line = line.strip()`.
3. ignore_docstrings state machine (637-648), BUG-COMPATIBLE:
   ```python
   if not docstring:
       if line.startswith(('"""', "'''")):
           docstring = line[:3]; line = line[3:]
       elif line.startswith(('r"""', "r'''")):
           docstring = line[1:4]; line = line[4:]
   if docstring:
       if line.endswith(docstring): docstring = None
       line = ""
   ```
   Quirks to replicate exactly: applies to ANY line starting with
   triple-quotes after strip (not just real docstrings — e.g. the closing
   line of a triple-quoted *string literal* that starts at column 0 will
   OPEN phantom docstring mode and blank following lines until one ends
   with the same quotes); one-liner `"""x"""` → consumed in one step;
   `"""x""" + y` → endswith fails → swallows subsequent lines; only `r`
   prefix recognized (not `b`, `f`, `R`); opener line itself always
   blanked.
4. ignore_comments: `line = line.split("#", 1)[0].strip()` — splits at the
   first `#` EVEN INSIDE STRING LITERALS (`x = "a#b"` → `x = "a`).
5. `if lineno in ignore_lines: line = ""` (imports/signatures).
6. `if line: strippedlines.append(LineSpecifs(text=line,
   line_number=LineNumber(lineno - 1)))` — note **0-based** line_number.

## B.4 LineSet and chunk hashing

`LineSet` (660-715): holds `name`, `_real_lines` (raw decoded lines),
`_stripped_lines`. `__len__` = `len(self._real_lines)` (TOTAL file lines —
used for the stats denominators). Ordering `__lt__` by name
(used for sorting couples in the standalone report only). `__hash__ =
id(self)`, `__eq__` compares `__dict__` — the id-hash is the source of the
close()-time nondeterminism (B.7).

`hash_lineset(lineset, min_common_lines=4)` (207-245):
- `lines = tuple(x.text for x in lineset.stripped_lines)`.
- sliding windows of `min_common_lines` consecutive stripped lines (via
  `zip(*[iter(lines[i:]) for i in range(M)])`) — a lineset with fewer than
  M stripped lines yields no chunks.
- For window starting at stripped index `i`:
  `start_linenumber = stripped_lines[i].line_number` (0-based real line);
  `end_linenumber = stripped_lines[i + M].line_number` — the real line of
  the stripped line AFTER the window — `except IndexError:
  stripped_lines[-1].line_number + 1`. Stored as
  `index2lines[i] = SuccessiveLinesLimits(start, end)` (end is mutable).
- `l_c = LinesChunk(lineset.name, i, *succ_lines)`;
  `hash2index[l_c].append(i)` (defaultdict(list); the dict KEY is the chunk
  object of the FIRST occurrence of that hash value).

`LinesChunk` (108-144): `self._hash = sum(hash(lin) for lin in lines)` —
**arithmetic sum (Python unbounded int, no wraparound) of CPython str
hashes** (siphash13 over the string's internal representation, keyed by
PYTHONHASHSEED=0's fixed secret). `__eq__`/`__hash__` use ONLY `_hash`:
equality is hash-sum equality. Consequences (port-relevant):
- two windows containing the same multiset of lines in DIFFERENT ORDER are
  "equal" (sum is order-insensitive). Such pairs are usually killed later by
  `filter_noncode_lines` (positional text comparison), but they DO enter
  `all_couples` and participate in `remove_successive` end-extension —
  faithful ports must keep sum-of-per-line-hash semantics (any 64-bit
  per-line hash preserves the order-insensitivity property; byte-identity
  with pylint additionally requires matching CPython collisions, which are
  ~2^-64 improbable — acceptable risk, but note PYTHONHASHSEED=0 siphash13
  would be needed for true bit-equality of the collision set);
- use ≥128-bit signed accumulation in Rust (no wrap; Python sums exactly).

## B.5 _find_common — pairwise matching (467-540)

For each ordered pair `(lineset1, lineset2)` from `_iter_sims` (542-548):
`for idx, lineset in enumerate(self.linesets[:-1]): for lineset2 in
self.linesets[idx+1:]` — i.e. all i<j pairs in lineset-list order;
**a file is never compared against itself**, so duplicate blocks WITHIN one
module never produce R0801.

1. `hash_to_index_k, index_to_lines_k = hash_lineset(lineset_k, M)`.
2. `hash_1 = frozenset(hash_to_index_1.keys()); hash_2 = ...;
   common_hashes = sorted(hash_1 & hash_2, key=lambda m: hash_to_index_1[m][0])`.
   CPython `frozenset.__and__` iterates the SMALLER operand and keeps ITS
   element objects (setobject.c set_intersection swaps so that it iterates
   the smaller; on equal sizes it iterates the RIGHT operand, i.e. hash_2).
   So the surviving representative LinesChunk objects — whose `_index`
   matters next — come from the operand with fewer distinct hashes, ties →
   from `hash_2`. The representative's `_index` = the first-occurrence
   stripped index of that hash within ITS lineset.
3. `for c_hash in sorted(common_hashes, key=operator.attrgetter("_index")):`
   — re-sorted by representative `_index` (stable, but `_index` values are
   unique within one lineset so the first sort is effectively dead code;
   final order = ascending first-occurrence index in the representative
   side). For each `(index_1, index_2)` in
   `itertools.product(hash_to_index_1[c_hash], hash_to_index_2[c_hash])`
   (both lists ascending):
   `all_couples[LineSetStartCouple(index_1, index_2)] =
   CplSuccessiveLinesLimits(copy.copy(index_to_lines_1[index_1]),
   copy.copy(index_to_lines_2[index_2]), effective_cmn_lines_nb=M)`.
   (`copy.copy` because remove_successive mutates `.end`.)
   `LineSetStartCouple.__hash__ = hash(fst) + hash(snd)` (line 194-195) —
   int hashes, so (1,2) and (2,1) collide but __eq__ disambiguates.
4. `remove_successive(all_couples)` (248-288): iterate a snapshot
   `tuple(all_couples.keys())` in INSERTION ORDER; for each couple, while
   `couple.increment(1)` (= (i+1, j+1)) is present: absorb it — copy its
   `first_file.end`/`second_file.end` into the head couple,
   `effective_cmn_lines_nb += 1`, queue for removal; continue with (i+2,…)
   etc.; then pop the queued keys (popping mid-iteration is why the
   while-loop re-tests against the live dict). ORDER DEPENDENCE: if a
   chain's middle couple is inserted BEFORE its head (possible when the
   middle's hash first occurs earlier in the representative lineset than
   the head's hash), the middle absorbs its tail first and gets popped when
   the head is processed, leaving the head with the FULL `.end`s but an
   UNDERCOUNTED `effective_cmn_lines_nb` (head absorbs the already-merged
   middle for only +1). Replicate insertion order exactly (steps 2-3) to
   reproduce this.
5. For each surviving `(couple, cmn_l)` in insertion order
   (520-540): build
   `Commonality(cmn_lines_nb=cmn_l.effective_cmn_lines_nb,
   fst_lset=lineset1, fst_file_start=cmn_l.first_file.start,
   fst_file_end=cmn_l.first_file.end, snd_lset=lineset2, snd_…)`; compute
   `eff_cmn_nb = filter_noncode_lines(lineset1, start_index_1, lineset2,
   start_index_2, nb_common_lines)` (291-322): take each side's stripped
   slice `[stindex : stindex + n]`, keep only lines matching
   `REGEX_FOR_LINES_WITH_CONTENT = re.compile(r".*\w+")` (i.e. containing
   at least one `\w` — bare `)` / `],` lines excluded), then
   `sum(l1 == l2 for l1, l2 in zip(...))` — positional equality count after
   filtering (the two filtered lists may be misaligned if the sides have
   noncode lines at different positions — replicate as-is).
   **Yield the Commonality iff `eff_cmn_nb > M` — STRICTLY GREATER.**
   EMPIRICALLY CONFIRMED: with default M=4, a 4-stripped-line identical
   block is NOT reported; 5+ is (`==a:[1:7]` example below had 6).

Note the yielded Commonality carries `cmn_lines_nb =
effective_cmn_lines_nb` (the merged stripped-window count), not
`eff_cmn_nb`; both line ranges come from the (possibly extended)
SuccessiveLinesLimits — start = 0-based real line of the first window line,
end = 0-based real line of the first stripped line AFTER the merged run
(or last stripped line + 1).

## B.6 _compute_sims — grouping & ordering (398-433)

```python
no_duplicates: defaultdict[int, list[set[LinesChunkLimits_T]]] = defaultdict(list)
for commonality in self._iter_sims():
    num = commonality.cmn_lines_nb
    ... # unpack
    duplicate = no_duplicates[num]
    for couples in duplicate:
        if (lineset1, start_1, end_1) in couples or (lineset2, start_2, end_2) in couples:
            break          # skip entirely — NOT merged into the existing set
    else:
        duplicate.append({(lineset1, start_1, end_1), (lineset2, start_2, end_2)})
sims = [(num, cpls) for num, ensembles in no_duplicates.items() for cpls in ensembles]
return sorted(sims, reverse=True)
```

- Dedup: within one `num` bucket, if either side's exact (lineset, start,
  end) triple already appears in any recorded pair, the new pair is dropped.
  So a block duplicated across files A, B, C yields pairs (A,B) and (A,C)?
  No — (A,B) recorded; (A,C) contains (A,…) → dropped; (B,C) contains
  (B,…) → dropped: **one message per duplicated block**, always
  `len(couples) == 2` files. (Triple-equality includes the lineset object.)
- Sort: tuples `(int, set)` with `reverse=True`. Tuple comparison: equal
  `num` → compares sets via `==` then `<`; `set.__lt__` is proper-subset —
  False for distinct 2-element sets — so equal-num entries keep their
  insertion order (CPython sort stability holds under reverse=True: ties
  are NOT reversed). Insertion order = pair-enumeration order
  (i<j lineset order) then all_couples order within a pair, flattened by
  ascending num-first-seen → effectively: descending `num`, ties in
  original discovery order.

## B.7 close() — emission (841-860)

```python
total = sum(len(lineset) for lineset in self.linesets)     # total real lines, all files
duplicated = 0
stats = self.linter.stats
for num, couples in self._compute_sims():
    msg = []
    lineset = start_line = end_line = None
    for lineset, start_line, end_line in couples:          # SET iteration order!
        msg.append(f"=={lineset.name}:[{start_line}:{end_line}]")
    msg.sort()
    if lineset:
        for line in lineset.real_lines[start_line:end_line]:
            msg.append(line.rstrip())
    self.add_message("R0801", args=(len(couples), "\n".join(msg)))
    duplicated += num * (len(couples) - 1)
stats.nb_duplicated_lines += int(duplicated)
stats.percent_duplicated_lines += float(total and duplicated * 100.0 / total)
```

Message text: first the sorted `==name:[start:end]` header lines
(lexicographic — deterministic), then the **rstripped real lines of
whichever couple the set iterated LAST** (slice uses 0-based start/end
real-line indices; `[1:7]` = file lines 2–7 1-based). Header + code joined
with `\n` and spliced into the template `Similar lines in %s files\n%s`
with `%s = len(couples)` (always 2, see B.6).

**CONFIRMED NONDETERMINISM IN PYLINT ITSELF**: `couples` is a set of
`(LineSet, int, int)` tuples; `LineSet.__hash__ = id(self)` → tuple hashes
depend on memory addresses → set iteration order varies run-to-run. When
the two regions' REAL lines differ (e.g. different trailing comments with
ignore-comments=y, different blank-line placement inside the merged range),
the displayed code block flips between files. Verified: 5 identical
invocations of `pylint --disable=all --enable=duplicate-code a.py b.py`
where the regions differ only by a comment produced `# AAA` twice and
`# BBB` three times. The `==` headers and everything else are stable.
Port policy decision required (see open questions); when the duplicate
regions' rstripped real lines are identical (the common case for true
copy-paste), any choice is byte-identical. Audit each corpus ground truth
for R0801 blocks where the paired regions' rstripped text differs — those
GT lines are not stable under re-runs of pylint itself.

Emission mechanics (`add_message` with NO node and NO line —
pylinter.py:1287-1319 → _add_one_message 1195-1285):
- enablement: `is_message_enabled("R0801", line=None)` →
  `_is_one_message_enabled` line-None path → `self._msgs_state.get(msgid,
  True)` — **config-level state only; per-file pragmas cannot suppress the
  close()-time emission** (they act earlier by dropping lines, B.3 step 1).
- location: `module = self.current_name`, `abspath = self.current_file` —
  whatever file was set LAST (single job: the last module that reached
  `_lint_file`, pylinter.py:813). `line or 1` → **line 1**, `col_offset or
  0` → 0, end_lineno/end_col_offset None.
  EMPIRICALLY CONFIRMED: `************* Module b` / `b.py:1:0: R0801: …`
  for inputs a.py, b.py. The reporter prints a fresh `************* Module`
  header iff the previous displayed message had a different module — if the
  last linted module already emitted messages, R0801 rides under its
  existing header; TextReporter linebreaks inside `msg` are printed raw.
- stats: each emitted R0801 increments refactor counts attributed to
  `current_name` (the last module) — affects the score footer denominator
  attribution but the global score only via total refactor count.
- `duplicated += num * (len(couples) - 1)` = num (couples==2); feeds
  `nb_duplicated_lines` / `percent_duplicated_lines` — ONLY visible via
  RP0801 (`--reports=y` "Duplication" table, report_similarities 729-737 /
  table_lines_from_stats checkers/__init__.py:59) and persistence pickles.
  `percent` uses `float(total and duplicated * 100.0 / total)` — 0.0 when
  total==0.
- Exit code: each displayed R0801 sets bit 8. CONFIRMED exit=8.

## B.8 close()-time ordering vs other end-of-run messages

`_astroid_module_checker` exit (pylinter.py:993-996): first
`self.stats.statement = walker.nbstatements`, then `for checker in
reversed(_checkers): checker.close()`. `_checkers = prepare_checkers()` is
`[PyLinter] + sorted-by-name builtin checkers` (get_checkers 574-576;
BaseChecker.__gt__ base_checker.py:54-69: main first, builtins
alphabetical, then extensions alphabetical). Alphabetically `"imports"` <
`"similarities"`, so in REVERSED order **similarities.close() runs BEFORE
imports.close()** → all R0801 messages appear BEFORE cyclic-import (R0401)
messages at the tail of the output (both attributed to the last module's
header). Stock checkers with message-emitting close(): only imports
(R0401) and similarities (R0801).

After close(), `generate_reports` (1121-1147) prints the score footer
(`_report_evaluation`, 1149-1193) — R0801s are included in
`stats.refactor` before evaluation.

## B.9 Behavior under -j N (parallel.py)

- Workers: `_worker_check_single_file` (parallel.py:64-97) per FileItem:
  `_worker_linter.open()` then `check_single_file_item` (pylinter.py:761-769)
  — which enters `_astroid_module_checker` PER FILE: checker.open() resets
  `self.linesets=[]`, the file is processed, then checkers' close() runs.
  With exactly one lineset, `_iter_sims` iterates `self.linesets[:-1]` =
  `[]` → **no R0801 from workers** (and per-worker stats добавки are 0).
  Then the worker harvests `checker.get_map_data()` for every checker
  (default None, base_checker.py:222; similarities returns its
  one-element lineset list) into `mapreduce_data[checker.name]`.
- Main: `executor.map(...)` (parallel.py:162) yields results in
  **submission order** → per-file messages replay deterministically in
  FileItem order; `linter.set_current_module(module, file_path)` per result.
  `all_mapreduce_data[worker_idx].append(mapreduce_data)` — keyed by
  `id(multiprocessing.current_process())` of the worker; **which worker got
  which file is scheduling-dependent**.
- `_merge_mapreduce_data` (100-121): flattens
  `for linter_data in all_mapreduce_data.values(): for run_data in
  linter_data: ...` — worker-key insertion order × per-worker completion
  order → the recombined lineset ORDER IS NONDETERMINISTIC across runs.
  Then `checker.reduce_map_data(linter, collated)` (symilar.py:866-874) =
  `combine_mapreduce_data` (flatten, 558-563) + `self.close()`.
  Since lineset order drives pair order, tie-breaking, and which name is
  lineset1 vs lineset2, **R0801 output under -j is inherently
  nondeterministic in pylint** (message ORDER for equal-num groups and
  even merge undercounts can vary). Also at this point main-process
  `current_name/current_file` = the LAST file from executor.map → R0801
  location as in B.7; the main linter's file_state has no pragma data, so
  the close-time enable check is config-only (same effective behavior).
  Note: `reduce_map_data` is invoked on the ORIGINAL main-process checker
  instance whose `open()` ran via `linter.open()`? No — main process never
  ran checker.open() (only workers did); `self.linesets` on the main
  instance is whatever `__init__` left (`[]`) until combine_mapreduce_data
  replaces it; `stats.reset_duplicated_lines()` never ran in main —
  duplicated-lines stats accumulate onto the initial zeros. Fine.
- prylint stance: single-process replication of -j1 semantics is the
  byte-identical target; document -j divergence as upstream-nondeterministic.

## B.10 Standalone report path (`Symilar.run`, `_get_similarity_report`)

Used only by the `symilar` CLI (Run(), 881-928; `python -m
pylint.checkers.symilar`), NOT by pylint runs — included for completeness:
sorted couples (`sorted(couples)` — LineSet `__lt__` by name), report lines
`=={name}:[{start}:{end}]`, code block from the LAST couple in SORTED order
(deterministic here, unlike close()), `f"   {line.rstrip()}\n"` with
3-space indent or bare newline for blank lines, and a trailer
`TOTAL lines={total} duplicates={dup} percent={dup*100.0/total:.2f}`
(ZeroDivisionError if no lines — upstream bug, CLI-only). Exits 0 always.

---------------------------------------------------------------------------
# C. Port checklist / gotchas recap
---------------------------------------------------------------------------

1. **R0915 statement table** (A.2.1/A.2.2): Return/While/For contribute 0;
   ExceptHandler contributes 1 *in addition to* visit_try's
   handlers-count; TryStar/AsyncFor/With contribute 1 flat; If contributes
   1-or-2 ONLY while R0912 or R0916 is enabled, else 1; ClassDef/FunctionDef
   contribute 0 while their visitors are registered. Frame init = 1.
   Nested-function statements leak into all enclosing frames.
2. **Gating**: replicate `only_required_for_messages` sets verbatim,
   including the stray `keyword-arg-before-vararg` on visit_functiondef;
   visit_default applies per-node-class only where the checker has no
   registered specific visitor.
3. **R0913 numerator bug**: compare `argnum` (ignored args excluded), report
   `len(args)`.
4. **`bulitins.frozenset` typo** in the R0901 ignore set.
5. All design thresholds are strict (`>` / `<` for R0903).
6. R0916 reports on the BoolOp node, not the If.
7. R0801 stripped-line pipeline order: pragma-drop → strip →
   docstring-machine → comment-split → import/signature blank → drop empty;
   0-based stored line numbers; bug-compatible docstring/comment handling.
8. Chunk equality = exact sum of per-line string hashes (unbounded int);
   frozenset-intersection representative choice (smaller side, tie → right
   operand) + sort by representative `_index` fixes all_couples insertion
   order, which `remove_successive` depends on (undercount quirk).
9. `eff_cmn_nb > min_similarity_lines` is STRICT (default ⇒ ≥5 effective
   lines needed).
10. close()-time R0801: module/abspath = last linted module, line 1 col 0;
    config-only enablement; messages precede imports' R0401; each adds exit
    bit 8 and a refactor stat on the last module.
11. The real-lines block in the message body follows SET iteration order —
    upstream-nondeterministic when the two regions' rstripped real lines
    differ; prylint must adopt a fixed policy.
12. SimilaritiesChecker is prepared when `--reports=y` even with R0801
    disabled (collects linesets, computes stats, emits nothing).
13. `len(LineSet)` (stats denominator) counts ALL real lines incl. ignored/
    undecodable(0) ones; only matters for RP0801/persistence.

# Open questions

- R0801 real-lines block nondeterminism (B.7): decide prylint policy
  (suggest: emit the SECOND lineset's block — matched 3/5 observed runs —
  or better, regenerate ground truths and accept either; flag corpora
  where the two regions' rstripped text differs).
- CPython str-hash sum: confirm acceptance of non-bit-identical collision
  behavior with a non-siphash 64-bit hash, or port siphash13 with
  PYTHONHASHSEED=0 secret (straightforward; hash input is the str's
  internal UCS1/2/4 buffer — for non-ASCII lines the encoding matters).
- astroid `instance_attrs` exact population (R0902) depends on pyinfer's
  delayed_assattr port fidelity (builder.py:248-284) — verify against
  corpora once pyinfer lands attribute assignment routing.
- `_class_type` caching (`klass._type`) is cross-checker shared state in
  astroid; ensure prylint's equivalent memoization matches first-computed
  semantics (visit order can theoretically affect the ancestor-loop guard
  path).
- Whether any corpus exercises the remove_successive undercount path
  (repeated chunks with out-of-order chain insertion) — worth a targeted
  differential fuzz on synthetic inputs.
