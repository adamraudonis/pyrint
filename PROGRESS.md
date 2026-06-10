# prylint rewrite — state of the world

Mission: byte-identical Rust port of `pylint . -E --disable=<list>` (the exact
list is in `harness/flags.txt`), ≥10x faster. Ground truth pinned: pylint
4.0.5 / astroid 4.0.4 / CPython 3.12.12 in `.venv-pylint`. NEVER "improve" on
pylint behavior — bugs are replicated.

## Done

- **Environment**: Rust 1.96 (brew), pinned venv, pylint/astroid sources at
  `reference/{pylint,astroid}` (tags v4.0.5/v4.0.4).
- **Spec extraction**: `reference/notes/00..08` (~15k lines) — port-ready
  specs for discovery, pipeline/output/exit codes, message control/pragmas,
  AST building, variables checker, typecheck, inference engine, all other
  checkers. 00-architecture.md is the build plan.
- **Corpora** (pinned clones in `corpora/`): django, pandas, salt, airflow,
  core (home-assistant), sentry + pylfunc (copy of pylint's
  tests/functional — pragma-heavy, has root `__init__.py` so exercises
  package-mode discovery).
- **Ground truth** (`harness/results/*.iso.out`, flags verbatim + empty
  rcfile): django 109s/898 lines, pandas 259s/616, salt 183s/8690 (E0602
  heavy), airflow 286s/667, sentry 323s/515, core 1357s/82243 (28k E0001 —
  py3.14 syntax; 37k E1123), pylfunc 5s/524 (exit 14 — inline `enable`
  pragmas resurrect W/R under -E!).
- **Harnesses**: `harness/ground_truth.sh`, `diffmsg.py` (FP/FN per code),
  `check_discovery.sh`, `check_treedump.sh`, `dump_ast.py` (astroid tree
  dumper), `dump_fileitems.py`, `syntax_oracle.py` (exact E0001 via pinned
  astroid; protocol in file), `gen_snapshot.py` (DONE: 103 C-ext/builtins
  modules → `crates/pyinfer/snapshot/*.json`, 6.4MB), `gen_msgs_rs.py`
  (→ `crates/pycheckers/src/msgs.rs`, 389 messages, 130 enabled).
- **Rust `cli` discovery**: byte-identical FileItem lists (name+path+order)
  on all 6 corpora (`--dump-fileitems`).
- **Rust `pyast`**: astroid-equivalent tree (arena, astroid positions,
  locals incl. delayed ImportFrom + global-decl routing, doc_node
  extraction, metaclass extraction, genexp paren expansion, def/class/async
  keyword anchoring, ExceptHandler/MatchAs/MatchStar AssignName quirks,
  MatchCase positionless, fromlineno first-child-chain fallback).
  `--dump-ast` byte-identical on feature file; corpus-wide grind delegated
  to the **tree-fidelity workflow** (running; sequential agent rounds; known
  open: Arguments tolineno rule, brain_dataclasses locals injection,
  encodings via from_utf8_lossy → must port open_source_file).

## Key empirical facts (verify in notes/01-03 for detail)

- `pylint .` (no __init__.py at root) = namespace walk: ALL dirs (hidden
  incl.), readdir order, files-before-subdirs, symlink dirs not descended,
  `.so/.pyd/.pyw` discovered but dropped by should_analyze_file (.py/.pyi
  kept); root with __init__.py = package mode (prune non-package dirs,
  names prefixed with root dir basename).
- Two-phase: ALL ASTs built first (E0001/F0010/F0002 stream here, file
  order), then per-file checks in same order. Within module: tokenize
  E0001 → pragmas → raw checkers (unicode only under -E) → AST walk
  (callbacks in prepared-checker order — extracted empirically, incl. the
  same-name reverse-registration quirk).
- Module header: `************* Module {msg.module}`; node msgs use
  astroid module name (`.__init__` stripped), node-less msgs (E0001) use
  raw FileItem name (unstripped). Path = abspath.replace(cwd+sep, "", 1).
  Exit: bitmask F=1 E=2 (displayed msgs only), 0 when clean.
- E0001 forms: `Parsing failed: '<str(SyntaxError)>'` (modname embedded in
  str when CPython is given filename=modname; `(<unknown>, line N)` after
  type-comment retry); tokenize TokenError form (no prefix); imports.py
  `Cannot import 'X' due to '<err>'` at import sites of broken modules
  (core has 28k of these); encoding errors embed ABSOLUTE path; null bytes
  → line 0 rendered as 1; col = raw 1-based offset.

## Running / next

1. **tree-fidelity workflow**: DONE — `--dump-ast` byte-identical on all 7
   corpora. Owns `crates/pyast`.
2. **pipeline shell**: DONE — full lint pipeline in `crates/cli`
   (`run.rs` two-phase orchestration w/ rayon + ordered flush, `pragma.rs`
   OPTION_PO/TOK_REGEX/parse_pragma regex-exact port w/ differential tests,
   `msgstate.rs` GlobalState/FileState block expansion (astroid block_range
   for Module/Class/Func/If/Try/TryStar/While + default) + process_tokens +
   is_message_enabled incl. past-EOF raw-pragma fallback, `reporter.rs`
   TextReporter, `oracle.rs` batched syntax-oracle subprocess), plus
   `pycheckers::{msgstore,unicode}` (message resolution w/ old_names +
   deleted/moved tables + checker-name/category/'all' expansion; unicode raw
   checker E2501/E2502/C2503/E2510-15 bug-for-bug). syntax_oracle.py also
   reports tokenize.TokenError (tokenize-form E0001). check_shell.py PASSES
   on ALL 7 corpora (pylfunc exit 6, django/sentry/salt/core exit 2,
   pandas/airflow clean exit 0); core runs in ~4s vs pylint's 1357s.
   Known gaps: F0002 crash-path message uses a placeholder crash-file path
   (wall-clock dependent in pylint; no corpus emits it); I0020/I0021
   useless-suppression machinery deliberately not emitted (needs checker
   _ignored_msgs bookkeeping — would FP without real checkers; 2 pylfunc GT
   lines are accepted FNs until then); files CPython accepts but ruff
   rejects with clean tokenize are skipped with a stderr note (none in
   corpora).
3. **pyinfer foundations**: DONE (phase 1) — `crates/pyinfer` ports the
   astroid 4.0.4 engine per notes/07; `prylint --dump-infer <items.jsonl>`
   mirrors harness/dump_infer.py (phase-1 prebuild of every fileitem in
   order, preorder Name/Attribute/Call dump, render parity incl. the
   blank-line-on-empty-file print quirk; runs in a 1GB-stack thread ≈
   setrecursionlimit(8000)).
   - Value model (value.rs), InferenceContext/CallContext + global cache
     keyed (node, lookupname, callcontext-IDENTITY via unique id,
     boundnode structural key) (ctx.rs/graph.rs), depth guard max_depth=350
     standing in for RecursionError (NOT yet probe-tuned).
   - ModuleGraph (graph.rs): sys.path = [realpath(cwd)] + pinned venv
     sys.path (PRYLINT_PYTHON probe, exe-relative .venv-pylint fallback);
     find_spec port (ImportlibFinder incl. pkg/__init__ + EXTENSION_SUFFIXES
     order, namespace portions for PathSpecFinder; frozen branch dead in
     this env, os.path special-cased like modutils); astroid_cache with
     setdefault semantics (PROBE-VERIFIED: first module wins, the old
     "second wins" gotcha below is WRONG for astroid 4); _post_build =
     cache-before-delayed + star-import locals expansion via real module
     resolution (replaces pyast's static stdlib table at engine load; per-add
     fromlineno re-sort like add_from_names_to_locals) + delayed_assattr
     with real inference (cross-module instance_attrs/locals mutation);
     module extenders ported for typing/collections/datetime (template
     modules parsed from brain sources, named like the target so qnames
     match).
   - Snapshot loader (snapshot.rs); snapshot REGENERATED with fixes:
     bootstrap-first (the old builtins.json was an unextended duplicate —
     no brain str/bytes stubs, no generator class), "xtra" sidecars for
     locals-only nodes, EmptyNode einf descriptors (resolved lazily via
     qname → graph), authoritative "qn" qnames (raw-built nodes are
     reparented by add_local_node; ser() nesting lies).
   - lookup.rs: scope_lookup per scope type + _filter_stmts verbatim +
     are_exclusive; infer.rs: NodeNG.infer dispatch with the §4.1 decorator
     table (path_wrapper dedup incl. exact-class Instance→proxied rule),
     _infer_stmts + constraints (NoneConstraint AND BooleanConstraint —
     notes/07 understates 4.0.4); protocols.rs: assigned_stmts family,
     subscript/getitem, BinOp/AugAssign (op arrives as "+=" — strip '='),
     UnaryOp, %-formatting subset; getattr.rs: Module/Class/Instance/Super
     getattr+igetattr (same-scope filter incl. proxy parents, descriptor→
     Uninferable, last-function-wins, metaclass lookup with the cls!=self
     guard — type's implicit metaclass is itself), MRO C3 + ancestors,
     FunctionDef.type incl. extra_decorators; calls.rs: CallSite +
     infer_argument, infer_call_result per callable (generator/implicit-None/
     is_abstract first-stmt quirk/builtin __new__/partial merge); Super
     binds methods to the MRO class and infers only cls[name][0]
     (objects.py:184-217). brains.rs: builtin call tips, .copy(),
     str.format fold, functools.partial, typing brain (TypeVar/NewType
     template, _alias/_TupleType/_CallableType synthetic classes,
     typing.X[...] subscripts, TypedDict) via template-module parsing.
   - **phase 2 (diff-reduction round, N=200)**: 22.3k -> 14.3k diff lines.
     LANDED: (a) lazy-pull restructure — sink-based generator-exact
     inference (Sink/Drive/End in value.rs; abandonment skips cache writes
     + post-yield bumps; path poisoning across callees; single-pull next()
     sites incl. safe_infer/_infer_builtin_new/declared_metaclass/binop/
     unaryop/getitem/context-managers); (b) _infer_type_call +
     _infer_type_new_call via runtime synthetic-class modules
     (build_synth_class + Engine.redirects); (c) enum class transform +
     namedtuple brain (4 tips) + type()-1-arg set semantics;
     TRANSFORM CACHE INVALIDATION: every applied transform wipes the GLOBAL
     inference cache (transforms.py:66-72) — ported as transforms.rs
     wipe_scan (registration-ordered predicates incl. inference-bearing
     ones) run at end of every build; EmptyNode._infer replays
     manager.infer_ast_from_something via module igetattr under the SHARED
     context (snapshot "ek" field; snapshots REGENERATED); streaming
     ancestors()/exact _class_type/_is_metaclass/ClassDef.slots()/
     _can_assign_attr/delayed_assattr; snapshot loader fixes (Return nodes
     — str/bytes stub bodies!, live Instance locals, NotImplemented const).
     TOOLS: PRYLINT_DUMP_COUNTS + PRYLINT_TRACE_INFER (rust) and
     harness/dump_infer_count.py (oracle) — per-node nodes_inferred
     counter diffing + NodeNG.infer trace diffing localize counter-parity
     bugs exactly; harness/infertests/ + run_infertests.sh regression
     probes (7 probes, all PASS).
     N=200 differing files/lines: django 61/2959, pylfunc 34/111, pandas
     132/6499, salt 104/2667, airflow 76/976, sentry 77/600, core 75/505.
     REMAINING (by volume): counter/cache-dynamics clusters (U<->Inst/Class
     flips, dup values — continue with the tracer; next known divergence:
     metaclass-lookup re-pull timing in the __slots__ igetattr chain, see
     /tmp/probe10 os.path.abspath trace); brain_numpy inference tips
     (pandas Func:.array ~400 lines; wipe predicates ported, tips not);
     dataclasses field tips; six.with_metaclass infer_call_result hack;
     recursion-guard probe-tuning (still 350); namedtuple/enum leftover:
     functional Enum("X", "a b") call tip.
   - **phase 3 (diff-reduction round 2, N=200)**: 14.3k -> 4.2k diff lines.
     LANDED (probe-verified, all in harness/infertests/): (a) yield-before-
     break cap semantics — node_ng.py:164-167 suspends at `yield U` BEFORE
     `break`; the post-loop cache write only runs if the consumer pulls
     again, so abandoning consumers leave NO poisoned [..,U] cache entries
     (os.path/abspath chains); (b) CallSite._unpack_* MUTATES the passed
     context: extra_context = argument_context_map (arguments.py:95/:135) —
     the Call._infer populated map is clobbered before arg inference (HUGE:
     django 2864->619); the _arguments_infer_argname path instead builds
     CallSite with context=None → FRESH unpack contexts + caller's
     extra_context as the map (protocols.py:387-389, super().m(**kwargs));
     (c) inference_tip.py:50-52 — EMPTY contexts are nulled before tips run:
     tip-internal inference burns its own counters, never the caller's;
     (d) tips_active per module — _explicit_inference only exists after the
     module's transform phase (builder.py:175-177): delayed_assattr
     inference runs tip-less (super() in _py_abc → Inst:builtins.super);
     (e) ancestors() does NOT clone the context (lookupname preserved into
     base-infer cache keys); declared_metaclass full port (bases loop runs
     EVERY call, with_metaclass hide-override side table, _find_metaclass
     recursion drops ctx); _metaclass_lookup_attribute copies ctx + runs
     attrs through the _infer_stmts hop before BM wrapping; (f) numpy brains
     (generated numpy_templates.rs: ndarray class template + multiarray/
     function_base/numeric member tips; lookup-based predicates work without
     numpy importable; pandas -55%); (g) ObjectModel __new__/__init__
     synthetic BMs (+ Super model fallback incl. __thisclass__ etc.,
     InstanceModel __doc__ = class docstring); (h) exact f-string port
     (FormattedValue format() of astroid objects → 'Instance of X'/
     'Uninferable' strings; JoinedStr {Uninferable} marker, first-failure-
     only U, node._infer no-bump pulls); (i) isinstance/len rewrites per
     helpers.py (object_type set semantics, bases.Instance sanitisation,
     object_len raise tail); (j) dataclass engine transform: instance_attrs
     Unknown placeholders + infer_dataclass_attribute/field_call tips;
     (k) DictModel items/keys/values special BMs + DictItems iteration;
     (l) brain_type/brain_pathlib/brain_collections/brain_attrs predicate+
     tip ports (predicate side effects at scan time, applicability recorded
     in side tables); (m) path_wrapper dedup keys synthetic values by Rc
     pointer identity (BoolOp product reuse); (n) assigned_stmts node
     results re-infer via _infer_stmts hop (nvify); (o) snapshot/sys.json
     regenerated in a dump_infer-equivalent process (sys.modules count is
     oracle-process-dependent: 203; harness/regen_sys_snapshot.py).
     N=200 differing files/lines: django 34/501, pylfunc 18/34, pandas
     91/1817, salt 60/1076, airflow 38/431, sentry 34/224, core 33/138.
     REMAINING (by volume): deep-chain counter drift in pandas/salt asv
     benchmarks (DataFrame/Index call chains hit the 100-cap a few bumps
     apart — values flip U<->Inst near the boundary; needs more per-bump
     parity, use PRYLINT_DUMP_COUNTS + dump_infer_count.py --only=...);
     %-formatting of non-tuple RHS still U-only; descriptor __get__ model
     (FunctionModel.attr___get__, 2 pylfunc lines); brain_re Pattern
     __class_getitem__; six.with_metaclass infer_call_result hack (hidden
     temporary_class — side tables exist, call-result synthesis not wired);
     subprocess/multiprocessing brains; recursion guard still 350 (no
     probe-detected divergence this round).
   - **phase 4 (diff-reduction round 3, N=200)**: 4.2k -> 3.25k diff lines.
     LANDED (all probe-verified in harness/infertests/): (a) stdlib
     module-extender brains via EXACT generated templates
     (harness/gen_ext_templates.py captures the post-dedent sources the
     pinned brains pass to parse() -> crates/pyinfer/src/ext_templates.rs):
     subprocess/hashlib/ssl/signal/re/http/http.client/threading/crypt/
     unittest + multiprocessing(.managers) — the mp probe instantiates
     DefaultContext()/BaseContext() and appends public context-class
     FunctionDefs as BoundMethod VALUES into module locals
     (Module.ext_locals consulted by module_getattr; brain_multiprocessing
     .py:31-48 set_local=append semantics); (b) brain_re Pattern/Match
     Call tip (fresh ClassDef w/ __class_getitem__ per inference); (c)
     ClassDef.implicit_locals() (scoped_nodes.py:1911-1933): every
     non-snapshot class lazily gets __module__/__qualname__ Const +
     __annotations__ Unknown FIRST in locals (construction-time values;
     synth Const/Unknown nodes + implicit_owner side map feeds the
     igetattr same-scope filter; snapshot classes already carry them);
     (d) f-string format(obj,'') folds ALL results via str(obj):
     NodeNG.__str__ pprint emulation (Dict/List/Tuple/Set/FrozenSet +
     synthetic values; fake ids sized like CPython's so pprint wrapping
     matches — ids virtually never inside the 40-char dump window),
     Instance/Generator/UnionType/Bound-UnboundMethod reprs; (e)
     property(...) tip Property named '<property>' under SYNTHETIC_ROOT;
     (f) brain_argparse Namespace tip (EmptyNode instance_attrs); (g)
     functools LruWrappedModel (__wrapped__/cache_clear/cache_info); (h)
     exact _infer_old_style_string_formatting branches (safe-inferred
     tuple elements, non-all-Const -> fmt % None fold, dict mapping) and
     a REAL printf-directive parser (%(key), flags -+0#space, width,
     .prec, s/r/d/i/u/x/X/o/f/F/c) — uuid4().hex now folds to '0'*32 via
     brain_uuid locals['int']=Const(0); (i) isinstance/issubclass accepts
     binop-synthetic Tuples (per-element single pull); (j) brain_typing
     PEP695 __class_getitem__ ClassDef tip (type_params classes, + scan
     wipe); (k) for_assigned_stmts propagates the raised error KIND when
     the iterable yields nothing (NameInferenceError reaches _infer_stmts
     -> silent skip -> Name ERR; genexp-in-class-scope).
     N=200 differing files/lines: django 33/475, pylfunc 8/20, pandas
     86/1725, salt 53/699, airflow 34/227, sentry 15/57, core 22/50.
     REMAINING (by volume): (1) counter/cache-dynamics drift — pandas core
     (frame/managers/series/blocks ~1.4k), django test clusters (~450),
     salt (ipaddress/git_pillar/zypper/nxos ~650), airflow mock clusters
     (~200). Concrete traced instance: during lazily-built stdlib module
     transform scans our base-Name cache pulls/wipes land in a different
     ORDER than astroid (extra full re-infer of e.g. Name Awaitable around
     the _collections_abc scan; salt.utils.data.decode counts ##118 vs GT
     ##120 -> 100-cap truncation flips downstream values). Tooling: the GT
     tracer /tmp/trace_infer_gt.py (NodeNG.infer monkeypatch) diffs
     structurally against PRYLINT_TRACE_INFER. (2) pylfunc leftovers (20
     lines): os.environ GT-env noise (4, irreducible), tokenize_error
     NOTREE (6 — ruff rejects trailing backslash-EOF CPython accepts;
     tree-fidelity owns), FunctionModel attr___get__ descriptor model (6),
     no_member_augassign (2), __code__ Unknown (2). (3) six.with_metaclass
     call-result synthesis still unwired (~20 lines). (4) recursion guard
     350 — no probe-detected divergence this round.
   - **phase 5 (diff-reduction round 4, N=200)**: 3.25k -> 1.77k diff
     lines. LANDED (all probe-verified in harness/infertests/):
     (a) **bases.Instance OBJECT IDENTITY in cache boundnode keys** — the
     big one: astroid keys context.boundnode by id() (no __eq__ on
     Proxy/Instance); Value::Inst now carries InstId (fresh per
     instantiate_class, preserved through cache replay); our old
     structural Inst(cls) key merged distinct receivers -> spurious cache
     hits (cheap replays where astroid re-burns toward the 100-cap; e.g.
     `with mock.patch(...) as b` -> [U] vs our stale 5-value replay).
     dedup_key (path_wrapper) still unproxies to the class node.
     (b) **consumer abandonment beats producer completion**: producers
     that complete via `yield U; return` (Subscript etc.) STILL skip the
     NodeNG.infer cache write when the consumer abandoned at that yield
     (wrapper dropped suspended) — infer_entry_to: Done+consumer_stopped
     -> Stopped. Killed whole spuriously-cached frozen chains
     (package.py line.split…strip counts now exact).
     (c) Value::Generator carries copy_context(call-time ctx)
     (bases.py:698); _infer_context_manager pulls
     next(infer_yield_types()) under THAT context, single lazy pull incl.
     YieldFrom (Yield subclass); decorator check uses the with-site ctx
     uncopied; Instance branch routes through BM infer_call_result
     (boundnode -> `return self` infers the SUBCLASS instance);
     ValueKey::Generator keyed by captured-ctx pointer.
     (d) exact builtin-container tips (_infer_builtin_container port):
     per-builtin iterables whitelists, klass-first check
     (frozenset(set-literal) -> FrozenSet), DictKeys/Values/Items
     elements (objectmodel.py:856-890), all-Const build_elts with python
     value-equality dedupe for set/frozenset, _use_default abort on
     non-Const dict keys, single-pull arg inference.
     (e) brain tips arg pulls are SINGLE PULLS with the SAME context
     (`next(arg.infer(context=context))`): int/bool/callable/property/
     getattr(+default)/hasattr/super/dict.fromkeys/functools.partial/
     typing-TypeVar — our eager copied-ctx pulls completed+cached chains
     astroid leaves frozen (int_tip_single_pull_counts probe is
     count-exact).
     (f) instance-call attribute shortcut propagates igetattr errors
     (bases.py:327-330): `inst.attr(...)` where the instance lacks attr
     -> InferenceError aborts the call (salt __zypper__ ERR cluster).
     (g) Decorators.scope() skip-to-class applies mid-walk (names in
     method decorators see class attrs); bytes/bool sequence repetition
     in const binop folds (b"x"*5); f-string error propagation
     (_safe_infer_from_node catches ONLY InferenceError -> trailing U;
     other kinds propagate; FormattedValue value/spec raises propagate
     after yielded values; suffix generators recreated per prefix).
     (h) sys snapshot canonicalized: sys.json regenerated via
     harness/regen_sys_snapshot.py (python -E, dump_infer import set;
     5-entry sys.path, 203-entry sys.modules); engine prepends
     realpath(cwd) to the snapshot sys.path List at load (oracle main()
     inserts the corpus root); dump_infer_count.py un-pollutes its
     sys.modules/sys.path so counter comparisons match the warm cache.
     TOOLS: harness/catdiffs.py (value-pattern diff categorizer),
     harness/run_probe_counts.sh, run_infertests.sh COUNTS=1 mode
     (counted dumps; 20/43 probes have KNOWN count-only gaps — the
     residual counter-parity punch list), CACHEW lines under
     PRYLINT_TRACE_INFER (cache-write tracing; pair with
     /tmp/trace_infer_gt.py + a logging _INFERENCE_CACHE for GT).
     N=200 differing files/lines: django 22/123, pylfunc 8/17, pandas
     73/1203, salt 30/343, airflow 14/44, sentry 4/8, core 10/27.
     REMAINING (by volume): (1) counter/cache-dynamics drift, now
     LOCALIZED to igetattr/MRO-walk internals — run
     `COUNTS=1 harness/run_infertests.sh` for 20 small reproducible
     count-only probe gaps (e.g. dict-model BM lookup does 3 extra
     base-resolution Name infers after the first FunctionDef pull —
     container_builtins_dictviews ##5 vs ##6; enum transform ##17 vs
     ##20; ctxmanager Gen ##3 vs ##6); fix these and the pandas/salt
     cap-boundary clusters should collapse. The clean count-diff
     workflow: dump_infer_count.py (full corpus, --only=<comma list>!)
     vs PRYLINT_DUMP_COUNTS=1 (note: check_inferdump N=200 uses the
     FIRST 200 items so cache prefixes match the full-dump warm cache).
     (2) pylfunc leftovers unchanged (17 lines, see phase 4). (3)
     six.with_metaclass call-result synthesis (~20 lines). (4) int tip:
     ints beyond i64 in int('huge-str') fold to 0 (astroid folds big) —
     no corpus hit. (5) set/frozenset all-Const dedupe keeps FIRST
     occurrence order; CPython set iteration order (hash-based) is not
     emulated — only visible if a folded set's ELEMENTS get dumped in
     order (none observed).
   - **phase 6 (diff-reduction round 5, N=200)**: 1.77k -> 1.30k diff
     lines. LANDED (probe-verified; new probes in harness/infertests/):
     (a) **transform-chain BREAK rule** (transforms.py:60-78): an APPLIED
     transform whose return's class differs from the node's class — incl.
     the COMMON `return None` (attrs, collections __class_getitem__,
     dataclasses-without-init-gen, brain_io, qt, uuid, functools lru) —
     STOPS the remaining transforms for that node, and ONLY non-None
     returns wipe the inference cache. scan_classdef/scan_functiondef
     rewritten in exact registration order with per-transform
     return-semantics (dataclass wipe now gated on
     _check_generate_dataclass_init; six.add_metaclass exact single-pull
     apply + meta_override; boto3 qname fixed to
     boto3.resources.base.ServiceResource; brain_io ported: BufferedReader/
     BufferedWriter locals["raw"]=FileIO instance, TextIOWrapper
     locals["buffer"]=BufferedWriter instance — required cache_module
     BEFORE the snapshot module's wipe_scan like raw_building.py:460).
     (b) **streaming/lazy generator parity**: BinOp/AugAssign per-pair
     streaming (results reach the consumer before the next product pair —
     yield ORDER + abandonment skips later pairs); FormattedValue/JoinedStr
     full suspended-generator semantics (spec/value loops lazy, post-yield
     bumps deferred to the next pull, raises abandon suspended generators;
     _safe_infer_from_node trailing-U streaming; fresh Const("") spec bump
     fires only after the body completes).
     (c) **counter parity batch**: tl binop elt inference under the SHARED
     ctx with boundnode cleared (ALL values flattened, U->UNATTACHED_UNKNOWN
     singleton; seq*int safe_infers each elt ONCE then repeats);
     has_known_bases _all_bases_known memo; binop operand object_type
     re-infers node-backed operands (fresh ctx, set-collapse); synthetic
     node hop-bumps: SynthConst/SynthSeq/SynthDict/SynthSlice/FrozenSet
     passing through _infer_stmts emulate stmt.infer cache-miss/replay via
     synth_hop_cache keyed by Rc pointer (+pins against ABA address reuse;
     ValueKey::Synth now pointer-keyed — fixes spurious boundnode cache
     hits on synthetic dicts); NV::V(Value::Node) routes through the full
     stmt.infer hop; issubclass/`_infer_type_call` bases/dict-model BM
     single pulls (DictModel wraps the UnboundMethod from class igetattr);
     decoratornames(context) passes the caller ctx AS-IS (lookupname in
     decorator cache keys; extra_decorators included); _is_property always
     ctx=None (all astroid call sites); ClassDef._all_slots walks the FULL
     mro (grouped_slots) even after a None; _islots elt.infer pulls.
     (d) exception instance models: OSError-family
     filename/errno/strerror/filename2 (exact BUILTIN_EXCEPTIONS qname
     list), ImportError name/path, UnicodeDecodeError object.
     TOOLS: PRYLINT_TRACE_INFER now prints ni= (nodes_inferred at entry) +
     SYNTHPULL/WIPE/SCAN events; /tmp norm_trace.py-style flat-event
     diffing against the GT NodeNG.infer monkeypatch localizes bump drift.
     N=200 differing files/lines: django 20/87, pylfunc 8/17, pandas
     58/906, salt 26/220, airflow 13/42, sentry 3/6, core 7/18.
     REMAINING (by volume): (1) pandas deep-chain cap-boundary drift
     (frame.py property/instance_attr chains flip values near the 100-cap;
     asv_bench getattr-receiver chains GT=U vs ours ERR ~58 lines;
     managers.py self.blocks value-set drift ~41); counted probes still
     failing (run COUNTS=1 harness/run_infertests.sh): enum transform
     (GT re-infers more in igetattr after the transform wipe; ours
     replays — ##17 vs ##20), namedtuple/dataclass-field (±1),
     ctxmanager_getattr_param (_io extender inference order),
     pathlib parents (+2), pep695 (+1), type_subscript ndarray,
     brain_ssl http.HTTPStatus (±1), os_path_abspath_cap (##107 vs
     ##109 — posixpath splitroot Call caching divergence under
     callcontext). (2) pylfunc leftovers unchanged (17 lines, see phase
     4). (3) six.with_metaclass call-result synthesis still unwired.
     (4) recursion guard still 350.
   - check_inferdump (60 files): django 1290/38063 lines differ (96.6%
     match), pylfunc 37/1987 (98.1%). Known gap clusters for phase 2:
     (a) nodes_inferred counter parity — our eager evaluation burns the
     shared 100-cap faster than astroid's lazy pulls, truncating chains
     like `self.client.get(...)` one value early (probe_s7; `c.get()`
     matches); needs lazy/capped inference plumbing. (b) _infer_type_call /
     _infer_type_new_call (type() 3-arg, modelform_factory) not ported —
     needs synthetic-class building. (c) enum brain, namedtuple brain,
     dataclasses field tips not ported. (d) with_metaclass hack skipped.
     (e) some GT diffs are STRUCTURAL at N=60: the cache was warmed with
     ALL files prebuilt (delayed_assattr from file 61+ feeds instance_attrs
     the 60-file run can't see) — compare with N=all for truth.
   - NOTE pyast changes: ConstValue::NotImplemented variant (synthetic
     only), Interner::len, vararg/kwarg AssignName nodes now built
     (astroid parity; not in get_children so dumps unchanged — tree gate
     still 0).
4. Then checkers fan-out (variables first: salt is the acid test), each code
   driven to 0 FP/FN via `diffmsg.py --code=EXXXX` on all 7 corpora — wire
   them into the phase-2 walk placeholder in `run.rs::lint_one` using
   `walk_order.rs` dispatch tables.
5. Then byte-exactness (ordering/headers), pylint-functional sweep, perf
   (10x = whole-suite ≤ ~250s; budget core ≤ 135s incl. oracle for 318
   broken files — currently ~4s).

## Gotchas for future rounds

- Don't sort anything pylint doesn't sort. Order comes from readdir + dict
  insertion everywhere.
- `# pylint: enable=` inside files can re-enable ANY message under -E
  (pylfunc exit 14 proves it) — message table must keep ALL messages, not
  just the 130 enabled ones.
- Inference caching in astroid is global across the whole run; per-module
  fresh caches may cause rare diffs — check before optimizing.
- Two files, same modname → astroid 4 cache_module is setdefault: FIRST
  wins for importers (probe-verified; an older note here claimed second
  wins — wrong). Reporter header printed once per module NAME.
- py-version gating: harness venv is 3.12.12; `MessageDef.may_be_emitted`
  already folded into msgs.rs `enabled`.
