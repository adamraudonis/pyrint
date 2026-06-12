# 09 — pylint/checkers/refactoring/ : exact port spec (pylint 4.0.5)

Source files (all paths relative to `reference/pylint/pylint/`):

- `checkers/refactoring/__init__.py` (33 lines) — registration
- `checkers/refactoring/refactoring_checker.py` (2455 lines) — `RefactoringChecker`, R1701–R1737
- `checkers/refactoring/not_checker.py` (84 lines) — `NotChecker`, C0117
- `checkers/refactoring/recommendation_checker.py` (452 lines) — `RecommendationChecker`, C0200/C0201/C0206/C0207/C0208/C0209
- `checkers/refactoring/implicit_booleaness_checker.py` (420 lines) — `ImplicitBooleanessChecker`, C1802–C1805

All four checkers share `name = "refactoring"`. Registration order
(`refactoring/__init__.py:29-33`):

```python
def register(linter: PyLinter) -> None:
    linter.register_checker(RefactoringChecker(linter))
    linter.register_checker(NotChecker(linter))
    linter.register_checker(RecommendationChecker(linter))
    linter.register_checker(ImplicitBooleanessChecker(linter))
```

Registration order matters for *callback ordering inside a single AST node
visit*: the walker appends `visit_*` callbacks per checker in registration
order, so e.g. for a `Call` node, RefactoringChecker's `visit_call` runs
before RecommendationChecker's `visit_call`, which determines message
emission order for messages on the same line (output is the emission order
within a module since pylint does not re-sort messages — see notes/02).

Exit-code / category mapping (already in notes/02): every `R` message sets
exit bit 8, every `C` message sets exit bit 16, on emission
(`lint/pylinter.py` `_add_one_message`: `self.msg_status |= MSG_TYPES_STATUS[msgid[0]]`).
All messages here also feed the stats counters used by the score footer.

---------------------------------------------------------------------------
## 0. Message inventory & metadata

### 0.1 Scope quirk: RefactoringChecker messages are LINE-scoped

`RefactoringChecker` subclasses `checkers.BaseTokenChecker`
(refactoring_checker.py:242). `BaseChecker.create_message_definition_from_tuple`
(checkers/base_checker.py:185-188):

```python
if isinstance(self, (BaseTokenChecker, BaseRawFileChecker)):
    default_scope = WarningScope.LINE
else:
    default_scope = WarningScope.NODE
```

⇒ **All R1701–R1737 have `WarningScope.LINE`** (confirmed by
`crates/pycheckers/src/msgs.rs`: `node_scope: false` for every R17xx).
The other three checkers subclass `BaseChecker` ⇒ C0117, C0200-C0209,
C1802-C1805 are `WarningScope.NODE` (`node_scope: true`).

Consequence (see notes/03): block-level `# pylint: disable` pragma expansion
over a node's line span (`FileState._set_message_state_on_block_lines`) only
applies to NODE-scoped messages; LINE-scoped messages obey only the
line-by-line pragma state. This is a real observable difference for e.g.
`no-else-return` disabled by a pragma placed on the `def` line.

### 0.2 Per-message table

msgid | symbol | template (verbatim) | default_enabled | emitted with confidence
---|---|---|---|---
R1701 | consider-merging-isinstance | `Consider merging these isinstance calls to isinstance(%s, (%s))` | yes | UNDEFINED
R1702 | too-many-nested-blocks | `Too many nested blocks (%s/%s)` | yes | UNDEFINED
R1703 | simplifiable-if-statement | `The if statement can be replaced with %s` | yes | UNDEFINED
R1704 | redefined-argument-from-local | `Redefining argument with the local name %r` | yes | UNDEFINED
R1705 | no-else-return | `Unnecessary "%s" after "return", %s` | yes | HIGH
R1706 | consider-using-ternary | `Consider using ternary (%s)` | yes | INFERENCE
R1707 | trailing-comma-tuple | `Disallow trailing comma tuple` | yes | HIGH
R1708 | stop-iteration-return | `Do not raise StopIteration in generator, use return statement instead` | yes | INFERENCE
R1709 | simplify-boolean-expression | `Boolean expression may be simplified to %s` | yes | INFERENCE
R1710 | inconsistent-return-statements | `Either all return statements in a function should return an expression, or none of them should.` | yes | UNDEFINED
R1711 | useless-return | `Useless return at end of function or method` | yes | UNDEFINED
R1712 | consider-swap-variables | `Consider using tuple unpacking for swapping variables` | yes | UNDEFINED
R1713 | consider-using-join | `Consider using str.join(sequence) for concatenating strings from an iterable` | yes | UNDEFINED
R1714 | consider-using-in | `Consider merging these comparisons with 'in' by using '%s %sin (%s)'. Use a set instead if elements are hashable.` | yes | HIGH
R1715 | consider-using-get | `Consider using dict.get for getting values from a dict if a key is present or a default if not` | yes | UNDEFINED
R1716 | chained-comparison | `Simplify chained comparison between the operands` | yes | UNDEFINED
R1717 | consider-using-dict-comprehension | `Consider using a dictionary comprehension` | yes | UNDEFINED
R1718 | consider-using-set-comprehension | `Consider using a set comprehension` | yes | UNDEFINED
R1719 | simplifiable-if-expression | `The if expression can be replaced with %s` | yes | UNDEFINED
R1720 | no-else-raise | `Unnecessary "%s" after "raise", %s` | yes | HIGH
R1721 | unnecessary-comprehension | `Unnecessary use of a comprehension, use %s instead.` | yes | UNDEFINED
R1722 | consider-using-sys-exit | `Consider using 'sys.exit' instead` | yes | HIGH
R1723 | no-else-break | `Unnecessary "%s" after "break", %s` | yes | HIGH
R1724 | no-else-continue | `Unnecessary "%s" after "continue", %s` | yes | HIGH
R1725 | super-with-arguments | `Consider using Python 3 style super() without arguments` | yes | UNDEFINED
R1726 | simplifiable-condition | `Boolean condition "%s" may be simplified to "%s"` | yes | UNDEFINED
R1727 | condition-evals-to-constant | `Boolean condition '%s' will always evaluate to '%s'` | yes | UNDEFINED
R1728 | consider-using-generator | `Consider using a generator instead '%s(%s)'` | yes | UNDEFINED
R1729 | use-a-generator | `Use a generator instead '%s(%s)'` | yes | UNDEFINED
R1730 | consider-using-min-builtin | `Consider using '%s' instead of unnecessary if block` | yes | UNDEFINED
R1731 | consider-using-max-builtin | `Consider using '%s' instead of unnecessary if block` | yes | UNDEFINED
R1732 | consider-using-with | `Consider using 'with' for resource-allocating operations` | yes | UNDEFINED
R1733 | unnecessary-dict-index-lookup | `Unnecessary dictionary index lookup, use '%s' instead` | yes | UNDEFINED
R1734 | use-list-literal | `Consider using [] instead of list()` | yes | UNDEFINED
R1735 | use-dict-literal | `Consider using '%s' instead of a call to 'dict'.` | yes | INFERENCE
R1736 | unnecessary-list-index-lookup | `Unnecessary list index lookup, use '%s' instead` | yes | HIGH or INFERENCE (computed)
R1737 | use-yield-from | `Use 'yield from' directly instead of yielding each element one by one` | yes | HIGH
C0117 | unnecessary-negation | `Consider changing "%s" to "%s"` | yes | UNDEFINED
C0200 | consider-using-enumerate | `Consider using enumerate instead of iterating with range and len` | yes | UNDEFINED
C0201 | consider-iterating-dictionary | `Consider iterating the dictionary directly instead of calling .keys()` | yes | INFERENCE
C0206 | consider-using-dict-items | `Consider iterating with .items()` | yes | UNDEFINED
C0207 | use-maxsplit-arg | `Use %s instead` | yes | HIGH or INFERENCE (computed)
C0208 | use-sequence-for-iteration | `Use a sequence type when iterating over values` | yes | HIGH
C0209 | consider-using-f-string | `Formatting a regular string which could be an f-string` | yes | UNDEFINED
C1802 | use-implicit-booleaness-not-len | `Do not use `` `len(SEQUENCE)` `` without comparison to determine if a sequence is empty` | yes | HIGH or INFERENCE (two sites)
C1803 | use-implicit-booleaness-not-comparison | `"%s" can be simplified to "%s", if it is strictly a sequence, as an empty %s is falsey` | yes | HIGH
C1804 | use-implicit-booleaness-not-comparison-to-string | `"%s" can be simplified to "%s", if it is strictly a string, as an empty string is falsey` | **NO (`default_enabled: False`)** | HIGH
C1805 | use-implicit-booleaness-not-comparison-to-zero | `"%s" can be simplified to "%s", if it is strictly an int, as 0 is falsey` | **NO (`default_enabled: False`)** | HIGH

C1802 template verbatim:
`Do not use ``len(SEQUENCE)`` without comparison to determine if a sequence is empty`
— wait, exact string is:

```
"Do not use `len(SEQUENCE)` without comparison to determine if a sequence is empty"
```

(single backticks, implicit_booleaness_checker.py:65).

Old names (affect `--disable=` aliasing and `# pylint: disable=` pragmas):

- R1702 ← `R0101` old-too-many-nested-blocks
- R1703 ← `R0102` old-simplifiable-if-statement
- C0117 ← `C0113` unneeded-not
- C1802 ← `C1801` len-as-condition
- C1804 ← `C1901` compare-to-empty-string
- C1805 ← `C2001` compare-to-zero

`default_enabled: False` (C1804, C1805) means the message is OFF unless
explicitly `--enable`d; per notes/03 this is implemented as an entry in the
linter's initial `_msgs_state` with value False (`MessageDefinition.default_enabled`).
Both are still *may_be_emitted* — the checker code runs (see §6.0 gating bug)
but `add_message` drops them.

Confidence is only output-relevant if the user passes `--confidence` with a
restricted list (default config lists all five levels ⇒ no filtering) or
uses `msg-template` containing confidence. It DOES affect
`is_message_enabled(..., confidence=...)` filtering inside `add_message`
when `--confidence` is restricted.

### 0.3 Message position (report node) resolution

All `add_message(..., node=X)` calls resolve positions in
`PyLinter._add_one_message` (lint/pylinter.py:1196-1232):

```python
if node:
    if node.position:           # only FunctionDef/ClassDef have .position set
        line/col/end_* from node.position   # (keyword-anchored)
    else:
        line = node.fromlineno; col_offset = node.col_offset
        end_lineno = node.end_lineno; end_col_offset = node.end_col_offset
```

Relevant here: R1710/R1711 pass `node=FunctionDef` ⇒ use `node.position`
(the `def name` keyword span, see notes/04). Everything else in these
modules passes non-frame nodes ⇒ `fromlineno/col_offset/end_*`.
Exceptions:

- R1707 trailing-comma-tuple: `add_message("trailing-comma-tuple",
  line=token.start[0], confidence=HIGH)` — **no node**; col_offset stays
  None ⇒ output column 0, no end position.
- C0209: `add_message(..., node=node, line=node.lineno, col_offset=node.col_offset)`
  — explicit line/col (same values that node resolution would give; end_*
  still resolved from the node).

---------------------------------------------------------------------------
## 1. Cross-cutting machinery

### 1.1 Walker dispatch is exact-class-name based

`utils/ast_walker.py` builds callback tables keyed on
`node.__class__.__name__.lower()`; **no MRO fallback**. Consequences:

- `RefactoringChecker.visit_functiondef` / `leave_functiondef` do NOT fire
  for `AsyncFunctionDef` ⇒ **R1710 (inconsistent-return-statements) and
  R1711 (useless-return) are never emitted for `async def` functions**, and
  `self._return_nodes` never gets entries for them.
- `visit_while = visit_try` (refactoring_checker.py:731) is an explicit
  alias because `While` wouldn't match `visit_try` otherwise.
- `visit_for` does not fire for `AsyncFor` ⇒ R1704/R1702/R1733/R1736 skip
  `async for` loops at the For-statement level. (`AsyncFor` subclasses
  `For` in astroid but dispatch is by exact name.)
- `Comprehension` nodes are visited for both sync and async comprehensions
  (`visit_comprehension`), and `node.is_async` is checked explicitly where
  relevant (R1721).

### 1.2 only_required_for_messages

`checkers/utils.py:480` — decorator sets `method.checks_msgs = messages`.
Walker `_is_method_enabled` (ast_walker.py:37-40): callback is registered
iff `any(linter.is_message_enabled(m) for m in checks_msgs)` evaluated
**once per run** (at `add_checker` time, i.e. before per-file pragma states).
If registered, the method runs for every file and every matching node; each
individual `add_message` is then filtered by full per-line message state.
A method *without* the decorator is always registered
(e.g. `RefactoringChecker.visit_functiondef`).

### 1.3 RefactoringChecker state-leak hazard across modules

`RefactoringChecker.__init__` calls `_init()`; `leave_module`
(refactoring_checker.py:716-722) calls `_init()` again:

```python
@utils.only_required_for_messages("consider-using-with")
def leave_module(self, _: nodes.Module) -> None:
    self._emit_consider_using_with_if_needed(
        self._consider_using_with_stack.module_scope)
    self._init()
```

Because of the decorator, **if `consider-using-with` (R1732) is disabled
globally, `leave_module` is never registered ⇒ `_init()` never runs between
modules** and the following state persists across files for the whole run:

- `self._elifs` (list of `(line, col)` token positions of `elif` keywords)
  keeps growing; a plain `if` inside an `else:` in file B at the same
  (line,col) as an `elif` of any previously-processed file is misclassified
  as an elif (affects R1703/R1705/R1720/R1723/R1724/R1702/R1730/R1731).
- `self._nested_blocks`, `self._reported_swap_nodes`,
  `self._consider_using_with_stack` likewise leak (the stacks compare nodes
  by identity so the swap set only wastes memory, but `_nested_blocks` from
  a previous module can suppress/trigger R1702 boundaries — in practice
  `_check_nested_blocks` resets the list when `node.parent == node.scope()`,
  which the first function-level block of each module does, so leakage is
  limited to `_elifs` in observable terms).

This must be replicated bit-for-bit (only matters when R1732 is disabled
while other refactoring messages are enabled).

`leave_functiondef` resets `self._nested_blocks = []` per function.

### 1.4 Options owned by RefactoringChecker (refactoring_checker.py:510-545)

- `max-nested-blocks`: int, default **5** (gates R1702).
- `never-returning-functions`: csv, default **`("sys.exit", "argparse.parse_error")`**
  (gates R1710's `_is_function_def_never_returning`).
- `suggest-join-with-non-empty-separator`: yn, default **True**
  (gates R1713 f-string separator case).

Read in `open()` (refactoring_checker.py:562-569) into
`self._never_returning_functions` (a set) and
`self._suggest_join_with_non_empty_separator`.

Other config consumed:

- `dummy-variables-rgx` (owned by VariablesChecker, default
  `_+$|(_[a-zA-Z0-9_]*[a-zA-Z0-9]+?$)|dummy|^ignored_|^unused_`) — cached
  property `_dummy_rgx` (refactoring_checker.py:571-573), gates R1704.
- `py-version` (default `sys.version_info[:2]` = `(3, 12)` for our pinned
  runtime; lint/base_options.py:356-365) — RecommendationChecker `open()`
  sets `self._py36_plus = py_version >= (3, 6)` (gates C0209).

---------------------------------------------------------------------------
## 2. RefactoringChecker — token pass

### 2.1 process_tokens (refactoring_checker.py:668-714)

Runs as a token checker over `tokenize.TokenInfo` list (same token stream
as message-control pragma processing; see notes/03).

```python
trailing_comma_tuple_enabled_for_file = self.linter.is_message_enabled("trailing-comma-tuple")
trailing_comma_tuple_enabled_once = trailing_comma_tuple_enabled_for_file
for index, token in enumerate(tokens):
    token_string = token[1]
    if (not trailing_comma_tuple_enabled_once
        and token_string.startswith("#")
        and "pylint:" in token_string[1:]
        and "enable" in token_string[8:]
        and any(c in token_string[15:] for c in ("trailing-comma-tuple", "R1707"))):
        trailing_comma_tuple_enabled_once = True
    if token_string == "elif":
        self._elifs.extend([token[2], tokens[index + 1][2]])
    elif (trailing_comma_tuple_enabled_for_file
          or trailing_comma_tuple_enabled_once) and _is_trailing_comma(tokens, index):
        self.add_message("trailing-comma-tuple", line=token.start[0], confidence=HIGH)
```

Notes:

- `is_message_enabled("trailing-comma-tuple")` here is the *file-start*
  state (line=None ⇒ `_msgs_state.get(msgid, True)`), i.e. global
  enable/disable only. If R1707 is globally disabled but a comment token
  anywhere in the file contains `#...pylint:...enable...` plus the substring
  `trailing-comma-tuple` or `R1707` at the right offsets, scanning turns on
  for **subsequent** tokens (the flag flips for tokens after the comment).
  The substring checks are sliced: `"pylint:" in token_string[1:]`,
  `"enable" in token_string[8:]`, `any(c in token_string[15:] ...)` — note
  `"enable" in s[8:]` also matches `disable`'s tail? No: "enable" is a
  substring of "disable"! `# pylint: disable=trailing-comma-tuple` contains
  "enable" within "disable" ⇒ also flips the flag. Replicate verbatim.
- `elif` handling stores TWO positions per elif: `token[2]` (the `elif`
  token's own (row,col) start) and `tokens[index+1][2]` (the next token's
  start). Both go into `self._elifs`. `_is_actual_elif` and the
  superfluous-else family check membership of `(node.lineno, node.col_offset)`
  in this list.
- Each R1707 is emitted with `line=` only, confidence HIGH, column 0.

### 2.2 `_is_trailing_comma(tokens, index)` (refactoring_checker.py:98-138)

```python
token = tokens[index]
if token.exact_type != tokenize.COMMA: return False
left_tokens = itertools.islice(tokens, index + 1, None)
more_tokens_on_line = False
for remaining_token in left_tokens:
    if remaining_token.start[0] == token.start[0]:
        more_tokens_on_line = True
        if remaining_token.type not in (tokenize.NEWLINE, tokenize.COMMENT):
            return False
if not more_tokens_on_line: return False

def get_curline_index_start():
    for subindex, token in enumerate(reversed(tokens[:index])):
        if token.type == tokenize.NEWLINE:
            return index - subindex
    return 0

curline_start = get_curline_index_start()
expected_tokens = {"return", "yield"}
return any("=" in prevtoken.string or prevtoken.string in expected_tokens
           for prevtoken in tokens[curline_start:index])
```

i.e. a COMMA token, followed on the *same physical row* only by
NEWLINE/COMMENT tokens (and at least one such token — i.e. a logical NEWLINE
exists, which excludes commas inside brackets since those produce NL not
NEWLINE), where the current logical line (since the previous NEWLINE token)
contains a token whose string contains `=` (this includes `==`, `<=`, `+=`,
default kwargs etc. — bug-for-bug) or is exactly `return`/`yield`.
Note the full-token-stream scan (`O(n)` per comma) — for porting, the scan
of `left_tokens` covers the remainder of the file, but returns early only
when a same-row non-NEWLINE/COMMENT token is found; tokens on later rows
just don't match `start[0]` and the loop continues to the end. Replicate
the exact semantics, not the cost.

`token.exact_type == COMMA`: the tokenize module reports OP tokens; the
`exact_type` property must be computed from the string (`,` ⇒ COMMA).

---------------------------------------------------------------------------
## 3. RefactoringChecker — AST visits, message by message

Visit method registration map (decorator message lists):

- `visit_try` / `visit_while` (alias): `("too-many-nested-blocks", "no-else-return")`
- `visit_for`: `("redefined-argument-from-local", "too-many-nested-blocks",
  "unnecessary-dict-index-lookup", "unnecessary-list-index-lookup")`
- `visit_excepthandler`: `("redefined-argument-from-local",)`
- `visit_with`: `("redefined-argument-from-local", "consider-using-with")`
- `visit_if`: `("too-many-nested-blocks", "simplifiable-if-statement",
  "no-else-return", "no-else-raise", "no-else-break", "no-else-continue",
  "consider-using-get", "consider-using-min-builtin", "consider-using-max-builtin")`
- `visit_ifexp`: `("simplifiable-if-expression",)`
- `leave_functiondef`: `("too-many-nested-blocks", "inconsistent-return-statements",
  "useless-return", "consider-using-with")`
- `leave_classdef`: `("consider-using-with",)`
- `leave_module`: `("consider-using-with",)`
- `visit_raise`: `("stop-iteration-return",)`
- `visit_call`: `("stop-iteration-return", "consider-using-dict-comprehension",
  "consider-using-set-comprehension", "consider-using-sys-exit",
  "super-with-arguments", "consider-using-generator", "consider-using-with",
  "use-list-literal", "use-dict-literal", "use-a-generator")`
- `visit_yield`: `("use-yield-from",)`
- `visit_boolop`: `("consider-merging-isinstance", "consider-using-in",
  "chained-comparison", "simplifiable-condition", "condition-evals-to-constant")`
- `visit_assign`: `("simplify-boolean-expression", "consider-using-ternary",
  "consider-swap-variables", "consider-using-with")`
- `visit_return`: `("simplify-boolean-expression", "consider-using-ternary",
  "consider-swap-variables")`
- `visit_augassign`: `("consider-using-join",)`
- `visit_comprehension`: `("unnecessary-comprehension",
  "unnecessary-dict-index-lookup", "unnecessary-list-index-lookup")`
- `visit_functiondef`: NO decorator (always registered).

A registered visit method runs ALL its sub-checks; each sub-check's
`add_message` is then individually filtered. E.g. enabling only
`use-dict-literal` still runs `_check_consider_using_with` (whose inference
side-effects are unobservable, but whose *stack mutations* feed R1732 which
is disabled — harmless).

### 3.1 `_is_actual_elif` (refactoring_checker.py:581-594)

```python
match node.parent:
    case nodes.If(orelse=[n]) if n == node:
        if (node.lineno, node.col_offset) in self._elifs:
            return True
return False
```

True iff: parent is an If, parent's orelse is exactly `[node]`, and the
node's `(lineno, col_offset)` was recorded from an `elif` token. Used by
R1703, R1702, R1705/R1720/R1723/R1724, R1730/R1731.

### 3.2 R1703 simplifiable-if-statement — `_check_simplifiable_if` (596-666)

Trigger (pseudocode):

```
if _is_actual_elif(node): bail
node must match If(body=[first_branch], orelse=[else_branch])   # exactly 1+1
if first_branch is Return:
    else_branch must be Return, reduced_to = "'return bool(test)'"
elif first_branch is Assign:
    else_branch must be Assign
    first_targets  = [t.name for t in first_branch.targets if AssignName]
    else_targets   = likewise
    bail if either list empty
    bail if sorted(first_targets) != sorted(else_targets)
    reduced_to = "'var = bool(test)'"
else: bail
bail unless BOTH branch values are Const bool   (_is_bool_const: value is Const and isinstance(value.value, bool))
bail if first_branch.value.value is falsy (i.e. the if-branch returns/assigns False)
add_message("simplifiable-if-statement", node=node, args=(reduced_to,))
```

The arg string includes the single quotes: `'return bool(test)'`.
Note an `elif` chain’s **last** `if` (with a plain else) is still exempted
because the elif node itself is `_is_actual_elif`. A nested plain
`if/else` where both branches return True/False triggers. `If` with
`orelse` of length != 1 (e.g. else block with 2 statements) bails.
The if-branch must be the **True** one (the `not first_branch.value.value`
bail). Report node: the `If` node (LINE scope; position = `if` keyword line).

### 3.3 R1705/R1720/R1723/R1724 — superfluous else family (791-842)

`visit_if` calls all four wrappers; `visit_try`/`visit_while` call only
return & raise variants. `_check_superfluous_else(node, msg_id, returning_node_class)`:

```
if isinstance(node, Try) and node.finalbody: bail   # try/except/else/finally
if not node.orelse: bail
if _is_actual_elif(node): bail                       # elif handled when visiting parent chain
emit-condition:
    (isinstance(node, If) and _if_statement_is_always_returning(node, cls))
    or (isinstance(node, Try) and not node.finalbody
        and _except_statement_is_always_returning(node, cls))
if emit:
    orelse = node.orelse[0]
    if (orelse.lineno, orelse.col_offset) in self._elifs:
        args = ("elif", 'remove the leading "el" from "elif"')
    else:
        args = ("else", 'remove the "else" and de-indent the code inside it')
    add_message(msg_id, node=node, args=args, confidence=HIGH)
```

- `_if_statement_is_always_returning` (82-85): `any(isinstance(n, cls) for n in if_node.body)`
  — i.e. ANY direct child of the if-body is a Return/Raise/Break/Continue
  (not necessarily the last statement!). `return` buried mid-body counts.
- `_except_statement_is_always_returning` (88-95): `all(any(isinstance(child, cls)
  for child in handler.body) for handler in node.handlers)` — every handler
  has a direct Return/Raise child. (Only reachable for R1705/R1720 since
  visit_try doesn't run break/continue variants.)
- When `While` nodes flow through this via `visit_while = visit_try`: the
  emit-condition is False on both arms (`While` is neither If nor Try), so
  **no message ever** for While — only `_check_nested_blocks` matters there.
- A genuine `elif` following the returning if: `orelse[0]` is the synthetic
  inner If whose (lineno,col) is in `_elifs` ⇒ args use "elif" wording. The
  message is attached to the **outer** If node, each If in an elif-chain is
  visited separately so chains produce one message per always-returning
  link with an else continuation (each If with orelse non-empty and not
  itself an elif... note inner elif Ifs ARE `_is_actual_elif` ⇒ bail; only
  the chain head can emit).

Wait — precise: the chain head `if A: return ... elif B: return ... else: ...`
is one outer If whose orelse=[inner If]. Outer: orelse non-empty, not elif,
body always-returning ⇒ message with ("elif", ...) because orelse[0] is at
an elif token position. Inner If: `_is_actual_elif` ⇒ bail. So exactly ONE
message per chain, on the head, args=("elif", ...) — UNLESS the head's body
doesn't return (then nothing, even if later elifs return).

- For Try (R1705/R1720): `try/except/else` where every except handler body
  directly contains return/raise ⇒ message on the Try node. `finalbody`
  present ⇒ never.

### 3.4 R1702 too-many-nested-blocks — `_check_nested_blocks` (1264-1301)

Called from visit_if / visit_try / visit_while / visit_for. Node types
tracked: `Try | While | For | If` (`NodesWithNestedBlocks`, line 30).

```
if not isinstance(node.scope(), nodes.FunctionDef): bail   # only inside functions
nested_blocks = self._nested_blocks[:]          # snapshot
if node.parent == node.scope():
    self._nested_blocks = [node]
else:
    for ancestor_node in reversed(self._nested_blocks):
        if ancestor_node == node.parent: break
        self._nested_blocks.pop()
    if isinstance(node, If) and self._is_actual_elif(node):
        if self._nested_blocks: self._nested_blocks.pop()
    self._nested_blocks.append(node)
if len(nested_blocks) > len(self._nested_blocks):
    self._emit_nested_blocks_message_if_needed(nested_blocks)
```

`_emit_nested_blocks_message_if_needed(blocks)`: if
`len(blocks) > self.linter.config.max_nested_blocks` (default 5) ⇒
`add_message("too-many-nested-blocks", node=blocks[0],
args=(len(blocks), max_nested_blocks))`. Report node = the **outermost**
block of the deep group. Also called from `leave_functiondef` with the
leftover stack (catches a deep nest at the end of a function), after which
`self._nested_blocks = []`.

Subtleties:

- `node.scope()` of an If at module level is Module ⇒ ignored. Class-level
  ⇒ ClassDef ⇒ ignored. Lambda can't contain statements.
- `node.parent == node.scope()` test is wrong for function bodies nested in
  if-blocks etc., but that's the point: the stack rebuild walks up via
  popping until `ancestor == node.parent`. If node.parent is not on the
  stack at all (e.g. node nested under a `with` which isn't tracked), the
  loop pops everything (no break) and then appends ⇒ stack `[node]` — note
  `with` blocks do NOT count as nesting levels.
- elif: pops one level so the elif doesn't add depth.
- Message fires when the new node's chain is SHORTER than the previous
  snapshot — i.e. on "leaving" a nested group — plus the leftover check at
  leave_functiondef. The message can fire at most once per group since the
  comparison is strict and the snapshot shrinks.
- DFS order: walker visits statements in order, so `nested_blocks`
  accumulates the deepest chain seen before any dedent.

### 3.5 R1704 redefined-argument-from-local (733-789)

`_check_redefined_argument_from_local(name_node: AssignName)`:

```
if self._dummy_rgx and self._dummy_rgx.match(name_node.name): bail
if not name_node.lineno: bail
scope = name_node.scope()
if not isinstance(scope, nodes.FunctionDef): bail   # NOTE: AsyncFunctionDef is a subclass — isinstance check, so async OK here
for defined_argument in scope.args.nodes_of_class(AssignName, skip_klass=(Lambda,)):
    if defined_argument.name == name_node.name:
        add_message("redefined-argument-from-local", node=name_node, args=(name_node.name,))
```

Template uses `%r` ⇒ rendered with Python repr: `Redefining argument with
the local name 'x'`. NO break after match — but argument names are unique
in a signature so at most one match (lambda default-value args skipped via
skip_klass).

Call sites:

- `visit_for`: for every `AssignName` under `node.target`
  (`node.target.nodes_of_class(nodes.AssignName)` — tuples unpacked,
  starred included).
- `visit_excepthandler`: `if node.name and isinstance(node.name, AssignName)`.
- `visit_with`: for each `(var, names)` in `node.items`: first the R1732
  stack-clearing for Name vars (see §3.20), then if `names` is not None,
  every AssignName under it.

`isinstance(scope, nodes.FunctionDef)` — `scope()` of a name inside a
comprehension is the comprehension's implicit function? In astroid 4,
comprehension targets live in the comprehension scope (ListComp etc. are
scopes) — but these call sites only feed For-statement targets, with-items,
and except names, all in function/module/class scopes.

### 3.6 R1715 consider-using-get — `_check_consider_get` (855-892)

`_is_dict_get_block(node)`:

```
node must match If(test=Compare() as test,
                   body=[Assign(targets=[AssignName()],
                                value=Subscript(slice=slice_value, value=value))])
# i.e. exactly one body statement: single-target name assignment from a subscript
bail unless _type_and_name_are_equal(value, test.ops[0][1])       # dict expr == RHS of compare
       and _type_and_name_are_equal(slice_value, test.left)       # subscript key == LHS of compare
return isinstance(utils.safe_infer(test.ops[0][1]), nodes.Dict)   # dict literal inference
```

`_type_and_name_are_equal` (844-853): both Name with equal `.name`, both
AssignName with equal `.name`, or both Const with equal `.value`. (Note:
Name vs AssignName mix ⇒ False; attributes never equal.)

Note `test` is any Compare — the operator is NOT checked here
(`test.ops[0][1]` is the first comparator); so `if k != d: d[k] = ...`
also triggers... but wait: pattern requires the test to be a Compare and
uses ops[0]; for `k in d` ops[0] = ("in", d). Operator unchecked —
`if k not in d: x = d[k]` triggers too (bug-for-bug).
Chained comparisons: `test.ops[0][1]` just takes the first.

`_check_consider_get`:

```
if not _is_dict_get_block(node): return
match node:
    case If(orelse=[]):                            -> add_message (no else at all)
    case If(body=[Assign(targets=[t1])], orelse=[Assign(targets=[t2])]) \
         if _type_and_name_are_equal(t1, t2):      -> add_message
```

So: no else branch, OR an else branch consisting of exactly one Assign to
the same single target. `elif` is irrelevant (an elif appears as
orelse=[If] ⇒ no match). Message node = the If, no args.

### 3.7 R1730/R1731 consider-using-min/max-builtin (915-988)

```
if _is_actual_elif(node) or node.orelse: bail
if len(node.body) != 1: bail            # note: redundant with match below taking [Assign, *_]
match node:
    case If(test=Compare(left=left, ops=[[operator, right_statement]]),
            body=[Assign(targets=[AssignName() | AssignAttr() as target], value=value), *_]) \
        if not isinstance(left, Subscript):
    case _: bail
```

`get_node_name(n)`: Name ⇒ `n.name`; Const ⇒ `str(n.value)`; otherwise
`n.as_string()`.

```
target_assignation = get_node_name(target)     # NOTE: for AssignAttr, get_node_name falls to as_string()
body_value  = get_node_name(value)
left_operand = get_node_name(left)
right_statement_value = get_node_name(right_statement)
if left_operand == target_assignation: pass                     # a OP b: a = ...
elif right_statement_value == target_assignation:
    operator = utils.get_inverse_comparator(operator)           # reverse form
else: bail
if body_value not in (right_statement_value, left_operand): bail
match operator:
    case "<" | "<=":  reduced_to = f"{target} = max({target}, {body_value})"; msg = consider-using-max-builtin
    case ">" | ">=":  reduced_to = f"{target} = min({target}, {body_value})"; msg = consider-using-min-builtin
    # other operators (==, !=, in, is...) fall through silently
add_message(msg, node=node, args=(reduced_to,))
```

`get_inverse_comparator` (utils.py:2265): `{"==": "!=", "!=": "==",
"<": ">=", ">": "<=", "<=": ">", ">=": "<", "in": "not in",
"not in": "in", "is": "is not", "is not": "is"}` — KeyError impossible here
(Compare ops are all in the table).

Subtleties: `body=[Assign(...), *_]` means **only the first** body statement
matters and extra statements are allowed... but the earlier
`len(node.body) != 1` bail makes the `*_` moot. The Compare must have
exactly one op. `left` must not be a Subscript (guard), but
`right_statement` may be. `str(Const)` for `True` gives `"True"`;
for strings gives the raw value without quotes (`get_node_name(Const("a"))
== "a"`) — this affects equality comparisons against Name "a"!
(bug-for-bug: `if a < "a": a = "a"` suggests `a = max(a, a)`? No wait:
body_value = "a" (str of const), right_statement_value = "a" ⇒ matches.
reduced_to = `a = max(a, a)`. Replicate.)

### 3.8 R1719 simplifiable-if-expression — `_check_simplifiable_ifexp` (990-1017)

```
node must match IfExp(body=Const(value=bool() as body_value),
                      orelse=Const(value=bool() as orelse_value))
test_reduced_to = "test" if isinstance(node.test, Compare) else "bool(test)"
(True, False)  -> reduced_to = f"'{test_reduced_to}'"     # 'test' or 'bool(test)'
(False, True)  -> reduced_to = "'not test'"
else bail
add_message("simplifiable-if-expression", node=node, args=(reduced_to,))
```

`bool()` pattern: value must be exactly a bool instance (`True is True`
match via class pattern — `bool()` class pattern matches only bool, not
int 1, since match class patterns use isinstance; NOTE isinstance(True,
int) is True but pattern is `bool()` so 1 does NOT match).

### 3.9 R1710 inconsistent-return-statements (1912-1937 + helpers 1939-2088)

`visit_functiondef` (1912-1915; FunctionDef only, see §1.1):

```python
self._return_nodes[node.name] = list(
    node.nodes_of_class(nodes.Return, skip_klass=nodes.FunctionDef))
```

`nodes_of_class(Return, skip_klass=FunctionDef)`: DFS yielding Return nodes
but not descending into nested FunctionDefs. NOTE: the root node itself is
type-checked against skip_klass? astroid's `nodes_of_class` checks
`isinstance(self, klass)` for yield and recurses into children with
`child.nodes_of_class(...)` where children that are instances of skip_klass
are skipped — the root FunctionDef itself is not skipped. Nested
**AsyncFunctionDef** IS skipped too (subclass of FunctionDef). Lambdas are
NOT skipped but cannot contain Return.

**Keyed by bare function name** ⇒ same-named nested/sibling functions
clobber each other: visiting inner `f` overwrites outer `f`'s list;
`leave_functiondef(inner)` then sets `self._return_nodes["f"] = []`;
`leave_functiondef(outer)` sees `[]` ⇒ no explicit returns ⇒ **R1710 and
R1711 silently skipped for the outer function**. Replicate.

`leave_functiondef` → `_check_consistent_returns(node)`:

```
explicit_returns = [r for r in self._return_nodes[node.name] if r.value is not None]
if not explicit_returns: return
if len(explicit_returns) == len(self._return_nodes[node.name]) \
   and self._is_node_return_ended(node):
    return
add_message("inconsistent-return-statements", node=node)
```

i.e. emit iff there is ≥1 `return <expr>` AND (∃ bare `return` OR the
function body can fall off the end). Report node = FunctionDef (uses
`node.position` ⇒ line/col of the `def f` keyword span).

#### `_is_node_return_ended(node)` (2006-2051) — full recursion

```
match node:
    case Return(): return True
    case Call():
        if utils.is_terminating_func(node): return True
        return any(isinstance(f, (FunctionDef, BoundMethod))
                   and self._is_function_def_never_returning(f)
                   for f in utils.infer_all(node.func))
    case While():
        return (node.test.bool_value() and not _loop_exits_early(node)) \
               or any(self._is_node_return_ended(c) for c in node.orelse)
    case Raise():  return self._is_raise_node_return_ended(node)
    case If():     return self._is_if_node_return_ended(node)
    case Try():
        handlers = {c for c in node.get_children() if isinstance(c, ExceptHandler)}
        all_but_handler = set(node.get_children()) - handlers
        return any(self._is_node_return_ended(c) for c in all_but_handler) \
               and all(self._is_node_return_ended(c) for c in handlers)
    case Assert(test=Const(value=False | 0)):
        return True
# default:
return any(self._is_node_return_ended(c) for c in node.get_children())
```

Determinism note: the `Try` case iterates **sets of nodes** — `any`/`all`
short-circuit order is set-iteration order (hash of node objects = id()-based
⇒ nondeterministic ordering), but since the predicate is pure and the
aggregate (any/all) is order-independent, the *result* is deterministic.
Only inference caching side-effects could differ; treat as order-free.

- The default branch makes a FunctionDef "return-ended" if ANY child is —
  children of FunctionDef = decorators?, args, returns-annotation, body
  statements. In practice the last body statement being return-ended is not
  required; a `return` anywhere as a direct child counts (matching pylint's
  loose semantics).
- `Assert(test=Const(value=False | 0))`: match literal `False` uses `is`,
  `0` uses `==`; `assert 0.0` / `assert 0j` match `0` via equality; note
  `Const(False)` matches the `False` alternative. `assert False, "msg"`
  also matches (msg not inspected).
- `While`: `node.test.bool_value()` — astroid `bool_value()`; for
  uninferable tests returns `Uninferable` whose `__bool__` is False ⇒ falls
  through to orelse check. `while True:` without break ⇒ return-ended.
  `_loop_exits_early` (checkers/base/basic_error_checker.py:47-67):

  ```python
  inner_loop_nodes = [n for n in loop.nodes_of_class((For, While),
                       skip_klass=(FunctionDef, ClassDef)) if n != loop]
  return any(n for n in loop.nodes_of_class(Break, skip_klass=(FunctionDef, ClassDef))
             if _get_break_loop_node(n) not in inner_loop_nodes)
  ```

  `_get_break_loop_node` (same file, :28-44): walks parents until a
  For/While that does not have the current chain node in its `orelse`.

#### `_is_if_node_return_ended` (1939-1967)

```
is_if_returning = any(self._is_node_return_ended(n) for n in node.body
                      if not isinstance(n, FunctionDef))
if not node.orelse:
    if not self._has_return_in_siblings(node): return False
    return is_if_returning
is_orelse_returning = any(... for n in node.orelse if not isinstance(n, FunctionDef))
return is_if_returning and is_orelse_returning
```

`_has_return_in_siblings` (2053-2061): walks `next_sibling()` chain looking
for a **direct** Return statement sibling.

The "no orelse" branch is the famous pylint heuristic: an if without else is
return-ended only if the if-body returns AND a `return` appears among its
following siblings (the sibling return covers the not-taken path).

#### `_is_raise_node_return_ended` (1969-2004)

```
if not node.exc: return True                       # bare raise
if not utils.is_node_inside_try_except(node): return True
exc = utils.safe_infer(node.exc)
if exc is None or Uninferable or no pytype attr: return False
exc_name = exc.pytype().split(".")[-1]
handlers = utils.get_exception_handlers(node, exc_name) or-coerce to list/[]
if handlers:
    return any(self._is_node_return_ended(h) for h in handlers)
return True                                        # exception escapes
```

`is_node_inside_try_except` (utils.py:1134): nearest
`find_try_except_wrapper_node` (utils.py:997 — climbs `current.parent`
until parent is ExceptHandler or Try, returns that parent) is a `Try`.
`get_exception_handlers(node, exc_name)` (utils.py:1061): if wrapper is a
Try, returns handlers for which `error_of_type(handler, exc_name)` — NOTE
bare `except:` does NOT count: `error_of_type` has `if not handler.type:
return False`, then `handler.catch(expected_errors)` (astroid
ExceptHandler.catch: any handler-type name in the given name set);
otherwise `[]` (never None in 4.0.5, the `is not None` guard is vestigial).
So a `raise StopIteration` under a bare `except:` is treated as escaping
(R1710 path returns True at the no-handlers fallthrough), and for R1708's
`node_ignores_exception` a bare except does NOT suppress the message.

#### `_is_function_def_never_returning` (2063-2088)

```
try: if node.qname() in self._never_returning_functions: return True
except (TypeError, AttributeError): pass
try: returns = node.returns
except AttributeError: return False
match returns:
    case Attribute(attrname=name) | Name(name=name):
        return name in {"NoReturn", "Never"}
return False
```

Default `never-returning-functions = {"sys.exit", "argparse.parse_error"}`.
Annotation must be a bare Name/Attribute (string annotations and
`Subscript` like `NoReturn[...]` don't match).

`utils.is_terminating_func` (utils.py:2211-2255):

```
func must be Attribute or Name; node.parent must not be Lambda
for inferred in node.func.infer():           # InferenceError/StopIteration → False
    if inferred.qname() in TERMINATING_FUNCS_QNAMES: return True
    # frozenset {"_sitebuiltins.Quitter","sys.exit","posix._exit","nt._exit",
    #            "unittest.case.TestCase.fail"}   (utils.py:240-249)
    unwrap BoundMethod(_proxied=UnboundMethod(_proxied=p)) -> p
    if (isinstance(inferred, FunctionDef)
        and (not AsyncFunctionDef or node.parent is Await)
        and isinstance(inferred.returns, Name)
        and safe_infer(inferred.returns).qname() in TYPING_NEVER|TYPING_NORETURN):
        return True
```

`TYPING_NORETURN = {"typing.NoReturn", "typing_extensions.NoReturn"}`,
`TYPING_NEVER = {"typing.Never", "typing_extensions.Never"}`
(constants.py:110-121).

### 3.10 R1711 useless-return — `_check_return_at_the_end` (2090-2120)

Called from `leave_functiondef` AFTER `_check_consistent_returns`
(both may fire for the same function only if... R1710 requires an explicit
return; R1711 path requires the single return be bare ⇒ mutually exclusive
in practice).

```
if len(self._return_nodes[node.name]) != 1: bail   # exactly one Return in the fn (excl. nested defs)
if not node.body: bail
last = node.body[-1]
if isinstance(last, Return) and len(node.body) == 1: bail   # whole body is just one return → exempt
while isinstance(last, (If, Try, ExceptHandler)):
    last = last.last_child()
match last:
    case Return(value=None):            emit
    case Return(value=Const(value=None)): emit
add_message("useless-return", node=node)    # node = FunctionDef → keyword position
```

- The single counted Return need not be `last` — but `last` must be a bare
  return/`return None`; with only one Return in the function they coincide
  unless the Return is nested elsewhere and `last` is something else (then
  no match ⇒ no message).
- `last_child()` descent: for If → last orelse stmt (or last body stmt if no
  orelse); for Try → last child per astroid child order (finalbody last if
  present, else handlers/orelse). This finds `return` as the syntactically
  last leaf through if/try nesting.
- `def f(): return` (body == [Return]) is exempt; `def f(): "doc"; return`
  emits.

### 3.11 R1708 stop-iteration-return

Two trigger sites.

(a) `visit_raise` → `_check_stop_iteration_inside_generator` (1049-1066):

```
frame = node.frame()
bail unless isinstance(frame, FunctionDef) and frame.is_generator()
bail if utils.node_ignores_exception(node, StopIteration)
bail if not node.exc                                 # bare raise
exc = utils.safe_infer(node.exc)
bail unless isinstance(exc, (bases.Instance, nodes.ClassDef))
if any(c.qname() == "builtins.StopIteration" for c in exc.mro()):
    add_message("stop-iteration-return", node=node, confidence=INFERENCE)
```

`exc.mro()` — for an Instance, proxied to its class's mro; includes the
class itself. `utils.EXCEPTIONS_MODULE = "builtins"` (utils.py:46).
`node_ignores_exception` (utils.py:1148): nearest Try wrapper has a handler
catching StopIteration (by name, incl. bare except and tuple types), or a
surrounding `contextlib.suppress(StopIteration)` `with`.
`frame.is_generator()`: astroid — function contains yield (not in nested
function); async generators: `AsyncFunctionDef.is_generator()` also True,
and isinstance(frame, FunctionDef) is True for async ⇒ raise-site applies
to async gens too.

(b) `visit_call` → `_check_raising_stopiteration_in_generator_next_call`
(1217-1262):

```
bail if node.func is an Attribute            # x.next()
bail if len(node.args) == 0                  # next() with no args (#7828)
inferred = utils.safe_infer(node.func)
bail unless isinstance(inferred, FunctionDef) and inferred.qname() == "builtins.next"
frame = node.frame()
has_sentinel_value = len(node.args) > 1
if (isinstance(frame, FunctionDef) and frame.is_generator()
    and not has_sentinel_value
    and not utils.node_ignores_exception(node, StopIteration)
    and not _looks_like_infinite_iterator(node.args[0])):
    add_message("stop-iteration-return", node=node, confidence=INFERENCE)
```

`_looks_like_infinite_iterator`: `safe_infer(param)` is a `bases.Instance`
whose `qname()` ∈ `{"itertools.count", "itertools.cycle"}` (line 32).

### 3.12 R1717/R1718 consider-using-{dict,set}-comprehension (1076-1110)

`visit_call` → `_check_consider_using_comprehension_constructor`:

```
node must match Call(func=Name(name=name), args=[ListComp(elt=element), *_])
match name:
    case "dict":
        bail if element is a Call
        if element is IfExp with body/orelse both 2-element Tuple|List:
            (key1,value1), (key2,value2) = body.elts, orelse.elts
            bail if key1.as_string() != key2.as_string()
                    and value1.as_string() != value2.as_string()    # both differ ⇒ bail (#5588)
        add_message("consider-using-dict-comprehension", node=node)
    case "set":
        add_message("consider-using-set-comprehension", node=node)
```

Purely syntactic on the callable name (`dict`/`set` shadowed locally still
triggers). Additional args after the ListComp are allowed (`*_`).
No message args.

### 3.13 R1728/R1729 consider-using-generator / use-a-generator (1112-1139)

```
node must match Call(func=Name(name=call_name), args=[ListComp() as comp])
    where call_name in {"any","all","sum","max","min","list","tuple"}   # exactly ONE positional arg
inside_comp = comp.as_string()[1:-1]            # strip the [ ]
if node.keywords:
    inside_comp = f"({inside_comp})" + ", " + ", ".join(kw.as_string() for kw in node.keywords)
if call_name in {"any", "all"}: add_message("use-a-generator", node, args=(call_name, inside_comp))
else: add_message("consider-using-generator", node, args=(call_name, inside_comp))
```

`comp.as_string()` is astroid's round-trip rendering — port must match
pyast's as_string exactly. `node.keywords` includes `**kwargs` entries
(kw.as_string() renders `key=val` / `**d`). Again purely syntactic by name.

### 3.14 R1722 consider-using-sys-exit (1184-1201)

```
node.func must be Name with name in {"quit", "exit"}    (BUILTIN_EXIT_FUNCS, line 33)
local_scope = node.scope()
if _has_exit_in_scope(local_scope) or _has_exit_in_scope(node.root()): bail
add_message("consider-using-sys-exit", node=node, confidence=HIGH)
```

`_has_exit_in_scope(scope)`: `scope.locals.get("exit")` first entry is an
`ImportFrom` or `Import` node. NOTE: only the name `"exit"` is looked up in
locals, even when the called name is `quit`. The check looks at the call's
own scope and the module root (not intermediate enclosing scopes).

### 3.15 R1725 super-with-arguments — `_check_super_with_arguments` (1203-1215)

```
node must match Call(func=Name(name="super"),
                     args=[Name(name=name), Name(name="self")])
    where (frame_class := node_frame_class(node)) is not None
      and name == frame_class.name
add_message("super-with-arguments", node=node)
```

`node_frame_class` (utils.py:677): climbs `node.frame()` then
`klass.parent.frame()` until reaching a ClassDef (or None). So
`super(Outer, self)` inside a method of class `Outer` (even within a nested
function inside the method — the climb passes through function frames)
triggers. Exactly two positional args, both bare Names, second literally
`self`. Keywords/starred → no match.

### 3.16 R1737 use-yield-from — `visit_yield` (1163-1182)

```
node must match Yield(value=Name(name=name),
                      parent=Expr(parent=For(body=[_]) as loop_node)) \
    if not isinstance(loop_node, AsyncFor)
bail if loop_node.target.name != name      # AttributeError-free: target Tuple has no .name?
                                            # (Tuple targets: loop_node.target.name raises? NO —
                                            # nodes.Tuple has no 'name' attribute → AttributeError!)
bail if isinstance(node.frame(), AsyncFunctionDef)
add_message("use-yield-from", node=loop_node, confidence=HIGH)
```

CAREFUL: `loop_node.target.name` — if the For target is a Tuple
(`for a, b in ...: yield a`), `.name` raises AttributeError... but the
pattern `For(body=[_])` only constrains body length; the yield's value is a
Name; target Tuple lacks `.name` ⇒ **AttributeError propagates** out of the
checker → walker prints traceback and re-raises → caught at
`check_astroid_module` level as fatal `astroid-error` (F0002)? — Actually
verified behavior: `nodes.Tuple` does not define `.name`; astroid NodeNG
`__getattr__` is not defined ⇒ genuine AttributeError. In practice pylint
4.0.5 would crash on `def f():\n for a,b in x: yield a`?? — NO: the match
guard requires `Yield.value` be a `Name` and the For body be exactly
`[Expr(Yield)]`; with target Tuple the code still reaches
`loop_node.target.name`. This appears to be a latent crash; pylint's
functional tests use `use_yield_from.py` which... **OPEN QUESTION** — test
empirically (`for a, b in items: yield a`). If it crashes upstream we must
crash identically (astroid-error F0001/F0002 semantics per notes/02).

Conditions recap: yield must be a bare statement (`Expr` wrapper), the SOLE
statement in a synchronous `for`, yielding exactly the loop variable, in a
non-async function. Message on the **For** node, HIGH.

### 3.17 R1701 consider-merging-isinstance (1309-1363)

`visit_boolop` → `_check_consider_merging_isinstance`:

```
if node.op != "or": bail
first_args = _duplicated_isinstance_types(node)
for duplicated_name, class_names in first_args.items():        # dict insertion order
    names = sorted(name for name in class_names)               # lexicographic sort of as_string()s
    add_message("consider-merging-isinstance", node=node,
                args=(duplicated_name, ", ".join(names)))
```

`_duplicated_isinstance_types`:

```
duplicated_objects = set(); all_types = defaultdict(set)
for call in node.values:                       # direct BoolOp values only (no recursion)
    skip unless isinstance(call, Call) and len(call.args) == 2
    inferred = utils.safe_infer(call.func)
    skip unless inferred and utils.is_builtin_object(inferred)   # root().name == "builtins"
    skip unless inferred.name == "isinstance"
    isinstance_object = call.args[0].as_string()
    if isinstance_object in all_types: duplicated_objects.add(isinstance_object)
    elems = [t.as_string() for t in call.args[1].itered()] if isinstance(call.args[1], Tuple) \
            else [call.args[1].as_string()]
    all_types[isinstance_object].update(elems)
return {k: v for k, v in all_types.items() if k in duplicated_objects}
```

Multiple messages possible (one per duplicated object), emitted in
first-occurrence order of the object string. The second arg contains ALL
types seen for that object (including from the first call), sorted as
strings. Set dedup ⇒ duplicates collapse. `is_builtin_object`
(utils.py:286): `node and node.root().name == "builtins"`.

### 3.18 R1714 consider-using-in — `_check_consider_using_in` (1365-1409)

```
allowed_ops = {"or": "==", "and": "!="}
bail unless node.op in allowed_ops and len(node.values) >= 2
for value in node.values:
    match value:
        case Compare(left=Call()) | Compare(ops=[(_, Call())]):  bail (whole check)
        case Compare(ops=[(op, _)]) if op in allowed_ops[node.op]:  ok
        case _: bail
# all values are single-op Compares with the right operator and no Call operands
variables, values = [], []
for value in node.values:
    variable_set = set()
    for comparable in (value.left, value.ops[0][1]):
        if isinstance(comparable, (Name, Attribute)):
            variable_set.add(comparable.as_string())
        values.append(comparable.as_string())            # ALL operand strings, in order
    variables.append(variable_set)
common_variables = reduce(set.intersection, variables)
bail if empty
common_variable = sorted(list(common_variables))[0]      # lexicographically smallest
values = list(collections.OrderedDict.fromkeys(values))  # dedup, keep first-seen order
values.remove(common_variable)                           # removes FIRST occurrence only
values_string = ", ".join(values) if len(values) != 1 else values[0] + ","
maybe_not = "" if node.op == "or" else "not "
add_message("consider-using-in", node=node,
            args=(common_variable, maybe_not, values_string), confidence=HIGH)
```

- `op in allowed_ops[node.op]` is a **substring test against "==" or "!="**
  — for real comparison operators this behaves as equality ("=" alone is
  not an operator), replicate as equality with the mapped operator.
- The dedup list `values` still contains the common variable string once;
  `values.remove` drops it. If a comparison is `x == x`, both operands are
  the common var; dedup leaves one entry which is removed ⇒ values possibly
  empty ⇒ `", ".join([])` = `""` (message with empty list) — edge case,
  replicate.
- Single remaining value gets a trailing comma: `'x' ==in ('1',)` style.

### 3.19 R1716 chained-comparison — `_check_chained_comparison` (1411-1465)

```
bail unless node.op == "and" and len(node.values) >= 2
uses = defaultdict(lambda: {"lower_bound": set(), "upper_bound": set()})
for comparison_node in node.values:
    if isinstance(comparison_node, Compare):
        left_operand = comparison_node.left
        for operator, right_operand in comparison_node.ops:   # walks chained ops!
            for operand in (left_operand, right_operand):
                match operand:
                    case Name(name=value) | Const(value=value) if value is not None: ok
                    case _: continue
                match operator:
                    case "<" | "<=":
                        if operand is left_operand:  uses[value]["lower_bound"].add(comparison_node)
                        elif operand is right_operand: uses[value]["upper_bound"].add(comparison_node)
                    case ">" | ">=":
                        if operand is left_operand:  uses[value]["upper_bound"].add(comparison_node)
                        elif operand is right_operand: uses[value]["lower_bound"].add(comparison_node)
            left_operand = right_operand
for bounds in uses.values():                    # dict insertion order; break on first hit
    num_shared = len(bounds["lower_bound"] & bounds["upper_bound"])
    if num_shared < len(bounds["lower_bound"]) and num_shared < len(bounds["upper_bound"]):
        add_message("chained-comparison", node=node); break
```

Keys of `uses` are **mixed**: variable names (str) and Const values (any
hashable; `value is not None` filters None) — `a < 3 and 3 < b` keys
include `3`. The bound sets store the Compare node objects; the "shared"
intersection counts comparisons providing both bounds for the same key
(i.e. already chained: `a < b < c` puts b in lower and upper from the SAME
node ⇒ shared). Emit when some key has a lower-bound from one comparison
and an upper-bound from a different one. One message max per BoolOp.

### 3.20 R1726/R1727 simplifiable-condition / condition-evals-to-constant (1467-1546)

`visit_boolop` → `_check_simplifiable_condition(node)`:

```
bail unless utils.is_test_condition(node)        # parent is If/While/IfExp/Assert and node is (inside) parent.test,
                                                 # or node in Comprehension.ifs, or parent is bool(...) call
self._can_simplify_bool_op = False
simplified_expr = self._simplify_boolean_operation(node)
bail unless self._can_simplify_bool_op
if not next(simplified_expr.nodes_of_class(Name), False):
    add_message("condition-evals-to-constant", node=node,
                args=(node.as_string(), simplified_expr.as_string()))
else:
    add_message("simplifiable-condition", node=node,
                args=(node.as_string(), simplified_expr.as_string()))
```

`is_test_condition` (utils.py:1708-1718):

```python
match parent := parent or node.parent:
    case While() | If() | IfExp() | Assert():
        return node is parent.test or parent.test.parent_of(node)
    case Comprehension():
        return node in parent.ifs
return is_call_of_name(parent, "bool") and parent.parent_of(node)
```

NOTE: visit_boolop fires for EVERY BoolOp, including nested ones inside a
test (parent of nested BoolOp is the outer BoolOp ⇒ is_test_condition False
unless... `parent.test.parent_of(node)` — for a nested BoolOp under an If
test, parent is the outer BoolOp, which matches NO case ⇒ falls to
bool-call check ⇒ False). So only the **top-level** BoolOp of a test is
checked... careful: `node.parent` of the top BoolOp is the If itself ⇒ case
matches, `node is parent.test` True. A BoolOp nested as `not (a or True)`:
parent is UnaryOp ⇒ no match ⇒ skipped. But `if (a or True) and b:` — the
inner `a or True` has parent = outer BoolOp ⇒ skipped individually, yet the
OUTER BoolOp's recursion simplifies it.

`_simplify_boolean_operation(bool_op)` (1496-1518):

```
children = list(bool_op.get_children())
intermediate = [self._simplify_boolean_operation(c) if isinstance(c, BoolOp) else c
                for c in children]
result = _apply_boolean_simplification_rules(bool_op.op, intermediate)
if len(result) < len(children): self._can_simplify_bool_op = True
if len(result) == 1: return result[0]
simplified = copy.copy(bool_op); simplified.postinit(result); return simplified
```

`_apply_boolean_simplification_rules(operator, values)` (1467-1494):

```
simplified_values = []
for subnode in values:
    inferred_bool = None
    if not next(subnode.nodes_of_class(Name), False):   # skip anything containing a Name
        inferred = utils.safe_infer(subnode)
        if inferred: inferred_bool = inferred.bool_value()
    if not isinstance(inferred_bool, bool):
        simplified_values.append(subnode)
    elif (operator == "or") == inferred_bool:
        return [subnode]                                 # short-circuit value kept
return simplified_values or [nodes.Const(operator == "and")]
```

- Subexpressions containing any `Name` are never simplified (kept verbatim).
- `or` with a truthy constant ⇒ entire op collapses to that constant
  subnode; `and` with falsy constant likewise.
- Removing only-irrelevant constants (`False` in or / `True` in and) keeps
  the rest; if all removed, synthesize `Const(True)` for `and`, `Const(False)`
  for `or` (rendered "True"/"False" by as_string).
- `copy.copy(bool_op)` shallow copy + `postinit(result)` — `as_string()` of
  the simplified BoolOp must render with astroid's precedence/parens rules
  operating on the original child nodes (their `.parent` is reassigned by
  postinit? astroid `postinit` just sets `self.values = result` — child
  parents still point at originals; as_string doesn't use parent, it uses
  operator precedence of the nodes themselves — port must mirror pyast
  as_string for BoolOp with mixed children).
- `bool_value()` on inferred Const/objects: standard astroid truthiness;
  for nodes like Const("x") → True; `Uninferable.bool_value()` is
  Uninferable but `safe_infer` returning Uninferable? `safe_infer` can
  return Uninferable (it's the first value) — then
  `inferred.bool_value()` is `Uninferable` ⇒ not a bool ⇒ kept. Also note
  `if inferred:` — Uninferable is falsy ⇒ skipped anyway.

Message arg 1 is the ORIGINAL `node.as_string()`; arg 2 the simplified
rendering. R1727 when simplified contains no Name anywhere; else R1726.

### 3.21 R1706/R1709 — visit_return / visit_assign (1561-1623)

`visit_assign` (1583-1591) first calls `_append_context_managers_to_stack`
(R1732, §3.22) then **delegates to `visit_return(node)`** — Assign nodes go
through the same ternary/swap logic.

```
_check_swap_variables(node)                       # R1712, §3.23
if self._is_and_or_ternary(node.value):
    cond, truth_value, false_value = _and_or_ternary_arguments(node.value)
else: return
if both truth_value and false_value are Compare: return
inferred_truth_value = utils.safe_infer(truth_value, compare_constants=True)
if inferred_truth_value is None or Uninferable: return
truth_boolean_value = inferred_truth_value.bool_value()
if truth_boolean_value is False:
    message, suggestion = "simplify-boolean-expression", false_value.as_string()
else:
    message = "consider-using-ternary"
    suggestion = f"{truth_value.as_string()} if {cond.as_string()} else {false_value.as_string()}"
add_message(message, node=node, args=(suggestion,), confidence=INFERENCE)
```

`_is_and_or_ternary` (1891-1902):

```
node matches BoolOp(op="or", values=[BoolOp(op="and", values=[_, v1]), v2])
    and not (isinstance(v2, BoolOp) or isinstance(v1, BoolOp))
```

Exactly 2 values in each BoolOp; the and's first value (the condition) may
be anything including another BoolOp. `truth_boolean_value is False` —
strict identity with False: `bool_value()` returning Uninferable (truthy
object? Uninferable is falsy but not False) ⇒ goes to consider-using-ternary
branch. Note `Return` nodes with `node.value is None` → `_is_and_or_ternary(None)`
→ no match → return.

### 3.22 R1732 consider-using-with — full machinery

Constants (refactoring_checker.py:34-63):

```
CALLS_THAT_COULD_BE_REPLACED_BY_WITH = frozenset((
    "threading.lock.acquire", "threading._RLock.acquire",
    "threading.Semaphore.acquire", "multiprocessing.managers.BaseManager.start",
    "multiprocessing.managers.SyncManager.start"))
CALLS_RETURNING_CONTEXT_MANAGERS = frozenset((
    "_io.open", "pathlib.Path.open", "pathlib._local.Path.open", "codecs.open",
    "urllib.request.urlopen", "tempfile.NamedTemporaryFile",
    "tempfile.SpooledTemporaryFile", "tempfile.TemporaryDirectory",
    "tempfile.TemporaryFile", "zipfile.ZipFile", "zipfile.PyZipFile",
    "zipfile.ZipFile.open", "zipfile.PyZipFile.open", "tarfile.TarFile",
    "tarfile.TarFile.open", "multiprocessing.context.BaseContext.Pool",
    "subprocess.Popen"))
```

State: `ConsiderUsingWithStack` (213-239) — NamedTuple of three dicts
(module_scope, class_scope, function_scope), iterated function→class→module.
`get_stack_for_frame(frame)`: FunctionDef→function_scope (async too —
isinstance-based match? `case nodes.FunctionDef()` class pattern matches
subclass AsyncFunctionDef as well), ClassDef→class_scope, else module_scope.

(a) Direct call check — `_check_consider_using_with(node: Call)` (1671-1705):

```
bail if _is_inside_context_manager(node) or _is_a_return_statement(node)
bail if node in self._consider_using_with_stack.get_stack_for_frame(node.frame()).values()
        # identity comparison (NodeNG eq is default); call already tracked via assignment
inferred = utils.safe_infer(node.func)
bail unless isinstance(inferred, (FunctionDef, ClassDef, bases.BoundMethod))
could_be_used_in_with =
    inferred.qname() in CALLS_THAT_COULD_BE_REPLACED_BY_WITH
    or (inferred.qname() in CALLS_RETURNING_CONTEXT_MANAGERS
        and not _is_part_of_with_items(node))
if could_be_used_in_with and not _will_be_released_automatically(node):
    add_message("consider-using-with", node=node)
```

- `_is_inside_context_manager` (141-149): node.frame() is
  FunctionDef/BoundMethod/UnboundMethod with name `__enter__` or decorated
  with `contextlib.contextmanager` (`utils.decorated_with`, utils.py:870 —
  inference-based on decorator names/qnames).
- `_is_a_return_statement` (152-159): some ancestor strictly below the
  frame is a Return.
- `_is_part_of_with_items` (162-174): walk parents to frame; if a `With` is
  found, check `items[0][0].lineno <= node.lineno <= items[-1][0].tolineno`
  — a line-range test on the with-items, NOT structural membership
  (bug-for-bug: a call on the same lines but inside the first item's
  expression counts).
- `_will_be_released_automatically` (177-192): node.parent is a Call whose
  func infers to qname in `{"contextlib._BaseExitStack.enter_context",
  "contextlib.ExitStack.enter_context"}`.

But: the assignment tracking (b) runs in `visit_assign`, and `visit_call`
for the RHS call happens AFTER visit_assign (parent visited first) — so the
`node in stack.values()` bail works because the Assign added the value
first.

(b) Assignment tracking — `_append_context_managers_to_stack(node: Assign)`
(1625-1669):

```
bail if _is_inside_context_manager(node)
if node.targets[0] is Tuple/List/Set:
    assignees = node.targets[0].elts
    value = utils.safe_infer(node.value)
    bail if value is None or not hasattr(value, "elts")
    values = value.elts
else:
    assignees, values = [node.targets[0]], [node.value]
bail if any UninferableBase in (assignees, values)      # NOTE: checks the LISTS, always False!
for assignee, value in zip(assignees, values):          # zip truncates
    continue unless isinstance(value, Call)
    inferred = utils.safe_infer(value.func)
    continue unless inferred and inferred.qname() in CALLS_RETURNING_CONTEXT_MANAGERS \
                    and isinstance(assignee, (AssignName, AssignAttr))
    stack = self._consider_using_with_stack.get_stack_for_frame(node.frame())
    varname = assignee.name if AssignName else assignee.attrname
    if varname in stack:
        existing_node = stack[varname]
        if astroid.are_exclusive(node, existing_node):
            stack[varname] = value; continue
        add_message("consider-using-with", node=existing_node)   # redefined before use
    stack[varname] = value
```

- The `any(isinstance(n, UninferableBase) for n in (assignees, values))`
  line tests the two *lists* themselves — never UninferableBase ⇒ dead
  guard; keep as no-op.
- Only `targets[0]` considered (chained `a = b = open(...)` tracks only `a`).
- `are_exclusive` (astroid nodes/node_classes.py:116-185): two statements
  are exclusive iff their lowest common ancestor is an If with the nodes in
  different branches (test involvement ⇒ False), or a Try with body/handler,
  body/orelse-vs-handlers, or different handlers. Quote ported separately
  if not already in pyinfer notes.

(c) Consumption — `visit_with` (773-789): for each `(var, names)` item, if
`var` is a Name, delete `var.name` from the FIRST stack (function, class,
module order) containing it, `break` after deletion.

(d) Flush — `_emit_consider_using_with_if_needed(stack)` (1303-1307): for
each leftover `node in stack.values()` emit
`add_message("consider-using-with", node=node)` — node here is the **Call
value node** stored at assignment. Emission order = dict insertion order
(varname first-assignment order). Flush points: `leave_functiondef`
(function_scope, then `.clear()`), `leave_classdef` (class_scope),
`leave_module` (module_scope, then `_init()`).

NOTE the timing: messages flushed at scope exit, so their line numbers are
out of source order relative to other messages of the same module — but
pylint reports in EMISSION order within a module... no wait — pylint's text
reporter prints messages as emitted, grouped per module; out-of-order lines
within a module DO occur (e.g. R1732 flushed at leave_functiondef appears
after messages from later lines inside the function). Replicate emission
sequencing faithfully (cross-check notes/02 §ordering).

### 3.23 R1712 consider-swap-variables — `_check_swap_variables` (1561-1581)

Called from visit_return AND visit_assign (so any Return/Assign node may
anchor the triple):

```
bail unless node.next_sibling() and node.next_sibling().next_sibling()
assignments = [node, ns, ns.ns]
bail unless all are Assign(targets=[AssignName()], value=Name())   (_is_simple_assignment)
bail if any of the three in self._reported_swap_nodes
left  = [a.targets[0].name for a in assignments]
right = [a.value.name for a in assignments]
if left[0] == right[-1] and left[1:] == right[:-1]:
    self._reported_swap_nodes.update(assignments)
    add_message("consider-swap-variables", node=node)
```

Pattern: `t = a; a = b; b = t` (left=[t,a,b], right=[a,b,t]: left[0]==right[2]
(t==t), left[1:]==[a,b]==right[:2]). Message on the first statement. The
reported-set prevents the 2nd/3rd statements re-reporting overlapping
windows. Return nodes never match `_is_simple_assignment` (Assign-only) so
a Return anchor always bails at `all(...)`.

### 3.24 R1734 use-list-literal — `_check_use_list_literal` (1707-1713)

```
if node.as_string() == "list()":
    inferred = utils.safe_infer(node.func)
    if isinstance(inferred, ClassDef) and not node.args and inferred.qname() == "builtins.list":
        add_message("use-list-literal", node=node)
```

The `as_string() == "list()"` gate excludes any args/keywords textually.

### 3.25 R1735 use-dict-literal — `_check_use_dict_literal` (1715-1746)

```
bail unless node.func is Name "dict"
inferred = utils.safe_infer(node.func)
if isinstance(inferred, ClassDef) and inferred.qname() == "builtins.dict" and not node.args:
    add_message("use-dict-literal", args=(_dict_literal_suggestion(node),),
                node=node, confidence=INFERENCE)
```

Keywords allowed (suggestion incorporates them); positional args bail.

`_dict_literal_suggestion(node)` (1732-1746):

```
elements = []
for keyword in node.keywords:
    if len(", ".join(elements)) >= 64: break
    if keyword not in node.kwargs:                 # skip **expansions here
        elements.append(f'"{keyword.arg}": {keyword.value.as_string()}')
for keyword in node.kwargs:
    if len(", ".join(elements)) >= 64: break
    elements.append(f"**{keyword.value.as_string()}")
suggestion = ", ".join(elements)
return f"{{{suggestion}{', ... '  if len(suggestion) > 64 else ''}}}"
```

`node.kwargs` (astroid Call property) = keywords with `arg is None`
(`**d`). Named keys rendered with double quotes. Truncation marker
`, ... ` (with trailing space before `}`) appended when the final joined
string exceeds 64 chars. `dict()` bare ⇒ suggestion `{}`.

### 3.26 R1713 consider-using-join (1748-1802)

`visit_augassign` → `_check_consider_using_join(aug_assign)`:

```
for_loop = aug_assign.parent
bail unless isinstance(for_loop, For) and len(for_loop.body) == 1
assign = for_loop.previous_sibling()
bail unless isinstance(assign, Assign)
result_assign_names = {t.name for t in assign.targets if isinstance(t, AssignName)}
is_concat_loop = (aug_assign.op == "+="
    and isinstance(aug_assign.target, AssignName)
    and len(for_loop.body) == 1
    and aug_assign.target.name in result_assign_names
    and isinstance(assign.value, Const) and isinstance(assign.value.value, str)
    and self._name_to_concatenate(aug_assign.value) == for_loop.target.name)
if is_concat_loop: add_message("consider-using-join", node=aug_assign)
```

- The AugAssign must be the DIRECT (and only) child of the For (parent
  check); `for ...: result += x` with anything else in the body bails.
- The statement immediately before the For must initialize the same name to
  a string constant (any string, not just empty).
- `for_loop.target.name` — Tuple target raises AttributeError? Equality
  with a string: `_name_to_concatenate` returns str|None; `None ==
  Tuple.name`→ AttributeError again — **only if target is a Tuple AND the
  aug value is a Name/JoinedStr**; e.g. `s = ""\nfor a, b in x: s += a` ⇒
  `for_loop.target.name` AttributeError → crash path. OPEN QUESTION to
  verify empirically (same class as §3.16).

`_name_to_concatenate(node)` (1748-1766):

```
Name → node.name
JoinedStr:
    values = [v for v in node.values if isinstance(v, FormattedValue)]
    bail unless len(values) == 1 and isinstance(values[0].value, Name)
    with_separators = len(node.values) > len(values)
    if with_separators and not self._suggest_join_with_non_empty_separator: return None
    return values[0].value.name
else None
```

So `s += f"{x}"` (exactly one interp, no literal text) always eligible;
`s += f"{x}, "` only when `suggest-join-with-non-empty-separator=True`
(default). Format specs on the FormattedValue are ignored (still 1 value).

### 3.27 R1721 unnecessary-comprehension — `_check_unnecessary_comprehension` (1814-1889)

visit_comprehension (fires once per `for` clause of every comprehension):

```
bail if parent is GeneratorExp
bail unless len(node.ifs) == 0 and len(node.parent.generators) == 1 and node.is_async is False
match node:
    case Comprehension(target=Tuple(elts=elts),
                       parent=DictComp(key=Name(name=key_name), value=Name(name=value_name))) \
         if all(isinstance(e, AssignName) for e in elts):
        expr_list = [key_name, value_name]; target_list = [e.name for e in elts]
    case Comprehension(parent=(ListComp() | SetComp()) as parent):
        elt:  Name → expr_list = name (a STRING)
              Tuple of all-Names → expr_list = [names]  (else bail)
              other → expr_list = []
        target: AssignName → target_list = name (a STRING)
                Tuple → target_list = [AssignName elt names]   # non-AssignName elts silently dropped!
                other → target_list = []
    case _: bail
if expr_list == target_list and expr_list:        # str==str or list==list; "" and [] falsy
    inferred = utils.safe_infer(node.iter)
    match (node.parent, inferred):
        case [DictComp(), objects.DictItems()]: args = (f"dict({node.iter.func.expr.as_string()})",)
        case [ListComp(), nodes.List()]:        args = (f"list({node.iter.as_string()})",)
        case [SetComp(), nodes.Set()]:          args = (f"set({node.iter.as_string()})",)
        else: args = None
    if args: add_message("unnecessary-comprehension", node=node.parent, args=args); return
    func = "dict"|"list"|"set" by parent type
    add_message("unnecessary-comprehension", node=node.parent,
                args=(f"{func}({node.iter.as_string()})",))
```

- DictComp case requires `{k: v for k, v in it}` exactly (key/value bare
  Names matching the 2-tuple target names IN ORDER); the iter inferring to
  `objects.DictItems` (i.e. `d.items()`) gives the refined suggestion
  `dict(d)` using `node.iter.func.expr.as_string()` (assumes iter is an
  Attribute call — guaranteed? NO: any iterable inferring to DictItems —
  if iter is a Name bound to `d.items()`, `node.iter.func` AttributeError!
  safe_infer of a Name CAN yield DictItems. Latent crash; replicate or
  verify — pylint tests cover only direct `.items()` calls. OPEN QUESTION).
- ListComp/SetComp: `[x for x in y]` (expr_list/target_list both strings)
  or `[(a, b) for a, b in y]` (Tuple elt of Names vs Tuple target). The
  target Tuple drops non-AssignName elts (e.g. starred) before comparing —
  `[(a, b) for a, *b in y]`: target_list=[a] (Starred dropped... actually
  Starred contains AssignName; isinstance(Starred, AssignName) False ⇒
  dropped) vs expr [a, b] ⇒ no match. Good.
- Message on the comprehension parent (whole `[...]` expression).

### 3.28 R1733 unnecessary-dict-index-lookup (2122-2264)

Trigger sites: `visit_for(node)` and `visit_comprehension(node)`.

```
node.iter must match Call(func=Attribute(attrname="items", expr=expr))
inferred = utils.safe_infer(node.iter.func); bail unless astroid.BoundMethod
iterating_object_name = expr.as_string()
messages = []                                    # deferred when nested loops present
children = node.body if For else list(node.parent.get_children())
has_nested_loops = any For/While inside children (nodes_of_class chain)
for child in children:
    for subscript in child.nodes_of_class(Subscript):
        continue unless subscript.value is Name|Attribute
        value = subscript.slice
        if For and _is_part_of_assignment_target(subscript): return   # WHOLE check aborts
        if subscript.parent is Delete: return
        if isinstance(value, Name):                                  # for k, v in d.items(): d[k]
            continue unless (node.target is Tuple and len(elts) >= 2
                             and value.name == node.target.elts[0].name
                             and iterating_object_name == subscript.value.as_string())
            if For and value.lookup(value.name)[1][-1].lineno > node.lineno: continue
            emit/queue args=(node.target.elts[1].as_string(),)
        elif isinstance(value, Subscript):                            # for item in d.items(): d[item[0]]
            continue unless (node.target is AssignName and value.value is Name
                             and node.target.name == value.value.name
                             and iterating_object_name == subscript.value.as_string())
            if For and value.value.lookup(...)[1][-1].lineno > node.lineno: continue
            inferred = utils.safe_infer(value.slice)
            continue unless Const value == 0
            suggestion = "1".join(value.as_string().rsplit("0", maxsplit=1))
            emit/queue args=(suggestion,)
for message in messages: add_message(... node=message["node"], args=(message["variable"],))
```

- `_is_part_of_assignment_target` (195-210): node (or enclosing
  Tuple/List chain) is in `Assign.targets` / is `AugAssign.target`.
  Finding ANY such subscript aborts the whole For check with NO messages
  (early `return`), including already-queued ones — but direct
  `add_message` calls already made (no nested loops) are NOT retracted.
  Order matters: children scanned in order; a write after a read keeps the
  earlier directly-emitted messages but kills queued ones. Replicate
  exactly.
- The Delete guard likewise hard-returns.
- `node.target.elts[0].name` — first tuple element must be an AssignName
  (Starred/sub-tuple → AttributeError crash potential; e.g.
  `for (a, b), v in d.items(): d[x]`... only reached when value.name
  equality is evaluated — left-to-right: `value.name == node.target.elts[0].name`
  evaluates RHS → AttributeError for non-AssignName elts[0]. Latent crash.)
- Suggestion for the item-subscript case replaces the LAST "0" in the
  rendered `item[0]` string with "1": `"1".join("item[0]".rsplit("0", 1))`
  → `item[1]`. For names containing 0 (e.g. `it0m[0]` → only last 0
  replaced → `it0m[1]`). Verbatim.
- `lookup(...)` redefinition guard ONLY for For nodes (comprehensions skip
  it). `value.lookup(value.name)[1][-1]` = LAST assignment statement node
  for that name in scope order; `.lineno > node.lineno` ⇒ redefined after
  loop ⇒ skip this subscript.
- For comprehensions, `children = list(node.parent.get_children())`
  includes the elt/key/value and the Comprehension node itself (whose iter
  contains... the items() call — its Subscript children get scanned too).

### 3.29 R1736 unnecessary-list-index-lookup (2266-2454)

```
node.iter must match Call(func=Name(name="enumerate"))
preliminary_confidence = HIGH
iterable_arg = utils.get_argument_from_call(node.iter, position=0, keyword="iterable")
   on NoSuchArgumentError: iterable_arg = utils.infer_kwarg_from_call(node.iter, "iterable")
                           preliminary_confidence = INFERENCE
bail unless isinstance(iterable_arg, Name)
node.target must match Tuple(elts=[AssignName(name1), AssignName(name2), *_])
has_start_arg, confidence = self._enumerate_with_start(node)
bail if has_start_arg
confidence = INFERENCE if preliminary_confidence == INFERENCE else confidence
iterating_object_name = iterable_arg.name
bad_nodes = []
children = node.body if For else list(node.parent.get_children())
has_nested_loops = ... ; has_if_statements = any If inside children
for child in children:
    for subscript in child.nodes_of_class(Subscript):
        if For and _is_part_of_assignment_target(subscript): return
        if subscript.parent is Delete: return
        index = subscript.slice
        if isinstance(index, Name):
            continue unless index.name == name1 and iterating_object_name == subscript.value.as_string()
            if For and (lookup_results := index.lookup(index.name)[1]) and lookup_results[-1].lineno > node.lineno: continue
            if For and index.lookup(name2)[1][-1].lineno > node.lineno: continue
            if has_nested_loops: bad_nodes.append(subscript)
            elif has_if_statements: continue
            else: add_message("unnecessary-list-index-lookup", node=subscript,
                              args=(name2,), confidence=confidence)
for subscript in bad_nodes: add_message(...same args/confidence)
```

- `get_argument_from_call` (utils.py:717): positional index 0 else keyword
  `iterable` in node.keywords; raises NoSuchArgumentError otherwise. Note
  `enumerate(iterable=x)` ⇒ found via keyword with HIGH confidence (the
  try/except only fires when neither positional nor keyword matched).
  `infer_kwarg_from_call` (utils.py:747): looks in `call.kwargs` (i.e.
  `**d`) inferring d as Dict and extracting the "iterable" item — returns
  the VALUE node (must still be a Name).
- `_enumerate_with_start` (2402-2434): second positional arg or `start=`
  keyword. `_get_start_value` (2436-2454): Const → (value, HIGH);
  UnaryOp(operand=Const) → (operand.value, HIGH) — NOTE sign ignored!
  `-1` returns 1?? No: returns `node.operand.value` = 1 (positive) with the
  unary op dropped — so `enumerate(x, -0)` → 0 ⇒ "no start". `start=-1` →
  start_val 1 ⇒ has_start True anyway. Else safe_infer Const →
  (value, INFERENCE); else (None, INFERENCE).
  Return `(not start_val == 0, confidence)`; start_val None ⇒ (False, conf).
  Note `not start_val == 0`: start_val=False → False==0 True ⇒ not → False
  ("no start").
- Unlike R1733, the dict-style value-subscript form doesn't exist here, but
  there's an extra conservatism: subscripts inside `if` statements are
  skipped entirely (`has_if_statements` ⇒ continue) unless there are nested
  loops (then queued via bad_nodes!). Note the asymmetry: nested loops
  take precedence over if-statements in the branch order.
- Message arg is `name2` (the value variable name), confidence as computed
  (INFERENCE if the iterable came from **kwargs or start needed inference,
  else HIGH).

---------------------------------------------------------------------------
## 4. NotChecker — C0117 unnecessary-negation (not_checker.py)

`visit_unaryop` (decorated `only_required_for_messages("unnecessary-negation")`):

```
bail unless node.op == "not"
operand = node.operand
if operand is UnaryOp with op "not":              # not not X
    add_message("unnecessary-negation", node=node,
                args=(node.as_string(), operand.operand.as_string()))
elif operand is Compare:
    left = operand.left
    bail if len(operand.ops) > 1                  # chained comparison
    operator, right = operand.ops[0]
    bail unless operator in reverse_op            # {"<",">=","<=",">","==","!=","in","is"}
                                                  # NOTE: "not in"/"is not" NOT in table ⇒ bail
    frame = node.frame()
    bail if frame.name == "__ne__" and operator == "=="
    for _type in (utils.node_type(left), utils.node_type(right)):
        bail if not _type
        bail if isinstance(_type, nodes.Set)              # skipped_nodes
        bail if isinstance(_type, astroid.Instance) and \
                _type.qname() in {"builtins.set", "builtins.frozenset"}
    suggestion = f"{left.as_string()} {reverse_op[operator]} {right.as_string()}"
    add_message("unnecessary-negation", node=node, args=(node.as_string(), suggestion))
```

`reverse_op` (not_checker.py:29-38): `<→>=, <=→>, >→<=, >=→<, ==→!=,
!=→==, in→not in, is→is not`.

`utils.node_type` (utils.py:1494-1513): iterate `node.infer()`, skip
Uninferable and None-values (`is_none`: None / Const(None) /
`Name(value="None")` — note Name has no `value` attr so that pattern never
matches, effectively None/Const(None)); collect into a set; >1 distinct ⇒
None; InferenceError ⇒ None; empty ⇒ None. So BOTH operands must be
confidently single-typed for the comparison form. `not x == y` where x is
an unannotated parameter ⇒ node_type None ⇒ no message (big conservatism in
practice).

Set exemptions: comparisons on sets are not order-reversible (`not a <= b`
≠ `a > b`), hence Set literals and set/frozenset instances bail.

Message args: `(node.as_string(), suggestion)` rendered into
`Consider changing "%s" to "%s"`.

Both `add_message` calls: default confidence (UNDEFINED), node = the outer
UnaryOp.

---------------------------------------------------------------------------
## 5. RecommendationChecker (recommendation_checker.py)

`open()`: `self._py36_plus = self.linter.config.py_version >= (3, 6)`
(py-version default = running interpreter ⇒ (3,12) ⇒ True).

`_is_builtin(node, function)` (69-74): `inferred = safe_infer(node)`;
`inferred and is_builtin_object(inferred) and inferred.name == function`.

### 5.1 C0200 consider-using-enumerate — `_check_consider_using_enumerate` (191-262)

visit_for:

```
bail unless node.iter is Call and _is_builtin(node.iter.func, "range") and node.iter.args
is_constant_zero = args[0] is Const with value == 0       # NOTE: True for Const(False) too (False==0)!
bail if len(args) == 2 and not is_constant_zero
bail if len(args) > 2
last arg must match Call(func=second_func, args=[iterating_object]) with _is_builtin(second_func, "len")
iterating_object must be Name (→ expect Name subscripts) or Attribute (→ expect Attribute subscripts); else bail
if iterating_object is Name("self") and node.scope().name == "__iter__": bail
for child in node.body:
    for subscript in child.nodes_of_class(Subscript):
        continue unless subscript.value is expected type
        value = subscript.slice; continue unless value is Name
        continue if subscript.value.scope() != node.scope()
        if value.name == node.target.name and (
             (subscript.value is Name and iterating_object.name == subscript.value.name)
          or (subscript.value is Attribute and iterating_object.attrname == subscript.value.attrname)):
            add_message("consider-using-enumerate", node=node); return
```

- Accepted ranges: `range(len(x))` and `range(0, len(x))` (also
  `range(False, len(x))` via the `== 0` quirk).
- Attribute matching compares ONLY `attrname` (`self.x` vs `other.x` both
  match — bug-for-bug).
- `node.target.name`: Tuple target → AttributeError (latent crash, same
  family as §3.16; only reached when a Name-sliced subscript of the right
  shape exists).
- One message max per For, node=the For statement, no args.

### 5.2 C0201 consider-iterating-dictionary (83-106)

visit_call → `_check_consider_iterating_dictionary`:

```
bail unless node.func is Attribute with attrname == "keys"
bail if node.parent is BinOp with op in {"&", "|", "^"}      # set ops on keys() are legit
comp_ancestor = utils.get_node_first_ancestor_of_type(node, Compare)
if (node.parent is For/Comprehension) or (comp_ancestor and any(
        op for op, comparator in comp_ancestor.ops
        if op in {"in", "not in"} and (comparator in node.node_ancestors() or comparator is node))):
    match utils.safe_infer(node.func):
        case astroid.BoundMethod(bound=nodes.Dict()):
            add_message("consider-iterating-dictionary", node=node, confidence=INFERENCE)
```

- `node.parent is For` means the keys() call IS the iterable
  (`for k in d.keys()`) — parent of iter is the For; for comprehensions the
  parent is the Comprehension node.
- Membership form: any `in`/`not in` whose right-hand comparator is the
  keys() call or an ancestor expression containing it
  (`k in sorted(d.keys())`? comparator=sorted-call which IS an ancestor of
  node ⇒ matches — bug-for-bug breadth).
- `BoundMethod.bound` must be a literal `nodes.Dict` (inference of `d` in
  `d.keys`): only dicts inferring to a Dict literal trigger.

### 5.3 C0207 use-maxsplit-arg — `_check_use_maxsplit_arg` (108-179)

```
bail unless node.func is Attribute, attrname in {"split", "rsplit"},
       and safe_infer(node.func) is astroid.BoundMethod
inferred_expr = safe_infer(node.func.expr)
if isinstance(inferred_expr, astroid.Instance) and any(inferred_expr.nodes_of_class(ClassDef)):
    bail
```

NOTE on that guard: `Instance.nodes_of_class` proxies to the underlying
ClassDef, and `ClassDef.nodes_of_class(ClassDef)` always yields at least the
class itself ⇒ **any `bases.Instance` result bails**. But `nodes.Const` IS
an Instance subclass (astroid node_classes.py:2014 `class Const(..., Instance)`)
— Const("x").nodes_of_class(ClassDef) iterates the *Const node's* children
(none) because Const is itself a NodeNG with its own nodes_of_class ⇒ empty
⇒ no bail. So effectively: expr inferring to a Const str (literal-backed)
passes; expr inferring to a plain str Instance (e.g. `str(x)`) bails.
Replicate via: bail iff inferred_expr is a non-node `bases.Instance`
(proxy) — in pyinfer terms: instance-of-class values bail, Const values
pass, other node values pass.

```
confidence = HIGH
sep = get_argument_from_call(node, 0, "sep")
  on NoSuchArgumentError: sep = infer_kwarg_from_call(node, "sep"); confidence = INFERENCE
                          if not sep: bail
# maxsplit must be absent:
get_argument_from_call(node, 1, "maxsplit") → found ⇒ bail
  on NoSuchArgumentError: if infer_kwarg_from_call(node, "maxsplit"): bail
bail unless node.parent is Subscript
subscript_value = utils.get_subscript_const_value(node.parent).value
  on InferredTypeError: bail            # slice not inferable to Const
if node.parent.slice is Name:
    scope = node.scope()
    for loop_node in scope.nodes_of_class((For, While)):
        continue unless loop_node.parent_of(node)
        for a in loop_node.nodes_of_class(AugAssign):
            if node.parent.slice.name == a.target.name: bail     # a.target may lack .name (AugAssign target Attribute/Subscript) → AttributeError risk
        for a in loop_node.nodes_of_class(Assign):
            if node.parent.slice.name in [n.name for n in a.targets]: bail   # same .name risk for non-Name targets
if subscript_value in (-1, 0):          # NOTE: False in (-1,0) is True (False==0)
    fn_name = node.func.attrname
    new_fn = "rsplit" if subscript_value == -1 else "split"
    new_name = node.func.as_string().rsplit(fn_name, maxsplit=1)[0] \
               + new_fn + f"({sep.as_string()}, maxsplit=1)[{subscript_value}]"
    add_message("use-maxsplit-arg", node=node, args=(new_name,), confidence=confidence)
```

- `f"[{subscript_value}]"` interpolates the *inferred Python value*:
  `[-1]`, `[0]`, or `[False]` (!) when the index inferred to Const False.
  Also `[0]` results from index `0.0`? `0.0 in (-1, 0)` True,
  `0.0 == -1` False ⇒ "split", rendered `[0.0]`. Verbatim formatting.
- `node.func.as_string().rsplit(fn_name, 1)[0]` keeps everything before the
  final occurrence of "split"/"rsplit" in the rendered receiver —
  e.g. `"a.b.split"` → `"a.b."` + `"split(...)"`.
- The mutation-in-loop guards iterate `scope.nodes_of_class(...)` — the
  WHOLE enclosing scope, restricted to loops that are ancestors of node.
  Assign targets that aren't Names raise AttributeError on `.name`
  (e.g. `x[0] = ...` inside the loop) — latent crash family. Empirically:
  `n.name` for AssignName fine; `nodes.AssignAttr` has no `.name` →
  AttributeError. OPEN QUESTION (verify with corpus; salt/django may hit).

### 5.4 C0206 consider-using-dict-items (264-345)

For-loop variant `_check_consider_using_dict_items(node: For)`:

```
iterating_object_name = utils.get_iterating_dictionary_name(node)
bail if None
for child in node.body:
    for subscript in child.nodes_of_class(Subscript):
        continue unless subscript.value is Name|Attribute
        value = subscript.slice
        continue unless value is Name and value.name == node.target.name \
                        and iterating_object_name == subscript.value.as_string()
        last_definition_lineno = value.lookup(value.name)[1][-1].lineno
        continue if last_definition_lineno > node.lineno          # key redefined after loop
        if (subscript.parent is Assign and subscript in subscript.parent.targets) \
           or (subscript.parent is AugAssign and subscript == subscript.parent.target):
            return                                                # write ⇒ abort silently
        if subscript.parent is Delete: return
        add_message("consider-using-dict-items", node=node); return
```

`get_iterating_dictionary_name` (utils.py:1781-1803):

```
node.iter matches Call(func=Attribute(attrname="keys")):
    bail unless safe_infer(node.iter.func) is BoundMethod
    return node.iter.as_string().rpartition(".keys")[0]    # text before LAST ".keys"
node.iter is Name|Attribute:
    bail unless safe_infer(node.iter) is nodes.Dict        # literal dict inference
    return node.iter.as_string()
else None
```

Both `for k in d.keys():` and `for k in d:` (d inferring to a Dict literal)
are eligible. `node.target.name` — Tuple target AttributeError risk again
(`for k, v in d:` plus `d[k]` in body... value.name==target.name evaluation
order: LHS first (fine), RHS `.name` on Tuple → AttributeError). In real
code `for k, v in d:` iterating a dict literal of pairs is rare.

Comprehension variant `_check_consider_using_dict_items_comprehension`
(323-345): same iterating-name resolution; children =
`node.parent.get_children()`; the inner filter is ONLY the
Name-slice/target-name/object-name equality (no lookup guard, no
write/delete guards); message `node=node` (the **Comprehension** node!
fromlineno = the `for` keyword position inside the comprehension? —
Comprehension nodes in astroid are positionless (no lineno of their own);
they inherit/compute fromlineno from first child = target. Cross-check
pyast: Comprehension is in the "positionless" family (notes/04: MatchCase
positionless; Comprehension HAS no direct position attrs in astroid 4 —
`Comprehension` carries... VERIFY: astroid Comprehension sets
lineno/col_offset? In astroid 4.0, Comprehension has position attributes
None and NodeNG.fromlineno falls back to first-child chain ⇒ target's
lineno). Must match astroid exactly — covered by pyast tree fidelity.

Emission: at most one message per For/comprehension, early-returned.

### 5.5 C0208 use-sequence-for-iteration (347-359)

visit_for + visit_comprehension → `_check_use_sequence_for_iteration`:

```
if isinstance(node.iter, nodes.Set) and not any(utils.has_starred_node_recursive(node)):
    add_message("use-sequence-for-iteration", node=node.iter, confidence=HIGH)
```

`has_starred_node_recursive` (utils.py:2064-2077): for For/Comprehension
nodes iterates `node.iter.elts` recursing into nested Sets, yields True on
Starred. So `for x in {*a, 1}:` exempt; `for x in {1, 2}:` triggers.
Message node = the Set literal.

### 5.6 C0209 consider-using-f-string (361-452)

`visit_const` (decorated only_required_for_messages("consider-using-f-string")):

```
bail unless self._py36_plus
bail unless node.pytype() == "builtins.str" and not isinstance(node.parent, JoinedStr)
_detect_replacable_format_call(node)
```

(a) `.format()` branch — node.parent is Attribute with attrname "format":

```
bail unless node.parent.parent is Call          # .format referenced but not called
keyword_args = [i[0] for i in utils.parse_format_method_string(node.value)[0]]
  on IncompleteFormatString: bail
if call.args:                                   # positional args present
    for arg in call.args:
        if arg is Starred and safe_infer(arg.value) is List with len(elts) > 1: bail
        if "\\" in arg.as_string(): bail        # backslash can't be in f-string expr
elif call.keywords:                             # NOTE: elif! kwargs only checked when no positional args
    for keyword in call.keywords:
        if keyword_args.count(keyword.arg) > 1: bail     # key used twice in template
        keyword = safe_infer(keyword.value)
        if keyword is Dict and len(keyword.items) > 1 and len(keyword_args) > 1: bail
add_message("consider-using-f-string", node=node, line=node.lineno, col_offset=node.col_offset)
```

`parse_format_method_string` (utils.py:637-663) uses
`string.Formatter().parse` via `collect_string_fields` (utils.py:603-633);
ValueError from the parser (other than the Jython manual/automatic case,
which yields "" and "1") raises `IncompleteFormatString`;
`split_format_field_names` (utils.py:594-600) =
`_string.formatter_field_name_split`, ValueError → IncompleteFormatString.
keyword_args are the non-numeric field head names (e.g. `{a.b}` → "a",
`{0}` → counted as explicit positional, `{}` → implicit positional).

(b) `%` branch — node.parent is BinOp with op "%":

```
if "\\" in node.parent.right.as_string(): bail
bail unless hasattr(node.parent.left, "value") and isinstance(node.parent.left.value, str)
if "{" in node.parent.left.value or "}" in node.parent.left.value: bail
match safe_infer(node.parent.right):
    case Dict(items=items) | List(elts=items) if len(items) > 1: bail
add_message("consider-using-f-string", node=node, line=node.lineno, col_offset=node.col_offset)
```

DOUBLE-EMISSION QUIRK: visit_const fires for EVERY str Const. In
`"%s" % "y"` BOTH constants have parent BinOp(%); the left passes (it has
.value str), the right also passes all right-side checks (left.value is
still the format string) ⇒ **two C0209 messages**, one at each Const's
position. Similarly `"a {0}".format("b")` — the inner "b" Const's parent is
the Call, not Attribute/BinOp ⇒ no double for .format. Replicate.

The mod branch checks `node.parent.left.value` even when node IS the left
operand (self-check). There's no validation of the % template at all (no
parse), and no check on the right operand type beyond Dict/List-len>1 —
`"%(k)s" % d` (single-key dict literal) triggers.

---------------------------------------------------------------------------
## 6. ImplicitBooleanessChecker (implicit_booleaness_checker.py)

`options = ()`. `_operators = {"!=", "==", "is not", "is"}` (line 107).

### 6.0 visit_compare gating + the "-to-str" symbol bug (175-188)

```python
@utils.only_required_for_messages(
    "use-implicit-booleaness-not-comparison",
    "use-implicit-booleaness-not-comparison-to-string",
    "use-implicit-booleaness-not-comparison-to-zero",
)
def visit_compare(self, node: nodes.Compare) -> None:
    if self.linter.is_message_enabled("use-implicit-booleaness-not-comparison"):
        self._check_use_implicit_booleaness_not_comparison(node)
    if self.linter.is_message_enabled(
        "use-implicit-booleaness-not-comparison-to-zero"
    ) or self.linter.is_message_enabled(
        "use-implicit-booleaness-not-comparison-to-str"   # <-- TYPO: not a registered symbol!
    ):
        self._check_compare_to_str_or_zero(node)
```

`is_message_enabled` with an unknown descriptor
(lint/message_state_handler.py:315-345): `get_active_msgids` raises
UnknownMessageError → caught → treated as a raw msgid →
`_is_one_message_enabled("use-implicit-booleaness-not-comparison-to-str", None)`
→ `self._msgs_state.get(msgid, True)` → **True always** (the typo'd id is
never in `_msgs_state`). Net effect:

- `_check_compare_to_str_or_zero` is invoked for EVERY Compare whenever
  visit_compare is registered (i.e. whenever any of the three real symbols
  is enabled — C1803 is default-on, so effectively always).
- Inside, the C1805 branch is correctly gated on
  `is_message_enabled("use-implicit-booleaness-not-comparison-to-zero")`
  (default False ⇒ skipped), but the C1804 branch is gated on the same
  typo'd `-to-str` name ⇒ **always taken**; the final `add_message(
  "use-implicit-booleaness-not-comparison-to-string", ...)` is then dropped
  by normal message filtering (C1804 default-off). So with default config
  the only cost is wasted work; with `--enable=C1804` it behaves correctly;
  with line-level pragmas there's no semantic difference because
  add_message re-checks state at the line. Port: replicate gating shape
  (the typo gate ≡ constant True).

Note the line=None semantics of these gates: file-global state at the time
of the visit (after pragma collection? `_msgs_state` is the global
en/disable map — module pragmas live in `file_state._module_msgs_state`,
consulted only when line is not None). So `# pylint: disable=C1805` inline
does NOT skip the computation, only the emission.

### 6.1 C1802 use-implicit-booleaness-not-len

(a) `visit_call` (109-150), gated `only_required_for_messages("use-implicit-booleaness-not-len")`:

```
bail unless utils.is_call_of_name(node, "len")       # Call(func=Name(name="len"))
parent = node.parent
while isinstance(parent, BoolOp): parent = parent.parent
bail unless utils.is_test_condition(node, parent)
len_arg = node.args[0]                               # IndexError if len() has no args → crash
if len_arg is ListComp|SetComp|DictComp:
    add_message(..., node=node, confidence=HIGH); return
instance = next(len_arg.infer())                     # FIRST result only (not safe_infer)
  on astroid.InferenceError: bail
mother_classes = self.base_names_of_instance(instance)
affected_by_pep8 = any(t in mother_classes for t in ("str","tuple","list","set"))
if "range" in mother_classes or (affected_by_pep8 and not self.instance_has_bool(instance)):
    add_message(..., node=node, confidence=INFERENCE)
```

- `is_test_condition(node, parent)`: note the BoolOp-unwrapping means
  `if x or len(y):` qualifies (parent ends at the If; `parent.test.parent_of(node)`
  True). `assert len(x)`, `while len(x)`, `bool(len(x))`, comprehension
  `if` guards qualify. `not len(x)` does NOT reach here as a test (parent
  UnaryOp ⇒ is_test_condition False) — handled by (b).
- `base_names_of_instance` (409-420): for `bases.Instance` (INCLUDING
  Const/List/Tuple/Set/Dict nodes — Const and BaseContainer subclass
  Instance; nodes.Dict also subclasses Instance) returns
  `[node.name] + [a.name for a in node.ancestors()]` where `.name` proxies
  to the class name ("list", "str", ...; ancestors typically ["object"]).
  For Uninferable/non-Instance: `[]`.
- `instance_has_bool` (152-159): `class_def.getattr("__bool__")` truthy →
  True; AttributeInferenceError → False. Called with the *instance*
  (proxied getattr looks up the class+ancestors). In the pinned astroid
  brain, builtins str/list/tuple/set/range have NO `__bool__` attr (and
  object doesn't either) ⇒ these emit; int/bool have `__bool__`.
  A user class deriving from list with `__bool__` defined ⇒ suppressed.
- `len()` (zero args) inside a test ⇒ IndexError crash (latent; → fatal
  astroid-error handling — OPEN QUESTION verify upstream behavior).

(b) `visit_unaryop` (161-173):

```
if node.op == "not" and utils.is_call_of_name(node.operand, "len"):
    add_message("use-implicit-booleaness-not-len", node=node, confidence=HIGH)
```

UNCONDITIONAL on context — `x = not len(y)` anywhere triggers; no inference,
no test-condition requirement. Message node = the UnaryOp.

### 6.2 C1803 use-implicit-booleaness-not-comparison (251-298)

```
bail if len(node.ops) != 1                       # chained comparison
operator, comparator = node.ops[0]
is_left_empty_literal  = is_base_container(node.left) or is_empty_dict_literal(node.left)
is_right_empty_literal = is_base_container(comparator) or is_empty_dict_literal(comparator)
bail unless is_left ^ is_right                   # exactly one side an empty literal
target_node  = node.left if is_right_empty_literal else comparator
literal_node = comparator if is_right_empty_literal else node.left
target_instance = utils.safe_infer(target_node); bail if None
mother_classes = base_names_of_instance(target_instance)
is_base_comprehension_type = any(t in mother_classes for t in ("tuple","list","dict","set"))
if not is_base_comprehension_type and instance_has_bool(target_instance): bail
if operator in {"==", "!=", ">=", ">", "<=", "<"}:
    add_message("use-implicit-booleaness-not-comparison",
                args=_implicit_booleaness_message_args(node, literal_node, operator, target_node),
                node=node, confidence=HIGH)
```

- `is_base_container` (utils.py:1933): BaseContainer (List/Set/Tuple) with
  empty `elts`; `is_empty_dict_literal` (1937): Dict with no items. So the
  literal side is one of `[]`, `()`, `{}` (set literal can't be empty).
- `safe_infer` returning Uninferable: `target_instance is None` check only —
  Uninferable passes! Then base_names_of_instance(Uninferable) = [] and
  `instance_has_bool(Uninferable)` → Uninferable.getattr? UninferableBase
  has `__getattr__` returning itself ⇒ `class_def.getattr("__bool__")`
  returns Uninferable (callable? `getattr` attribute access yields
  Uninferable, CALLING it yields Uninferable, truthy? Uninferable is falsy!)
  → `return True` requires the call result... code: `class_def.getattr("__bool__"); return True`
  — the call doesn't raise for Uninferable ⇒ returns True ⇒ bail. So
  Uninferable targets are SUPPRESSED via the has-bool path. Replicate: in
  pyinfer, Uninferable → bail.
- `is` / `is not` are NOT in the operator set (silently no message);
  ordering ops ARE (e.g. `x > []` triggers).
- Args helper (300-341):

```
description = {List: "list", Tuple: "tuple", Dict: "dict", Const: "str"}.get(type(literal_node), "iterable")
collection_literal = {"list": "[]", "tuple": "()", "dict": "{}"}.get(description, "iterable")
instance_name = "x"
target_node Call      → f"{target_node.func.as_string()}(...)"
target_node Attribute|Name → target_node.as_string()
original_comparison = f"{instance_name} {operator} {collection_literal}"
suggestion = _get_suggestion(node, instance_name, operator, {"!="})
return (original_comparison, suggestion, description)
```

  `type(literal_node)` exact-type lookup: literal is List/Tuple/Dict in
  practice ⇒ "list"/"tuple"/"dict" (Const/str unreachable via this path).
  Non-Name/Attribute/Call targets render as literal `x`.
- `_get_suggestion(node, name, operator, negation_redundant_ops)` (332-341):

```
if operator in negation_redundant_ops:           # {"!="} here
    return name if _in_boolean_context(node) else f"bool({name})"
return f"not {name}"
```

  So `x != []` → suggestion `x` (in bool context) / `bool(x)`;
  `x == []`, `x >= []`, etc. → `not x`.
- `_in_boolean_context(node)` (343-407): climb (current,parent) pairs:
  - If/While/Assert with `current is parent.test` → True
  - IfExp with current is parent.test → True
  - UnaryOp("not") with current is parent.operand → True
  - Comprehension with current in parent.ifs → True
  - GeneratorExp where current is parent.elt AND parent.parent is
    `all(...)`/`any(...)` call AND parent in parent.parent.args → True
  - Lambda where current is parent.body AND parent.parent is `filter(...)`
    AND parent in parent.parent.args → True
  - bool(...) call with current in parent.args → True
  - BoolOp with current in parent.values → climb (current=parent) and loop
  - anything else → break ⇒ False

### 6.3 C1804/C1805 — `_check_compare_to_str_or_zero` (190-249)

```
bail if len(node.ops) != 1
negation_redundant_ops = {"!=", "is not"}
ops = [("", node.left), *node.ops]; flatten → (_, left_operand, operator, right_operand)
bail unless operator in {"!=", "==", "is not", "is"}
if is_message_enabled("use-implicit-booleaness-not-comparison-to-zero"):     # real gate (C1805)
    operand = right if _is_constant_zero(left) else (left if _is_constant_zero(right) else None)
    if operand is not None:
        original = f"{left_operand.as_string()} {operator} {right_operand.as_string()}"
        suggestion = _get_suggestion(node, operand.as_string(), operator, negation_redundant_ops)
        add_message("use-implicit-booleaness-not-comparison-to-zero",
                    args=(original, suggestion), node=node, confidence=HIGH)
if is_message_enabled("use-implicit-booleaness-not-comparison-to-str"):      # typo ⇒ always True
    node_name = right.as_string() if is_empty_str_literal(left) else \
                (left.as_string() if is_empty_str_literal(right) else None)
    if node_name is not None:
        suggestion = _get_suggestion(node, node_name, operator, negation_redundant_ops)
        add_message("use-implicit-booleaness-not-comparison-to-string",
                    args=(node.as_string(), suggestion), node=node, confidence=HIGH)
```

- `_is_constant_zero` (17-20): `Const and node.value == 0 and node.value
  is not False` — `0`, `0.0`, `0j`, `-0.0`?? (`-0.0` is UnaryOp(Const) not
  Const ⇒ no), `Decimal`? only Const literals: 0, 0.0, 0j. False excluded
  explicitly.
- `is_empty_str_literal` (utils.py:1941): Const, str, falsy (`""`).
- C1805 arg 1 is reconstructed `left OP right` (as_string of operands);
  C1804 arg 1 is `node.as_string()` (whole comparison). Suggestions:
  `!=`/`is not` → bare name (bool-context) or `bool(name)`; `==`/`is` →
  `not name`.
- NO inference on the other operand at all — `if x == 0:` triggers C1805
  for any x (when enabled).
- Both sides zero (`0 == 0`)? `_is_constant_zero(left)` wins ⇒ operand =
  right (`0`), suggestion `not 0`. Both empty strings: left match wins,
  node_name = right's as_string `''`.

---------------------------------------------------------------------------
## 7. Helper-function port specs (exact semantics)

Already specified inline above; consolidated list with sources:

- `utils.safe_infer` (utils.py:1348-1410): first inferred value; ambiguity
  detection via `_get_python_type_of_node` set membership; optional
  `compare_constants` (used by R1706/R1709): two Consts with `!=` values ⇒
  None; FunctionDef pairs with ambiguous signatures ⇒ None; InferenceError
  on first → None; later InferenceError → None; StopIteration → value.
  May RETURN Uninferable if first value is Uninferable (the set only adds
  non-Uninferable types; a second non-Uninferable value of some type would
  not match the empty/None? — careful: first value Uninferable ⇒
  inferred_types stays empty; second value adds its type; len ≤ 1 ⇒
  returns the FIRST (Uninferable) value).
- `utils.infer_all` (1414-1422): list(node.infer()) with `lru_cache(512)`;
  [] on InferenceError. (Cache shared per astroid run — port may ignore
  cache, it's semantics-neutral.)
- `utils.is_call_of_name` (1700): Call with func Name(name).
- `utils.is_test_condition` (1708): see §3.20.
- `utils.node_type` (1494): see §4.
- `utils.get_argument_from_call` (717) / `infer_kwarg_from_call` (747) /
  `NoSuchArgumentError` (utils.py:252): see §3.29.
- `utils.get_subscript_const_value` (1806): safe_infer(slice) must be
  Const else InferredTypeError.
- `utils.get_iterating_dictionary_name` (1781): see §5.4.
- `utils.is_base_container` (1933) / `is_empty_dict_literal` (1937) /
  `is_empty_str_literal` (1941).
- `utils.get_node_first_ancestor_of_type` (1963): first matching ancestor
  via `node_ancestors()`.
- `utils.has_starred_node_recursive` (2064): see §5.5.
- `utils.is_terminating_func` (2211): see §3.9; TERMINATING_FUNCS_QNAMES
  utils.py:240.
- `utils.get_inverse_comparator` (2265): table in §3.7.
- `utils.node_frame_class` (677): §3.15.
- `utils.decorated_with` (870): decorator list; Call decorators unwrap to
  .func; infer() each; any ClassDef/FunctionDef with `.name in qnames or
  .qname() in qnames`; InferenceError → continue.
- `utils.is_builtin_object` (286): `node.root().name == "builtins"`.
- `utils.find_try_except_wrapper_node` (997), `get_exception_handlers`
  (1061), `is_node_inside_try_except` (1134), `node_ignores_exception`
  (1148, + contextlib.suppress scan 1081-1131): §3.9/§3.11.
- `utils.parse_format_method_string` (637) + `collect_string_fields` (603)
  + `split_format_field_names` (594) + `IncompleteFormatString` (504):
  §5.6 — port needs a faithful `string.Formatter.parse` +
  `_string.formatter_field_name_split` reimplementation (shared with the
  strings checker — coordinate with notes/08).
- `basic_error_checker._loop_exits_early` (:47) + `_get_break_loop_node`
  (:28): §3.9.
- `astroid.are_exclusive` (astroid nodes/node_classes.py:116): §3.22 —
  LCA-based If/Try branch exclusivity; quote in pyinfer notes if not there.
- `only_required_for_messages` (utils.py:480): §1.2.

`bases.Instance` subclass facts needed: `nodes.Const`, `nodes.List/Set/Tuple`
(via BaseContainer, node_classes.py:269) and `nodes.Dict` are Instance
subclasses ⇒ they flow through `base_names_of_instance` and pattern
`case astroid.BoundMethod(bound=nodes.Dict())` etc.

---------------------------------------------------------------------------
## 8. Iteration order, determinism, and emission-order notes

1. Within one module, message ORDER = emission order (notes/02). For one
   line with several messages from different checkers, order follows walker
   callback order: per node, checkers in registration order
   (Refactoring → Not → Recommendation → ImplicitBooleaness for nodes both
   visit, e.g. UnaryOp: RefactoringChecker has no visit_unaryop; NotChecker
   C0117 then ImplicitBooleaness C1802(b) — NotChecker registered first).
2. dict/defaultdict iteration = insertion order everywhere
   (R1701 first-args, R1716 uses, R1732 stacks). The only true *set*
   iterations (R1710 Try-children, R1701 class-name sets) are followed by
   order-insensitive aggregation or explicit `sorted(...)`.
3. R1732 messages flush at scope exit (leave_functiondef/classdef/module) ⇒
   non-monotonic line numbers within a module; replicate exactly.
4. `_elifs`/state leak across modules when R1732 disabled (§1.3).
5. `visit_assign` → `visit_return` delegation double-registers nothing (the
   walker calls visit_assign for Assign, visit_return for Return; the
   delegation is an ordinary call).
6. add_message line=None vs explicit line: only R1707 (line only, col 0)
   and C0209 (explicit line/col equal to node's) deviate from pure node
   positioning; R1710/R1711 use FunctionDef.position (def-keyword span).

---------------------------------------------------------------------------
## 9. Test-surface checklist (minimum corpus probes per message)

- R1701: `isinstance(x,int) or isinstance(x,(str,bytes)) or isinstance(y,int) or isinstance(y,float)`
  ⇒ messages for x then y; sorted type lists.
- R1702: 6-deep nest; elif not adding depth; `with` not counting; leftover
  at function end.
- R1703: True/False return pair; False-first exemption; assign pair with
  multi-target sorted match.
- R1704: `for x in ...` redefining arg x; dummy rgx suppression (`_x`).
- R1705/20/23/24: elif-arg wording vs else; try/except/else; finalbody
  exemption; mid-body return.
- R1706/09: `a and b or c` forms; Compare-pair bail; truth-value False ⇒ R1709.
- R1707: `x = 1,` / `return 1,` / enable-pragma rescan path; comma inside
  parens NOT flagged.
- R1708: raise StopIteration in gen; subclass via inference; next() without
  default; itertools.count exemption; handled-exception exemption.
- R1710/11: async def exemption; same-name shadowing suppression;
  single-bare-return body exemption; try/finally last_child descent.
- R1712: temp-swap triple; overlap dedup.
- R1713: `s = ''` + for + `s += x`; f-string with separator (option on/off).
- R1714: `x == 1 or x == 2` HIGH; Call operand bail; trailing-comma single
  value.
- R1715: subscript assign guarded by `in` compare with Dict-literal
  inference; same-target else.
- R1716: `a < b and b < c`; shared-bound non-emission for `a < b < c and d`.
- R1717/18: `dict([...])` incl. IfExp #5588 exemption; `set([...])`.
- R1719: Compare test ⇒ 'test'; else 'bool(test)'; inverted ⇒ 'not test'.
- R1721: `[x for x in y]` with/without literal-List inference; dict-items
  refinement; async/ifs bails.
- R1722: exit()/quit(); `from sys import exit` local & module scope.
- R1725: `super(Cls, self)` in method incl. nested fn.
- R1726/27: `foo or True` (R1727: True), `foo and True` (R1726: foo);
  parenthesized rendering of simplified BoolOp.
- R1728/29: list-comp arg to any/all (R1729) vs list/tuple/sum/max/min
  (R1728); keywords append `(elt for ...), key=...` formatting.
- R1730/31: forward/reverse comparisons, Const str(...) rendering.
- R1732: direct acquire(); assignment + with-consumption; exclusive-branch
  reassign; redefinition message on the FIRST node; scope flush order.
- R1733/36: nested-loop deferral; assignment-target abort; lookup-after-loop
  guard; rsplit("0") suggestion; enumerate start arg forms incl. UnaryOp
  sign-drop quirk; if-statement skip (R1736 only).
- R1734/35: shadowed dict/list (inference qname gate); `dict(a=1)` suggestion
  quoting; 64-char truncation `, ... `.
- R1737: bare `for x in it: yield x`; async exemptions both sides.
- C0117: `not not x`; `not x == y` with both operands single-typed;
  `__ne__`/`==` exemption; set-type exemptions.
- C0200: `range(len(x))` / `range(0, len(x))`; self.__iter__ exemption;
  attr-vs-name subscript matching.
- C0201: `for k in d.keys()` / `if k in d.keys()`; `&|^` exemption;
  Dict-literal-bound inference requirement.
- C0206: keys()-call and plain-dict iteration; write/delete aborts;
  comprehension variant node position.
- C0207: `s.split(',')[0]` / `[-1]`; Const-backed receiver vs str()
  instance; loop-mutation guard; maxsplit-present bails; **kwargs sep ⇒
  INFERENCE.
- C0208: `for x in {1,2}` and comprehension over set literal; starred
  exemption.
- C0209: format with kwargs/positional rules; % with dict>1 bail; the
  double-emission `"%s" % "y"` quirk; py-version gate.
- C1802: len in if/while/assert/bool/comprehension-if/or-chain;
  `not len(x)` unaryop unconditional; comprehension arg HIGH; range
  INFERENCE; custom __bool__ suppression.
- C1803: literal side either side; xor bail; `is` ops silent; suggestion
  bool-context variants; "x" placeholder for complex targets.
- C1804/05 (default OFF — only with --enable): zero/empty-string forms,
  `is not`/`!=` suggestion shapes; confirm default-off suppression and the
  always-run gating doesn't leak other messages.

---------------------------------------------------------------------------
## 10. Open questions / crash-parity items (verify empirically before port)

1. Tuple-target AttributeError family — `loop_node.target.name` /
   `node.target.name` accesses without AssignName guards in R1737 (§3.16),
   R1713 (§3.26), C0200 (§5.1), C0206 (§5.4), R1733 elts[0].name (§3.28).
   Determine actual pylint 4.0.5 behavior (crash → astroid-error F0002? or
   unreachable in practice). Must match byte-for-byte including any fatal
   message.
2. `len()` zero-arg IndexError in C1802 visit_call (§6.1).
3. R1721 DictComp + iter that infers to DictItems but isn't an
   Attribute-call (`node.iter.func` AttributeError) (§3.27).
4. C0207 loop-mutation guard `.name` on non-Name Assign targets (§5.3).
5. Confirm astroid-brain facts assumed in §6.1: builtins str/list/tuple/
   set/range/object lack `__bool__` in the pinned snapshot; int/bool have it
   (drives C1802 INFERENCE emissions).
6. Confirm walker behavior on `AsyncFor`: visit_for NOT called (exact-name
   dispatch) — affects R1704/R1733/R1736/C0200/C0206/C0208 skipping
   `async for`.
7. The `_is_trailing_comma` interaction with NL (non-logical newline)
   tokens inside parens — confirm commas inside brackets never see a
   NEWLINE on their row (tokenizer emits NL) ⇒ never flagged.
