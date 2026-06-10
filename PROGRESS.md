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
   - **phase 7 (diff-reduction round 6, N=1000)**: 8.2k -> 2.07k diff
     lines (first N=1000 round; the harness now prebuilds the full corpus
     and restricts dumping via PRYLINT_DUMP_ONLY). LANDED (all
     probe-verified in harness/infertests/, now 54 probes):
     (a) **inference-tip cache semantics**: astroid's _inference_tip_cached
     keys (func, node, context) — non-empty contexts RE-RUN tip internals
     (typing typevar/subscript template branches rebuilt per run, template
     class inferred via a live-ctx NodeNG hop); ClassDef.getitem's
     __class_getitem__ fallback getattr runs with NO context
     (scoped_nodes.py:2561); brain_pathlib tip = fresh-ctx single pull;
     getattr tip = next(igetattr) SINGLE pull (chain abandoned).
     (b) **fresh-ctx ports**: excepthandler unpack_infer(self.type) with NO
     context + exact double-pull port; _slice_value single pull under the
     LIVE ctx; namedtuple synthetic base = real template Name node (two
     infer hops like _extract_single_node("tuple")).
     (c) **dataclass field default_factory** rebuilt as a REAL
     parse("<factory>()") template build — the transform scan applies the
     builtin Call tip and WIPES the global inference cache MID-DUMP exactly
     like astroid (brain_dataclasses.py:430).
     (d) **enum transform exactness**: member fake classes built with
     apply_transforms=False and REPARENTED into the real module
     (`fake.parent = target.parent`; new cross-module `reparents` table in
     treeutil::parent); member locals hold Instance PROXIES (bare yield —
     no NodeNG machinery; proxy_placeholders set).
     (e) **copy-tip path poisoning** (THE pandas fix, -3k lines):
     _infer_copy_method infers the receiver under the LIVE context with
     all(...) short-circuit — the abandoned pull poisons the caller's path
     so the default Attribute._infer re-pull of the same Name is
     path-blocked -> the call site yields U (sort_values cap clusters).
     (f) **brain_attrs attr_attributes_transform** ported (airflow task-sdk
     -770 lines): __attrs_attrs__ + per-target Unknown placeholders REPLACE
     locals/instance_attrs; ClassVar skip via is_class_var.
     (g) **Compare literal folding completed**: in/not-in are COMPARE_OPS
     (str substring + container membership), _to_literal covers literal
     containers, mixed-type ==/!= folds to False/True.
     (h) PropertyModel attr_setter/deleter/getter = fresh synthetic empty
     FunctionDef parented to the property; Lambda.type "method" rule +
     UnboundMethod-on-Lambda call result = body.infer; implicit class
     locals (__module__/__qualname__/__annotations__) resolve in Name
     LOOKUP (not just getattr); AssignAttr._infer delegates AugAssign
     parents + AssignAttr.infer_lhs is path_wrapped (self.x += 1 lhs
     recursion blocks -> U); NV::V(Value::Node) routes through the full
     NodeNG.infer hop; subscript getitem results that astroid builds as
     FRESH nodes (Const/container getitem) bump on drain.
     N=1000 differing files/lines: django 27/166, pylfunc 12/20, pandas
     196/1547, salt 35/163, airflow 26/94, sentry 14/44, core 16/36.
     REMAINING (by volume): (1) pandas deep-chain cap-boundary drift
     (~1.4k lines: frame.py/_validate_dtype/pandas_dtype chains burn +3-17
     vs GT before the 100-cap — divergence localized to a registry.find/
     construct_from_string path picking different arg names ('val' vs
     'dtype'); use the /tmp prefix-counted-dump workflow from this round:
     PRYLINT_DUMP_ONLY=<prefix list> + PRYLINT_DUMP_COUNTS=1 vs
     dump_infer_count.py, then trace_gt_ni2.py/trace_gt_hit.py (in /tmp,
     copy into harness/ if needed) event-stream diffs); (2) brain_ssl
     counted probe ±1 (EnumType.__new__ metaclass-lookup pull-count;
     single probe line); (3) pylfunc NOTREE x3 (tree-fidelity owns) +
     os.environ GT-env noise; (4) GT wipe-frequency parity: astroid wipes
     the global cache during mid-dump template builds more often than we
     do (e.g. Name cast ##3 vs ##0 in frame.py) — audit which template
     builds apply tip transforms.
   - **phase 8 (diff-reduction round 7, N=1000)**: 2.07k -> 1.16k diff
     lines. LANDED (probe-verified, harness/infertests now 58 probes):
     (a) **context-threading parity batch**: ClassDef.getitem threads the
     LIVE context into dunder_lookup (dunder_lookup.py:60-67 ->
     metaclass(context) -> declared_metaclass base inference under the
     shared counter cell); declared_metaclass(None) gives each
     base.infer(None) its OWN fresh cell (scoped_nodes.py:2640-48);
     _inferred_bases normalizes ONE fresh ctx for the bases walk (clones
     share the cell) while _compute_mro recursion keeps passing None.
     (b) **_metaclass_lookup_attribute @lru_cache(1024)** keyed
     (cls, name, context-IDENTITY) — context=None keys are stable for the
     whole run: repeated no-context getattrs replay the set with ZERO
     re-inference (huge event-stream aligner; scoped_nodes.py:2375).
     (c) **brain_typing infer_typing_cast** ported (cast(typ, val) ->
     single-pull func check, then val.infer under the live ctx) — was THE
     'val vs dtype' divergence from phase 7: frame.py cast() sites now
     bind through the tip.
     (d) **Lambda.infer_call_result** = body.infer (scoped_nodes.py:987)
     — lambda properties (django's `url = property(lambda self: ...)`)
     now solve through ClassDef.igetattr's Property branch (django
     middleware cluster, -83 lines).
     (e) **Instance subscript index parity** (node_classes.py:3752-58):
     Instance owners receive the INFERRED index — we passed the raw slice
     node, leaking wrong CallContext args into nested __getitem__ chains
     (ResponseHeaders[key.lower()] etc.).
     (f) InstanceModel.__dict__ = _dunder_dict(instance_attrs) — Dict:N
     of (Const name, LAST assign node) (objectmodel.py:49-68, 747-49).
     (g) NodeNG.__str__ heads for ClassDef/FunctionDef/Module in
     f-string format() of non-Const results (asv_bench hdf.py cluster).
     (h) container tips: safe_infer results that are Uninferable are
     SKIPPED in _container_generic_transform (bool(Uninferable) is False,
     brain_builtin_inference.py:281; salt vsphere Tuple:0); infer_dict's
     _get_elts is next(arg.infer(ctx)) SINGLE pull with exact
     is_iterable/pair/key-kind checks (dict(parse_qsl(q)) -> Dict:0).
     (i) extra_decorators: `meth = frame[name]` is locals[name][0] (FIRST
     local) — `as_manager = classmethod(as_manager)` -> BoundMethod.
     (j) **_islots streaming abandonment** (scoped_nodes.py:2728-29):
     `return values` on an EMPTY slots container abandons the igetattr
     generator AT ITS YIELD — suspended AssignName/Tuple NodeNG.infer
     frames never cache, so later mro walks re-MISS them exactly like
     astroid (typing._NotIterable re-inference pattern).
     TOOLS: harness/diff_infer.py (aligned GT/RS value diffs per file);
     WALK/SLOTSOF/ALLSLOTS/SCAN/WIPE markers under PRYLINT_TRACE_INFER;
     trace yields now print ni=; /tmp/seg_diff.py + flat-event diffing
     with ni-wildcards for GT context=None calls (ni=-1).
     N=1000 differing files/lines: django 14/57, pylfunc 10/15, pandas
     156/827, salt 27/132, airflow 21/76, sentry 10/25, core 11/23.
     REMAINING (by volume): (1) pandas cap-cliff count drift (~800 lines)
     — context-CELL accounting still diverges deep in groupby/frame/
     nanops chains (e.g. notna probe: GT ##114 vs ours ##109, values
     truncate at the 100-cap one pull apart; localized next divergence:
     `type(list[int])` in _collections_abc ancestors walk accumulates
     cell bumps differently around the builtin type() tip / GenericAlias
     getitem). asv U-vs-ERR (~160 lines) and frame.py GT-extra clusters
     are downstream of the same cap timing. (2) enum-class CALL result
     (TableauJobFinishCode(x): GT solves EnumType.__call__ ->
     cls.__new__ -> member-fake-class chain to Inst:<enum cls>; ours
     reaches real Enum.__new__ -> Class:enum.EnumType + Const None×2;
     ~7 airflow + few salt lines). (3) salt _Constant value-order (18) +
     ImmutableDict instance_attrs (22). (4) pylfunc NOTREE x3 +
     os.environ GT-env noise (irreducible here). (5) six.with_metaclass
     call-result synthesis still unwired (no diff evidence at N=1000).
     (6) recursion guard still 350 (no diff evidence at N=1000).
   - **phase 9 (diff-reduction round 8, N=1000)**: 1.16k -> 970 diff
     lines. LANDED (probe-verified, harness/infertests now 61 probes):
     (a) **LookupMixIn.lookup `@lru_cache` maxsize=128 EXACT**
     (_base_nodes.py:262 — the DEFAULT size, not unbounded!): one tiny
     GLOBAL LRU over (node, name); hits refresh recency, inserts at
     capacity evict the LRU entry. Evictions are SEMANTIC: a re-miss
     recomputes against LIVE module locals — cross-module delayed_assattr
     (salt compat.py `copy._deepcopy_dispatch = pre_dispatch` lands in
     copy.py's locals AFTER copy was built; the stale 128-LRU entry ages
     out and the recompute sees it) and re-mints fresh module-model
     Consts. Our old unbounded memo replayed stale lookups forever
     (salt _Constant value-order cluster; salt 132 -> 99).
     (b) **cap-cascade cache parity** (node_ng.py:163-167): after the
     truncation `yield Uninferable` the wrapper is SUSPENDED before
     `break`; the cache write runs ONLY if the consumer pulls again —
     regardless of how the producer ended. A producer completing Done
     after the truncation Stop previously hit the unconditional cache
     branch, freezing every mid-cascade node of a cap blow (GT re-burns
     ##103 per follow-up dump node, we replayed ##0; groupby.py
     count-diff lines 63 -> 1).
     (c) **Instance.getitem igetattr under the ORIGINAL ctx**
     (bases.py:421): only infer_call_result gets the boundnode-bound
     new_context; the __getitem__ MRO/ancestors walks run bn-free and
     hit/write the (node, name, None, None) global cache keys (the
     asv groupby subscript ERR-vs-U cluster's root count drift).
     (d) ancestors()/_inferred_bases unproxy astroid-`Instance` baseobjs
     (Const/containers!): `class Color(Enum)` base Name infers to
     [Const None, Class Enum] -> NoneType+object enter the ancestors
     stream FIRST -> igetattr's same-scope filter keeps object.__new__
     -> metaclass-call chain solves Color(x) -> Inst:Color (airflow
     RetryAction/TableauJobFinishCode cluster). ClassModel.attr___call__
     = instantiate_class (objectmodel.py:707-710) — calling a Const None
     burns the '__get__' descriptor-check metaclass walk before failing.
     (e) helpers._object_type uses the caller's ctx AS-IS (helpers.py:43
     — no copy: ln/path flow through; type(self) under a Property
     igetattr keeps ln='_constructor'); infer_issubclass pulls the OBJ
     arg FIRST (UseInferenceDefault before the 2nd arg is touched),
     infer_isinstance the container first.
     (f) **PY_FROZEN spec table**: pyenv probe captures
     `_imp._frozen_module_names()` + FrozenImporter loader_state.filename;
     importlib_finder ports spec.py:169-192 (stdlib-gated, AFTER the
     search-path scan). `import _frozen_importlib as _bootstrap` now
     resolves to astroid's EMPTY stub module instead of failing
     (importlib.import_module chains in pandas nanops).
     TOOLS: @@DUMPNODE sentinels in both tracers (dump.rs +
     /tmp/trace_gt_ni5.py with WIPE markers), /tmp/cmp_dumptrace.py
     (per-dump-node normalized event-stream diff + SequenceMatcher
     ni-drift localizer), prefix-bisect workflow for cross-file cache
     archaeology (PRYLINT_DUMP_ONLY={prefix files + target}).
     N=1000 differing files/lines: django 14/57, pylfunc 10/15, pandas
     133/686, salt 23/99, airflow 18/65, sentry 10/25, core 11/23.
     REMAINING (by volume): (1) pandas count/cache drift (~600 lines):
     frame.py self.columns/index instance_attr walks — tuple-target
     AssignAttr (`ts.index, ts.columns = rng, rng` at
     tests/frame/methods/test_at_time.py:106) replays a stale [U] for
     the rhs Tuple where GT re-infers (suspected: assigned_stmts value
     pull keyed without the live cc/bn identity, or an earlier
     truncation froze (tuple,None,None,bnkey) — trace pair saved in
     /tmp/trace_{gt,rs}_fr.txt, first diverging attr = #132 of 180 in
     @@DUMPNODE 1426:57:Attribute); nanops cap-cliff (62 lines, first
     count drift now at 143:29 ##9 vs ##7 — FunctionDef.type/_is_property
     ctx-None decorator pulls around @overload defs, GT re-walks
     ImportFrom chains we replay — likely ALSO the 128-LRU on lookup
     interacting with decoratornames); asv U-vs-ERR residue (36). (2) django
     test_writer types.NoneType 4th value (12) + ErrorDict cluster (9).
     (3) salt ImmutableDict/log_parsers GT-extra Inst values (16) +
     rsax931 trailing Const None (23). (4) pylfunc NOTREE x3 + os.environ
     noise (irreducible). (5) six.with_metaclass still unwired (no diff
     evidence). (6) recursion guard still 350 (no diff evidence).
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
