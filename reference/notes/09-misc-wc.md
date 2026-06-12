# 09 — Misc checkers: remaining W/C/R messages + shared frameworks (exact spec, pylint 4.0.5 / astroid 4.0.4)

Scope: every message NOT covered by notes/05 (variables E), notes/06 (typecheck E),
notes/08 (all other E/F for `-E`), notes/09-basic-wc.md (checkers/base W/C/R) and
notes/09-variables-imports-classes-wc.md (variables/imports/classes/exceptions/
method_args W/C/R). Concretely, this doc owns:

- `typecheck.py`: W1113 W1114 W1115 W1116 W1117, plus the FULL E1101/I1101
  no-member funnel (notes/06 explicitly excluded E1101/I1101 — they are needed
  for full-pylint mode; spec'd here in §1.7).
- `deprecated.py`: the DeprecatedMixin framework (W4901-W4906) — §2.
- `stdlib.py`: W1501 W1502 W1503 W1506 W1507 W1508 W1509 W1510 W1514 W1515
  W1518 + the deprecation data dicts feeding W4902-W4906 — §3. (E1507/E1519/
  E1520 are in notes/08 §13.)
- `strings.py`: StringFormatChecker W1300-W1310 (§4), StringConstantChecker
  W1401 W1402 W1404 W1405 W1406 (§5). (E13xx/E1310 in notes/08 §6.)
- `logging.py`: W1201 W1202 W1203 — §6. (E1200/E1201/E1205/E1206 in 08 §7.)
- `newstyle.py`: E1003 bad-super-call — §7. FULL trigger spec (gap-filler: no
  other doc specs the checker side; notes/07 §12.8 covers the Super object).
- `spelling.py`: C0401 C0402 C0403 — §8 (inert by default; enablement rules).
- `threading_checker.py`: W2101 — §9.
- `nested_min_max.py`: W3301 — §10.
- `bad_chained_comparison.py`: W3601 — §11.
- `dunder_methods.py`: C2801 — §12.
- `ellipsis_checker.py`: W2301 — §13.
- `lambda_expressions.py`: C3001 C3002 — §14.
- `non_ascii_names.py`: C2401 C2403 W2402 — §15.
- `unsupported_version.py`: W2601-W2606 — §16 (all dead at default py-version).
- `modified_iterating_checker.py`: W4701 — §17 (E4702/E4703 in 08).
- `match_statements_checker.py`: R1905 R1906 — §18 (E1901-E1904 in 08).
- `misc.py`: W0511 fixme, I0023 use-symbolic-message-instead — §19.
- `symilar.py`: R0801 duplicate-code — §20.
- §21: checkers/files with nothing to port.
- §22: MASTER COVERAGE TABLE — all 389 messages in
  `crates/pycheckers/src/msgs.rs` → owner → default-enabled → -E flag → doc.
- §23: ordering/conservatism consolidated notes; §24: open questions.

NOT in scope here (other/pending docs): format.py (C03xx, W0301, W0311),
refactoring/ (R17xx, C0117, C0200-C0209, C1802-C1805), design_analysis.py
(R09xx), and the score footer / exit-code bitmask (notes/02 owns output; for
full mode: bit values F=1 E=2 W=4 R=8 C=16, accumulated over DISPLAYED
messages; score footer comes from LinterStats — pipeline doc).

All file:line cites are `reference/pylint/pylint/...` @ v4.0.5 and
`reference/astroid/astroid/...` @ v4.0.4. Runtime pinned: CPython 3.12.12,
PYTHONHASHSEED=0.

Conventions (same as the sibling 09 docs):
- Report position: `add_message(..., node=N)` → PyLinter._add_one_message
  (lint/pylinter.py:1195-1280): if `N.position` is set (FunctionDef/ClassDef
  keyword anchoring) use it, else `N.fromlineno/.col_offset/.end_lineno/
  .end_col_offset`. Explicit `line=`/`col_offset=` args override (check is
  `if not line` — 0 counts as missing). Node-less messages: `line or 1`,
  `col_offset or 0`.
- Confidence: irrelevant to default output (`--confidence` default = all);
  carried for fidelity. Default UNDEFINED unless stated.
- "safe_infer" = checkers/utils.py:1348-1410 (notes/08 §0.1);
  "infer_all" = utils.py:1413-1422 (notes/08 §0.2).
- `%`-interpolation of args: `template % args` (pylinter.py:1252-1254).

================================================================================
# 0. Enablement infrastructure facts needed by this doc
================================================================================

## 0.1 Which checker modules are loaded by default

`pylint/checkers/__init__.py:128-130`:
```
def initialize(linter):
    register_plugins(linter, __path__[0])
```
`register_plugins` (pylint/utils/utils.py) imports every importable module in
`pylint/checkers/` that defines `register` and calls it. Every checker file
spec'd in this doc is therefore loaded by default. The registration ORDER
(and hence walker-callback order) is the directory-listing order used by
register_plugins — already extracted empirically (PROGRESS.md; the harness
order dump is ground truth, do not re-derive).

`pylint/extensions/*` are NOT loaded unless `--load-plugins` names them.
None of their messages appear in msgs.rs. OUT OF SCOPE (full list §22.1).

## 0.2 py-version gating of whole messages (may_be_emitted)

- Option `py-version` default = `sys.version_info[:2]` of the RUNNING
  interpreter (lint/base_options.py:356-364) → `(3, 12)` on the pinned
  runtime. Type "py_version".
- At `PyLinter.initialize()` (lint/pylinter.py:624-634):
  ```
  for msg in self.msgs_store.messages:
      if not msg.may_be_emitted(self.config.py_version):
          self._msgs_state[msg.msgid] = False
  ```
  `may_be_emitted` (message/message_definition.py:75-81):
  `minversion > py_version → False; maxversion <= py_version → False`.
- Affected default-store messages at py_version=(3,12):
  - E0106 return-arg-in-generator: maxversion (3,3) → force-disabled.
  - W1502 boolean-datetime: maxversion (3,5) (stdlib.py:508) → force-disabled.
  - W1514 unspecified-encoding: maxversion (3,15) (stdlib.py:583) → (3,15) <=
    (3,12) is False → STILL EMITTABLE.
  - E0118 used-prior-global-declaration: minversion (3,6) → emittable.
- NOTE: this gate uses the CONFIG py-version (user-settable), not
  sys.version_info. Setting `--py-version=3.4` resurrects W1502 and kills
  E0118 etc. The stdlib deprecation dicts in §3.1 use sys.version_info
  (runtime) instead — a deliberate asymmetry, replicate exactly.
- The force-disable writes `_msgs_state[msgid] = False`, i.e. config-level
  state. Inline `# pylint: enable=` pragmas go through _set_msg_status which
  CAN re-enable per-module (notes/03 owns that machinery).

## 0.3 default_enabled: False messages in the default store

Exactly ten (grep `default_enabled` outside tests):
- pylinter.py MSGS: I0001, I0010, I0011, I0013, I0020, I0021, I0022
  (lint/pylinter.py:139,149,158,167,179,189,201).
- misc.py: I0023 (checkers/misc.py:31).
- refactoring/implicit_booleaness_checker.py: C1804 (:90), C1805 (:102).
Everything else in the 389-message store is enabled by default in full mode.
(run.py:205 / message_state_handler.py:40-43,96 implement the "enable=all
does not resurrect default-off" rule — notes/03.)

## 0.4 Messages owned by THIS doc (cross-checked against msgs.rs)

W1113 W1114 W1115 W1116 W1117 E1101 I1101 | W4901 W4902 W4903 W4904 W4905
W4906 | W1501 W1502 W1503 W1506 W1507 W1508 W1509 W1510 W1514 W1515 W1518 |
W1300 W1301 W1302 W1303 W1304 W1305 W1306 W1307 W1308 W1309 W1310 | W1401
W1402 W1404 W1405 W1406 | W1201 W1202 W1203 | E1003 | C0401 C0402 C0403 |
W2101 | W3301 | W3601 | C2801 | W2301 | C3001 C3002 | C2401 C2403 W2402 |
W2601 W2602 W2603 W2604 W2605 W2606 | W4701 | R1905 R1906 | W0511 I0023 |
R0801.
Every msgid/symbol/template below matches msgs.rs verbatim (checked).

================================================================================
# 1. typecheck.py — TypeChecker: W1113-W1117 + the E1101/I1101 funnel
================================================================================

Message tuples: typecheck.py:220-413. Checker class TypeChecker
(typecheck.py:831). Options (typecheck.py:839-983) with defaults that gate
behavior here:

| option | default | used by |
|---|---|---|
| ignore-on-opaque-inference | True | E1101/I1101 (§1.7 step 3) |
| mixin-class-rgx | `.*[Mm]ixin` (regexp) | _emit_no_member |
| ignore-mixin-members | True (deprecated alias) | (only via new option) |
| ignored-checks-for-mixins | ["no-member", "not-async-context-manager", "not-context-manager", "attribute-defined-outside-init"] | `ignored_mixins = "no-member" in cfg` (typecheck.py:1152-1154) |
| ignore-none | True | _emit_no_member None-owner bail |
| ignored-classes | ("optparse.Values", "thread._local", "_thread._local", "argparse.Namespace") | _is_owner_ignored |
| generated-members | () (type "string") | §1.7 step 0/4 |
| contextmanager-decorators | ["contextlib.contextmanager"] | E1129 (notes/06) |
| missing-member-hint-distance | 1 | hint |
| missing-member-max-choices | 1 | hint |
| missing-member-hint | True | hint |
| signature-mutators | [] | call checks (notes/06) |
Linter-level `ignored-modules` default () also feeds _is_owner_ignored.

`open()` (typecheck.py:985-990): `_py310_plus = py_version >= (3,10)`,
`_py314_plus = py_version >= (3,14)`, `_postponed_evaluation_enabled=False`,
`_mixin_class_rgx = cfg.mixin_class_rgx`.
`visit_module` (:992-995): `_postponed_evaluation_enabled = _py314_plus or
is_postponed_evaluation_enabled(node)` (`from __future__ import annotations`).

## 1.1 W1113 keyword-arg-before-vararg — visit_functiondef (typecheck.py:1012-1024)

Template: `Keyword argument before variable positional arguments list in the
definition of %s function`, args = `(node.name)` — NB: a bare string, not a
tuple (works for the single `%s`).

```
@only_required_for_messages("keyword-arg-before-vararg")
def visit_functiondef(self, node):
    if node.args.vararg and node.args.defaults:
        # When `positional-only` parameters are present then only
        # `positional-or-keyword` parameters are checked. I.e:
        # >>> def name(pos_only_params, /, pos_or_keyword_params, *args): ...
        if node.args.posonlyargs and not node.args.args:
            return
        self.add_message("keyword-arg-before-vararg", node=node, args=(node.name))
visit_asyncfunctiondef = visit_functiondef
```
Trigger: function has `*args` AND any positional defaults. astroid puts
posonly defaults and pos-or-kw defaults together in `args.defaults`, so
`def f(a=1, /, *args)` triggers UNLESS there are posonlyargs and NO regular
args (the early return). `def f(a=1, /, b=2, *args)` → message (posonlyargs
and args both non-empty). Report at FunctionDef (keyword-anchored position).
No inference, no confidence (UNDEFINED). Lambdas: not visited (visit only
FunctionDef/AsyncFunctionDef).

## 1.2 W1114 arguments-out-of-order — _check_argument_order (typecheck.py:1377-1421)

Called from visit_call (typecheck.py:1561-1563) AFTER the parameter analysis
list is built and BEFORE positional matching — see notes/06 for the full
visit_call flow (its step ordering is normative; W1114 fires before E1121/
E1124/E1123/W1117 on the same call).

Inputs: `called_param_names = [p[0][0] for p in parameters]` = names of
posonly+regular params (kw-only excluded), in order.

```
try:
    is_classdef = isinstance(called.parent, nodes.ClassDef)
    if is_classdef and called_param_names[0] == "self":
        called_param_names = called_param_names[1:]
except IndexError:
    return
try:
    calling_parg_names = [p.name for p in call_site.positional_arguments]
    calling_kwarg_names = [arg.name for arg in call_site.keyword_arguments.values()]
except AttributeError:
    return        # any positional/keyword arg without .name (non-Name node)
arg_set = set(calling_parg_names) | set(calling_kwarg_names)
param_set = set(called_param_names)
if arg_set != param_set:
    return
if calling_parg_names != called_param_names[: len(calling_parg_names)]:
    self.add_message("arguments-out-of-order", node=node, args=())
```
Bail-outs: (a) empty param list (IndexError); (b) ANY argument that is not a
Name-like node (AttributeError on `.name` — Const has `.name`! `nodes.Const`
defines `name` property? NO: Const has `.name` attribute via `Const.name`?
Const nodes have a `.name` attribute equal to the type name? — they do NOT;
`Const` has no `name`, raises AttributeError → bail. But `Name` and
`AssignName` do. Lambdas (`.name` raises) bail too); (c) the *set* of
supplied names must EQUAL the set of parameter names exactly (every param
passed, no extras). Emission: positional arg names not a prefix-match of
param names. args=() (template has no placeholder). Report at Call node.
Confidence UNDEFINED.

## 1.3 W1115 non-str-assignment-to-dunder-name — visit_assign → _check_dundername_is_string (typecheck.py:1309-1328)

visit_assign (:1226-1234) is decorated only_required_for_messages(
"assignment-from-no-return", "assignment-from-none",
"non-str-assignment-to-dunder-name") and calls
`_check_assignment_from_function_call` (E1111/E1128 — notes/06) then
`_check_dundername_is_string`.

```
lhs = node.targets[0]
if not isinstance(lhs, nodes.AssignAttr): return
if not lhs.attrname == "__name__": return
rhs = node.value
if isinstance(rhs, nodes.Const) and isinstance(rhs.value, str): return
match inferred := utils.safe_infer(rhs):
    case _ if not inferred: return
    case nodes.Const(value=str()): pass
    case _:
        self.add_message("non-str-assignment-to-dunder-name", node=node)
```
Only the FIRST target is examined. `x.__name__ = 1` → message;
`x.__name__ = f()` where safe_infer fails/Uninferable → `not inferred`?
Uninferable is falsy → bail (conservative). Inferred Const-str → ok.
Report at the Assign node. No args, confidence UNDEFINED.

## 1.4 W1116 isinstance-second-argument-not-valid-type — _check_isinstance_args (typecheck.py:1423-1452)

Reached from visit_call (:1470-1475): after `_determine_callable(called)`
succeeds and `called.args.args is None` (builtin without arg info) and
`called.name == "isinstance"`. So ANY function named "isinstance" whose
FunctionDef has args.args None — in practice the builtins.isinstance from
the C-snapshot (raw-built builtins have args.args=None).

```
if len(node.args) > 2:
    add_message("too-many-function-args", node=node, args=(callable_name,), confidence=HIGH)
elif len(node.args) < 2:
    parameters = ("'_obj'", "'__class_or_tuple'")
    for parameter in parameters[len(node.args):]:
        add_message("no-value-for-parameter", node=node, args=(parameter, callable_name), confidence=HIGH)
    return
second_arg = node.args[1]
if _is_invalid_isinstance_type(second_arg):
    add_message("isinstance-second-argument-not-valid-type", node=node, confidence=INFERENCE)
```
(The E1121/E1120 branches are spec'd in notes/06 §; note the E1120 args
embed pre-quoted parameter names `'_obj'`.) NB keyword/star args don't count
toward len(node.args).

`_is_invalid_isinstance_type` (typecheck.py:806-828) — returns True only when
SURE arg is not a type:
```
if isinstance(arg, nodes.BinOp) and arg.op == "|":
    return any(_is_invalid_isinstance_type(elt) and not is_none(elt)
               for elt in (arg.left, arg.right))
match inferred := utils.safe_infer(arg):
    case _ if not inferred: return False          # can't infer → skip
    case nodes.Tuple(): return any(_is_invalid_isinstance_type(elt) for elt in inferred.elts)
    case nodes.ClassDef(): return False
    case astroid.Instance() if inferred.qname() == "builtins.tuple": return False
    case bases.UnionType():
        return any(_is_invalid_isinstance_type(elt) and not is_none(elt)
                   for elt in (inferred.left, inferred.right))
return True
```
(BUILTIN_TUPLE = "builtins.tuple", typecheck.py top constants.) Note the
syntactic `X | Y` check recurses on the RAW children, the inferred UnionType
on inferred ones; `None` operands allowed (`is_none`, utils.py:1487-1491).
Tuple: any invalid element poisons. Everything else inferred (Const str,
int, Instance of non-tuple, Module, FunctionDef...) → True → message.
Report at Call, confidence INFERENCE.

## 1.5 W1117 kwarg-superseded-by-positional-arg — visit_call step 2 (typecheck.py:1582-1595)

Inside the keyword-matching loop of visit_call (full flow in notes/06 §
"Step 7"):
```
for keyword in keyword_args:
    # Skip if `keyword` is the same name as a positional-only parameter
    # and a `**kwargs` parameter exists.
    if called.args.kwarg and keyword in [arg.name for arg in called.args.posonlyargs]:
        self.add_message(
            "kwarg-superseded-by-positional-arg",
            node=node,
            args=(keyword, f"**{called.args.kwarg}"),
            confidence=HIGH,
        )
        continue
```
Template: `%r will be included in %r since a positional-only parameter with
this name already exists`; args e.g. `("x", "**kwargs")` → rendered
`'x' will be included in '**kwargs' ...`. The `continue` means this keyword
takes part in NO further matching (no E1123/E1124 for it). keyword_args
iteration order = CallSite.keyword_arguments dict insertion order +
already_filled_keywords appended (notes/06). Report at Call, HIGH.

## 1.6 dispatch summary for the W-messages

- visit_functiondef/asyncfunctiondef → W1113 only (decorator gates).
- visit_call → W1114/W1116/W1117 inline with the E11xx call machinery.
- visit_assign → W1115 (+E1111/E1128).
All of these are walker callbacks of the SAME TypeChecker instance; their
relative order on one module is AST-walk order (notes/02).

## 1.7 E1101 no-member / I1101 c-extension-no-member — visit_attribute (typecheck.py:1059-1224)

Templates:
- E1101: `%s %r has no %r member%s` (old_names [("E1103","maybe-no-member")]).
- I1101: `%s %r has no %r member%s, but source is unavailable. Consider
  adding this module to extension-pkg-allow-list if you want to perform
  analysis based on run-time introspection of living objects.`
args = `(owner.display_type(), name, node.attrname, hint)`; confidence
INFERENCE. Report at the Attribute/AssignAttr/DelAttr node.

Entry points:
```
def visit_assignattr(self, node):            # :1059-1061
    if isinstance(node.assign_type(), nodes.AugAssign):
        self.visit_attribute(node)
def visit_delattr(self, node):               # :1063-1064
    self.visit_attribute(node)
@only_required_for_messages("no-member", "c-extension-no-member")
def visit_attribute(self, node): ...         # :1067-1200
```
So plain attribute reads, `del x.y`, and `x.y += 1` (AugAssign target) are
checked; plain assignment targets are not.

Funnel, in order (verbatim semantics):

0. generated-members short-circuit (:1078-1083): if any compiled
   generated-members regex `.match()`es `node.attrname` OR
   `node.as_string()` → return. Compilation (:997-1010): if the option value
   is a string (it is, type "string", default ""), tokenize with
   `shlex.shlex(s)` where `whitespace += ","`, `wordchars += r"[]-+\.*?()|"`,
   strip surrounding `"` from each token, `re.compile` each. Default "" →
   no patterns.
1. postponed-annotations (:1085-1088): if `_postponed_evaluation_enabled and
   is_node_in_type_annotation_context(node)` (utils.py:1614+: walks parents
   to find AnnAssign.annotation / Arguments annotations / FunctionDef.returns)
   → return.
2. `inferred = list(node.expr.infer())` ; InferenceError → return.
3. Opaque filter (:1098-1110): `non_opaque = [o for o in inferred if not
   isinstance(o, (nodes.Unknown, util.UninferableBase))]`; if
   `len(non_opaque) != len(inferred) and cfg.ignore_on_opaque_inference`
   (default True) → return. (So ANY Uninferable in the results kills the
   check by default.)
4. Per-owner loop over non_opaque (ORDER = inference result order):
   ```
   name = getattr(owner, "name", None)
   if _is_owner_ignored(owner, name, cfg.ignored_classes, cfg.ignored_modules):
       continue
   qualname = f"{owner.pytype()}.{node.attrname}"
   if any(p.match(qualname) for p in generated_members): return   # NB return, not continue
   try:
       attr_nodes = owner.getattr(node.attrname)
   except AttributeError: continue
   except astroid.DuplicateBasesError: continue
   except astroid.NotFoundError:
       if isinstance(owner, (nodes.FunctionDef, astroid.BoundMethod)) and owner.decorators:
           continue
       if not _emit_no_member(node, owner, name, self._mixin_class_rgx,
                              ignored_mixins=("no-member" in cfg.ignored_checks_for_mixins),
                              ignored_none=cfg.ignore_none):
           continue
       missingattr.add((owner, name)); continue
   else:
       for attr_node in attr_nodes:
           attr_parent = attr_node.parent
           try:
               if isinstance(attr_node.statement(), nodes.AugAssign) or (
                   isinstance(attr_parent, nodes.Assign)
                   and utils.is_augmented_assign(attr_parent)[0]):
                   continue                       # skip augmented assignments
           except astroid.exceptions.StatementMissing:
               break
           if attr_parent is node.parent:         # skip self-referencing assignment
               continue
           break
       else:
           missingattr.add((owner, name)); continue
   break    # first owner that HAS the attribute ends the whole check
   ```
   The for/else: the attribute "exists" only if some attr_node is neither an
   augmented assignment nor the sibling of the very node being checked
   (`x.count = x.count + 1` pattern → both definitions skipped → counted
   missing). StatementMissing → treat as found (break out of attr_nodes loop
   → falls to the outer `break`? NO — `break` inside the attr_nodes loop
   skips the for-else, then the owner loop hits `break` → found. Careful:
   the inner `break` on StatementMissing exits the attr_nodes loop WITHOUT
   running its else → owner considered to HAVE the attr → outer break).
5. Outer for/else (:1181-1200): only if NO owner had the attribute:
   ```
   done = set()
   for owner, name in missingattr:        # SET iteration — see ordering note
       actual = owner._proxied if isinstance(owner, astroid.Instance) else owner
       if actual in done: continue
       done.add(actual)
       msg, hint = self._get_nomember_msgid_hint(node, owner)
       self.add_message(msg, node=node,
                        args=(owner.display_type(), name, node.attrname, hint),
                        confidence=INFERENCE)
   ```
   ORDERING DEPENDENCY: `missingattr` is a `set` of (owner, name) tuples —
   iteration order is hash-based (object identity hashes for nodes + str
   hash for name; PYTHONHASHSEED=0 pins str part but node hashes are id()s).
   With a single owner (the overwhelmingly common case) order is moot; with
   ≥2 distinct missing owners the EMISSION ORDER of the 2+ messages on the
   same line is formally nondeterministic in CPython. Replicate "insertion
   order" and verify against corpora diffs; flag any mismatch (open Q §24).

`_is_owner_ignored` (typecheck.py:108-131): True if
`is_module_ignored(owner.root().qname(), ignored_modules)` (utils.py:2190-
2202: for each dotted prefix of the qname — `_qualified_name_parts` yields
["a", "a.b", "a.b.c"] — test exact membership in ignored_modules, then
`fnmatch.fnmatch(prefix, ignore)` per entry), OR
`any(ignore in (attrname_of_owner := name, owner.qname()) for ignore in
ignored_classes)`. NB: it compares against the owner's *name* and *qname* —
default ignored_classes ("optparse.Values", "thread._local", "_thread._local",
"argparse.Namespace") only matches qnames in practice.

`_emit_no_member` (typecheck.py:429-531) — return False ⇒ suppress:
```
if node_ignores_exception(node, AttributeError): return False
    # utils.py:1148+: node is inside a try whose handlers catch
    # AttributeError / Exception / BaseException or bare except
if ignored_none and isinstance(owner, nodes.Const) and owner.value is None: return False
if is_super(owner) or getattr(owner, "type", None) == "metaclass": return False
if owner_name and ignored_mixins and mixin_class_rgx.match(owner_name): return False
if isinstance(owner, nodes.FunctionDef) and (owner.decorators or owner.is_abstract()): return False
if isinstance(owner, (astroid.Instance, nodes.ClassDef)):
    try: metaclass = owner.metaclass()
    except astroid.MroError: pass
    else:
        if metaclass and metaclass.qname() in {"enum.EnumMeta", "enum.EnumType"}:
            return not _enum_has_attribute(owner, node)        # see below
    if owner.has_dynamic_getattr(): return False               # __getattr__/__getattribute__
    if not has_known_bases(owner): return False
    if utils.is_attribute_typed_annotation(owner, node.attrname): return False
if isinstance(owner, objects.Super):
    try: owner.super_mro()
    except (astroid.MroError, astroid.SuperError): return False
    if not all(has_known_bases(base) for base in owner.type.mro()): return False
if isinstance(owner, nodes.Module):
    try: owner.getattr("__getattr__"); return False
    except astroid.NotFoundError: pass
if owner_name and node.attrname.startswith("_" + owner_name):
    unmangled_name = node.attrname.split("_" + owner_name)[-1]
    try:
        if owner.getattr(unmangled_name, context=None) is not None: return False
    except astroid.NotFoundError: return True
# IF/IfExp guard suppression: walk parents up to node.scope(); for each
# If/IfExp parent, if safe_infer(parent.test) is Const with bool_value()
# False and the original child chain was in parent.body → return False
return True
```
`_enum_has_attribute` (typecheck.py:557-604): collects attribute names
assigned in the enum class's `__new__` (assignments onto the name returned
by its Return stmt) and `__init__` (assignments onto arg0/self) via
`_get_all_attribute_assignments` (:534-554, recurses into Tuple targets);
returns `node.attrname in enum_attributes` (True ⇒ attribute exists ⇒
`not` ⇒ no message).

`_get_nomember_msgid_hint` (typecheck.py:1202-1224):
```
if _is_c_extension(owner): return "c-extension-no-member", ""
if not cfg.missing_member_hint: return "no-member", ""
names = _similar_names(owner, node.attrname, cfg.missing_member_hint_distance,
                       cfg.missing_member_max_choices)
if not names: return "no-member", ""
names = [repr(n) for n in names]
names_hint = names[0] if len(names)==1 else f"one of {', '.join(names[:-1])} or {names[-1]}"
return "no-member", f"; maybe {names_hint}?"
```
`_is_c_extension` (typecheck.py:798-803): `isinstance(module_node,
nodes.Module) and not astroid.modutils.is_stdlib_module(module_node.name)
and not module_node.fully_defined()`. So I1101 fires only when the OWNER is
a Module object that is (a) not stdlib (first dotted component in
sys.stdlib_module_names — astroid modutils caches this) and (b) not
fully_defined (`file is None` for C extensions — for our port: synthetic
snapshot modules outside the stdlib set). I1101 hint is always "".

Hint computation: `_similar_names` (typecheck.py:177-217, lru_cache 256):
candidate names from `_node_names(owner)` — singledispatch (:134-152):
default = `owner.locals.keys()` (empty if no locals attr); for
ClassDef/Instance = chain(instance_attrs.keys(), locals.keys()) + recursive
`_node_names` over `mro()[1:]` (falls back to `ancestors()` on
NotImplementedError/TypeError/MroError). For each candidate != attrname with
`abs(len diff) <= distance_threshold` compute `_string_distance` (:155-174 —
a Levenshtein with a QUIRK: returns `row[seq2_length - 1]`, and the row
buffer is rotated such that the final value is the standard edit distance;
port the function verbatim, including the `row = [0]*len2 + [idx+1]`
initialization and negative-index wraparound `row[seq2_index - 1]` for
seq2_index==0 reading the LAST slot = seq1_index+1) ; keep those with
distance <= threshold; `heapq.nsmallest(max_choices, possible, key=dist)`
(stable: ties broken by insertion order = candidate iteration order =
locals/instance_attrs dict order + MRO order), then `sorted(picked)`
alphabetically. Default distance 1, max choices 1 → hint like
`; maybe 'foo'?`.
Message rendering examples:
`Instance of 'Foo' has no 'bar' member; maybe 'baz'?` /
`Module 'x.y' has no 'z' member, but source is unavailable. Consider ...`.
`display_type()` values: "Module", "Class", "Instance of '...'"? —
display_type is astroid's: Module→"Module", ClassDef→"Class",
Instance→"Instance of", actually astroid `object_type` display: owner.
display_type() returns e.g. "Instance of" → full first arg is
`Instance of` + ` %r` name. Verify against astroid bases.py:
Instance.display_type() = "Instance of"; ClassDef = "Class"; Module =
"Module"; Function = "Function"... (astroid/bases.py & nodes). The template
`%s %r has no %r member%s` therefore renders
`Instance of 'Foo' has no 'bar' member`.

================================================================================
# 2. deprecated.py — DeprecatedMixin (W4901-W4906 framework)
================================================================================

File: checkers/deprecated.py (295 lines, fully quoted below where load-
bearing). The mixin defines message dicts that implementing checkers splice
into their own `msgs` (all carry `{"shared": True}` so two checkers may own
the same msgid — the msgstore accepts duplicate registration when shared).

| id | symbol | template | old_names | default-loaded implementors |
|---|---|---|---|---|
| W4901 | deprecated-module | `Deprecated module %r` | W0402 old-deprecated-module | ImportsChecker (imports.py) |
| W4902 | deprecated-method | `Using deprecated method %s()` | W1505 old-deprecated-method | StdlibChecker |
| W4903 | deprecated-argument | `Using deprecated argument %s of method %s()` | W1511 old-deprecated-argument | StdlibChecker |
| W4904 | deprecated-class | `Using deprecated class %s of module %s` | W1512 old-deprecated-class | StdlibChecker |
| W4905 | deprecated-decorator | `Using deprecated decorator %s()` | W1513 old-deprecated-decorator | StdlibChecker |
| W4906 | deprecated-attribute | `Using deprecated attribute %r` | — | StdlibChecker |

(deprecated.py:37-89.) ImportsChecker registers ONLY the module message
(see 09-variables-imports-classes-wc.md §2.14: W4901 from imports with the
`deprecated-modules` option default and DEPRECATED_MODULES constant).
StdlibChecker registers W4902-W4906 AND uses check_deprecated_module only
for the `__import__("name")` special case below — it has NO
deprecated_modules() data (returns () default), so its module path never
fires. ImportsChecker conversely only implements deprecated_modules.

`ACCEPTABLE_NODES = (astroid.BoundMethod, astroid.UnboundMethod,
nodes.FunctionDef, nodes.ClassDef, nodes.Attribute)` (deprecated.py:22-28).

## 2.1 visit_attribute → W4906 (deprecated.py:91-94, 222-235)

```
inferred_expr = safe_infer(node.expr)
if not isinstance(inferred_expr, (nodes.ClassDef, Instance, nodes.Module)): return
attribute_qname = ".".join((inferred_expr.qname(), node.attrname))
for deprecated_name in self.deprecated_attributes():
    if attribute_qname == deprecated_name:
        add_message("deprecated-attribute", node=node, args=(attribute_qname,), confidence=INFERENCE)
```
Note args carry the full dotted qname (e.g. `'sqlite3.version'` rendered with
%r). Instance.qname() proxies to its class qname — so
`datetime.datetime.utcnow` accessed via an INSTANCE would build
"_pydatetime.datetime.utcnow"-style qnames; the data set (§3.1) contains
plain module-level qnames, so instance matches are rare. Loop emits once per
matching entry (set, so at most once).

## 2.2 visit_call (deprecated.py:96-116)

```
self.check_deprecated_class_in_call(node)              # W4904 (Attribute(Name) call)
for inferred in infer_all(node.func):                  # [] on InferenceError
    self.check_deprecated_method(node, inferred)       # W4902/W4903
    if (isinstance(inferred, nodes.FunctionDef)
        and inferred.qname() == "builtins.__import__"
        and len(node.args) == 1
        and (mod_path_node := utils.safe_infer(node.args[0]))
        and isinstance(mod_path_node, nodes.Const)):
        self.check_deprecated_module(node, mod_path_node.value)   # W4901
```
NOTE: StdlibChecker OVERRIDES visit_call (stdlib.py:690-721, §3.0) and does
NOT call super(); the mixin's visit_call body runs only for checkers that
don't override (ImportsChecker doesn't define visit_call → the mixin one
runs for it, but only_required_for_messages gates: ImportsChecker only owns
deprecated-module of the four listed → callback registered;
check_deprecated_method is a no-op there since deprecated_methods()=()).
So the `__import__("x")` → W4901 path IS live via ImportsChecker.

## 2.3 visit_import → W4901 (+W4904 for dotted) (deprecated.py:118-129)

```
for name in (name for name, _ in node.names):
    self.check_deprecated_module(node, name)
    if "." in name:
        mod_name, class_name = name.split(".", 1)
        self.check_deprecated_class(node, mod_name, (class_name,))
```
`import a.b.c` checks module "a.b.c" and class "b.c" of module "a" (split
maxsplit=1!).

## 2.4 visit_decorators → W4905 (deprecated.py:139-153)

```
children = list(node.get_children())
if not children: return
if isinstance(children[0], nodes.Call): inferred = safe_infer(children[0].func)
else: inferred = safe_infer(children[0])
if not isinstance(inferred, (nodes.ClassDef, nodes.FunctionDef)): return
qname = inferred.qname()
if qname in self.deprecated_decorators():
    add_message("deprecated-decorator", node=node, args=qname)
```
ONLY THE FIRST decorator is checked (children[0])! args is a bare string.
Report node = the Decorators node (fromlineno = first decorator's line).

## 2.5 visit_importfrom → W4901/W4904 (deprecated.py:155-162)

```
basename = get_import_name(node, node.modname)   # resolves relative imports
self.check_deprecated_module(node, basename)
class_names = (name for name, _ in node.names)
self.check_deprecated_class(node, basename, class_names)
```
get_import_name (utils.py:1820-1843): for `ImportFrom` with level>0 uses
`root.relative_to_absolute_name(modname, level)` (TooManyLevelsError →
unchanged).

## 2.6 check_deprecated_module → W4901 (deprecated.py:237-243)

```
for mod_name in self.deprecated_modules():
    if mod_path == mod_name or (mod_path and mod_path.startswith(mod_name + ".")):
        add_message("deprecated-module", node=node, args=mod_path)
```
Prefix semantics: deprecated "x" also flags "x.y". Iteration over a
set/csv-config — can emit multiple times if several entries match (e.g.
both "x" and "x.y" configured and importing "x.y.z" → TWO messages).

## 2.7 check_deprecated_method → W4902/W4903 (deprecated.py:245-278)

```
if not isinstance(inferred, ACCEPTABLE_NODES): return
match node.func:
    case nodes.Attribute(attrname=func_name) | nodes.Name(name=func_name): pass
    case _: return
qnames = {inferred.qname(), func_name}
if any(name in self.deprecated_methods() for name in qnames):
    add_message("deprecated-method", node=node, args=(func_name,))
    return
num_of_args = len(node.args)
kwargs = {kw.arg for kw in node.keywords} if node.keywords else {}
deprecated_arguments = (self.deprecated_arguments(qn) for qn in qnames)
for position, arg_name in chain(*deprecated_arguments):
    if arg_name in kwargs:
        add_message("deprecated-argument", node=node, args=(arg_name, func_name))
    elif position is not None and position < num_of_args:
        add_message("deprecated-argument", node=node, args=(arg_name, func_name))
```
Subtleties:
- Matching is on the inferred QNAME **or the bare call-site name** —
  `qnames = {qname, func_name}`. That's why the (3,0,0) set in §3.1 contains
  bare names like "assertEquals": ANY call `self.assertEquals(...)` matches
  by func_name regardless of inference. Bug-for-bug: a user function named
  `assert_` also triggers W4902.
- `inferred.qname()`: Attribute nodes are in ACCEPTABLE_NODES but have NO
  qname() → AttributeError would propagate... in practice infer_all returns
  Attribute only as part of unresolved inference (rare); pylint would crash
  to F0002 (astroid-error). Keep semantics.
- W4902 short-circuits W4903 (`return` after deprecated-method).
- qnames is a SET of ≤2 strings — `deprecated_arguments` generator order over
  the set is hash-dependent; with PYTHONHASHSEED=0 it's deterministic.
  Emission order of multiple W4903 on one call follows chain(set-order). If
  qname == func_name (Name call of top-level func) the set has 1 element.
- Position match: positional index < len(node.args) (starred args count as
  one element each — no expansion).
- W4902 fires once even if both qname and func_name match (any()).
- Called from BOTH the mixin visit_call (imports — no method data → no-op)
  and StdlibChecker.visit_call per inferred value (§3.0) — a call whose func
  infers to N deprecated values can emit N duplicate messages.

## 2.8 check_deprecated_class / check_deprecated_class_in_call → W4904 (deprecated.py:280-294)

```
def check_deprecated_class(node, mod_name, class_names):
    for class_name in class_names:
        if class_name in self.deprecated_classes(mod_name):
            add_message("deprecated-class", node=node, args=(class_name, mod_name))

def check_deprecated_class_in_call(node):
    match node.func:
        case nodes.Attribute(expr=nodes.Name(name=mod_name), attrname=class_name):
            self.check_deprecated_class(node, mod_name, (class_name,))
```
The call-form check is purely SYNTACTIC: `configparser.SafeConfigParser()`
matches by the literal Name "configparser" — `import configparser as cp;
cp.SafeConfigParser()` does NOT match (mod_name "cp" not in data). No
inference. args=(class, module).

================================================================================
# 3. stdlib.py — StdlibChecker
================================================================================

Class StdlibChecker(DeprecatedMixin, BaseChecker), name="stdlib"
(stdlib.py:485-487). msgs = the five DEPRECATED_*_MESSAGE dicts + W1501,
W1502 (maxversion (3,5) → dead, §0.2), W1503, W1506, W1507, E1507, E1519,
E1520, W1508, W1509, W1510, W1514 (maxversion (3,15) → live), W1515, W1518
(old_names [("W1516","lru-cache-decorating-method"),
("W1517","cache-max-size-none")]) — stdlib.py:488-606. No options.

Module-level constants (stdlib.py:27-44):
```
OPEN_FILES_MODE = ("open", "file")
OPEN_FILES_FUNCS = ("open", "file", "read_text", "write_text")
UNITTEST_CASE = "unittest.case"
THREADING_THREAD = "threading.Thread"
COPY_COPY = "copy.copy"
OS_ENVIRON = "os._Environ"
ENV_GETTERS = ("os.getenv",)
SUBPROCESS_POPEN = "subprocess.Popen"
SUBPROCESS_RUN = "subprocess.run"
OPEN_MODULE = {"_io", "pathlib", "pathlib._local"}
PATHLIB_MODULE = {"pathlib", "pathlib._local"}
DEBUG_BREAKPOINTS = ("builtins.breakpoint", "sys.breakpointhook", "pdb.set_trace")
LRU_CACHE = {"functools.lru_cache", "functools._lru_cache_wrapper.wrapper",
             "functools.lru_cache.decorating_function"}
NON_INSTANCE_METHODS = {"builtins.staticmethod", "builtins.classmethod"}
```

## 3.0 visit_call dispatch (stdlib.py:675-721)

Decorated only_required_for_messages("bad-open-mode",
"redundant-unittest-assert", "deprecated-method", "deprecated-argument",
"bad-thread-instantiation", "shallow-copy-environ", "invalid-envvar-value",
"invalid-envvar-default", "subprocess-popen-preexec-fn",
"subprocess-run-check", "deprecated-class", "unspecified-encoding",
"forgotten-debug-statement").
```
self.check_deprecated_class_in_call(node)              # §2.8
for inferred in utils.infer_all(node.func):
    if isinstance(inferred, util.UninferableBase): continue
    if inferred.root().name in OPEN_MODULE:
        open_func_name = node.func.name if isinstance(node.func, nodes.Name) else None
        if isinstance(node.func, nodes.Attribute): open_func_name = node.func.attrname
        if open_func_name in OPEN_FILES_FUNCS:
            self._check_open_call(node, inferred.root().name, open_func_name)
    elif inferred.root().name == UNITTEST_CASE:
        self._check_redundant_assert(node, inferred)
    elif isinstance(inferred, nodes.ClassDef):
        if inferred.qname() == THREADING_THREAD: self._check_bad_thread_instantiation(node)
        elif inferred.qname() == SUBPROCESS_POPEN: self._check_for_preexec_fn_in_popen(node)
    elif isinstance(inferred, nodes.FunctionDef):
        name = inferred.qname()
        if name == COPY_COPY: self._check_shallow_copy_environ(node)
        elif name in ENV_GETTERS: self._check_env_function(node, inferred)
        elif name == SUBPROCESS_RUN: self._check_for_check_kw_in_run(node)
        elif name in DEBUG_BREAKPOINTS: self.add_message("forgotten-debug-statement", node=node)
    self.check_deprecated_method(node, inferred)        # §2.7 — per inferred!
```
Notes:
- The branch chain keys on `inferred.root().name` (module of the inferred
  object) FIRST: anything defined in `_io`/`pathlib` named open/file/
  read_text/write_text goes to the open check; anything in unittest.case
  (any method!) goes to the redundant-assert check (which itself filters).
- The elif chain means a unittest.case FunctionDef never reaches the
  FunctionDef branch, etc.
- check_deprecated_method runs for EVERY inferred value (duplicates
  possible).
- `builtins.open` lives in module `_io` in the astroid brain (builtins.open
  is re-exported); `Path.open/read_text/write_text` root is "pathlib" (3.12)
  / "pathlib._local" (3.13+; both in the set).

## 3.1 Deprecation data — EFFECTIVE sets on the pinned runtime

__init__ (stdlib.py:608-630) filters the data dicts by
**`sys.version_info`** (RUNTIME = (3,12,12,'final',0)), NOT config
py-version:
```
for since_vers, func_list in DEPRECATED_METHODS[sys.version_info[0]].items():
    if since_vers <= sys.version_info: self._deprecated_methods.update(func_list)
... same pattern for DEPRECATED_ARGUMENTS / _CLASSES / _DECORATORS / _ATTRIBUTES
```
Port as constants computed for (3,12,12). Gates: a `(3,13,0)` or `(3,14,0)`
key is EXCLUDED ((3,13,0) <= (3,12,12) is False).

### W4902 methods (DEPRECATED_METHODS, stdlib.py:118-314)
Outer key `sys.version_info[0]` = 3 → ONLY the `3:` dict is read. The `0:`
dict (cgi.parse_qs, ctypes.c_buffer, distutils..., tkinter...) and `2:` dict
are DEAD on Python 3 — do not port their contents as live.
Included sub-keys: (3,0,0) (3,1,0) (3,2,0) (3,3,0) (3,4,0) (3,4,4) (3,5,0)
(3,6,0) (3,7,0) (3,8,0) (3,9,0) (3,10,0) (3,11,0) (3,12,0).
Excluded: (3,13,0), (3,14,0).
The union (transcribe verbatim from stdlib.py:151-290; bare names included —
they match call-site names per §2.7):
- (3,0,0): inspect.getargspec, failUnlessEqual, assertEquals, failIfEqual,
  assertNotEquals, failUnlessAlmostEqual, assertAlmostEquals,
  failIfAlmostEqual, assertNotAlmostEquals, failUnless, assert_,
  failUnlessRaises, failIf, assertRaisesRegexp, assertRegexpMatches,
  assertNotRegexpMatches
- (3,1,0): base64.encodestring, base64.decodestring, ntpath.splitunc,
  os.path.splitunc, os.stat_float_times, turtle.RawTurtle.settiltangle
- (3,2,0): cgi.escape, configparser.RawConfigParser.readfp,
  xml.etree.ElementTree.Element.getchildren,
  xml.etree.ElementTree.Element.getiterator,
  xml.etree.ElementTree.XMLParser.getiterator,
  xml.etree.ElementTree.XMLParser.doctype
- (3,3,0): inspect.getmoduleinfo, logging.warn, logging.Logger.warn,
  logging.LoggerAdapter.warn, nntplib._NNTPBase.xpath, platform.popen,
  sqlite3.OptimizedUnicode, time.clock
- (3,4,0): importlib.find_loader, importlib.abc.Loader.load_module,
  importlib.abc.Loader.module_repr, importlib.abc.PathEntryFinder.find_loader,
  importlib.abc.PathEntryFinder.find_module, plistlib.readPlist,
  plistlib.writePlist, plistlib.readPlistFromBytes, plistlib.writePlistToBytes
- (3,4,4): asyncio.tasks.async
- (3,5,0): fractions.gcd, inspect.formatargspec, inspect.getcallargs,
  platform.linux_distribution, platform.dist
- (3,6,0): importlib._bootstrap_external.FileLoader.load_module,
  _ssl.RAND_pseudo_bytes
- (3,7,0): sys.set_coroutine_wrapper, sys.get_coroutine_wrapper, aifc.openfp,
  threading.Thread.isAlive, asyncio.Task.current_task, asyncio.Task.all_task,
  locale.format, ssl.wrap_socket, ssl.match_hostname, sunau.openfp, wave.openfp
- (3,8,0): gettext.lgettext, gettext.ldgettext, gettext.lngettext,
  gettext.ldngettext, gettext.bind_textdomain_codeset,
  gettext.NullTranslations.output_charset,
  gettext.NullTranslations.set_output_charset, threading.Thread.isAlive
- (3,9,0): binascii.b2a_hqx, binascii.a2b_hqx, binascii.rlecode_hqx,
  binascii.rledecode_hqx
- (3,10,0): _sqlite3.enable_shared_cache, importlib.abc.Finder.find_module,
  pathlib.Path.link_to, zipimport.zipimporter.load_module,
  zipimport.zipimporter.find_module, zipimport.zipimporter.find_loader,
  threading.currentThread, threading.activeCount,
  threading.Condition.notifyAll, threading.Event.isSet,
  threading.Thread.setName, threading.Thread.getName,
  threading.Thread.isDaemon, threading.Thread.setDaemon, cgi.log
- (3,11,0): importlib.resources.contents, locale.getdefaultlocale,
  locale.resetlocale, re.template, unittest.findTestCases, unittest.makeSuite,
  unittest.getTestCaseNames, unittest.TestLoader.loadTestsFromModule,
  unittest.TestLoader.loadTestsFromTestCase,
  unittest.TestLoader.getTestCaseNames, unittest.TestProgram.usageExit
- (3,12,0): asyncio.get_child_watcher, asyncio.set_child_watcher,
  asyncio.AbstractEventLoopPolicy.get_child_watcher,
  asyncio.AbstractEventLoopPolicy.set_child_watcher,
  builtins.bool.__invert__, datetime.datetime.utcfromtimestamp,
  datetime.datetime.utcnow, pkgutil.find_loader, pkgutil.get_loader,
  pty.master_open, pty.slave_open, xml.etree.ElementTree.Element.__bool__

### W4903 arguments (DEPRECATED_ARGUMENTS, stdlib.py:49-104)
Included keys: (0,0,0), (3,5,0), (3,8,0), (3,9,0), (3,12,0).
Excluded: (3,13,0) {dis.get_instructions}, (3,14,0) {argparse..., threading.RLock}.
Effective dict (method-qname → ((pos|None, name), ...)):
- "int": ((None,"x"),) ; "bool": ((None,"x"),) ; "float": ((None,"x"),)
  — NB bare names: any call-site named `int`/`bool`/`float` (or inferring to
  a function whose qname is literally "int") with keyword `x=` → W4903. The
  builtins infer to qname "int" etc? builtins int is a ClassDef qname
  "builtins.int" — does NOT match "int"; but func_name "int" ∈ qnames set
  DOES match (§2.7) → `int(x=5)` flags. Positional never flags (pos None).
- importlib._bootstrap_external.cache_from_source: ((1,"debug_override"),)
- asyncio.tasks.sleep/gather/shield/wait_for/wait/as_completed: ((None,"loop"),)
- asyncio.subprocess.create_subprocess_exec: ((None,"loop"),) ;
  asyncio.subprocess.create_subprocess_shell: ((4,"loop"),)
- gettext.translation: ((5,"codeset"),) ; gettext.install: ((2,"codeset"),)
- functools.partialmethod: ((None,"func"),)
- weakref.finalize: ((None,"func"),(None,"obj"))
- profile.Profile.runcall / cProfile.Profile.runcall / bdb.Bdb.runcall /
  trace.Trace.runfunc / curses.wrapper: ((None,"func"),)
- unittest.case.TestCase.addCleanup: ((None,"function"),)
- concurrent.futures.thread.ThreadPoolExecutor.submit /
  concurrent.futures.process.ProcessPoolExecutor.submit: ((None,"fn"),)
- contextlib._BaseExitStack.callback /
  contextlib.AsyncExitStack.push_async_callback: ((None,"callback"),)
- multiprocessing.managers.Server.create /
  multiprocessing.managers.SharedMemoryServer.create: ((None,"c"),(None,"typeid"))
- random.Random.shuffle: ((1,"random"),)
- argparse.BooleanOptionalAction: ((3,"type"),(4,"choices"),(7,"metavar"))
- coroutine.throw: ((1,"value"),(2,"traceback"))
- email.utils.localtime: ((1,"isdst"),)
- shutil.rmtree: ((2,"onerror"),)
- sysconfig.is_python_build: ((0,"check_home"),)

### W4905 decorators (DEPRECATED_DECORATORS, stdlib.py:106-115)
Included: (3,8,0) {asyncio.coroutine}; (3,3,0) {abc.abstractclassmethod,
abc.abstractstaticmethod, abc.abstractproperty}; (3,4,0)
{importlib.util.module_for_loader}. Excluded: (3,13,0)
{typing.no_type_check_decorator}.

### W4904 classes (DEPRECATED_CLASSES, stdlib.py:317-430)
Included keys in insertion order (3,2,0),(3,3,0),(3,9,0),(3,11,0),(3,12,0);
excluded (3,13,0),(3,14,0). CRITICAL: merging is `dict.update` keyed by
MODULE NAME — later versions REPLACE earlier sets for the same module:
- "configparser": {LegacyInterpolation, SafeConfigParser}        ((3,2,0))
- "importlib.abc": from (3,3,0) {"Finder"} OVERWRITTEN by (3,12,0) →
  {ResourceReader, Traversable, TraversableResources}  ("Finder" LOST — bug,
  replicate)
- "pkgutil": {ImpImporter, ImpLoader}
- "collections": {Awaitable, Coroutine, AsyncIterable, AsyncIterator,
  AsyncGenerator, Hashable, Iterable, Iterator, Generator, Reversible,
  Sized, Container, Callable, Collection, Set, MutableSet, Mapping,
  MutableMapping, MappingView, KeysView, ItemsView, ValuesView, Sequence,
  MutableSequence, ByteString}
- "smtpd": {MailmanProxy}
- "typing": (3,11,0) {"Text"} OVERWRITTEN by (3,12,0) →
  {ByteString, Hashable, Sized}   ("Text" LOST — replicate)
- "urllib.parse": {Quoter} ; "webbrowser": {MacOSX}
- "ast": {Bytes, Ellipsis, NameConstant, Num, Str}
- "asyncio": {AbstractChildWatcher, MultiLoopChildWatcher, FastChildWatcher,
  SafeChildWatcher}
- "collections.abc": {ByteString}

### W4906 attributes (DEPRECATED_ATTRIBUTES, stdlib.py:433-452)
Included: (3,2,0) {configparser.ParsingError.filename}; (3,12,0)
{calendar.January, calendar.February, sqlite3.version, sqlite3.version_info,
sys.last_traceback, sys.last_type, sys.last_value}. Excluded: (3,13,0).

## 3.2 W1501 bad-open-mode + W1514 unspecified-encoding — _check_open_call (stdlib.py:847-920)

Entered with (node, open_module ∈ {"_io","pathlib","pathlib._local"},
func_name ∈ OPEN_FILES_FUNCS).

Phase A — mode argument:
```
mode_arg = None; confidence = HIGH
try:
    if open_module == "_io":
        mode_arg = get_argument_from_call(node, position=1, keyword="mode")
    elif open_module in PATHLIB_MODULE:
        mode_arg = get_argument_from_call(node, position=0, keyword="mode")
except NoSuchArgumentError:
    mode_arg = infer_kwarg_from_call(node, keyword="mode")   # looks inside **{...}
    if mode_arg: confidence = INFERENCE
if mode_arg:
    mode_arg = safe_infer(mode_arg)
    if (func_name in OPEN_FILES_MODE                 # only "open"/"file" — NOT read_text/write_text
        and isinstance(mode_arg, nodes.Const)
        and not _check_mode_str(mode_arg.value)):
        add_message("bad-open-mode", node=node,
                    args=mode_arg.value or str(mode_arg.value),  # "" → "" or "''"? "" is falsy → str("")=""
                    confidence=confidence)
```
`args=mode_arg.value or str(mode_arg.value)`: for value `""` → `str("")` =
`""` (renders `"" is not a valid mode for open.`); for value 0 → "0"; for a
non-str Const like `b"rb"` → bytes pass through (template %s →
`b'rb'`)... wait `_check_mode_str` returns False for non-str → message with
args = b'rb' (truthy) → rendered `"b'rb'" is not a valid mode for open.`.
Keep `%s` of the raw value.

`get_argument_from_call` (utils.py:717-744): positional index if present,
else keyword match in node.keywords, else NoSuchArgumentError.
`infer_kwarg_from_call` (utils.py:747-763): for each `**expr` kwarg,
safe_infer; if Dict, scan items for key Const == keyword → return value node.

`_check_mode_str` (stdlib.py:455-482) — port VERBATIM:
```
if not isinstance(mode, str): return False
modes = set(mode); _mode = "rwatb+Ux"; creating = "x" in modes
if modes - set(_mode) or len(mode) > len(modes): return False   # bad char or dup char
reading="r" in modes; writing="w" in modes; appending="a" in modes
text="t" in modes; binary="b" in modes
if "U" in modes:
    if writing or appending or creating: return False
    reading = True
if text and binary: return False
total = reading + writing + appending + creating
if total > 1: return False
if not (reading or writing or appending or creating): return False
return True
```
(Note "+" alone → total 0 → invalid; "Ub" → valid (reading+binary);
duplicates like "rr" → len(mode)>len(modes) → invalid.)

Phase B — encoding (W1514), only reached when `not mode_arg` OR mode is a
Const whose value doesn't contain "b" (`not (mode_arg.value and "b" in
str(mode_arg.value))` — note str() of value, so b"rb" contains "b" →
skipped):
```
confidence = HIGH
try:
    if open_module in PATHLIB_MODULE:
        match node.func.attrname:                    # AttributeError if func is a Name → F0002 crash path
            case "read_text":  encoding_arg = get_argument_from_call(node, position=0, keyword="encoding")
            case "write_text": encoding_arg = get_argument_from_call(node, position=1, keyword="encoding")
            case _:            encoding_arg = get_argument_from_call(node, position=2, keyword="encoding")
    else:
        encoding_arg = get_argument_from_call(node, position=3, keyword="encoding")
except NoSuchArgumentError:
    encoding_arg = infer_kwarg_from_call(node, keyword="encoding")
    if encoding_arg: confidence = INFERENCE
    else:
        add_message("unspecified-encoding", node=node, confidence=confidence)  # HIGH
if encoding_arg:
    encoding_arg = safe_infer(encoding_arg)
    if isinstance(encoding_arg, nodes.Const) and encoding_arg.value is None:
        add_message("unspecified-encoding", node=node, confidence=confidence)
```
So: missing encoding arg → message; explicit `encoding=None` (inferred
Const None) → message; uninferable encoding → no message. NOTE Phase B runs
for read_text/write_text too (W1514 only; W1501 never for them). Position 3
for _io.open = the real `open(file, mode, buffering, encoding)` signature.
W1514 fires even when the mode was BAD (Phase A and B are independent),
provided mode value has no "b".

## 3.3 W1502 boolean-datetime — DEAD on 3.12 (maxversion (3,5))

visit_unaryop (op == "not" → check operand), visit_if (test), visit_ifexp
(test), visit_boolop (each value) → `_check_datetime(node)` (stdlib.py:
723-739, 835-845): `next(node.infer())` (InferenceError → return); if
Instance and `inferred.qname() in {"_pydatetime.time", "datetime.time"}` →
add_message("boolean-datetime", node=node). Port behind the may_be_emitted
gate; only reachable when user sets `--py-version` < 3.5 (then the visits
run — only_required_for_messages consults emittability+enabledness at
prepare time).

## 3.4 W1503 redundant-unittest-assert — _check_redundant_assert (stdlib.py:822-833)

Reached when any inferred func has `root().name == "unittest.case"`.
```
if (isinstance(infer, astroid.BoundMethod) and node.args
        and isinstance(node.args[0], nodes.Const)
        and infer.name in {"assertTrue", "assertFalse"}):
    add_message("redundant-unittest-assert",
                args=(infer.name, node.args[0].value), node=node)
```
Template `Redundant use of %s with constant value %r` → e.g.
`Redundant use of assertTrue with constant value 'foo'` / `... value True`.
%r of the python value (None → None, str → quoted). Only the FIRST
positional arg, must be a literal Const at the call site (no inference).

## 3.5 W1506 bad-thread-instantiation (stdlib.py:634-642)

Reached when an inferred ClassDef qname == "threading.Thread".
```
func_kwargs = {key.arg for key in node.keywords}
if "target" in func_kwargs: return
if len(node.args) < 2 and not (node.kwargs and "target" in func_kwargs):
    add_message("bad-thread-instantiation", node=node, confidence=HIGH)
```
NOTE the second clause is DEAD: "target" in func_kwargs was already False
(early return otherwise), so the condition reduces to `len(node.args) < 2`.
`node.kwargs` (Keyword entries with arg=None, i.e. `**d`) never rescues.
Replicate: emit iff no `target=` keyword and fewer than 2 positional args.
(`Thread(group, target)` 2 positionals → ok; `Thread(**opts)` → message.)
`key.arg` for `**d` is None — fine in the set.

## 3.6 W1507 shallow-copy-environ (stdlib.py:655-673)

Reached when inferred FunctionDef qname == "copy.copy".
```
confidence = HIGH
try: arg = get_argument_from_call(node, position=0, keyword="x")
except NoSuchArgumentError:
    arg = infer_kwarg_from_call(node, keyword="x")
    if not arg: return
    confidence = INFERENCE
try: inferred_args = arg.inferred()
except astroid.InferenceError: return
for inferred in inferred_args:
    if inferred.qname() == "os._Environ":
        add_message("shallow-copy-environ", node=node, confidence=confidence); break
```
`arg.inferred()` = full inference list (NOT safe_infer); Uninferable in the
list → `Uninferable.qname()`?? UninferableBase has a `__getattr__` returning
itself; `inferred.qname()` returns Uninferable (callable → calling it gives
Uninferable, which != "os._Environ") — no crash, no match. No args.

## 3.7 W1508 invalid-envvar-default (+E1507) — _check_env_function (stdlib.py:922-985)

Reached when inferred FunctionDef qname ∈ ("os.getenv",).
```
env_name_kwarg = "key"; env_value_kwarg = "default"
kwargs = {kw.arg: kw.value for kw in node.keywords} if node.keywords else None
env_name_arg = node.args[0] if node.args else (kwargs["key"] if kwargs and "key" in kwargs else None)
if env_name_arg:
    _check_invalid_envvar_value(node, message="invalid-envvar-value",
        call_arg=safe_infer(env_name_arg), infer=infer, allow_none=False)   # E1507
env_value_arg = node.args[1] if len(node.args)==2 else (kwargs["default"] if ... else None)
if env_value_arg:
    _check_invalid_envvar_value(node, message="invalid-envvar-default",
        call_arg=safe_infer(env_value_arg), infer=infer, allow_none=True)   # W1508
```
NB `len(node.args) == 2` EXACTLY — `os.getenv(a, b, c)` (illegal anyway)
wouldn't check b. `_check_invalid_envvar_value` (stdlib.py:961-985):
```
if call_arg is None or isinstance(call_arg, UninferableBase): return
name = infer.qname()           # "os.getenv"
if isinstance(call_arg, nodes.Const):
    emit = False
    match call_arg.value:
        case None: emit = not allow_none
        case str(): pass
        case _: emit = True
    if emit: add_message(message, node=node, args=(name, call_arg.pytype()))
else:
    add_message(message, node=node, args=(name, call_arg.pytype()))
```
args = ("os.getenv", "builtins.int") etc. pytype of Const None =
"builtins.NoneType". Non-Const inferred (e.g. a List node) → always emit
with its pytype ("builtins.list"). W1508 template:
`%s default type is %s. Expected str or None.`; E1507 (notes/08 §13):
`%s does not support %s type argument`.

## 3.8 W1509 subprocess-popen-preexec-fn (stdlib.py:644-648)

Inferred ClassDef qname == "subprocess.Popen":
```
if node.keywords:
    for keyword in node.keywords:
        if keyword.arg == "preexec_fn":
            add_message("subprocess-popen-preexec-fn", node=node)
```
No args, UNDEFINED confidence. Fires once per matching keyword (can't
duplicate — duplicate kwargs are syntax errors; `**d` has arg None).

## 3.9 W1510 subprocess-run-check (stdlib.py:650-653)

Inferred FunctionDef qname == "subprocess.run":
```
kwargs = {keyword.arg for keyword in (node.keywords or ())}
if "check" not in kwargs:
    add_message("subprocess-run-check", node=node, confidence=INFERENCE)
```
NOTE: `**{"check": True}` does NOT count (arg None) → still flagged.
No args. Per inferred value (duplicate risk if func infers twice to
subprocess.run — infer_all may return the same FunctionDef multiple times;
pylint emits duplicates).

## 3.10 W1515 forgotten-debug-statement (stdlib.py:719-720)

Inferred FunctionDef qname ∈ ("builtins.breakpoint", "sys.breakpointhook",
"pdb.set_trace") → add_message("forgotten-debug-statement", node=node).
No args. NB `pdb.Pdb().set_trace()` infers to a BoundMethod (not
FunctionDef branch) → not flagged; bare `breakpoint()` is.

## 3.11 W1518 method-cache-max-size-none — visit_functiondef → _check_lru_cache_decorators (stdlib.py:746-792)

visit_functiondef (decorated only_required_for_messages(
"method-cache-max-size-none", "singledispatch-method",
"singledispatchmethod-function")):
```
if node.decorators:
    if isinstance(node.parent, nodes.ClassDef): self._check_lru_cache_decorators(node)
    self._check_dispatch_decorators(node)        # E1519/E1520 → notes/08 §13
```
NB: no visit_asyncfunctiondef alias here — async methods are NOT checked
(pylint quirk; AsyncFunctionDef does not trigger visit_functiondef in the
walker... actually pylint's walker dispatches AsyncFunctionDef to
visit_asyncfunctiondef only; StdlibChecker doesn't define it → async defs
skip W1518/E1519/E1520).

```
def _check_lru_cache_decorators(node):
    if any(utils.is_enum(ancestor) for ancestor in node.parent.ancestors()):
        return                       # Enum methods exempt (is_enum: name=="Enum" and root "enum")
    lru_cache_nodes = []
    for d_node in node.decorators.nodes:
        try:
            for infered_node in d_node.infer():
                q_name = infered_node.qname()
                if q_name in NON_INSTANCE_METHODS: return     # staticmethod/classmethod ANYWHERE aborts whole check
                if q_name in LRU_CACHE and isinstance(d_node, nodes.Call):
                    try: arg = get_argument_from_call(d_node, position=0, keyword="maxsize")
                    except NoSuchArgumentError: arg = infer_kwarg_from_call(d_node, "maxsize")
                    if not isinstance(arg, nodes.Const) or arg.value is not None:
                        break                                  # next decorator
                    lru_cache_nodes.append(d_node); break
                if q_name == "functools.cache":
                    lru_cache_nodes.append(d_node); break
        except astroid.InferenceError: pass
    for lru_cache_node in lru_cache_nodes:
        add_message("method-cache-max-size-none", node=lru_cache_node, confidence=INFERENCE)
```
Triggers: `@lru_cache(maxsize=None)` (Call whose func infers into LRU_CACHE
set, first-pos/keyword maxsize == Const None) or bare/called
`@functools.cache` (`@cache` bare Name infers to functools.cache FunctionDef
→ qname match — note bare `@lru_cache` (no call) has qname
functools.lru_cache ∈ LRU_CACHE but `isinstance(d_node, nodes.Call)` False →
falls through to the functools.cache test → no; bare @lru_cache NOT
flagged). `@cache` USED AS CALL `@cache()`: d_node is Call; infer of the
Call node — d_node.infer() infers the DECORATOR EXPRESSION (the call
result) → qname of result (lru wrapper instance?) — replicate inference
faithfully. Report at the DECORATOR node (Call or Name), INFERENCE.

## 3.12 Mixin overrides summary for StdlibChecker

- visit_attribute → W4906 (mixin, §2.1) using _deprecated_attributes (§3.1).
- visit_call → §3.0 (override; calls check_deprecated_class_in_call +
  check_deprecated_method; NOT the __import__ path — that lives only in the
  mixin's visit_call, which stdlib REPLACED. So `__import__("imp")` is
  flagged via IMPORTS checker's mixin visit_call instead).
- visit_import / visit_importfrom → mixin (§2.3/2.5): for stdlib,
  deprecated_modules() = () → only W4904 class-from-module checks fire
  (e.g. `from collections import Callable` → W4904; `import ast.Bytes`
  impossible syntactically, but `import collections.abc` + dotted class
  split per §2.3).
- visit_decorators → mixin §2.4 with _deprecated_decorators.

================================================================================
# 4. strings.py — StringFormatChecker (W1300-W1310; E-side in notes/08 §6)
================================================================================

Checker name "string" (strings.py:247), msgs = MSGS (strings.py:68-197).
Two checkers in this file share name "string" — message attribution
unaffected (name only matters for --disable=string which disables BOTH).

Format-string parsing helpers `parse_format_string` /
`parse_format_method_string` / `split_format_field_names` are spec'd in
notes/08 §0.20 — same functions feed the W messages here.

## 4.1 visit_binop — %-formatting (strings.py:251-405)

Full E-message flow in 08 §6; the W-message rows with exact order:
```
if node.op != "%": return
if not (isinstance(node.left, nodes.Const) and isinstance(left.value, str)): return
try: required_keys, required_num_args, required_key_types, required_arg_types = parse_format_string(fs)
except UnsupportedFormatCharacter as exc: → E1300; return
except IncompleteFormatString: → E1301; return
if not required_keys and not required_num_args:
    add_message("format-string-without-interpolation", node=node); return    # W1310
if required_keys and required_num_args: → E1302 (mixed) [no return]
elif required_keys:
    if isinstance(args, nodes.Dict):
        for k,_ in args.items:
            if isinstance(k, nodes.Const):
                if isinstance(k.value, str): keys.add(k.value)
                else: add_message("bad-format-string-key", node=node, args=k.value)  # W1300, args=raw value (%s)
            else: unknown_keys = True
        if not unknown_keys:
            for key in required_keys:
                if key not in keys: → E1304 per missing key
        for key in keys:
            if key not in required_keys:
                add_message("unused-format-string-key", node=node, args=key)  # W1301, %r
        for key, arg in args.items: → E1307 typed-key checks (08)
    elif isinstance(args, (OTHER_NODES, nodes.Tuple)): → E1303
else: (unnamed-only branch → E1305/E1306/E1307, 08)
```
ORDER DEPENDENCY: W1300 messages come in `args.items` order; E1304 in
`required_keys` order — required_keys is a `set` built by
parse_format_string → ITERATION ORDER IS str-hash order, deterministic only
under PYTHONHASHSEED=0. W1301 in `keys` (set) order — same caveat. Replicate
hashseed-0 set iteration for str sets, or verify corpora diffs show
single-key cases only (open Q §24).
W1300 args is the raw non-str key value via %s: `1` → `Format string
dictionary key should be a string, not 1`; `(1, 2)` tuple-const key →
`(1, 2)`.
W1310 from visit_binop: format string with NO conversion specifiers at all
(e.g. `"hello" % x`). Report at BinOp. No confidence.

## 4.2 visit_call → str.format checks (strings.py:419-437)

`visit_call` is UNDECORATED (always runs). Single match:
```
match func := utils.safe_infer(node.func):
    case astroid.BoundMethod(bound=astroid.Instance(name="str"|"unicode"|"bytes" as bound_name)):
        if func.name in {"strip","lstrip","rstrip"} and node.args: → E1310 (08)
        elif func.name == "format": self._check_new_format(node, func)
```

`_check_new_format` (strings.py:452-534):
```
if isinstance(node.func, nodes.Attribute) and not isinstance(node.func.expr, nodes.Const): return
if node.starargs or node.kwargs: return
try: strnode = next(func.bound.infer())
except InferenceError: return
if not (isinstance(strnode, nodes.Const) and isinstance(strnode.value, str)): return
try: call_site = arguments.CallSite.from_call(node)
except InferenceError: return
try: fields, num_args, manual_pos = parse_format_method_string(strnode.value)
except IncompleteFormatString:
    add_message("bad-format-string", node=node); return                     # W1302
positional_arguments = call_site.positional_arguments
named_arguments = call_site.keyword_arguments
named_fields = {field[0] for field in fields if isinstance(field[0], str)}  # SET of str
if num_args and manual_pos:
    add_message("format-combined-specification", node=node); return         # W1305
check_args = False
num_args += sum(1 for field in named_fields if not field)   # "{[0]}" empty-name fields count as positional
if named_fields:
    for field in named_fields:                               # SET ORDER (hashseed-0!)
        if field and field not in named_arguments:
            add_message("missing-format-argument-key", node=node, args=(field,))   # W1303
    for field in named_arguments:                            # dict order (deterministic)
        if field not in named_fields:
            add_message("unused-format-string-argument", node=node, args=(field,)) # W1304
    num_args = num_args or manual_pos
    if positional_arguments or num_args:
        empty = not all(field for field in named_fields)
        if named_arguments or empty: check_args = True
else:
    check_args = True
if check_args:
    num_args = num_args or manual_pos
    if not num_args:
        add_message("format-string-without-interpolation", node=node); return     # W1310
    if len(positional_arguments) > num_args: → E1305
    elif len(positional_arguments) < num_args: → E1306
self._detect_vacuous_formatting(node, positional_arguments)                 # W1308
self._check_new_format_specifiers(node, fields, named_arguments)            # W1306/W1307
```
Bail-outs to memorize: (1) `node.func.expr` must be a Const (literal
`"...".format(...)` only — `s.format()` via variable skipped); (2) any
`*args`/`**kwargs` at call site skipped; (3) bound value must infer to a
Const str. W1302/W1305 RETURN (no further checks). The first `return` in
this function is reachable with node.func a Name (`fmt = "...".format;
fmt()`): then node.func not Attribute → falls through with func.bound being
the original Const — still checked! (issue-351 comment, strings.py:454-462).

`_detect_vacuous_formatting` (strings.py:439-450): Counter over `arg.name
for arg in positional_arguments if isinstance(arg, nodes.Name)`; for each
name with count > 1 → W1308 args=(name,). Counter iteration = insertion
order (deterministic). NB positional_arguments are the CallSite-resolved
nodes.

## 4.3 _check_new_format_specifiers — W1306/W1307 (strings.py:537-636)

For each `(key, specifiers)` in fields (LIST order = appearance order):
```
if not key: key = 0                      # "{[0]}"-style → positional 0
if isinstance(key, int):
    try: argname = get_argument_from_call(node, key)
    except NoSuchArgumentError: continue
else:
    if key not in named: continue
    argname = named[key]
if argname is None or isinstance(argname, UninferableBase): continue
try: argument = utils.safe_infer(argname)
except InferenceError: continue
if not (specifiers and argument): continue          # no attr/index path, or inference failed
if argument.parent and isinstance(argument.parent, nodes.Arguments): continue  # function params: skip
previous = argument; parsed = []
for is_attribute, specifier in specifiers:
    if isinstance(previous, UninferableBase): break
    parsed.append((is_attribute, specifier))
    if is_attribute:
        try: previous = previous.getattr(specifier)[0]
        except astroid.NotFoundError:
            if hasattr(previous, "has_dynamic_getattr") and previous.has_dynamic_getattr(): break
            path = get_access_path(key, parsed)
            add_message("missing-format-attribute", args=(specifier, path), node=node)  # W1306
            break
    else:
        warn_error = False
        if hasattr(previous, "getitem"):
            try: previous = previous.getitem(nodes.Const(specifier))
            except (AstroidIndexError, AstroidTypeError, AttributeInferenceError): warn_error = True
            except InferenceError: break
            if isinstance(previous, UninferableBase): break
        else:
            try: previous.getattr("__getitem__"); break
            except astroid.NotFoundError: warn_error = True
        if warn_error:
            path = get_access_path(key, parsed)
            add_message("invalid-format-index", args=(specifier, path), node=node)      # W1307
            break
    try: previous = next(previous.infer())
    except InferenceError: break
```
`get_access_path(key, parts)` (strings.py:210-220): str(key) + for each
(is_attr, spec): `.spec` if attr else `[{spec!r}]`. Example: field
`{0.length}` missing → args=("length", "0.length")? path = "0" + ".length"
→ `Missing format attribute 'length' in format specifier '0.length'`.
Index example `{a[1]}` → path "a" + "[" + repr("1"|1) + "]" — NB specifier
from parse keeps ints as int (notes/08 §0.20 split rules): `a[1]` →
`['a'][1]` → path `a[1]`; `a[b]` → `a['b']`.

## 4.4 W1309 f-string-without-interpolation — visit_joinedstr (strings.py:407-417)

```
@only_required_for_messages("f-string-without-interpolation")
def visit_joinedstr(self, node):
    if isinstance(node.parent, (nodes.TemplateStr, nodes.FormattedValue)): return
    for value in node.values:
        if isinstance(value, nodes.FormattedValue): return
    add_message("f-string-without-interpolation", node=node)
```
f-string with zero `{}` fields (values all Const). Nested JoinedStr inside a
FormattedValue (format-spec) or TemplateStr exempt. Report at JoinedStr.
`f""` (empty, values=[]) → flagged.

================================================================================
# 5. strings.py — StringConstantChecker (W1401 W1402 W1404 W1405 W1406)
================================================================================

BaseTokenChecker + BaseRawFileChecker, name "string" (strings.py:639-642).
Options (strings.py:679-703):
- check-str-concat-over-line-jumps: default False ("yn")
- check-quote-consistency: default False ("yn")
Class constants: ESCAPE_CHARACTERS = `abfnrtvx\n\r\t\\'\"01234567`;
UNICODE_ESCAPE_CHARACTERS = "uUN" (strings.py:707-711).

`process_module` (raw hook, strings.py:721-722): sets `_unicode_literals`
from future imports — value is never read afterwards (dead; skip in port).

## 5.1 process_tokens — token bookkeeping (strings.py:724-755)

For each token:
- ENCODING token (always index 0): `encoding = token` string (e.g. "utf-8").
- STRING token: (a) `process_string_token(token, start_row, start_col)` →
  W1401/W1402; (b) find next non-NEWLINE/NL/COMMENT token; (c) if
  `encoding != "ascii"` convert `start` col to BYTE offset:
  `start = (row, len(line[:col].encode(encoding)))` — tokenize cols are
  character-based, astroid col_offset is byte-based; since the ENCODING
  token is virtually always "utf-8" (≠"ascii") this conversion ALWAYS runs;
  (d) `self.string_tokens[start] = (str_eval(token), next_token)`;
  (e) `self._parenthesized_string_tokens[start] =
  _is_initial_string_token(i) and _is_parenthesized(i)`.

`str_eval` (strings.py:1023-1036): strip 2-char prefix if lower in
{"fr","rf"} else 1-char if lower in {"r","u","f"} (NB "b"/"br"/"rb" NOT
stripped — bytes keep prefix, making `matching_token != elt.value` true for
bytes... but check_for_concatenated_strings only handles str pytypes, so
moot); strip triple or single quotes.

`_is_initial_string_token` (:757-766): previous non-(NEWLINE/NL/COMMENT)
token is NOT a STRING, and next such token IS a STRING.
`_is_parenthesized` (:768-779): nearest non-(NEWLINE/NL/COMMENT/STRING)
tokens before and after are OP "(" and OP ")" respectively.

## 5.2 W1401 anomalous-backslash-in-string / W1402 anomalous-unicode-escape-in-string

`process_string_token` (strings.py:913-937): find first quote char in token;
`prefix = token[:_index].lower()`; quote_length = 3 if
`after_prefix[:3] == after_prefix[-3:] == 3*quote_char` else 1;
`string_body = after_prefix[quote_length:-quote_length]`. Raw strings
(`"r" in prefix`) skipped entirely.

`process_non_raw_string_token` (strings.py:939-999):
```
index = 0
while True:
    index = string_body.find("\\", index)
    if index == -1: break
    next_char = string_body[index+1]          # cannot IndexError: trailing \ is a SyntaxError
    match = string_body[index:index+2]
    last_newline = string_body.rfind("\n", 0, index)
    if last_newline == -1:
        line = start_row; col_offset = index + string_start_col
    else:
        line = start_row + string_body.count("\n", 0, index)
        col_offset = index - last_newline - 1
    if next_char in "uUN":
        if "u" in prefix: pass
        elif "b" not in prefix: pass          # str: unicode escapes valid
        else: add_message("anomalous-unicode-escape-in-string", line=line,
                          args=(match,), col_offset=col_offset)            # W1402 (bytes only)
    elif next_char not in ESCAPE_CHARACTERS:
        add_message("anomalous-backslash-in-string", line=line,
                    args=(match,), col_offset=col_offset)                  # W1401
    index += 2
```
where `string_start_col = start_col + len(prefix) + quote_length`
(character columns from tokenize, NOT byte-converted!). args=(match,) is
the 2-char `\x` slice → rendered like `Anomalous backslash in string:
'\d'. String constant might be missing an r prefix.`. Line/col passed
explicitly, no node → no end positions. Multi-line strings: line advanced
by count of "\n" before index, col relative to last newline.
NB f-strings: in py3.12 tokenize, f-strings are FSTRING_START/MIDDLE/END
tokens, NOT STRING → their bodies are NOT checked here (only plain/b/u
strings produce STRING tokens). Replicate by only processing STRING tokens.

## 5.3 W1404 implicit-str-concat (old name W1403)

AST-side visitors (strings.py:805-823):
```
@only_required_for_messages("implicit-str-concat") visit_call:  check(node.args, "call")
... visit_list: (node.elts, "list");  visit_set: (node.elts, "set");  visit_tuple: (node.elts, "tuple")
def visit_assign(self, node):        # UNDECORATED
    if isinstance(node.value, nodes.Const) and isinstance(node.value.value, str):
        check([node.value], "assignment")
```
`check_for_concatenated_strings(elements, iterable_type)` (strings.py:874-911):
```
for elt in elements:
    if not (isinstance(elt, nodes.Const) and elt.pytype() in
            ("__builtin__.unicode", "__builtin__.str", "builtins.str")): continue
    if elt.col_offset < 0: continue                  # escaped newlines edge
    token_index = (elt.lineno, elt.col_offset)
    if token_index not in self.string_tokens: continue    # e.g. Latin1 mismatch
    matching_token, next_token = self.string_tokens[token_index]
    if (matching_token != elt.value and next_token is not None
            and next_token.type == tokenize.STRING):
        if next_token.start[0] == elt.lineno or (
                cfg.check_str_concat_over_line_jumps
                and not self._parenthesized_string_tokens.get((elt.lineno, elt.col_offset))):
            add_message("implicit-str-concat", line=elt.lineno,
                        args=(iterable_type,), confidence=HIGH)
```
Semantics: the AST Const spans the WHOLE concatenation; the token at its
start position holds only the FIRST literal — if they differ AND the next
token is another STRING, it's an implicit concat. Same-line concat → always
flagged. Cross-line → only with check-str-concat-over-line-jumps=y AND the
first token not freestanding-parenthesized. DEFAULT: only same-line concats
fire. Report: line=elt.lineno, NO node → col 0, no end. One message per
matching element (a tuple `("a" "b", "c" "d")` → two messages).
NB f-string components don't match `string_tokens` (FSTRING tokens not
recorded) → skipped. Byte strings: pytype builtins.bytes → excluded by the
pytype filter.

## 5.4 W1405 inconsistent-quotes — OFF by default

Only when `check-quote-consistency` = y (default n):
`check_for_consistent_string_delimiters(tokens)` (strings.py:825-872):
- Pass 1: Counter of `_get_quote_delimiter(token)` over STRING tokens where
  `_is_quote_delimiter_chosen_freely(token)` (not triple-quoted; the other
  quote char not present in str_eval body — strings.py:1083-1101).
  py3.12: FSTRING_START/END toggle `inside_fstring`; contents skipped only
  if `cfg.py_version < (3,12)` (target_py312 check, strings.py:839-854) —
  at default py-version (3,12) f-string FSTRING_* tokens are NOT STRING
  tokens anyway, so nothing changes.
- If >1 distinct delimiters counted: most_common(1) (ties broken by Counter
  insertion order) then pass 2 re-scan: each STRING token freely-chosen with
  a different delimiter → `add_message("inconsistent-quotes",
  line=start[0], args=(quote_delimiter,))`. args like `"'"` rendered via %s.

## 5.5 W1406 redundant-u-string-prefix — visit_const (strings.py:1001-1015)

```
@only_required_for_messages("redundant-u-string-prefix")
def visit_const(self, node):
    if node.pytype() == "builtins.str" and not isinstance(node.parent, nodes.JoinedStr):
        if node.kind == "u":
            add_message("redundant-u-string-prefix", line=node.lineno, col_offset=node.col_offset)
```
`Const.kind` is "u" only for u-prefixed literals (CPython ast keeps it).
No args, explicit line+col (node's own), no end positions (no node passed).

================================================================================
# 6. logging.py — LoggingChecker (W1201 W1202 W1203; E-side in 08 §7)
================================================================================

Checker name "logging" (logging.py:131). Options (logging.py:134-156):
- logging-modules: default ("logging",) csv
- logging-format-style: default "old", choices old|new
All three W templates are IDENTICAL: `Use %s formatting in logging functions`
(W1201 logging-not-lazy, W1202 logging-format-interpolation, W1203
logging-fstring-interpolation) — only the symbol differs.

## 6.1 Module-name tracking (logging.py:158-189)

visit_module resets state: `_logging_names = set()`; `_logging_modules =
set(cfg.logging_modules)`; `_from_imports = {parent: child}` for any
configured dotted module (default "logging" has no dot → empty).
visit_importfrom: if node.modname in _from_imports and the matching child is
imported → add its as-name (or name) to _logging_names.
visit_import: for each (module, as_name): if module in _logging_modules →
`_logging_names.add(as_name or module)`.
So with defaults: `import logging` → {"logging"}; `import logging as log` →
{"log"}; `from logging import ...` adds NOTHING (no dotted config).

## 6.2 visit_call dispatch (logging.py:191-220) — UNDECORATED

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
Path 1 is purely syntactic (`logging.X(...)` / `log.X(...)`) — name is the
attrname whatever it is. Path 2 is inference: a bound method whose defining
class is logging.Logger or a subclass; name = the METHOD name from the
proxied function.

`_check_log_method` (logging.py:222-269):
```
if name == "log":
    if node.starargs or node.kwargs or len(node.args) < 2: return
    format_pos = 1
elif name in {"critical","debug","error","exception","fatal","info","warn","warning"}:
    if node.starargs or node.kwargs or not node.args: return
    format_pos = 0
else: return
match format_arg := node.args[format_pos]:
    case nodes.BinOp(): → W1201 logic (§6.3)
    case nodes.Call():  self._check_call_func(format_arg)      # W1202 (§6.4)
    case nodes.Const(): self._check_format_string(node, format_pos)  # E1200/01/05/06 (08 §7)
    case nodes.JoinedStr():
        if str_formatting_in_f_string(format_arg): return
        → W1203 (§6.5)
```
ANY `*`/`**` at the call site bails everything.

## 6.3 W1201 logging-not-lazy (logging.py:240-257)

```
binop = format_arg; emit = binop.op == "%"
if binop.op == "+" and not self._is_node_explicit_str_concatenation(binop):
    total_number_of_strings = sum(
        1 for operand in (binop.left, binop.right)
        if self._is_operand_literal_str(utils.safe_infer(operand)))
    emit = total_number_of_strings > 0
if emit:
    add_message("logging-not-lazy", node=node, args=(self._helper_string(node),))
```
- `_is_operand_literal_str(x)`: `isinstance(x, nodes.Const) and x.name ==
  "str"` (Const.name property = type name).
- `_is_node_explicit_str_concatenation(n)` (logging.py:293-304): recursive —
  BinOp whose left and right are each (literal-str Const [RAW node, not
  inferred] or themselves explicit concatenations). `"a" + "b"` → explicit →
  exempt. `"a" + x` → not explicit → infer both operands; if either infers
  to a literal str → emit.
- op "%" → always emit. Other ops (`*`, f-string-join…) → no.
Report at the CALL node (not the binop). args = helper string (§6.6).

## 6.4 W1202 logging-format-interpolation — _check_call_func (logging.py:306-320)

format arg is a Call: `func = safe_infer(node.func)` (node = the inner
call); emit iff func is a BoundMethod, `is_method_call(func, types=("str",
"unicode"), methods=("format",))` (logging.py:106-125: bound is Instance
with name in types, func.name in methods), and NOT
`is_complex_format_str(func.bound)` (logging.py:378-388: safe_infer the
bound; if not Const-str → True (complex → SKIP message); else
`string.Formatter().parse(value)`; ValueError → False; else True iff any
field has a non-empty format_spec — i.e. `"{:.2f}".format(...)` is exempt
from W1202). Report at the INNER Call node (format_arg), args = helper
string.

## 6.5 W1203 logging-fstring-interpolation (logging.py:262-269)

format arg is a JoinedStr; exempt if `str_formatting_in_f_string`
(logging.py:407-417): any Const part containing "%" AND any of
{"%s","%d","%f","%r"} — i.e. f-strings that THEMSELVES contain %-style
placeholders are skipped (they'd be handled lazily). Otherwise emit at the
OUTER Call node, args = helper string. NB: an f-string with no
FormattedValue still emits W1203 here (and W1309 from the string checker).

## 6.6 _helper_string (logging.py:271-286) — the args computation

```
valid_types = ["lazy %"]
if not is_message_enabled("logging-fstring-formatting", node.fromlineno): valid_types.append("fstring")
if not is_message_enabled("logging-format-interpolation", node.fromlineno): valid_types.append(".format()")
if not is_message_enabled("logging-not-lazy", node.fromlineno): valid_types.append("%")
return " or ".join(valid_types)
```
BUG, replicate: "logging-fstring-formatting" is NOT a registered symbol
(real symbol: logging-fstring-interpolation). is_message_enabled
(lint/message_state_handler.py:315-345) catches UnknownMessageError and
treats the string as a raw msgid; `_is_one_message_enabled` then misses in
`_module_msgs_state` and falls back to `self._msgs_state.get(msgid, True)`
→ ALWAYS True → "fstring" is NEVER appended. So:
- default config: all enabled → args = "lazy %".
- `--disable=logging-format-interpolation`: W1201/W1203 args =
  "lazy % or .format()".
- `--disable=logging-not-lazy`: W1202/W1203 args = "lazy % or %".
- both disabled: "lazy % or .format() or %".
- Line-level pragmas count too (is_message_enabled is line-aware with
  node.fromlineno of the call).
Port requires consulting the message-state machinery at emission time.

================================================================================
# 7. newstyle.py — NewStyleConflictChecker: E1003 bad-super-call
================================================================================

(E-category; enabled in -E mode; spec'd HERE because notes/06/08 skip it.)
Checker name "newstyle", msgs only E1003 `Bad first argument %r given to
super()` (newstyle.py:21-28). No options.

visit_functiondef / visit_asyncfunctiondef (newstyle.py:46-109):
```
if not node.is_method(): return
klass = node.parent.frame()
for stmt in node.nodes_of_class(nodes.Call):          # ALL calls in the method, doc order
    if node_frame_class(stmt) != node_frame_class(node): continue   # skip nested-class scopes
    expr = stmt.func
    if not isinstance(expr, nodes.Attribute): continue
    match call := expr.expr:
        case nodes.Call(func=nodes.Name(name="super"), args=[arg0, *_]): pass
        case _: continue
    # so: super(<arg0>, ...).<attr>(...)  — needs >=1 super arg AND a method call on it
    match arg0:
        case nodes.Call(func=nodes.Name(name="type")):
            add_message("bad-super-call", node=call, args=("type",)); continue
    match call.args:
        case [nodes.Attribute(attrname="__class__"), nodes.Name(name="self"), *_]:
            add_message("bad-super-call", node=call, args=("self.__class__",)); continue
    try: supcls = call.args and next(call.args[0].infer(), None)
    except astroid.InferenceError: continue
    if klass is not supcls and all(i != supcls for i in klass.ancestors()):
        name = None
        if supcls: name = supcls.name
        elif call.args and hasattr(call.args[0], "name"): name = call.args[0].name
        if name: add_message("bad-super-call", node=call, args=(name,))
```
Details:
- ONLY fires for `super(X, ...).something` — a bare `super(X, self)` without
  an attribute access is never reported (expr must be Attribute on the super
  Call).
- `node_frame_class` (utils.py:677-699): nearest enclosing ClassDef frame of
  the call vs the method — calls inside nested funcs of the same class still
  pass (frame_class equality, not scope).
- `super(type(self), self)` → args=("type",) regardless of inference.
- `super(self.__class__, self)` → args=("self.__class__",) — pattern needs
  EXACTLY Attribute(__class__) then Name "self" as first two args.
- General case: first super-arg inferred (FIRST result only via next, None
  default; Uninferable → falsy supcls). Emit when the inferred first arg is
  neither the enclosing class nor ANY of its ancestors (ancestors() —
  duplicates allowed, generator). `all(i != supcls ...)` uses __eq__ (node
  identity for ClassDef).
- name: inferred node's `.name` if inferred truthy; else syntactic
  `call.args[0].name` if present (Name node); if still None → no message
  (e.g. `super(some.attr, self)` uninferable → silent).
- Report node = the inner `super(...)` Call node (`call`), args=(name,) %r.
- Position: Call node fromlineno/col (no .position).
- Method bodies only (`is_method()`: frame parent is ClassDef); plain
  functions and lambdas skipped.

================================================================================
# 8. spelling.py — SpellingChecker (C0401 C0402 C0403) — inert by default
================================================================================

BaseTokenChecker, name "spelling" (spelling.py:206-209). Messages
(spelling.py:210-228): C0401 wrong-spelling-in-comment, C0402
wrong-spelling-in-docstring (template
`Wrong spelling of a word '%s' in a comment:\n%s\n%s\nDid you mean: '%s'?` /
docstring variant), C0403 invalid-characters-in-docstring
(`Invalid characters %r in a docstring`).

ENABLEMENT RULES (the important part):
- Messages are default-ENABLED in the store (no default_enabled flag), but
  the checker is a NO-OP unless BOTH (a) pyenchant is importable
  (PYENCHANT_AVAILABLE, spelling.py:22-36) and (b) option `spelling-dict`
  is non-empty (default "" — spelling.py:231-238; choices = [""] +
  installed enchant dicts).
- `open()` (spelling.py:293-336): returns early (initialized=False) when
  either fails; `process_tokens` and `_check_docstring` both gate on
  `self.initialized`.
- The pinned venv does NOT ship pyenchant → on the reference runtime the
  checker can NEVER emit, even with --spelling-dict (choice validation would
  reject non-"" values anyway when no dicts exist).
PORT DECISION: implement as a registered checker that never emits; do not
port the enchant tokenization (out of reach without the C library). Note
the options exist for config parsing: spelling-dict "", spelling-ignore-words
"", spelling-private-dict-file "", spelling-store-unknown-words "n",
max-spelling-suggestions 4, spelling-ignore-comment-directives
"fmt: on,fmt: off,noqa:,noqa,nosec,isort:skip,mypy:" (spelling.py:229-291).
If ever needed, the emission logic is spelling.py:339-469 (token comments →
C0401 skipping shebang line-1 `#!/`, `# pylint:`, `# type: ` prefixes;
module/class/function docstrings line-by-line → C0402 starting at
node.lineno+1; enchant.errors.Error → C0403 with args=(word,)).

================================================================================
# 9. threading_checker.py — W2101 useless-with-lock
================================================================================

Checker name "threading" (threading_checker.py:18-43). LOCKS = frozenset
{"threading.Lock", "threading.RLock", "threading.Condition",
"threading.Semaphore", "threading.BoundedSemaphore"}.

```
@only_required_for_messages("useless-with-lock")
def visit_with(self, node):
    context_managers = (c for c, _ in node.items if isinstance(c, nodes.Call))
    for context_manager in context_managers:
        if isinstance(context_manager, nodes.Call):       # redundant re-check
            infered_function = safe_infer(context_manager.func)
            if infered_function is None: continue
            qname = infered_function.qname()
            if qname in self.LOCKS:
                add_message("useless-with-lock", node=node, args=qname)
```
`with threading.Lock():` → message. Trigger: any with-item whose expression
is a DIRECT Call whose func safe-infers to one of the five qnames
(ClassDef.qname). args = bare qname string → `'threading.Lock()' directly
created in 'with' has no effect`. Report at the With node (one message per
matching item — `with Lock(), RLock():` → two). visit_with only — `async
with` (AsyncWith) NOT checked. safe_infer Uninferable → qname via
UninferableBase.__getattr__ → returns Uninferable (callable) — wait:
`infered_function.qname()` on Uninferable returns Uninferable, `in LOCKS` →
False. No crash. (safe_infer CAN return Uninferable, notes/08 §0.1.)

================================================================================
# 10. nested_min_max.py — W3301 nested-min-max
================================================================================

Checker name "nested_min_max" (nested_min_max.py:30-46). FUNC_NAMES =
("builtins.min", "builtins.max"). DICT_TYPES = (objects.DictValues,
objects.DictKeys, objects.DictItems, nodes.Dict).

`maybe_get_inferred_min_max_call(node)` (:48-58): safe_infer(node.func) is a
FunctionDef with qname in FUNC_NAMES → return it, else None.

`get_redundant_calls(node, inferred_call)` (:60-76):
```
return [arg for arg in node.args
        if (isinstance(arg, nodes.Call)
            and (inferred := maybe_get_inferred_min_max_call(arg))
            and inferred.qname == inferred_call.qname     # BOUND-METHOD comparison (no call!)
            and len(arg.parent.args) > 1)]                # allow max(max(matrix)) single-arg
```
QUIRK: `inferred.qname == inferred_call.qname` compares bound methods —
equal iff same `__self__` AND same `__func__`, i.e. iff `inferred is
inferred_call` (the same FunctionDef object). Both min and max infer to
their respective builtins FunctionDef singletons, so `min(1, max(2, 3))` →
inner inferred (max FunctionDef) is not outer (min FunctionDef) → NOT
redundant (good); but the comparison being identity also means a re-built
duplicate builtins tree would break it — in our port: compare GNode
identity of the inferred FunctionDef.

`visit_call` (:78-139), decorated for "nested-min-max":
```
inferred = maybe_get_inferred_min_max_call(node)
if inferred is None: return
redundant_calls = get_redundant_calls(node, inferred)
if not redundant_calls: return
fixed_node = copy.copy(node)                  # SHALLOW copy; fixed_node.args mutated below
while len(redundant_calls) > 0:
    for i, arg in enumerate(fixed_node.args):
        if isinstance(arg, nodes.Call) and any(isinstance(a, nodes.GeneratorExp) for a in arg.args):
            return                            # any nested call w/ genexp arg → bail entirely
        if arg in redundant_calls:
            fixed_node.args = fixed_node.args[:i] + arg.args + fixed_node.args[i+1:]
            break
    redundant_calls = get_redundant_calls(fixed_node, inferred)
for idx, arg in enumerate(fixed_node.args):
    if not isinstance(arg, nodes.Const):
        if self._is_splattable_expression(arg):
            splat_node = nodes.Starred(ctx=Context.Load, lineno=arg.lineno, col_offset=0,
                                       parent=nodes.NodeNG(...dummy...), end_lineno=0, end_col_offset=0)
            splat_node.value = arg
            fixed_node.args = [*fixed_node.args[:idx], splat_node,
                               *fixed_node.args[idx+1 : idx]]     # ← EMPTY SLICE: TRAILING ARGS DROPPED
func_name = node.func.attrname if isinstance(node.func, nodes.Attribute) else node.func.name
add_message("nested-min-max", node=node, args=(func_name, fixed_node.as_string()),
            confidence=INFERENCE)
```
BUGS to replicate:
1. The genexp bail checks EVERY Call arg of fixed_node (not just redundant
   ones) — `min(min(x), foo(i for i in y))` → bail, no message.
2. The splat rewrite `[*args[:idx], splat, *args[idx+1:idx]]` — the second
   slice is ALWAYS EMPTY, so every arg after the first splatted non-Const
   arg is DROPPED from the suggestion string. E.g.
   `min(min(a_list), 5)` where a_list infers to a list → suggestion
   becomes `min(*a_list)` (the `5` vanishes). Only ONE splat ever happens
   in effect (loop continues but enumerate is over the new shorter list —
   note the loop is `for idx, arg in enumerate(fixed_node.args)` evaluated
   ONCE at loop start over the ORIGINAL flattened list object… actually
   fixed_node.args is REBOUND to a new list inside; enumerate holds the OLD
   list → subsequent iterations index the old list while testing
   membership/Const-ness; additional splats append to the latest rebound
   list. Port by simulating exact CPython semantics of iterating the
   captured list while rebinding `fixed_node.args`.)
3. `as_string()` of the synthetic tree: Starred renders `*<value>`; the
   flattened call renders e.g. `min(1, 2, 3)`. Our pyast must provide
   astroid-compatible as_string for this synthesized Call (same renderer
   used by E1130/E1131 messages, notes/06).
`_is_splattable_expression(arg)` (:141-172): BinOp "+"/"|" → both sides
splattable (recursive); safe_infer pytype in {builtins.list, builtins.tuple}
→ True; `isinstance(inferred or arg, (List, Tuple, Set, ListComp, DictComp,
DictValues, DictKeys, DictItems, Dict))` → True; else False.
Message example: `Do not use nested call of 'min'; it's possible to do
'min(1, 2, 3)' instead`. Report at the OUTER Call, INFERENCE.
"Multiple nested min/max calls on the same line will raise multiple
messages" (class docstring) — each outer call visited independently.

================================================================================
# 11. bad_chained_comparison.py — W3601 bad-chained-comparison
================================================================================

Checker name "bad-chained-comparison" (bad_chained_comparison.py:22-35).
Groups: COMPARISON_OP {<, <=, >, >=, !=, ==}; IDENTITY_OP {is, is not};
MEMBERSHIP_OP {in, not in}.

```
def visit_compare(self, node):                  # UNDECORATED
    operators = sorted({op[0] for op in node.ops})   # sorted unique op strings
    if self._has_diff_semantic_groups(operators):
        num_parts = f"{len(node.ops)}"
        incompatibles = ", ".join(f"'{o}'" for o in operators[:-1]) + f" and '{operators[-1]}'"
        add_message("bad-chained-comparison", node=node, args=(num_parts, incompatibles), confidence=HIGH)

def _has_diff_semantic_groups(self, operators):
    for semantic_group in (COMPARISON_OP, IDENTITY_OP, MEMBERSHIP_OP):
        if operators[0] in semantic_group:
            group = semantic_group
    return not all(o in group for o in operators)
```
- Works on ANY Compare (single-op compares have one unique operator → same
  group → False; so effectively only chained comparisons fire).
- `operators` is the SORTED UNIQUE list — e.g. `a < b is c` → ["<", "is"] →
  groups differ → args=("2", "'<' and 'is'").
- Sorting is lexicographic on the op strings: "!=" < "<" < "<=" < "==" <
  ">" < ">=" < "in" < "is" < "is not" < "not in" (ASCII).
- `num_parts` = number of comparison OPS (len(node.ops)), not operands.
- Single-element operators list: `incompatibles = "" + "and '<'"` →
  `" and '<'"`? NO — single-element can't reach emission (same group).
- `group` variable: operators[0] always belongs to exactly one group (every
  possible Compare op is in one) → no NameError.
Report at Compare node, HIGH. Message ex.: `Suspicious 2-part chained
comparison using semantically incompatible operators ('<' and 'is')`.

================================================================================
# 12. dunder_methods.py — C2801 unnecessary-dunder-call
================================================================================

Checker name "unnecessary-dunder-call" (dunder_methods.py:21-45).
`open()` builds `self._dunder_methods` from constants.DUNDER_METHODS
(constants.py:123-221) filtered by `since_vers <= cfg.py_version` —
CONFIG py-version (default (3,12)) → both the (0,0) dict (~90 entries,
mapping dunder name → suggestion text, e.g. "__len__" → "Use len built-in
function") and the (3,10) dict ("__aiter__"/"__anext__") are included.
Transcribe the full (0,0)+(3,10) maps verbatim from constants.py:123-221
(they are message-arg text — byte-exact required).
NOT in the map (EXTRA_DUNDER_METHODS, constants.py:222-246, never flagged):
__new__, __subclasses__, __init_subclass__, __set_name__, __class_getitem__,
__missing__, __exit__, __await__, __aexit__, __getnewargs_ex__,
__getnewargs__, __getstate__, __index__, __setstate__, __reduce__,
__reduce_ex__, __post_init__, _generate_next_value_, _missing_,
_numeric_repr_ (+ (3,13): _add_alias_, _add_value_alias_ — excluded at
py-version 3.12).

`visit_call` (dunder_methods.py:74-98) — UNDECORATED:
```
if (isinstance(node.func, nodes.Attribute)
        and node.func.attrname in self._dunder_methods
        and not self.within_dunder_or_lambda_def(node)
        and not (isinstance(node.func.expr, nodes.Call)
                 and isinstance(node.func.expr.func, nodes.Name)
                 and node.func.expr.func.name == "super")):
    inf_expr = safe_infer(node.func.expr)
    if not (inf_expr is None or isinstance(inf_expr, (Instance, UninferableBase))):
        return                       # dunder call on a non-instantiated class etc. → skip
    add_message("unnecessary-dunder-call", node=node,
                args=(node.func.attrname, self._dunder_methods[node.func.attrname]),
                confidence=HIGH)
```
- `within_dunder_or_lambda_def` (:53-65): walk PARENT chain; True if any
  ancestor is a FunctionDef whose name starts+ends with "__", or
  (is_lambda_rule_exception) any ancestor Lambda AND the CALLED dunder is in
  UNNECESSARY_DUNDER_CALL_LAMBDA_EXCEPTIONS (constants.py:261-282: __init__,
  __del__, __delattr__, __set__, __delete__, __setitem__, __delitem__,
  __iadd__, __isub__, __imul__, __imatmul__, __itruediv__, __ifloordiv__,
  __imod__, __ipow__, __ilshift__, __irshift__, __iand__, __ixor__, __ior__
  — read the tail of the list from constants.py:261+ to confirm the close).
  I.e. inside ANY dunder method body (any depth) → exempt; inside a lambda →
  exempt only for the statement-only dunders.
- `super().__init__()` exempt (syntactic super-call check).
- Inference gate: emit only if safe_infer(receiver) is None, Uninferable, or
  an Instance (incl. Const!). A ClassDef/Module receiver → skip (accessing
  base-class dunder through the class is legitimate).
- args = (dunder name, suggestion) → e.g. `Unnecessarily calls dunder method
  __len__. Use len built-in function.` (template adds the trailing period).
Report at Call, HIGH.

================================================================================
# 13. ellipsis_checker.py — W2301 unnecessary-ellipsis
================================================================================

Checker name "unnecessary_ellipsis" (ellipsis_checker.py:20-31).
```
@only_required_for_messages("unnecessary-ellipsis")
def visit_const(self, node):
    if (node.pytype() == "builtins.Ellipsis"
        and isinstance(node.parent, nodes.Expr)
        and ((isinstance(node.parent.parent, (nodes.ClassDef, nodes.FunctionDef))
              and node.parent.parent.doc_node)
             or len(node.parent.parent.body) > 1)):
        add_message("unnecessary-ellipsis", node=node)
```
Triggers for a bare `...` STATEMENT (Expr→Const Ellipsis) when EITHER
(a) directly inside a Class/FunctionDef that has a docstring, or
(b) its parent's parent has `body` longer than 1 — NB this clause applies to
ANY grandparent with a body (Module, If, For, While, With...): an `...`
statement alongside ≥1 sibling statements anywhere is flagged. Grandparents
without `.body` (e.g. TryExcept handler? handlers have body) — If/Module/
FunctionDef all have body; an `...` inside an `orelse` whose parent body has
1 element: `len(parent.body)` checks the BODY list only, not orelse — so
`if x: pass\nelse: ...` → If.body has 1 → (b) False → no message (quirk).
AsyncFunctionDef: isinstance check is (ClassDef, FunctionDef) —
AsyncFunctionDef SUBCLASSES FunctionDef in astroid → included. No args,
report at the Const node, UNDEFINED confidence.

================================================================================
# 14. lambda_expressions.py — C3001 / C3002
================================================================================

Checker name "lambda-expressions" (lambda_expressions.py:19-38).

C3001 unnecessary-lambda-assignment — visit_assign (:40-70) + visit_namedexpr
(:72-81):
```
match node:
    case nodes.Assign(targets=[nodes.AssignName(), *_], value=nodes.Lambda() as value):
        add_message("unnecessary-lambda-assignment", node=value, confidence=HIGH)
    case nodes.Assign(targets=[nodes.Tuple() as target, *_],
                      value=nodes.Tuple() | nodes.List() as value):
        for lhs_elem, rhs_elem in zip_longest(target.elts, value.elts):
            if lhs_elem is None or rhs_elem is None: break    # unbalanced → stop
            if isinstance(lhs_elem, nodes.AssignName) and isinstance(rhs_elem, nodes.Lambda):
                add_message("unnecessary-lambda-assignment", node=rhs_elem, confidence=HIGH)
# visit_namedexpr:
    case nodes.NamedExpr(target=nodes.AssignName(), value=nodes.Lambda() as value):
        add_message(..., node=value, confidence=HIGH)
```
Notes: first target only must be AssignName/Tuple (chained `a = b = lambda:`
matches via `[AssignName(), *_]`). Tuple-unpack case flags each
AssignName←Lambda pair until length mismatch. Report node = the LAMBDA
(fromlineno/col of the lambda expr). NB `nodes.Lambda` — FunctionDef
subclasses Lambda in astroid! But Assign.value can't be a FunctionDef, so
fine. AnnAssign (`f: Callable = lambda: 1`) NOT matched (visit_assign only
gets Assign). HIGH confidence, no args.

C3002 unnecessary-direct-lambda-call — visit_call (:83-90): `if
isinstance(node.func, nodes.Lambda)` → message at the CALL node, HIGH.
`(lambda x: x)(1)` → fires (func is the parenthesized Lambda).

================================================================================
# 15. non_ascii_names.py — NonAsciiNameChecker (C2401 C2403 W2402)
================================================================================

Checker name "NonASCII-Checker" (non_ascii_names.py:64). Messages
(:37-62): C2401 non-ascii-name (old_names [("C0144","old-non-ascii-name")]),
W2402 non-ascii-file-name, C2403 non-ascii-module-import.

`_check_name(node_type, name, node)` (:66-85):
```
if name is None: return
if not str(name).isascii():
    type_label = constants.HUMAN_READABLE_TYPES[node_type]
    args = (type_label.capitalize(), name)
    msg = {"file": "non-ascii-file-name", "module": "non-ascii-module-import"}.get(node_type, "non-ascii-name")
    add_message(msg, node=node, args=args, confidence=HIGH)
```
HUMAN_READABLE_TYPES (constants.py:64-81): file→"file", module→"module",
const→"constant", class→"class", function→"function", method→"method",
attr→"attribute", argument→"argument", variable→"variable",
class_attribute→"class attribute", class_const→"class constant",
inlinevar→"inline iteration", typevar→"type variable", ... (only file/
module/const/class/function/argument/attr/variable used by this checker).

Visit sites (each decorated only_required_for_messages as cited):
- visit_module (:87-89) ["non-ascii-name","non-ascii-file-name"]:
  `_check_name("file", node.name.split(".")[-1], node)` → W2402, args like
  ("File", "modnamé"). Reported at Module → line 1 col 0.
- visit_functiondef/asyncfunctiondef (:91-115) ["non-ascii-name"]:
  function name as "function" (label "Function"), then every
  posonlyarg/arg/kwonlyarg AssignName as "argument" (reported at the ARG
  node). NB vararg/kwarg (*args/**kw names) NOT checked (they're plain
  attrs, not AssignName children in astroid? — they are `arguments.vararg`
  strings; skipped by pylint).
- visit_global (:117-120): each name as "const" (label "Constant"), node =
  the Global stmt.
- visit_assignname (:122-144): frame dispatch —
  FunctionDef frame: only if `node.parent in frame.body` (direct statement
  of the function body — NOT nested in if/for! quirk) → "variable";
  ClassDef frame: "attr" (label "Attribute");
  else (module/comprehension/...): "variable".
- visit_classdef (:146-151): class name as "class"; then for each
  instance_attrs entry `attr` with NO `instance_attr_ancestors(attr)` (i.e.
  not inherited) → `_check_name("attr", attr, anodes[0])` at the first
  AssignAttr node. instance_attrs dict order = build order.
- visit_import / visit_importfrom (:153-164) ["non-ascii-name",
  "non-ascii-module-import"]: for each (module_name, alias): name = alias or
  module_name → "module" → C2403 at the import stmt.
- visit_call (:166-170) ["non-ascii-name"]: every keyword arg name
  (`keyword.arg`, None for ** → skipped by the None guard) as "argument",
  node = the Keyword node.
str.isascii() is the test — names are always str. Message ex.:
`Argument name "café" contains a non-ASCII character, consider renaming it.`
All HIGH confidence.

================================================================================
# 16. unsupported_version.py — W2601-W2606 (dead at default py-version)
================================================================================

Checker name "unsupported_version" (unsupported_version.py:27-70). open()
(:72-78): _py36_plus/_py38_plus/_py311_plus/_py312_plus from CONFIG
py-version. At the default (3,12) ALL flags are True → every check below
early-returns → no message can ever be emitted under default config.
Implement the gates for custom --py-version:

- W2601 using-f-string-in-unsupported-version — visit_joinedstr: if not
  _py36_plus → at JoinedStr, HIGH (:80-86).
- W2605 using-assignment-expression-... — visit_namedexpr: if not _py38_plus
  → at NamedExpr, HIGH (:88-95).
- W2606 using-positional-only-args-... — visit_arguments: if not _py38_plus
  and node.posonlyargs → at Arguments node, HIGH (:97-104). (Arguments has
  fromlineno of its function — astroid Arguments position quirk.)
- W2602 using-final-decorator-... — visit_decorators → _check_typing_final
  (:106-129): if _py38_plus return; collect decorators whose safe_infer
  qname == "typing.final"; `for decorator in decorators or
  uninferable_final_decorators(node)` (utils.py:894+ — syntactic
  `@typing.final`/`@final` matching via import lookup when inference fails)
  → message at each DECORATOR node, HIGH.
- W2603 using-exception-groups-...: visit_trystar (TryStar stmt) if not
  _py311_plus (:131-138); visit_excepthandler if handler.type is
  Name("ExceptionGroup") (:140-151); visit_raise if raising
  Call(Name("ExceptionGroup")) (:153-165). All at the visited node, HIGH.
- W2604 using-generic-type-syntax-...: visit_typealias / visit_typevar /
  visit_typevartuple (PEP 695 nodes) if not _py312_plus (:167-192), HIGH.

================================================================================
# 17. modified_iterating_checker.py — W4701 (E4702/E4703 → notes/08)
================================================================================

Checker name "modified_iteration" (modified_iterating_checker.py:22-52).
_LIST_MODIFIER_METHODS = {"append", "remove"}; _SET_MODIFIER_METHODS =
{"add", "clear", "discard", "pop", "remove"}.

visit_for (decorated for all three messages): for each body_node in
node.body → `_modified_iterating_check_on_node_and_children(body_node,
node.iter)` = check the node itself, then recurse into get_children()
(every descendant visited once, document order).

`_modified_iterating_check(node, iter_obj)` (:70-98), msg_id selection:
1. `Delete` node whose any target passes `_deleted_iteration_target_cond`
   (:180-194: target is DelName; iter_obj.parent is the For; the For target
   is AssignName/BaseContainer; the deleted name ∈
   find_assigned_names_recursive(for_target) — utils.py:2051+) → msg by
   `safe_infer(iter_obj)`: List→W4701, Dict→E4702, Set→E4703.
   (i.e. `for k in d: del k` — deleting the LOOP VARIABLE, quirky but
   that's the code.)
2. elif iter_obj not Name/Attribute → nothing.
3. elif `_modified_iterating_list_cond` → W4701:
   node is `Expr(Call(Attribute(expr=Name)))`
   (_is_node_expr_that_calls_attribute_name, :100-105);
   `safe_infer(node.value.func.expr)` is a nodes.List;
   `_common_cond_list_set` (:107-120): that inferred List ==
   safe_infer(iter_obj) (NODE EQUALITY — identity for nodes) AND
   `node.value.func.expr.name == iter_obj_name` (iter_obj.attrname if
   Attribute else .name);
   and attrname ∈ {"append","remove"}.
4. elif dict cond → E4702; elif set cond → E4703 (08).
Emission: `add_message(msg_id, node=node, args=(iter_obj.repr_name(),),
confidence=INFERENCE)` — node is the OFFENDING statement inside the loop
(the Expr or Delete), args = repr_name of the iterated expr (Name.name or
Attribute.attrname). Template: `Iterated list '%s' is being modified inside
for loop body, consider iterating through a copy of it instead.`
Conservatism: requires the iterable to safe-infer to a literal List AND the
syntactic name to match — `for x in get_list(): lst.append(..)` never fires.

================================================================================
# 18. match_statements_checker.py — R1905 / R1906 (E1901-E1904 → notes/08)
================================================================================

Checker name "match_statements". MATCH_CLASS_SELF_NAMES =
{builtins.bool, bytearray, bytes, dict, float, frozenset, int, list, set,
str, tuple} (match_statements_checker.py:24-36).

## 18.1 R1905 match-class-bind-self — visit_matchas (:124-142)

```
match node:
    case nodes.MatchAs(parent=nodes.MatchClass(cls=nodes.Name() as cls_name, patterns=[_]),
                       name=nodes.AssignName(name=name), pattern=None):
        inferred = safe_infer(cls_name)
        if isinstance(inferred, nodes.ClassDef) and inferred.qname() in MATCH_CLASS_SELF_NAMES:
            add_message("match-class-bind-self", node=node, args=(cls_name.name, name), confidence=HIGH)
```
Trigger: `case int(x):` — a MatchClass over a self-matching builtin with
EXACTLY ONE positional pattern which is a bare capture (MatchAs, no
sub-pattern). Suggestion args: (class-name-as-written, binding) →
`Use 'int() as x' instead`. Report at the MatchAs node, HIGH.

## 18.2 R1906 match-class-positional-attributes — visit_matchclass (:183-226)

```
attrs, dups = set(), set()
if node.patterns and (match_args := get_match_args_for_class(node.cls)) is not None:
    if len(node.patterns) > len(match_args): → E1903; return
    inferred = safe_infer(node.cls)
    if not (isinstance(inferred, nodes.ClassDef)
            and (inferred.qname() in MATCH_CLASS_SELF_NAMES or "tuple" in inferred.basenames)):
        attributes = [f"'{attr}'" for attr in match_args[: len(node.patterns)]]
        add_message("match-class-positional-attributes", node=node,
                    args=(", ".join(attributes),), confidence=INFERENCE)     # R1906
    for i in range(len(node.patterns)):
        check_duplicate_sub_patterns(match_args[i], node, attrs=attrs, dups=dups)  # E1904
for kw_name in node.kwd_attrs:
    check_duplicate_sub_patterns(kw_name, node, attrs=attrs, dups=dups)             # E1904
```
`get_match_args_for_class` (:144-166): safe_infer(cls) must be ClassDef;
getattr("__match_args__") — NotFoundError → ["<self>"] if self-matching
builtin else None; found: first assignment must be
AssignName←Assign(Tuple of Const-str) → list of values, else None.
R1906 fires for `case Point(x, y):` when Point has __match_args__
("x","y") and is neither a self-matching builtin nor a tuple subclass
(syntactic basenames check!). args = single string `'x', 'y'` →
`Use keyword attributes instead of positional ones ('x', 'y')`. Report at
MatchClass, INFERENCE. (namedtuples: basenames contain "tuple" only if
literally written; astroid's namedtuple brain builds bases with name
"tuple" → exempt as intended.)

================================================================================
# 19. misc.py — EncodingChecker (W0511) + ByIdManagedMessagesChecker (I0023)
================================================================================

## 19.1 W0511 fixme — EncodingChecker (BaseTokenChecker + BaseRawFileChecker)

Checker name "miscellaneous" (misc.py:53-102). Options:
- notes: csv, default ("FIXME", "XXX", "TODO")
- notes-rgx: string, default ""
- check-fixme-in-docstring: yn, default False

`open()` (misc.py:104-123):
```
notes = "|".join(re.escape(note) for note in cfg.notes)
if cfg.notes_rgx: notes += f"|{cfg.notes_rgx}"
self._comment_fixme_pattern = re.compile(rf"#\s*(?P<msg>({notes})(?=(:|\s|\Z)).*?$)", re.I)
# docstring patterns only used when check-fixme-in-docstring (default off):
self._docstring_fixme_pattern = re.compile(rf"((\"\"\")|(\'\'\'))\s*(?P<msg>({notes})(?=(:|\s|\Z)).*?)((\"\"\")|(\'\'\'))", re.I)
self._multiline_docstring_fixme_pattern = re.compile(rf"^\s*(?P<msg>({notes})(?=(:|\s|\Z)).*$)", re.I)
```
`process_tokens` (misc.py:150-180): if cfg.notes empty → return. For each
COMMENT token: `self._comment_fixme_pattern.match(token.string)` (MATCH =
anchored at "#") → `add_message("fixme", col_offset=token.start[1] + 1,
args=match.group("msg"), line=token.start[0])`.
- CASE-INSENSITIVE (re.I): `# todo x` fires.
- The tag must be followed by `:`, whitespace, or end (`(?=(:|\s|\Z))`) —
  `# TODOX` doesn't fire; `# TODO` (bare) fires.
- msg group = from the tag to end of the comment (non-greedy `.*?$` →
  matches to EOL; `$` without re.M = end of string) e.g. args
  `TODO: fix this` → message text is exactly that (template `%s`).
- col_offset = comment start col PLUS ONE (quirk; rendered col is +1 off).
- Comments after `#` with spaces: `#   FIXME` — `#\s*` consumes them.
Docstring path (default OFF): for STRING tokens —
`_is_multiline_docstring` (token is STRING, line starts with """/''' after
lstrip, token text contains newline before rstrip) → split lines, match
each with the multiline pattern, line = token.start[0] + line_no; else
match the single-line docstring pattern at token start. Both emit with
col_offset = token.start[1] + 1.

## 19.2 Encoding sweeps in process_module (misc.py:125-148)

For each raw line of the module stream: `line.decode(node.file_encoding or
"ascii")`; UnicodeDecodeError → silently pass; LookupError → if the line
starts with b"#" and contains "coding" and the encoding name → add_message
("syntax-error", line=lineno, args=f"Cannot decode using encoding
'{enc}', bad encoding"). Practically dead (a file with an unknown coding
declaration fails AST build first → E0001 from the build phase); port as
no-op with a comment, verify corpora.

## 19.3 I0023 use-symbolic-message-instead — ByIdManagedMessagesChecker

misc.py:22-50. msgs: I0023 ("%s", default_enabled: False → only with
explicit `--enable=use-symbolic-message-instead` / I0023).
process_module: for each ManagedMessage (mod_name, msgid, symbol, lineno,
is_disabled) in `linter._by_id_managed_msgs` (appended by the pragma
machinery whenever a pragma uses a NUMERIC id, notes/03): if mod_name ==
node.name → add_message("use-symbolic-message-instead", line=lineno,
args=f"'{msgid}' is cryptic: use '# pylint: {verb}={symbol}' instead")
with verb = "disable" if is_disabled else "enable". Then CLEARS the global
list. Port note: the list accumulates during pragma processing of the
CURRENT module (cleared each module by this checker when enabled; when the
checker is disabled the list still accumulates but
only_required_for_messages... — BaseRawFileChecker process_module is gated
by the checker's messages being enabled? Raw checkers run unconditionally;
the `add_message` is gated by enabledness. The clear happens only when the
checker RUNS — raw checkers always run their process_module; gating of raw
checkers: pylint only invokes raw/token checkers whose messages intersect
enabled set? — see notes/02 §raw-checker gating; replicate as: always run,
message suppressed when disabled).

================================================================================
# 20. symilar.py — SimilaritiesChecker (R0801 duplicate-code)
================================================================================

R0801 template: `Similar lines in %s files\n%s` (symilar.py:718-726).
Checker name "similarities", BaseRawFileChecker + Symilar (symilar.py:741+).
Options (:756-802): min-similarity-lines int default 4; ignore-comments yn
default True; ignore-docstrings yn default True; ignore-imports yn default
True; ignore-signatures yn default True. `reports = (("RP0801",
"Duplication", report_similarities),)` (stats table only — off by default).

Lifecycle:
- open(): `self.linesets = []`; stats.reset_duplicated_lines().
- process_module (per file, :821-839): `with node.stream() as stream:
  self.append_stream(self.linter.current_name, stream, node.file_encoding)`.
- close() (after the LAST module, :841-860): compute + emit (below).
- `min_similarity_lines == 0` disables (Symilar.run gate; in checker mode
  close() still runs `_compute_sims` — NB close() has NO ==0 gate! With
  min 0, hash_lineset(…, 0) → zip(*[]) → no chunks → no sims; fine).

## 20.1 append_stream / stripped_lines (symilar.py:359-390, 566-657)

`readlines` from the decoded stream (decoding_stream w/ file encoding);
UnicodeDecodeError → lines = []. Build
`LineSet(name=current_name, lines, ignore_comments, ignore_docstrings,
ignore_imports, ignore_signatures,
line_enabled_callback=self.linter._is_one_message_enabled)`.

`stripped_lines(lines, ...)`:
1. If ignore_imports or ignore_signatures (defaults on):
   `tree = astroid.parse("".join(lines))` — RE-PARSES the file (port: reuse
   our existing tree; the parse cannot fail for files that reached
   process_module). ignore_imports → collect line ranges
   `range(node.lineno, (node.end_lineno or node.lineno)+1)` of every
   Import/ImportFrom. ignore_signatures → `_get_functions` collects ALL
   FunctionDef/AsyncFunctionDef recursively (via .body chains of
   Module/ClassDef/FunctionDef); for each, ignore
   `range(func.lineno, func.body[0].lineno if func.body else func.tolineno+1)`
   — i.e. the `def` line through the line BEFORE the first body stmt
   (decorators NOT included — lineno is the def line in astroid).
2. Per line (1-based lineno):
   - `line_enabled_callback("R0801", lineno)` False → SKIP the line entirely
     (pragma-disabled regions don't participate in hashing).
   - strip whitespace.
   - ignore_docstrings (STATEFUL, crude): if not currently in a docstring
     and line starts with `"""`/`'''` → docstring = those 3 chars, line =
     line[3:]; elif starts with `r"""`/`r'''` → docstring = line[1:4],
     line = line[4:]. If in docstring: if line ends with the delimiter →
     docstring = None; line = "" (the closing line is blanked too).
     (NOT syntax-aware: any line beginning with triple quotes toggles.)
   - ignore_comments: `line = line.split("#", 1)[0].strip()` (NOT
     string-literal-aware: `x = "#"` → truncated! replicate).
   - lineno in ignore_lines → line = "".
   - non-empty result → append LineSpecifs(text=line, line_number=lineno-1)
     (ZERO-BASED line numbers stored).

## 20.2 hashing & matching (symilar.py:207-245, 248-288, 291-322, 467-548)

`hash_lineset(lineset, min_common_lines=4)`: for every window of 4
successive STRIPPED lines starting at stripped-index i:
LinesChunk(name, i, *4 lines) with `_hash = sum(hash(text) for text in
window)` (SUM of Python str hashes — PYTHONHASHSEED=0-dependent! For the
port: any deterministic hash works since hashes are only compared for
equality between identical text sequences; collisions across different
windows could in principle create false "common hashes", but equality of
LinesChunk is hash-only (`__eq__` compares _hash, symilar.py:127-130) —
SO HASH COLLISIONS PRODUCE FALSE POSITIVE MATCHES THAT ARE LATER FILTERED
ONLY BY filter_noncode_lines' text comparison... which compares texts and
counts equal pairs — a collision yields eff_cmn_nb < min → dropped. Use a
strong 64-bit hash; replicating CPython's siphash is unnecessary IF no
real-world collision changes output — sum-of-hashes collisions with
hashseed 0 are astronomically unlikely but technically possible; accept.)
index2lines[i] = SuccessiveLinesLimits(start=stripped[i].line_number,
end=stripped[i+4].line_number if exists else stripped[-1].line_number+1)
— both 0-based.

`_find_common(ls1, ls2)`: common hash keys, iterated
`sorted(common_hashes, key=attrgetter("_index"))` — NOTE the first
`sorted(hash_1 & hash_2, key=lambda m: hash_to_index_1[m][0])` result is
immediately re-sorted by `_index` (the chunk's OWN index attribute — whose
value comes from whichever set's element survived the `&`; set intersection
keeps elements from the LEFT operand hash_1 → _index is the index in ls1 —
actually frozenset & frozenset: CPython iterates the smaller set and keeps
elements from... the result contains elements from the FIRST operand when
sizes allow; replicate "chunk objects from ls1"). For each common hash:
cartesian product of start-indices in both files → all_couples[(i1,i2)] =
CplSuccessiveLinesLimits(copy of lines-limits1, limits2,
effective_cmn_lines_nb=4).
`remove_successive(all_couples)` (:248-288): for each key (i1,i2) in
INSERTION ORDER, while (i1+1,i2+1) exists: extend ends, effective += 1,
delete successor. Result: maximal runs.
Then per remaining couple: `eff_cmn_nb = filter_noncode_lines(ls1, i1, ls2,
i2, nb_common_lines)` (:291-322: count pairwise-equal stripped texts among
the window's lines that match `.*\w+` — lines without word chars, e.g. `)`,
are excluded BEFORE pairing, separately per file, then zip) ; yield the
Commonality only `if eff_cmn_nb > self.namespace.min_similarity_lines`
(STRICTLY greater → an exact 4-stripped-line duplicate is NOT reported;
minimum reportable run is 5 stripped code lines at default).

`_iter_sims`: ordered pairs (i<j) of linesets in APPEND ORDER (= module
check order). `_compute_sims` (:398-433): dedupe — per num, a list of
2-element couple-sets; a new couple whose (lineset,start,end) appears in ANY
existing set for that num is DROPPED (not merged!) → 3-way duplications
collapse to ONE pair message (len(couples) is always 2 in practice).
`return sorted(sims, reverse=True)` — tuples (num, set); primary key num
DESC; ties: set.__lt__ is subset-comparison → incomparable sets are stable
(insertion order = num-bucket then discovery order).

## 20.3 close() emission (symilar.py:841-860)

```
total = sum(len(lineset) for lineset in self.linesets)    # len = REAL line count
duplicated = 0
for num, couples in self._compute_sims():
    msg = []
    lineset = start_line = end_line = None
    for lineset, start_line, end_line in couples:          # SET iteration order (id-hash!)
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
- Message body: sorted `==module.name:[start:end]` lines (0-BASED start,
  end exclusive — they print as e.g. `==pkg.mod:[10:16]`), followed by the
  rstripped REAL lines of the chunk — taken from whichever couple element
  was iterated LAST out of the SET. LineSet.__hash__ = id(self)
  (symilar.py:701-702) → set order depends on object addresses →
  FORMALLY NONDETERMINISTIC which file's real lines are printed (they can
  differ in comments/whitespace). Empirically stable per run; compare
  against ground truth and pin to "second-appended lineset" if diffs show
  that (open Q §24). Empty lines render as "" (the f-string branch in
  _get_similarity_report is stdout-only; close() rstrips only).
- `add_message("R0801", ...)` — by MSGID, no node, no line →
  reported at line 1 col 0 (line None → `line or 1`) of the CURRENT module
  at close time = the LAST checked module. All R0801 messages therefore
  appear under the LAST module's header, AFTER all per-module messages.
  Enablement check at emission: `is_message_enabled("R0801", line=None)` →
  config-level state only (line-level pragmas act via the
  line_enabled_callback exclusion instead, plus block-level disables make
  lines drop out).
- Stats: duplicated lines/percent accumulate into LinterStats → affect the
  SCORE via the formula's warning count? No — R0801 messages count as R
  (refactor) in the score; nb_duplicated_lines only feeds RP0801 table.

================================================================================
# 21. Remaining checker files with nothing (more) to port
================================================================================

- `async_checker.py` — E1700 (+ uses CONTEXT manager checks E1701 via
  typecheck shared utils): all E, notes/08.
- `dataclass_checker.py` — E3701 only (08).
- `method_args.py` — W3101/E3102: 09-variables-imports-classes-wc §5.
- `unicode.py` — E2501/E2502/C2503/E2510-15: ported (PROGRESS; spec in 02).
- `raw_metrics.py` — NO messages (token stats for RP-reports only; reports
  disabled by default → no-op for output; stats irrelevant to score).
- `clear_lru_cache.py` — not a checker (cache-clearing utility; no register
  of messages — it registers nothing message-bearing).
- `base_checker.py`, `utils.py`, `deprecated.py` — infrastructure.
- `symilar.py` Run()/CLI — out of scope.
- `design_analysis.py` (R0901-R0917), `format.py` (C03xx/W0301/W0311),
  `refactoring/` (R17xx, C0117, C0200-C0209, C1802-C1805) — DEFAULT-LOADED
  and REQUIRED for full mode but NOT yet spec'd by any notes doc (§24).

================================================================================
# 22. MASTER COVERAGE TABLE — all 389 messages in crates/pycheckers/src/msgs.rs
================================================================================

Columns: msgid | symbol | owning checker file (under pylint/checkers/ unless
noted) | ON = enabled by default in FULL mode (n = default_enabled False;
dead = not emittable at py-version (3,12) via may_be_emitted) | -E = enabled
under the `-E --disable=<flags>` config (msgs.rs `enabled`) | spec doc.
Doc keys: 02=02-pipeline-output, 03=03-message-control, 05=05-variables,
06=06-typecheck, 07=07-inference, 08=08-other-checkers, 9B=09-basic-wc,
9V=09-variables-imports-classes-wc, 9M=THIS DOC, PEND=no spec doc yet
(format.py / refactoring/ / design_analysis.py / E0110 — see §24).

## 22.0 Extensions excluded (NOT loaded by default; none appear in msgs.rs)

bad_builtin W0141 · broad_try_clause W0717 · check_elif R5501 · code_style
R6101-R6106 · comparison_placement C0122 C2201 · confusing_elif R5601 ·
consider_refactoring_into_while_condition R3501 · consider_ternary_expression
W0160 · dict_init_mutate C3401 · docparams W9003-W9021 · docstyle C0198
C0199 · dunder W3201 (bad-dunder-name) · empty_comment R2044 ·
eq_without_hash W1641 · for_any_all C0501 · magic_value R2004 · mccabe R1260
· no_self_use R0201/R6301 · overlapping_exceptions W0714 · private_import
C2701 · redefined_loop_name W2901 · redefined_variable_type R0204 ·
set_membership R6201 · typing E6004 E6005 R6002 R6003 R6006 R6007 W6001 ·
while_used W0149. OUT OF SCOPE unless --load-plugins is ever targeted.

## 22.1 C messages (50)

| id | symbol | owner | ON | -E | doc |
|---|---|---|---|---|---|
| C0103 | invalid-name | base/name_checker | Y | – | 9B |
| C0104 | disallowed-name | base/name_checker | Y | – | 9B |
| C0105 | typevar-name-incorrect-variance | base/name_checker | Y | – | 9B |
| C0112 | empty-docstring | base/docstring_checker | Y | – | 9B |
| C0114 | missing-module-docstring | base/docstring_checker | Y | – | 9B |
| C0115 | missing-class-docstring | base/docstring_checker | Y | – | 9B |
| C0116 | missing-function-docstring | base/docstring_checker | Y | – | 9B |
| C0117 | unnecessary-negation | refactoring/not_checker | Y | – | PEND |
| C0121 | singleton-comparison | base/comparison_checker | Y | – | 9B |
| C0123 | unidiomatic-typecheck | base/comparison_checker | Y | – | 9B |
| C0131 | typevar-double-variance | base/name_checker | Y | – | 9B |
| C0132 | typevar-name-mismatch | base/name_checker | Y | – | 9B |
| C0200 | consider-using-enumerate | refactoring/recommendation | Y | – | PEND |
| C0201 | consider-iterating-dictionary | refactoring/recommendation | Y | – | PEND |
| C0202 | bad-classmethod-argument | classes/class_checker | Y | – | 9V |
| C0203 | bad-mcs-method-argument | classes/class_checker | Y | – | 9V |
| C0204 | bad-mcs-classmethod-argument | classes/class_checker | Y | – | 9V |
| C0205 | single-string-used-for-slots | classes/class_checker | Y | – | 9V |
| C0206 | consider-using-dict-items | refactoring/recommendation | Y | – | PEND |
| C0207 | use-maxsplit-arg | refactoring/recommendation | Y | – | PEND |
| C0208 | use-sequence-for-iteration | refactoring/recommendation | Y | – | PEND |
| C0209 | consider-using-f-string | refactoring/recommendation | Y | – | PEND |
| C0301 | line-too-long | format.py | Y | – | PEND |
| C0302 | too-many-lines | format.py | Y | – | PEND |
| C0303 | trailing-whitespace | format.py | Y | – | PEND |
| C0304 | missing-final-newline | format.py | Y | – | PEND |
| C0305 | trailing-newlines | format.py | Y | – | PEND |
| C0321 | multiple-statements | format.py | Y | – | PEND |
| C0325 | superfluous-parens | format.py | Y | – | PEND |
| C0327 | mixed-line-endings | format.py | Y | – | PEND |
| C0328 | unexpected-line-ending-format | format.py | Y | – | PEND |
| C0401 | wrong-spelling-in-comment | spelling.py | Y(inert) | – | 9M §8 |
| C0402 | wrong-spelling-in-docstring | spelling.py | Y(inert) | – | 9M §8 |
| C0403 | invalid-characters-in-docstring | spelling.py | Y(inert) | – | 9M §8 |
| C0410 | multiple-imports | imports.py | Y | – | 9V |
| C0411 | wrong-import-order | imports.py | Y | – | 9V |
| C0412 | ungrouped-imports | imports.py | Y | – | 9V |
| C0413 | wrong-import-position | imports.py | Y | – | 9V |
| C0414 | useless-import-alias | imports.py | Y | – | 9V |
| C0415 | import-outside-toplevel | imports.py | Y | – | 9V |
| C1802 | use-implicit-booleaness-not-len | refactoring/implicit_booleaness | Y | – | PEND |
| C1803 | use-implicit-booleaness-not-comparison | refactoring/implicit_booleaness | Y | – | PEND |
| C1804 | use-implicit-booleaness-not-comparison-to-string | refactoring/implicit_booleaness | n | – | PEND |
| C1805 | use-implicit-booleaness-not-comparison-to-zero | refactoring/implicit_booleaness | n | – | PEND |
| C2401 | non-ascii-name | non_ascii_names.py | Y | – | 9M §15 |
| C2403 | non-ascii-module-import | non_ascii_names.py | Y | – | 9M §15 |
| C2503 | bad-file-encoding | unicode.py | Y | – | 02 (ported) |
| C2801 | unnecessary-dunder-call | dunder_methods.py | Y | – | 9M §12 |
| C3001 | unnecessary-lambda-assignment | lambda_expressions.py | Y | – | 9M §14 |
| C3002 | unnecessary-direct-lambda-call | lambda_expressions.py | Y | – | 9M §14 |

## 22.2 E messages (130)

| id | symbol | owner | ON | -E | doc |
|---|---|---|---|---|---|
| E0001 | syntax-error | pylinter | Y | E | 02 |
| E0011 | unrecognized-inline-option | pylinter | Y | E | 03 |
| E0013 | bad-plugin-value | pylinter | Y | E | 02 |
| E0014 | bad-configuration-section | pylinter | Y | E | 02 |
| E0015 | unrecognized-option | pylinter | Y | E | 02 |
| E0100 | init-is-generator | base/basic_error | Y | E | 08 |
| E0101 | return-in-init | base/basic_error | Y | E | 08 |
| E0102 | function-redefined | base/basic_error | Y | E | 08 |
| E0103 | not-in-loop | base/basic_error | Y | E | 08 |
| E0104 | return-outside-function | base/basic_error | Y | E | 08 |
| E0105 | yield-outside-function | base/basic_error | Y | E | 08 |
| E0106 | return-arg-in-generator | base/basic_error | dead | – | 08 (never emits) |
| E0107 | nonexistent-operator | base/basic_error | Y | E | 08 |
| E0108 | duplicate-argument-name | base/basic_error | Y | E | 08 |
| E0110 | abstract-class-instantiated | base/basic_error | Y | – | PEND (GAP: excluded from 08) |
| E0111 | bad-reversed-sequence | base/basic_checker | Y | E | 08 |
| E0112 | too-many-star-expressions | base/basic_error | Y | E | 08 |
| E0113 | invalid-star-assignment-target | base/basic_error | Y | E | 08 |
| E0114 | star-needs-assignment-target | base/basic_error | Y | E | 08 |
| E0115 | nonlocal-and-global | base/basic_error | Y | E | 08 |
| E0117 | nonlocal-without-binding | base/basic_error | Y | E | 08 |
| E0118 | used-prior-global-declaration | base/basic_error | Y | E | 08 |
| E0119 | misplaced-format-function | base/basic_checker | Y | E | 08 |
| E0202 | method-hidden | classes/class_checker | Y | E | 08 |
| E0203 | access-member-before-definition | classes/class_checker | Y | E | 08 |
| E0211 | no-method-argument | classes/class_checker | Y | E | 08 |
| E0213 | no-self-argument | classes/class_checker | Y | E | 08 |
| E0236 | invalid-slots-object | classes/class_checker | Y | E | 08 |
| E0237 | assigning-non-slot | classes/class_checker | Y | E | 08 |
| E0238 | invalid-slots | classes/class_checker | Y | E | 08 |
| E0239 | inherit-non-class | classes/class_checker | Y | E | 08 |
| E0240 | inconsistent-mro | classes/class_checker | Y | E | 08 |
| E0241 | duplicate-bases | classes/class_checker | Y | E | 08 |
| E0242 | class-variable-slots-conflict | classes/class_checker | Y | E | 08 |
| E0243 | invalid-class-object | classes/class_checker | Y | E | 08 |
| E0244 | invalid-enum-extension | classes/class_checker | Y | E | 08 |
| E0245 | declare-non-slot | classes/class_checker | Y | E | 08 |
| E0301 | non-iterator-returned | classes/special_methods | Y | E | 08 |
| E0302 | unexpected-special-method-signature | classes/special_methods | Y | E | 08 |
| E0303 | invalid-length-returned | classes/special_methods | Y | E | 08 |
| E0304 | invalid-bool-returned | classes/special_methods | Y | E | 08 |
| E0305 | invalid-index-returned | classes/special_methods | Y | E | 08 |
| E0306 | invalid-repr-returned | classes/special_methods | Y | E | 08 |
| E0307 | invalid-str-returned | classes/special_methods | Y | E | 08 |
| E0308 | invalid-bytes-returned | classes/special_methods | Y | E | 08 |
| E0309 | invalid-hash-returned | classes/special_methods | Y | E | 08 |
| E0310 | invalid-length-hint-returned | classes/special_methods | Y | E | 08 |
| E0311 | invalid-format-returned | classes/special_methods | Y | E | 08 |
| E0312 | invalid-getnewargs-returned | classes/special_methods | Y | E | 08 |
| E0313 | invalid-getnewargs-ex-returned | classes/special_methods | Y | E | 08 |
| E0401 | import-error | imports.py | Y | – | 9V §2.16 (partial) + 07; flag §24 |
| E0402 | relative-beyond-top-level | imports.py | Y | E | 08 |
| E0601 | used-before-assignment | variables.py | Y | E | 05 |
| E0602 | undefined-variable | variables.py | Y | E | 05 |
| E0603 | undefined-all-variable | variables.py | Y | E | 05 |
| E0604 | invalid-all-object | variables.py | Y | E | 05 |
| E0605 | invalid-all-format | variables.py | Y | E | 05 |
| E0606 | possibly-used-before-assignment | variables.py | Y | E | 05 |
| E0611 | no-name-in-module | variables.py | Y | – | 05 |
| E0633 | unpacking-non-sequence | variables.py | Y | E | 05 |
| E0643 | potential-index-error | variables.py | Y | E | 05 |
| E0701 | bad-except-order | exceptions.py | Y | E | 08 |
| E0702 | raising-bad-type | exceptions.py | Y | E | 08 |
| E0704 | misplaced-bare-raise | exceptions.py | Y | E | 08 |
| E0705 | bad-exception-cause | exceptions.py | Y | E | 08 |
| E0710 | raising-non-exception | exceptions.py | Y | E | 08 |
| E0711 | notimplemented-raised | exceptions.py | Y | E | 08 |
| E0712 | catching-non-exception | exceptions.py | Y | E | 08 |
| E1003 | bad-super-call | newstyle.py | Y | E | 9M §7 |
| E1101 | no-member | typecheck.py | Y | – | 9M §1.7 |
| E1102 | not-callable | typecheck.py | Y | E | 06 |
| E1111 | assignment-from-no-return | typecheck.py | Y | E | 06 |
| E1120 | no-value-for-parameter | typecheck.py | Y | E | 06 |
| E1121 | too-many-function-args | typecheck.py | Y | E | 06 |
| E1123 | unexpected-keyword-arg | typecheck.py | Y | E | 06 |
| E1124 | redundant-keyword-arg | typecheck.py | Y | E | 06 |
| E1125 | missing-kwoa | typecheck.py | Y | E | 06 |
| E1126 | invalid-sequence-index | typecheck.py | Y | E | 06 |
| E1127 | invalid-slice-index | typecheck.py | Y | E | 06 |
| E1128 | assignment-from-none | typecheck.py | Y | E | 06 |
| E1129 | not-context-manager | typecheck.py | Y | E | 06 |
| E1130 | invalid-unary-operand-type | typecheck.py | Y | E | 06 |
| E1131 | unsupported-binary-operation | typecheck.py | Y | E | 06 |
| E1132 | repeated-keyword | typecheck.py | Y | E | 06 |
| E1133 | not-an-iterable | typecheck.py | Y | E | 06 |
| E1134 | not-a-mapping | typecheck.py | Y | E | 06 |
| E1135 | unsupported-membership-test | typecheck.py | Y | E | 06 |
| E1136 | unsubscriptable-object | typecheck.py | Y | E | 06 |
| E1137 | unsupported-assignment-operation | typecheck.py | Y | E | 06 |
| E1138 | unsupported-delete-operation | typecheck.py | Y | E | 06 |
| E1139 | invalid-metaclass | typecheck.py | Y | E | 06 |
| E1141 | dict-iter-missing-items | typecheck.py | Y | E | 06 |
| E1142 | await-outside-async | typecheck.py | Y | E | 06 |
| E1143 | unhashable-member | typecheck.py | Y | E | 06 |
| E1144 | invalid-slice-step | typecheck.py | Y | E | 06 |
| E1145 | async-context-manager-with-regular-with | typecheck.py | Y | E | 06 |
| E1200 | logging-unsupported-format | logging.py | Y | E | 08 §7 |
| E1201 | logging-format-truncated | logging.py | Y | E | 08 §7 |
| E1205 | logging-too-many-args | logging.py | Y | E | 08 §7 |
| E1206 | logging-too-few-args | logging.py | Y | E | 08 §7 |
| E1300 | bad-format-character | strings.py | Y | E | 08 §6 |
| E1301 | truncated-format-string | strings.py | Y | E | 08 §6 |
| E1302 | mixed-format-string | strings.py | Y | E | 08 §6 |
| E1303 | format-needs-mapping | strings.py | Y | E | 08 §6 |
| E1304 | missing-format-string-key | strings.py | Y | E | 08 §6 |
| E1305 | too-many-format-args | strings.py | Y | E | 08 §6 + 9M §4.2 |
| E1306 | too-few-format-args | strings.py | Y | E | 08 §6 + 9M §4.2 |
| E1307 | bad-string-format-type | strings.py | Y | E | 08 §6 |
| E1310 | bad-str-strip-call | strings.py | Y | E | 08 §6 |
| E1507 | invalid-envvar-value | stdlib.py | Y | E | 08 §13 + 9M §3.7 |
| E1519 | singledispatch-method | stdlib.py | Y | E | 08 §13 |
| E1520 | singledispatchmethod-function | stdlib.py | Y | E | 08 §13 |
| E1700 | yield-inside-async-function | async_checker.py | Y | E | 08 |
| E1701 | not-async-context-manager | async_checker.py | Y | E | 08 |
| E1901 | bare-name-capture-pattern | match_statements | Y | E | 08 |
| E1902 | invalid-match-args-definition | match_statements | Y | E | 08 |
| E1903 | too-many-positional-sub-patterns | match_statements | Y | E | 08 |
| E1904 | multiple-class-sub-patterns | match_statements | Y | E | 08 |
| E2501 | invalid-unicode-codec | unicode.py | Y | E | 02 (ported) |
| E2502 | bidirectional-unicode | unicode.py | Y | E | 02 (ported) |
| E2510 | invalid-character-backspace | unicode.py | Y | E | 02 (ported) |
| E2511 | invalid-character-carriage-return | unicode.py | Y | E | 02 (ported) |
| E2512 | invalid-character-sub | unicode.py | Y | E | 02 (ported) |
| E2513 | invalid-character-esc | unicode.py | Y | E | 02 (ported) |
| E2514 | invalid-character-nul | unicode.py | Y | E | 02 (ported) |
| E2515 | invalid-character-zero-width-space | unicode.py | Y | E | 02 (ported) |
| E3102 | positional-only-arguments-expected | method_args.py | Y | E | 08 + 9V §5.2 |
| E3701 | invalid-field-call | dataclass_checker.py | Y | E | 08 |
| E4702 | modified-iterating-dict | modified_iterating | Y | E | 08 + 9M §17 |
| E4703 | modified-iterating-set | modified_iterating | Y | E | 08 + 9M §17 |

## 22.3 F / I messages (14)

| id | symbol | owner | ON | -E | doc |
|---|---|---|---|---|---|
| F0001 | fatal | pylinter | Y | E | 02 |
| F0002 | astroid-error | pylinter | Y | E | 02 |
| F0010 | parse-error | pylinter | Y | E | 02 |
| F0011 | config-parse-error | pylinter | Y | E | 02 |
| F0202 | method-check-failed | classes/class_checker | Y | E | 08 |
| I0001 | raw-checker-failed | pylinter | n | – | 02 |
| I0010 | bad-inline-option | pylinter | n | – | 03 |
| I0011 | locally-disabled | pylinter | n | – | 03 |
| I0013 | file-ignored | pylinter | n | – | 03 |
| I0020 | suppressed-message | pylinter | n | – | 03 |
| I0021 | useless-suppression | pylinter | n | – | 03 |
| I0022 | deprecated-pragma | pylinter | n | – | 03 |
| I0023 | use-symbolic-message-instead | misc.py | n | – | 9M §19.3 |
| I1101 | c-extension-no-member | typecheck.py | Y | – | 9M §1.7 |

## 22.4 R messages (61)

| id | symbol | owner | ON | -E | doc |
|---|---|---|---|---|---|
| R0022 | useless-option-value | pylinter | Y | – | 03 |
| R0123 | literal-comparison | base/comparison_checker | Y | – | 9B |
| R0124 | comparison-with-itself | base/comparison_checker | Y | – | 9B |
| R0133 | comparison-of-constants | base/comparison_checker | Y | – | 9B |
| R0202 | no-classmethod-decorator | classes/class_checker | Y | – | 9V |
| R0203 | no-staticmethod-decorator | classes/class_checker | Y | – | 9V |
| R0205 | useless-object-inheritance | classes/class_checker | Y | – | 9V |
| R0206 | property-with-parameters | classes/class_checker | Y | – | 9V |
| R0401 | cyclic-import | imports.py | Y | – | 9V |
| R0402 | consider-using-from-import | imports.py | Y | – | 9V |
| R0801 | duplicate-code | symilar.py | Y | – | 9M §20 |
| R0901 | too-many-ancestors | design_analysis.py | Y | – | PEND |
| R0902 | too-many-instance-attributes | design_analysis.py | Y | – | PEND |
| R0903 | too-few-public-methods | design_analysis.py | Y | – | PEND |
| R0904 | too-many-public-methods | design_analysis.py | Y | – | PEND |
| R0911 | too-many-return-statements | design_analysis.py | Y | – | PEND |
| R0912 | too-many-branches | design_analysis.py | Y | – | PEND |
| R0913 | too-many-arguments | design_analysis.py | Y | – | PEND |
| R0914 | too-many-locals | design_analysis.py | Y | – | PEND |
| R0915 | too-many-statements | design_analysis.py | Y | – | PEND |
| R0916 | too-many-boolean-expressions | design_analysis.py | Y | – | PEND |
| R0917 | too-many-positional-arguments | design_analysis.py | Y | – | PEND |
| R1701 | consider-merging-isinstance | refactoring/refactoring_checker | Y | – | PEND |
| R1702 | too-many-nested-blocks | refactoring/refactoring_checker | Y | – | PEND |
| R1703 | simplifiable-if-statement | refactoring/refactoring_checker | Y | – | PEND |
| R1704 | redefined-argument-from-local | refactoring/refactoring_checker | Y | – | PEND |
| R1705 | no-else-return | refactoring/refactoring_checker | Y | – | PEND |
| R1706 | consider-using-ternary | refactoring/refactoring_checker | Y | – | PEND |
| R1707 | trailing-comma-tuple | refactoring/refactoring_checker | Y | – | PEND |
| R1708 | stop-iteration-return | refactoring/refactoring_checker | Y | – | PEND |
| R1709 | simplify-boolean-expression | refactoring/refactoring_checker | Y | – | PEND |
| R1710 | inconsistent-return-statements | refactoring/refactoring_checker | Y | – | PEND |
| R1711 | useless-return | refactoring/refactoring_checker | Y | – | PEND |
| R1712 | consider-swap-variables | refactoring/refactoring_checker | Y | – | PEND |
| R1713 | consider-using-join | refactoring/refactoring_checker | Y | – | PEND |
| R1714 | consider-using-in | refactoring/refactoring_checker | Y | – | PEND |
| R1715 | consider-using-get | refactoring/refactoring_checker | Y | – | PEND |
| R1716 | chained-comparison | refactoring/refactoring_checker | Y | – | PEND |
| R1717 | consider-using-dict-comprehension | refactoring/refactoring_checker | Y | – | PEND |
| R1718 | consider-using-set-comprehension | refactoring/refactoring_checker | Y | – | PEND |
| R1719 | simplifiable-if-expression | refactoring/refactoring_checker | Y | – | PEND |
| R1720 | no-else-raise | refactoring/refactoring_checker | Y | – | PEND |
| R1721 | unnecessary-comprehension | refactoring/refactoring_checker | Y | – | PEND |
| R1722 | consider-using-sys-exit | refactoring/refactoring_checker | Y | – | PEND |
| R1723 | no-else-break | refactoring/refactoring_checker | Y | – | PEND |
| R1724 | no-else-continue | refactoring/refactoring_checker | Y | – | PEND |
| R1725 | super-with-arguments | refactoring/refactoring_checker | Y | – | PEND |
| R1726 | simplifiable-condition | refactoring/refactoring_checker | Y | – | PEND |
| R1727 | condition-evals-to-constant | refactoring/refactoring_checker | Y | – | PEND |
| R1728 | consider-using-generator | refactoring/refactoring_checker | Y | – | PEND |
| R1729 | use-a-generator | refactoring/refactoring_checker | Y | – | PEND |
| R1730 | consider-using-min-builtin | refactoring/refactoring_checker | Y | – | PEND |
| R1731 | consider-using-max-builtin | refactoring/refactoring_checker | Y | – | PEND |
| R1732 | consider-using-with | refactoring/refactoring_checker | Y | – | PEND |
| R1733 | unnecessary-dict-index-lookup | refactoring/refactoring_checker | Y | – | PEND |
| R1734 | use-list-literal | refactoring/refactoring_checker | Y | – | PEND |
| R1735 | use-dict-literal | refactoring/refactoring_checker | Y | – | PEND |
| R1736 | unnecessary-list-index-lookup | refactoring/refactoring_checker | Y | – | PEND |
| R1737 | use-yield-from | refactoring/refactoring_checker | Y | – | PEND |
| R1905 | match-class-bind-self | match_statements | Y | – | 9M §18.1 |
| R1906 | match-class-positional-attributes | match_statements | Y | – | 9M §18.2 |

## 22.5 W messages (134)

| id | symbol | owner | ON | -E | doc |
|---|---|---|---|---|---|
| W0012 | unknown-option-value | pylinter | Y | – | 03 |
| W0101 | unreachable | base/basic_checker | Y | – | 9B |
| W0102 | dangerous-default-value | base/basic_checker | Y | – | 9B |
| W0104 | pointless-statement | base/basic_checker | Y | – | 9B |
| W0105 | pointless-string-statement | base/basic_checker | Y | – | 9B |
| W0106 | expression-not-assigned | base/basic_checker | Y | – | 9B |
| W0107 | unnecessary-pass | base/pass_checker | Y | – | 9B |
| W0108 | unnecessary-lambda | base/basic_checker | Y | – | 9B |
| W0109 | duplicate-key | base/basic_checker | Y | – | 9B |
| W0120 | useless-else-on-loop | base/basic_error | Y | – | 9B |
| W0122 | exec-used | base/basic_checker | Y | – | 9B |
| W0123 | eval-used | base/basic_checker | Y | – | 9B |
| W0124 | confusing-with-statement | base/basic_checker | Y | – | 9B |
| W0125 | using-constant-test | base/basic_checker | Y | – | 9B |
| W0126 | missing-parentheses-for-call-in-test | base/basic_checker | Y | – | 9B |
| W0127 | self-assigning-variable | base/basic_checker | Y | – | 9B |
| W0128 | redeclared-assigned-name | base/basic_checker | Y | – | 9B |
| W0129 | assert-on-string-literal | base/basic_checker | Y | – | 9B |
| W0130 | duplicate-value | base/basic_checker | Y | – | 9B |
| W0131 | named-expr-without-context | base/basic_checker | Y | – | 9B |
| W0133 | pointless-exception-statement | base/basic_checker | Y | – | 9B |
| W0134 | return-in-finally | base/basic_checker | Y | – | 9B |
| W0135 | contextmanager-generator-missing-cleanup | base/function_checker | Y | – | 9B |
| W0136 | continue-in-finally | base/basic_error | Y | – | 9B |
| W0137 | break-in-finally | base/basic_error | Y | – | 9B |
| W0143 | comparison-with-callable | base/comparison_checker | Y | – | 9B |
| W0150 | lost-exception | base/basic_checker | Y | – | 9B |
| W0177 | nan-comparison | base/comparison_checker | Y | – | 9B |
| W0199 | assert-on-tuple | base/basic_checker | Y | – | 9B |
| W0201 | attribute-defined-outside-init | classes/class_checker | Y | – | 9V |
| W0211 | bad-staticmethod-argument | classes/class_checker | Y | – | 9V |
| W0212 | protected-access | classes/class_checker | Y | – | 9V |
| W0213 | implicit-flag-alias | classes/class_checker | Y | – | 9V |
| W0221 | arguments-differ | classes/class_checker | Y | – | 9V |
| W0222 | signature-differs | classes/class_checker | Y | – | 9V |
| W0223 | abstract-method | classes/class_checker | Y | – | 9V |
| W0231 | super-init-not-called | classes/class_checker | Y | – | 9V |
| W0233 | non-parent-init-called | classes/class_checker | Y | – | 9V |
| W0236 | invalid-overridden-method | classes/class_checker | Y | – | 9V |
| W0237 | arguments-renamed | classes/class_checker | Y | – | 9V |
| W0238 | unused-private-member | classes/class_checker | Y | – | 9V |
| W0239 | overridden-final-method | classes/class_checker | Y | – | 9V |
| W0240 | subclassed-final-class | classes/class_checker | Y | – | 9V |
| W0244 | redefined-slots-in-subclass | classes/class_checker | Y | – | 9V |
| W0245 | super-without-brackets | classes/class_checker | Y | – | 9V |
| W0246 | useless-parent-delegation | classes/class_checker | Y | – | 9V |
| W0301 | unnecessary-semicolon | format.py | Y | – | PEND |
| W0311 | bad-indentation | format.py | Y | – | PEND |
| W0401 | wildcard-import | imports.py | Y | – | 9V |
| W0404 | reimported | imports.py | Y | – | 9V |
| W0406 | import-self | imports.py | Y | – | 9V |
| W0407 | preferred-module | imports.py | Y | – | 9V |
| W0410 | misplaced-future | imports.py | Y | – | 9V |
| W0416 | shadowed-import | imports.py | Y | – | 9V |
| W0511 | fixme | misc.py | Y | – | 9M §19.1 |
| W0601 | global-variable-undefined | variables.py | Y | – | 9V |
| W0602 | global-variable-not-assigned | variables.py | Y | – | 9V |
| W0603 | global-statement | variables.py | Y | – | 9V |
| W0604 | global-at-module-level | variables.py | Y | – | 9V |
| W0611 | unused-import | variables.py | Y | – | 9V |
| W0612 | unused-variable | variables.py | Y | – | 9V |
| W0613 | unused-argument | variables.py | Y | – | 9V |
| W0614 | unused-wildcard-import | variables.py | Y | – | 9V |
| W0621 | redefined-outer-name | variables.py | Y | – | 9V |
| W0622 | redefined-builtin | variables.py | Y | – | 9V |
| W0631 | undefined-loop-variable | variables.py | Y | – | 9V |
| W0632 | unbalanced-tuple-unpacking | variables.py | Y | – | 9V |
| W0640 | cell-var-from-loop | variables.py | Y | – | 9V |
| W0641 | possibly-unused-variable | variables.py | Y | – | 9V |
| W0642 | self-cls-assignment | variables.py | Y | – | 9V |
| W0644 | unbalanced-dict-unpacking | variables.py | Y | – | 9V |
| W0702 | bare-except | exceptions.py | Y | – | 9V |
| W0705 | duplicate-except | exceptions.py | Y | – | 9V |
| W0706 | try-except-raise | exceptions.py | Y | – | 9V |
| W0707 | raise-missing-from | exceptions.py | Y | – | 9V |
| W0711 | binary-op-exception | exceptions.py | Y | – | 9V |
| W0715 | raising-format-tuple | exceptions.py | Y | – | 9V |
| W0716 | wrong-exception-operation | exceptions.py | Y | – | 9V |
| W0718 | broad-exception-caught | exceptions.py | Y | – | 9V |
| W0719 | broad-exception-raised | exceptions.py | Y | – | 9V |
| W1113 | keyword-arg-before-vararg | typecheck.py | Y | – | 9M §1.1 |
| W1114 | arguments-out-of-order | typecheck.py | Y | – | 9M §1.2 |
| W1115 | non-str-assignment-to-dunder-name | typecheck.py | Y | – | 9M §1.3 |
| W1116 | isinstance-second-argument-not-valid-type | typecheck.py | Y | – | 9M §1.4 |
| W1117 | kwarg-superseded-by-positional-arg | typecheck.py | Y | – | 9M §1.5 |
| W1201 | logging-not-lazy | logging.py | Y | – | 9M §6.3 |
| W1202 | logging-format-interpolation | logging.py | Y | – | 9M §6.4 |
| W1203 | logging-fstring-interpolation | logging.py | Y | – | 9M §6.5 |
| W1300 | bad-format-string-key | strings.py | Y | – | 9M §4.1 |
| W1301 | unused-format-string-key | strings.py | Y | – | 9M §4.1 |
| W1302 | bad-format-string | strings.py | Y | – | 9M §4.2 |
| W1303 | missing-format-argument-key | strings.py | Y | – | 9M §4.2 |
| W1304 | unused-format-string-argument | strings.py | Y | – | 9M §4.2 |
| W1305 | format-combined-specification | strings.py | Y | – | 9M §4.2 |
| W1306 | missing-format-attribute | strings.py | Y | – | 9M §4.3 |
| W1307 | invalid-format-index | strings.py | Y | – | 9M §4.3 |
| W1308 | duplicate-string-formatting-argument | strings.py | Y | – | 9M §4.2 |
| W1309 | f-string-without-interpolation | strings.py | Y | – | 9M §4.4 |
| W1310 | format-string-without-interpolation | strings.py | Y | – | 9M §4.1/4.2 |
| W1401 | anomalous-backslash-in-string | strings.py | Y | – | 9M §5.2 |
| W1402 | anomalous-unicode-escape-in-string | strings.py | Y | – | 9M §5.2 |
| W1404 | implicit-str-concat | strings.py | Y | – | 9M §5.3 |
| W1405 | inconsistent-quotes | strings.py | Y(opt-gated) | – | 9M §5.4 |
| W1406 | redundant-u-string-prefix | strings.py | Y | – | 9M §5.5 |
| W1501 | bad-open-mode | stdlib.py | Y | – | 9M §3.2 |
| W1502 | boolean-datetime | stdlib.py | dead | – | 9M §3.3 (never emits @3.12) |
| W1503 | redundant-unittest-assert | stdlib.py | Y | – | 9M §3.4 |
| W1506 | bad-thread-instantiation | stdlib.py | Y | – | 9M §3.5 |
| W1507 | shallow-copy-environ | stdlib.py | Y | – | 9M §3.6 |
| W1508 | invalid-envvar-default | stdlib.py | Y | – | 9M §3.7 |
| W1509 | subprocess-popen-preexec-fn | stdlib.py | Y | – | 9M §3.8 |
| W1510 | subprocess-run-check | stdlib.py | Y | – | 9M §3.9 |
| W1514 | unspecified-encoding | stdlib.py | Y | – | 9M §3.2 |
| W1515 | forgotten-debug-statement | stdlib.py | Y | – | 9M §3.10 |
| W1518 | method-cache-max-size-none | stdlib.py | Y | – | 9M §3.11 |
| W2101 | useless-with-lock | threading_checker.py | Y | – | 9M §9 |
| W2301 | unnecessary-ellipsis | ellipsis_checker.py | Y | – | 9M §13 |
| W2402 | non-ascii-file-name | non_ascii_names.py | Y | – | 9M §15 |
| W2601 | using-f-string-in-unsupported-version | unsupported_version.py | Y(dead@3.12 cfg) | – | 9M §16 |
| W2602 | using-final-decorator-in-unsupported-version | unsupported_version.py | Y(dead@3.12 cfg) | – | 9M §16 |
| W2603 | using-exception-groups-in-unsupported-version | unsupported_version.py | Y(dead@3.12 cfg) | – | 9M §16 |
| W2604 | using-generic-type-syntax-in-unsupported-version | unsupported_version.py | Y(dead@3.12 cfg) | – | 9M §16 |
| W2605 | using-assignment-expression-in-unsupported-version | unsupported_version.py | Y(dead@3.12 cfg) | – | 9M §16 |
| W2606 | using-positional-only-args-in-unsupported-version | unsupported_version.py | Y(dead@3.12 cfg) | – | 9M §16 |
| W3101 | missing-timeout | method_args.py | Y | – | 9V §5.1 |
| W3301 | nested-min-max | nested_min_max.py | Y | – | 9M §10 |
| W3601 | bad-chained-comparison | bad_chained_comparison.py | Y | – | 9M §11 |
| W4701 | modified-iterating-list | modified_iterating | Y | – | 9M §17 |
| W4901 | deprecated-module | deprecated.py via imports.py | Y | – | 9M §2 + 9V §2.14 |
| W4902 | deprecated-method | deprecated.py via stdlib.py | Y | – | 9M §2.7/§3.1 |
| W4903 | deprecated-argument | deprecated.py via stdlib.py | Y | – | 9M §2.7/§3.1 |
| W4904 | deprecated-class | deprecated.py via stdlib.py | Y | – | 9M §2.8/§3.1 |
| W4905 | deprecated-decorator | deprecated.py via stdlib.py | Y | – | 9M §2.4/§3.1 |
| W4906 | deprecated-attribute | deprecated.py via stdlib.py | Y | – | 9M §2.1/§3.1 |

Tally: 50 C + 130 E + 5 F + 9 I + 61 R + 134 W = 389 (verified by msgid diff against msgs.rs - zero missing, zero extra).
Coverage state: spec'd = everything except the PEND rows — format.py (11
msgs), refactoring/ (37 + C0117 + C0200/01/06/07/08/09 + C1802-C1805 = 48
msgs), design_analysis.py (11 msgs), and E0110 (1 msg) → 71 messages still
need spec docs for full-pylint mode.

================================================================================
# 23. Consolidated ordering & conservatism notes (this doc's checkers)
================================================================================

Emission-order dependencies to replicate:
1. E1101/I1101: `missingattr` is a Python set of (node, name) tuples —
   multi-owner emission order is set-iteration order (§1.7 step 5).
2. W1303 (and E1304/W1301/W1300 in %-format): iterate over str SETS —
   deterministic only under PYTHONHASHSEED=0; port needs hashseed-0-equal
   iteration or corpus verification (§4.1, §4.2).
3. W4903: `chain(*[deprecated_arguments(qn) for qn in {qname, func_name}])`
   — 2-element str set order (hashseed-0) (§2.7).
4. R0801: (a) sims sorted by num desc, stable buckets; (b) the code-lines
   block uses the LAST element of an id-hashed set — formally
   nondeterministic; (c) all R0801 messages attach to the LAST module at
   line 1 (§20.3).
5. stdlib visit_call loops over infer_all results — message per matching
   inferred value; duplicates possible and emitted (§3.0, §3.9).
6. Within a module, checker callback order is the prepared-walker order
   (already extracted); within one callback, source order of add_message
   calls as spec'd per section.

Conservatism bail-outs (quick checklist): safe_infer None/Uninferable skips
in W1115, W1116 (returns False → no msg), C2801 (non-Instance receiver →
skip), W2101 (None → continue), W3301 (non-min/max → skip), W1502/W1503
(literal Const only), W1501/W1514 (mode/encoding must safe-infer to Const),
W4906 (receiver must infer to ClassDef/Instance/Module), W1202
(is_complex_format_str: uninferable bound → treated complex → SKIP),
modified-iterating (iterable must safe-infer to a literal container),
R1905/R1906 (class must safe-infer to ClassDef), E1101 funnel (opaque
inference, mixins, dynamic getattr, unknown bases, try/except AttributeError,
False-const If guards). NO inference at all in: W1113, W3601, C3001/C3002,
W2301, C2401/C2403/W2402 (name tests only), W0511 (tokens), W1401/W1402/
W1404/W1405/W1406 (tokens), E1902-side of match (syntactic).

Position summary (non-node messages): W1401/W1402 explicit line+col;
W1404 line only (col 0); W1405 line only; W1406 explicit line+col;
W0511 line + col(start+1); I0023 line only; R0801 none (1:0, last module);
C0401/C0402/C0403 line only. Everything else: node-based (FunctionDef/
ClassDef keyword anchor applies to W1113 only in this doc).

================================================================================
# 24. Open questions
================================================================================

1. PEND coverage gap (71 msgs): format.py, refactoring/ (refactoring_checker,
   not_checker, recommendation_checker, implicit_booleaness_checker),
   design_analysis.py have NO spec doc yet; E0110 abstract-class-instantiated
   was excluded from notes/08 by the -E flags and needs a spec for full mode.
   E0401 import-error trigger spec is only partial (9V §2.16 shared path +
   notes/01/07 resolution); needs a dedicated pass for full mode.
2. R0801 code-block source: which lineset's real lines get printed depends
   on iteration order of a set keyed by id() (symilar.py:849-855 +
   :701-702). Determine empirically from a full-mode ground-truth run
   whether "last appended" wins consistently; pin accordingly.
3. Str-set iteration order (W1303/E1304/W1301/W1300, W4903 chain order):
   decide between replicating CPython hashseed-0 small-set order in Rust vs
   accepting and verifying that corpora never hit multi-element cases.
4. E1101 multi-owner emission order (missingattr set of node tuples): node
   hash = id() → verify corpora only produce single-owner cases, else
   replicate insertion order and diff.
5. LRU caches (safe_infer 1024, infer_all 512, _similar_names 256) can
   change results across calls ONLY via cache-size eviction nondeterminism —
   pylint relies on them being semantically transparent; our port should be
   pure-functional (verify no observable divergence on corpora).
6. stdlib `_check_open_call` Phase B does `node.func.attrname` when
   open_module is pathlib — if node.func were a Name (e.g. `read_text =
   Path.read_text; read_text(p)`), pylint raises AttributeError → F0002
   astroid-error for the whole module. Decide whether to replicate the
   crash (F0002 with what message?) or verify unreachable on corpora.
7. W1502/E0106 resurrection: `# pylint: enable=boolean-datetime` after the
   may_be_emitted force-disable — notes/03 must confirm whether enable
   pragmas can re-enable non-emittable messages (pylint's _set_msg_status
   path) — affects pylfunc-style pragma corpora.
8. ByIdManagedMessagesChecker timing: confirm raw-checker invocation isn't
   skipped when all its messages are disabled (pylint gates raw checkers by
   enabled messages? notes/02) — affects whether `_by_id_managed_msgs` is
   cleared per module when I0023 is off (it accumulates otherwise; harmless
   for output but memory-relevant).
