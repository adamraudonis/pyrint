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
   - **phase 10 (diff-reduction round 9, N=1000)**: 970 -> 496 diff lines.
     LANDED (probe-verified, harness/infertests now 69 probes):
     (a) **inference_tip 64-FIFO EXACT** (inference_tip.py:22-86): the
     OrderedDict caps at 64 with popitem(last=False); EVERY successful
     miss INSERTS — non-empty-context entries are keyed by InferenceContext
     OBJECT IDENTITY (we pin the Rc in the entry so the pointer can't
     recycle) and virtually never hit, but they EVICT the useful None-keyed
     entries, so astroid re-runs tips constantly where our old
     empty-ctx-only cache replayed forever. Guard trips REMOVE the
     in-flight key (re-entry allowed once); a tip raising mid-stream drops
     its partial yields (eager `list(func(...))`). Per-node
     `node._explicit_inference = lambda` replacements (typing alias/
     special-alias/Generic-Annotated subscripts) bypass guard+FIFO
     entirely; infer_typedDict re-runs per miss (no per-node memo).
     (b) **tips run with context=None**, NOT one shared fresh ctx: every
     internal node.infer(None) materializes its OWN InferenceContext
     (Ctx::new_none() marker + infer_to substitution; clone of the marker
     = plain fresh ctx, mirroring copy_context(None)).
     (c) **lazy _resolve_assignment_parts** (protocols.py:482-519): the
     rhs `parts` generator is pulled lazily; every `return` ABANDONS the
     suspended chain mid-stream (no unwind bumps / truncated-wrapper cache
     writes); `if not assigned: return` fires for U regardless of
     remaining path. This alone was ~300 pandas lines (tuple-target
     `ts.index, ts.columns = rng, rng` stale-[U] cluster from phase 9).
     (d) **model-attr infer hops**: objectmodel attrs returned as FRESH
     NODES (ClassModel __name__/__qualname__/__module__/__doc__/__mro__/
     __bases__/__dict__/__annotations__, FormattedValue's `Const("")`
     spec) get a full NodeNG.infer hop via model_hop_node (fresh synth
     node + redirect; bump/cap/cache exactly like astroid); Proxy results
     (attr___call__, mro, __subclasses__) stay hop-free; __class__ hops
     on the REAL object_type node.
     (e) **single-pull/abandonment parity batch**: Dict.getitem lazy key
     scan (`return value` abandons the key generator), helpers.object_len
     (`next(igetattr("__len__"))` + `next(inferred, None)`; empty result
     -> Const 0), PartialFunction.__init__ SECOND fresh
     `next(wrapped.infer())` (+ nested Partial filled-arg merge),
     _get_namedtuple_fields SECOND fresh `next(args[1].infer())` +
     field_names keyword fallback + BaseContainer gate, infer_enum's
     any() stopping at the first enum.Enum ClassDef.
     (f) **class_getitem AttributeError parity** (scoped_nodes.py:
     2575-2595): methods without infer_call_result (e.g. os.PathLike's
     `__class_getitem__ = classmethod(GenericAlias)` AssignName) raise
     -> Subscript._infer -> InferenceError (was: swallowed to U);
     infer_call_result errors propagate.
     (g) **_wrap_attr BM re-walk** (bases.py:304): BoundMethod IS an
     UnboundMethod subclass — classmethods from the class-igetattr
     fallback re-run the FULL un-contexted _is_property walk and re-wrap.
     (h) **brains**: ctypes + curses module extenders (locals REPLACE,
     gen_ext_templates.py regenerated — fixes `Class:c_long | c_int`
     pairs), functional `Enum("X", "a b")` call brain (EnumMeta template
     per invocation, synthetic class instance), FULL format-spec
     mini-language for f-string Const folding (fill/align/sign/#/0/width/
     grouping/precision/types s d b o x X c e E f F g G n % — probe
     fstring_format_spec.py covers 65 cases), f-string str() folds for
     bytes/complex/Ellipsis consts.
     (i) **lambda-in-decorator lookup**: `parent_function.lookup(name)`
     uses the FUNCTION as filter base node (is_from_decorator doesn't
     re-fire), so the higher-function-scope fallback resolves params
     (pytest parametrize lambda cluster, was ERR-vs-U).
     TOOLS: /tmp/cmp_dumptrace2.py (ENTRY-event + entry-ni comparator —
     relay-yield prints are tracer artifacts at differing depths and are
     ignored); trace_gt_ni6.py (= ni5 + `del sys.modules['dump_infer']` —
     the tracer import polluted sys.modules vs the __main__ cache-warm
     run, making sys.modules Dict:204 vs 203 a phantom diff).
     N=1000 differing files/lines: django 12/39, pylfunc 10/15, pandas
     78/305, salt 13/71, airflow 10/31, sentry 6/16, core 9/19 (total
     496; also landed: nested functools.partial wrapped-function —
     PartialFunction isinstance FunctionDef). REMAINING (by volume): (1) pandas count drift continues
     (~250 lines): copy_view/test_methods bool tails (GT folds
     `df.method(copy=copy)` chains one branch further), `[None] * n`
     binop operand-pull event diffs (values+counts re-sync; cache-state
     second-order only), asv U-vs-ERR residue. (2) salt zypperpkg
     `Inst:dict | Dict:0` vs U (18) + ModuleType instance_attrs Dict:9
     vs Dict:0 (test_path) + ipaddress GT-cap-earlier chains. (3) django
     as_view __doc__ pair / backend_class __module__ ordering (each
     needs a per-node trace). (4) pylfunc NOTREE x3 + os.environ noise
     (irreducible). (5) six.with_metaclass still unwired (no diff
     evidence at N=1000).
   - **phase 17 (diff-reduction round 16, sample=ALL files)**: 42 -> 26
     diff lines; **CORE joins django/pandas/sentry at ZERO** (core 0/0,
     django 0/0, pandas 0/0, sentry 0/0, airflow 3/8, salt 5/13, pylfunc
     4/5; tree gate 0, shell gate PASS x7, 150 probes PASS). KEY WORKFLOW
     UNLOCK: GT-side dump_infer.py with FULL corpus items +
     --only=<target> reproduces the cached dump EXACTLY for most
     remaining files (prebuild state is what matters, not earlier dumped
     files) — single-target GT trace sessions became cheap (verified for
     airflow secrets_masker/spark_sql/ctl + ALL core targets; salt http
     is prefix-dependent BUT its 4 diff lines reproduce in only-mode
     too). LANDED (probe-verified):
     (a) **TryStar class-level exceptions** (protocols.py:553-556):
     ExceptionInstance is a Proxy — `assigned.instance_attrs["exceptions"]
     = [List.from_elements(...)]` writes through to the
     builtins.ExceptionGroup ClassDef GLOBALLY (last except* site wins,
     REPLACE); plain `except ExceptionGroup as eg` and direct
     instantiations see the leaked List via instance_attr-before-model
     order (bases.py:249). ONE synthetic redirect node per mutation
     (re-reads replay its cache entry). Kills the old per-ExcInst
     double-wrap (List:1 for every except* tuple). The throwaway
     extract_node Name lookup burns a 128-LRU slot BEFORE the
     handler-type unpack (lookup_burn_throwaway). core ring/coordinator.
     (b) **synthetic stmt-infer hops apply the 100-cap truncation**
     (node_ng.py:160-167): a cache-MISS hop over a synthetic value (model
     Tuple of `.args` at ni>100) yields Uninferable INSTEAD of the value,
     NO bump, caches [U] (synth_hop_trunc twin set; replays yield U;
     proxies exempt; Property/Partial always-hop truncate too). The miss
     let over-cap exception-model accesses yield REAL values where
     astroid's U is path_wrapper-deduped away -> extra trailing values +
     uncached abandoned producers flipping the NEXT dump node from
     1-event replay to full re-infer (airflow secrets_masker -4).
     (c) **tl-concat synthetic elements get the FULL elt.infer drain hop**
     (_filter_uninferable_nodes, protocols.py:161-172): astroid's
     accumulated aug-chain lists hold re-materialized REAL nodes (str()
     folds are fresh Consts) — each `+=` step re-drains every element
     under the SHARED ctx with the step's own CallContext key: re-miss +
     BUMP per step per synthetic elt (synth_elt_full_drain; over-cap ->
     U -> UNATTACHED_UNKNOWN elt; EvaluatedObject drains to its wrapped
     value). airflow spark_sql -2.
     (d) **Dict.getitem key-infer raises ABORT the scan**
     (node_classes.py:2307 — `for inferredkey in key.infer(context)` has
     NO try): a path-blocked/unresolvable key propagates InferenceError
     out of Dict.getitem through Subscript._infer to _infer_stmts' yield
     U. We kept scanning and solved subscripts astroid leaves U. core
     test_history -3 AND config_validation -1.
     (e) **_is_str_format_call applicability FIXED at transform-scan
     time**: the predicate's safe_infer(node.func.expr) runs ONCE during
     the module's scan (node._explicit_inference is attached THEN);
     infer-time re-evaluation pulled the expr under live state.
     str_format_calls side table (like pathlib_subscripts). airflow
     ctl -1.
     (f) **Super cache-boundnode OBJECT identity**: objects.Super has no
     __eq__ — astroid keys the cache boundnode slot by id() and every
     super() Call._infer builds a FRESH Super. Our structural
     (mro_pointer, self_class) key false-HIT [U] entries across distinct
     super() evaluations (iron_os 498:17 replayed line 465's truncation).
     Identity = the per-construction mro_type Rc pointer (pinned).
     core iron_os + hassio + shelly -3.
     (g) **dataclass lambda default_factory re-parse + reparent-aware
     annotation root** (brain_dataclasses.py:430-432, :614): the factory
     re-parse `(lambda: ...)()` (Call.as_string precedence parens) gives
     the synthetic Call a REAL infer hop + fresh template Lambda;
     _infer_instance_from_annotation uses the REPARENT-AWARE root name
     (extender-template defaultdict lives in a ''-named module -> the ''
     branch wrongly yielded U). core esphome -1 -> core ZERO.
     REMAINING (26 lines): (1) irreducible-by-construction (~19):
     os.environ content/order (airflow conf 2/parser 3/setup_idea 3,
     pylfunc unused_import 2), PYTHONHASHSEED (salt test_man 4),
     random.sample RNG (salt deltaproxy 2), sys.path_importer_cache
     growth (salt lazy 3), pylfunc NOTREE x3 (pyast/ruff rejects
     `""\\<EOF>` CPython accepts — tree-fidelity owns). (2) salt
     http.py 4 (reducible, needs its own session): `opts.get(key,
     DEFAULT_MINION_OPTS[...])` — the `default` param's CallSite
     safe_infer sees ambiguity/raise in GT (-> U) where we complete with
     ONE value (Inst:ImmutableDict); the third-Subscript subtrees are
     ENTRY-IDENTICAL on both sides (155 events) — the divergence is in
     the value/ambiguity stream around Instance.getitem of the
     freeze()-chain owners (ImmutableSet has no __getitem__: getitem
     raise mid-stream should abort like (d) does for keys); traces in
     /tmp/{gt,rs}_http_trace.err seg 800, reproduces with
     PRYLINT_DUMP_ONLY/--only single-target runs.
   - **phase 16 (diff-reduction round 15, sample=ALL files)**: 64 -> 42
     diff lines; django 0/0, pandas 0/0, sentry 0/0 (first corpora at
     ZERO), pylfunc 4/5, salt 5/13, airflow 6/15, core 7/9 (tree gate 0,
     shell gate PASS x7, 144 probes PASS). LANDED (probe-verified):
     (a) **%-format dict rhs** (CPython unicodeobject.c unicode_mod arg
     model): non-tuple rhs is ONE positional value (the object itself —
     `"%s" % {}` folds to '{}'); a NAMED conversion REPLACES ctx->args and
     resets argidx (unicode_format_arg_parse), so '%(a)s %s' % d is
     TypeError but '%s %(a)s' % d works; dict rhs skips the trailing
     not-all-converted check ('x' % {} -> 'x'); mapping keys are ANY Const
     with python dict insert semantics (first position, last value);
     dict-as-value renders via repr (django rasterfield).
     (b) **ClassDef.igetattr same-scope filter exact** (scoped_nodes.py:
     2442-2449): parentless ClassModel consts (__doc__/__name__/
     __qualname__/__module__, objectmodel.py:499-513) DROP per the
     `attr.parent and` guard; Inst proxies delegate .parent to their class
     (django base.py cls.__doc__ no longer leaks type's doc).
     (c) **Compare sequence ordering**: tuple/list lexicographic fold
     (first `!=` element decides via lit_cmp, shorter is less), set
     subset relations; `psycopg_version() >= (3, 2)` folds to False via
     Tuple:0 >= (3,2) (django introspection).
     (d) **unpack rhs raise**: Instance-without-__getitem__ getitem raises
     InferenceError OUT of _resolve_assignment_parts — AssignName._infer's
     EAGER list(assigned_stmts) (node_classes.py:451) discards earlier
     parts -> single U (django distapp `dist1, dist2 = dist`); same for
     for/comprehension assigned_stmts mid-stream iter raises (ListComp
     values have NO inference function -> the whole target U; django
     test_templatetags spec["choices"]).
     (e) **getattr() brain hasattr-igetattr gate**: Lambda/Unknown/
     EvaluatedObject lack igetattr -> Uninferable, NEVER the default
     (django test_middleware get_redirect_field_name(lambda: None)).
     (f) **NamedExpr/Starred falsy raise** in _infer_sequence_helper:
     safe_infer -> Uninferable is FALSY (`if not value: raise`), the
     walrus literal poisons whole (sentry test_snowflake).
     (g) **count-exact _infer_slice**: lower/upper/step ALL inferred
     before the all() check (node_classes.py:222-226) and Const.getitem
     classifies the index FIRST — _infer_slice runs for ANY Const receiver
     before the str/bytes check raises (the discarded pulls warm the
     GLOBAL _INFERENCE_CACHE: pandas sas7bdat went count-byte-identical,
     fixing its f-string cap truncations).
     (h) **object_len const fall-through** (helpers.py:276-291): None/int
     consts run object_type + __len__ lookup before AstroidTypeError
     (pandas test_indexers).
     (i) **Property/Partial stmt-infer hop**: objects.Property/
     PartialFunction subclass FunctionDef (objects.py:334) — _infer_stmts'
     stmt.infer on them is a FULL NodeNG.infer hop, always-fresh (never
     replays); django model_enums enum-list truncation now exact.
     TOOLING: harness/show_inferdiff.py (per-file GT-vs-ours diff lines);
     trace-block comparator pattern (cmp_blocks: GT trace_gt_file.py
     blocks vs @@DUMPNODE blocks, ENTRY-event streams) localizes count
     divergences to single dump nodes — used for (g)/(h)/(i).
     REMAINING (42 lines): (1) irreducible-by-construction (~21):
     os.environ content/order (airflow conf 2/parser 3/setup_idea 3,
     pylfunc unused_import 2), PYTHONHASHSEED (salt test_man 4),
     random.sample RNG (salt deltaproxy x2 files 4), sys.path_importer_
     cache growth (salt lazy 3), pylfunc NOTREE x3 (pyast parser rejects
     `""\\<EOF>` where CPython ast.parse accepts — tree-fidelity owns).
     (2) lazy-module-build count cascades (~19): brain transform-time
     inference during LAZY builds (urllib.parse namedtuple chains, frame
     f_back/inspect, collections.namedtuple type(...) source returns)
     burns counts in different ORDER than ours -> 100-cap lands one stmt
     early/late under full-prebuild global-cache state: salt http.py 4
     (tl-concat _filter_uninferable extra Tuple entry), airflow ctl
     format 1 / spark_sql aug-chain 2 / secrets_masker 4 (GT caps after
     5th ExcInst at ##111, ours ##104 — divergence starts at the
     inspect.currentframe dump node), core singles 9. Each needs its own
     trace-block session against the FULL-corpus prebuild state
     (single-file and pair count dumps all MATCH — the drift is
     cross-file cache warmth only).
   - **phase 15 (diff-reduction round 14, sample=ALL files)**: 126 -> 64
     diff lines (django 7/10, pylfunc 4/5, pandas 4/7, salt 5/13, airflow
     7/17, sentry 1/2, core 8/10; tree gate 0, shell gate PASS x7, 135
     probes PASS). LANDED (probe-verified):
     (a) **snapshot OBJECT IDENTITY** (the StrEnum/mixin fix, core -47):
     gen_snapshot.ser() now dedups by id() — `{"k":"Ref","r":i}` whenever
     the SAME astroid object recurs (raw_building re-attaches one node at
     many positions: builtins.type in every exception's __class__ locals,
     OSError==IOError==EnvironmentError body re-appends, object.__base__);
     "parfix" side map records nodes whose ser position != astroid's final
     .parent (last add_local_node attach wins) and the loader rewires.
     Identity is load-bearing: `cls != self` in _metaclass_lookup_attribute
     (scoped_nodes.py:2383) — our duplicated type made MLA(type,'__new__')
     run a spurious GAFM -> +4 bumps in EVERY enum-mixin call chain
     (S(None) ##112 vs GT ##108 -> exact). CAUTION: a qn/content-based
     LOADER dedup was tried first and OVER-MERGED distinct-but-identical
     raw builds (sys.excepthook vs sys.__excepthook__ — astroid builds a
     fresh FunctionDef PER MEMBER NAME; airflow structlog regressed) —
     id()-at-ser-time is the only correct identity source. Snapshots
     regenerated (7 files changed; sys.json BYTE-IDENTICAL via
     regen_sys_snapshot.py — env reproducibility confirmed). MLA also
     collects into an identity-deduped set (attrs=set(); id() since NodeNG
     has no __eq__).
     (b) **tl-concat element raises propagate** (protocols.py:161-172
     _filter_uninferable_nodes lets elt.infer raises out of list(chain());
     _base_nodes.py:650-652 `except InferenceError` — NameInferenceError
     is a subclass — converts to ONE Uninferable and stops):
     `conditions += [{**base, ...}]` with an uninferable element is U,
     not List:N (core device_condition -19, django formsets/related).
     (c) **namedtuple+Enum mixin count parity**: ClassDef.basenames is
     [b.as_string() for b in bases] — the FULL text; our dotted_string
     DROPPED Call bases so member fakes lost the namedtuple(...) base that
     astroid's ancestors walks infer PLAIN through the real
     collections.namedtuple (+13 bumps/member); + the namedtuple tip runs
     util.safe_infer(extract_node("import collections;
     collections.namedtuple")) PER INVOCATION (fresh throwaway module,
     fresh-ctx Attribute chain) — probe ##61/##99 exact (airflow
     simple_auth_manager).
     (d) DictItems/DictKeys/DictValues path_wrapper identity dedup
     (DedupKey::Ptr on the DictRef Rc — not exact-class "Instance", so
     id() dedup; django test_choices).
     (e) CPython-exact complex const binop folds (complexobject.c port:
     Smith's _Py_c_quot, c_powi |n|<=100 repeated squaring, polar
     _Py_c_pow; TypeError ops -> NotImplemented, ZeroDiv/ERANGE -> U)
     — num_of(complex) was None -> U (pandas test_box_unbox/test_nanops).
     (f) ModuleModel attrs hop through model_hop_node (fresh Const/List/
     Unknown per access, objectmodel.py:167-241) — panel.__name__ chains
     count-exact (core config/__init__ truncation point).
     TOOLS: GETATTR/MLA/GAFM/IGA-ATTRS markers under PRYLINT_TRACE_INFER.
     REMAINING (64 lines, by volume): (1) irreducible-by-construction
     (~24): os.environ content/order (airflow conf 2/parser 3/setup_idea 3,
     pylfunc 2), PYTHONHASHSEED set order (salt test_man 4), random.sample
     RNG (salt deltaproxy 2), sys.path_importer_cache growth (salt lazy 3),
     pylfunc NOTREE x3 (tree-fidelity owns). (2) context-dependent
     cap/ctx singles, each needs its own prefix-trace session (~40):
     core trailing-value flips (esphome/shelly/hassio/iron_os/test_entity
     +1 value, test_history List:0-vs-U x3, config_validation ERRvsU,
     ring TryStar List:1 — needs class-level exceptions storage),
     django (middleware 'next' 2, model_enums 1, templatetags 2,
     rasterfield 1, distapp 2, introspection 1, base.py __doc__ 1),
     pandas (sas7bdat 2, to_latex 2, fiscal 2, indexers 1), salt http 4,
     airflow (ctl format 1, spark_sql aug-chain 2, secrets_masker 4,
     cli_parser 2), sentry test_snowflake 2. (3) six.with_metaclass
     call-result synthesis still unwired; recursion guard still 350 (no
     diff evidence at full-corpus sample).
   - **phase 14 (diff-reduction round 13, sample=ALL files)**: 298 -> 126
     diff lines (round logs /tmp/inferdump_all_round{1,2,3}.log; final:
     django 10/15, pylfunc 4/5, pandas 6/9, salt 6/14, airflow 8/22,
     sentry 1/2, core 16/59; tree gate 0, shell gate PASS x7, 130 probes
     PASS). LANDED (probe-verified):
     (-) **infer_slice safe-infers ALL args EAGERLY under the SHARED
     context** (brain_builtin_inference.py:687-688 list comprehension runs
     BEFORE validation; bumps land even when a later check bails to
     default) — pandas test_indexing 40 -> 9 (astroid caps at the slice
     callee; we used to bail after the first non-Const arg).
     (a) **EvaluatedObject elements** (new Value variant): container tips
     wrap mixed-branch elements (brain_builtin_inference.py:283-285);
     infer hop yields the inner value, but NO getitem on the element —
     loop-unpack `stmt.getitem` AttributeError -> continue
     (protocols.py:268-276). Killed the 50-line salt service.py cluster.
     unpack_infer recurses through them (except tuple(MAP.keys()) as e).
     (b) **per-InstId proxy-class instance_attrs** (helpers.py:39-57
     _build_proxy_class is FRESH per evaluation): delayed assattrs on
     instances of function/module/method/builtin_function_or_method land in
     proxy_iattrs[(cls, InstId)]; transform WIPEs + fresh re-derivation
     decide later visibility exactly like astroid (probe pair:
     module-level visible / function-level invisible). Killed salt
     functools/test_path/lazy Dict:N, sentry importer, core frame.py.
     (c) **sys.argv/orig_argv reconstruction**: the sys snapshot rebuilds
     them at load to match warm_infercache.sh's dump_infer.py invocation
     (django autoreload cluster).
     (d) **UnboundMethodModel gating**: UM model = ObjectModel-based
     (__class__/__func__/__self__/im_* only); everything else hops through
     FunctionDef.igetattr/_infer_stmts (count parity as_view ##5).
     BM.__func__ = `._proxied._proxied` -> AttributeError for BMs proxying
     a FunctionDef directly (class-access classmethods -> ERR).
     (e) **PropertyModel.attr_fset find_setter** evaluates `.name` on EVERY
     class child -> AttributeError on nameless kinds (Assign/Attribute/
     Keyword children; django ChoiceField.choices.fset -> ERR); synth
     properties (parent SYNTHETIC_ROOT) have no children -> InferenceError.
     (f) **Lambda has NO igetattr** (only getattr, scoped_nodes.py:1047-60):
     Attribute on a Lambda owner -> AttributeError -> owner skipped.
     (g) str.format folds {x!r}/{x!s}/{x!a} conversions; exact decimal
     bigint add/sub past i128 ((2**128)-1); extender-template Lambda
     renders root().name through reparents.
     REMAINING (126 lines, by volume): (1) ONE dominant count-parity root:
     the StrEnum/IntEnum **mixin-enum CALL chain** (core sensor trio 31 +
     device_condition 19 + airflow simple_auth_manager 5; minimal repro:
     `class S(StrEnum): pass; S(None)` gives GT##108 vs RS##112 — plain
     Enum and metaclass-__call__ probes MATCH). Corpus trace and the
     minimal probe diverge IDENTICALLY: in the `metacls.__new__` resolution
     (enum chain) GT pulls `ClassDef type` 6x then ONE FunctionDef __new__
     (no bump, consumer abandons); we pull 7x type + extra
     `Name object/ClassDef object` (+1 bump) + THREE FunctionDef pulls
     (+2 bumps) -> the shared cap fires ~4 pulls later than astroid
     downstream. Corpus-faithful trace replay procedure (full items +
     --only=<prefix paths> on GT / PRYLINT_DUMP_ONLY + PRYLINT_TRACE_START
     WITHOUT PRYLINT_TRACE_INFER on RS) verifies byte-exact vs the cache —
     traces in /tmp/gttrace_core_sensor2.err + /tmp/rstrace_core_sensor.err
     and /tmp/probe5. (2) context-dependent ERR-vs-U / cap singles: ibm/mq
     conftest (4 — our module-instance attr stays visible where GT's
     transform-wipe re-derivation lost it), salt http.py (4), django
     test_middleware/test_formsets/admin_views/distapp/introspection/
     rasterapp/test_choices/related/base (12), core config/shelly/hassio/
     iron_os/config_validation/entry_data (7), airflow secrets_masker/
     cli_parser/airflowctl/spark_sql (6), sentry test_snowflake (2),
     salt build.py/test_clear_funcs (2), pandas singles (9: sas7bdat,
     test_fiscal, test_box_unbox, test_nanops, test_to_latex). (3)
     irreducible GT-environment noise (~21):
     os.environ content/order (pylfunc 2, airflow 7), PYTHONHASHSEED
     set-iteration order (salt test_man 4, deltaproxy RNG 2), live-process
     sys.path_importer_cache growth (salt lazy.py 3), pylfunc NOTREE x3
     (tree-fidelity owns). (3) TryStar `assigned.instance_attrs[...] = ...`
     mutates the eg CLASS instance_attrs globally in astroid (core ring
     coordinator List:1, 1 line) — needs class-level exceptions storage.
     (4) two known count off-by-ones in the suite (typeddict_lru_strenum
     ##111 vs ##110; standalone StrEnum(None) call ##108 vs ##112).
   - **phase 13 (diff-reduction round 12, sample=ALL files)**: 530 -> ~280
     diff lines (final numbers in the round log /tmp/measure_round6.log;
     round-5 checkpoint: pylfunc 5, django 30, pandas 42, salt 99, airflow
     56, sentry 5, core 61). LANDED (probe-verified; harness/infertests now
     121 probes):
     (a) **dataclass __init__ SYNTHESIS** (_generate_dataclass_init full
     port, brain_dataclasses.py:244-390): parsed init template installed in
     class locals (qname <cls>.__init__ via reparent), _HAS_DEFAULT_FACTORY
     root local, base-class param merging via a side table standing in for
     Arguments._get_arguments_data, kw_only quirks ("self" SUBSTRING check
     on the rendered prev-params string), is_dataclass FLAG (no re-infer at
     render). Pull/WIPE parity in the transform: per-field is_class_var
     (next) + kw-sentinel (safe_infer) + _is_init_var (cache-hit) pulls,
     per-field Unknown-tip predicate re-check + WIPE, pass-4 re-pulls
     (property decoratornames, field-call func pulls, prev-default mro
     walks). Killed core llm.py super().__init__ + esphome counter drift.
     (b) **module-extender templates build with modname ''** (astroid
     parse() default) then REPARENT top-level objs into the target module
     (brain/helpers.py:25-27): name-gated scan predicates (collections
     _looks_like_subscriptable) correctly FAIL at template-scan time —
     defaultdict/deque subscripts now hit the EmptyNode -> `return self`
     path of ClassDef.getitem instead of calling an injected
     __class_getitem__ (core filter/sensor deque cluster); typing-alias tip
     resolves the base import via the reparent-aware root walk.
     (c) **enum __members__ values are the LAZY locals proxies** (Name refs
     in astroid, brain_namedtuple_enum.py:489-507) — no second
     instantiate_class, no second per-member INFBASES walk at scan time.
     (d) **walrus double-stmt**: NamedExpr.optional_assign = True makes
     _filter_stmts keep BOTH copies (the `_stmts = [node]` branch THEN the
     unconditional append; filter_statements.py:143-159,195,227); the
     duplicate collapses through value-cache replay + the new BoundMethod
     REPLAY-IDENTITY dedup (DedupKey::BMId on the bound Rc pointer —
     mirrors id(BoundMethod); update/__init__ latest_version pairs).
     SynthSlice dedups by bounds-Rc likewise (pandas
     _convert_slice_indexer repeated `return key`).
     (e) **are_exclusive handler-vs-handler**: astroid's locate_child
     returns the whole LIST for sequence fields, so `c1node is not c2node`
     is a FIELD-identity check — two different ExceptHandlers share
     `handlers` and take the elif (exclusive). Killed the salt
     templates.py dup-accumulation + dsmr/excl clusters.
     (f) **BoolOp operand mid-drain raise PROPAGATES** (only generator
     CREATION is inside the try, node_classes.py:1651-57; product() drains
     outside) — django get_system_encoding locale cluster ('iso-8859-15'
     fold -> U).
     (g) **brains**: brain_statistics (quantiles -> U, syntactic predicate),
     brain_random sample (real sampling at inference time; our selection is
     a deterministic LCG — the warm oracle's RNG selection is IRREDUCIBLE,
     only the List:k shape matches), lru_cache attr_cache_info
     (CacheInfoBoundMethod -> Inst:__astroid_synthetic.CacheInfo via fresh
     _CacheInfo(0,0,0,0) extract per access), GroupExceptionInstanceModel
     (eg.exceptions -> fresh EMPTY Tuple, exact builtins.ExceptionGroup),
     ClassModel attr_mro (MroBoundMethod proxying builtins.type.mro, call
     -> Tuple of mro), Generator ContextManagerModel __enter__/__exit__
     (synthetic defs qname builtins.object.*, call -> Const None; salt
     minion span cluster), ObjectModel __init__/__new__ BMs on
     function/UM/BM models (cls._dataclass.__init__ ->
     BM:builtins.object.__init__), namedtuple _get_namedtuple_fields exact
     BaseContainer check (dict-view proxies -> UseInferenceDefault),
     TypedDict tip builds its template with NO transform scan.
     (h) **`self` boundnode hijack** (protocols.py:375-376): ANY ambient
     bases.Instance boundnode (incl. Const/container NODES — Const
     subclasses Instance!) replaces the owning class in
     _arguments_infer_argname: a %-fold's str.__mod__ context makes
     `self.__class__` infer to builtins.str (django Q.deconstruct
     'builtins.str' cluster, ~12 lines).
     (i) **has_dynamic_getattr checks attrs[0] ONLY** and __getattribute__
     only when __getattr__ is missing (scoped_nodes.py:2516-38) — sentry
     IPlugin(threading.local) missing attrs are ERR again.
     (j) **instance-attr UM stays RAW** (bases.py:283-285: _wrap_attr runs
     over the raw attr list BEFORE inference) — core stream/conftest
     _original_recv yields UM not BM.
     (k) **dict-view bool_value** = bool of the synthesized List's elts
     (BooleanConstraint rejects items() of an empty dict literal — core
     triggers/event.py), dict-view default-repr folds
     ('<astroid.objects.DictKeys object at 0x10' under the 40-char cut).
     (l) **AUG binop attempts keep the augmented op string** (_aug_op):
     tl_infer_binary_op's EXACT `operator == "+"` check fails for '+=' ->
     NotImplemented -> the plain __add__ attempt runs too (extra method
     pull, airflow spark_sql); Const folds accept the BIN_OP_IMPL aug
     aliases.
     (m) bool & | ^ bool folds stay BOOL (CPython semantics via the real
     operator; django where.py/admin main, core purge/event/mqtt).
     REMAINING (~280 lines, by volume): (1) **nodes_inferred/cap dynamics +
     aug-chain structure** (~110: salt consensus/service.py ~45 the
     largest; salt http/lazy/ssh; core try_parse_enum trailing-None ~14 +
     config 3; django request/response iri_to_uri + formatted_description
     ~15; airflow ctl str.format counts; spark_sql aug-chain pull structure
     — traced to per-pair tl-concat element re-inference timing, needs its
     own session). The tpe3 probe pins a 1-bump gap (##110 vs ##111) in
     the EnumType.__call__ -> type.__new__ igetattr region. (2)
     **irreducible-by-construction** (~45): os.environ/sys.argv snapshot
     content+order from the live warm process (django autoreload 17,
     airflow conf/setup_idea, pylfunc, salt test_man hash order, core),
     random.sample SELECTION (salt deltaproxy 4), str(obj) heap addresses
     past the 40-char cut. (3) cross-file cache-state long tail (~40):
     module-class instance_attrs accumulation order (sentry importer,
     salt test_path/lazy Dict:N), sys.modules mutation
     (reset_warning_registry, test_deprecation_tools, ibm/mq conftest,
     core frame/conftest), pandas sas7bdat/fiscal/datetimes ctx flips.
     (4) pandas to_latex JoinedStr cap folds (4), test_nanops
     Class:builtins.complex.real values (3), model_enums one-early
     truncation. TOOLS: /tmp/cmp_dumptrace2.py alignment via
     difflib.SequenceMatcher over (kind,name) entry streams (drift
     blocks); single-file oracle items lists (/tmp/one_*.jsonl pattern)
     reproduce most context clusters without full-corpus warms.
   - **phase 12 (diff-reduction round 11, sample=ALL files)**: harness fix
     (PRYLINT_DUMP_ONLY prebuilds full corpus, dumps subset) made full-corpus
     runs the gate. 998 -> 530 diff lines across all 7 (django 121->66,
     pylfunc 8->7, pandas 46->46, salt 330->117, airflow 227->75, sentry
     50->43, core 216->176; ~13.6M inference lines checked total).
     LANDED (probe-verified; harness/infertests now 101 probes):
     (a) **NameInferenceError KIND propagation**: tuple-unpack rhs pulls
     (_resolve_assignment_parts top level, protocols.py:465/482-519 — the
     nested recursion's except swallows, the top level re-raises AS-IS) and
     _infer_context_manager's next(mgr.infer()) (protocols.py:568-571 — only
     StopIteration converts). _infer_stmts skips NameInferenceError stmts
     silently (bases.py:190) -> whole-Name ERR. Killed the 247-line ERRvsU
     cluster (salt states/modules `source=source` kwargs, airflow psrp/asb
     `with ... as ps`).
     (b) **path_wrapper identity dedup for ExcInst/Generator**
     (decorators.py:46 checks __class__.__name__ == "Instance" — everything
     else dedups by object id): InstId / captured-ctx Rc ptr mirror python
     identity (airflow run_utils duplicate ExcInst, test_choices Gen dups).
     (c) **_transform_wrapper PERMANENT module reparent**
     (brain_builtin_inference.py:206-210): builtin-tip results with no
     parent (Modules — `getattr(self, "x", pickle)` default) get
     result.parent = the Call node; qname() of the whole subtree changes for
     the rest of the run (Func:<mod>.DockerOperator._copy_from_docker.pickle
     ._loads — 28-line airflow cluster). reparents now apply to module roots;
     qname() walks through them.
     (d) **infer_hasattr returns Uninferable on UseInferenceDefault**
     (brain_builtin_inference.py:579-581) — never falls back to default Call
     inference (vesync/connection.py UvsERR cluster).
     (e) **enum-member PROXY placeholders survive _filter_stmts**: Instance
     objects in class locals delegate statement/assign_type/ancestors to
     their _proxied fake ClassDef (reparented) — Name refs to earlier
     members inside the enum body (QUEUED in `NON_TERMINAL = (QUEUED,)`)
     infer to the member instance.
     (f) **container tips safe_infer raw-node elements**
     (brain_builtin_inference.py:277-285): elements that are AST nodes —
     incl. nodes held INSIDE a materialized *args tuple — are safe-inferred
     and SKIPPED on failure (django i18n_patterns list(urls) -> List:0);
     synthetic container __str__ prints ctx=None (fabricated without ctx).
     (g) **dict-view proxies (DictKeys/Values/Items)**: hasattr(x,"elts") is
     True (objectmodel synthesized List) — starred unpack and CallSite
     *args iterate them; igetattr delegates to the List node as a
     builtins.list instance (d.keys().sort -> BM:builtins.list.sort); but
     object_len (helpers.py:278-81) and dict.fromkeys keep their EXACT
     node-class checks (proxies fall through -> ERR / empty dict).
     (h) **BooleanConstraint on synthetic model-attr hops** (bases.py:184-89
     constraint filtering runs for every stmt result): exception-model
     .args under `... if self.args else ...` fails truthiness -> trailing
     yield U; the rejected value still burns the pull-again bump.
     (i) **ModuleModel inherits ObjectModel __new__/__init__** BMs — the raw
     builder's `from builtins import __new__` C-member shims resolve through
     module getattr (ctypes Structure cluster, salt platform/win.py).
     (j) **PartialFunction generator qname**: Generator.parent is the
     PartialFunction whose qname() is the literal class name
     (objects.py:325-26) -> Gen:PartialFunction.
     REMAINING (530 lines, by volume): (1) **nodes_inferred counter/cap
     dynamics** (~330 lines: gtU-we-values 189 + chunks of EXTRA/MISSING/
     mismatch): e.g. namedtuple+Enum mixin attr access burns ##61 in astroid
     vs ##37 ours (the fake member classes re-infer the textual
     `namedtuple(...)` base Call in a no-tip module, 13+11 bumps), salt
     consensus/service.py instance-attr dict walks cap-truncate to U in GT,
     core config/__init__ module list truncates one element earlier, esphome
     try_parse_enum third value. Two known count off-by-ones in the suite
     (as_view_function_attrs ##5 vs ##4, brain_ssl_signal_re_http ##116 vs
     ##115). (2) **irreducible-by-construction** (~25 lines): os.environ
     snapshot content/order from the live warm process (airflow conf/parser,
     pylfunc; would need re-snapshot in an identical env), and CPython
     set-iteration ORDER of string sets (PYTHONHASHSEED randomization in the
     warm run — salt test_man, core). (3) long tail of context-dependent
     clusters (test_deprecation_tools module-__getattr__, ibm/mq conftest,
     reset_warning_registry sys.modules mutation) that only reproduce with
     full-corpus cache state.
   - **phase 11 (diff-reduction round 10, N=1000)**: 496 -> 50 diff lines.
     LANDED (probe-verified, harness/infertests now 84 probes):
     (a) **streaming UnaryOp._infer** (node_classes._infer_unaryop is a
     true generator): per-operand fold reaches the consumer BEFORE the
     next operand pull; the dunder branch REBINDS the loop-local context
     bug-for-bug; `not` folds call operand.bool_value() with NO context
     (fresh counter cell). BoolOp pair eval computes bool_value for ALL
     pair items (incl. the LAST, list-comprehension no-short-circuit) —
     any U bool -> U. THE pandas notna/isna fix (nanops.py count-exact).
     (b) **Compare operands infer under the SHARED ctx** (no clone,
     node_classes.py:1846-53): operand path pushes persist so recursion
     frames copied later inherit them (tm.shares_memory self-recursion
     path blocks; -50 pandas lines).
     (c) **is_abstract decorator check = next(node.infer()) SINGLE
     abandoned pull** (scoped_nodes.py:1487-89) — @overload stub chains
     never cache; FunctionDef.type gives the func pull and decorator pull
     each their OWN fresh ctx; _infer_decorator_callchain accepts
     PartialFunction (functools.wraps).
     (d) **ExceptionInstance OBJECT IDENTITY** in cache boundnode keys
     (fresh InstId per materialization — django ErrorDict cluster).
     (e) **STREAMING FunctionDef.igetattr** instance_attrs through lazy
     _infer_stmts relays (salt __options_dict__ cluster), and
     FunctionDef.getattr APPENDS the model attr to instance_attrs
     (scoped_nodes.py:1303-06; django as_view __doc__ pairs); model
     results hop through _infer_stmts.
     (f) **FunctionModel completed**: attr___get__ DescriptorBoundMethod
     (new Value::DescBM with descriptor-binding call result), Unknown
     family (__code__/__class__/__closure__/__call__/...; UM/BM model
     yields RAW per bases.py:466-69, FunctionDef path hops -> U), exact
     __defaults__/__kwdefaults__/__annotations__ fresh nodes.
     (g) **PropertyModel exact**: fget/fset PropertyFuncAccessors (fresh
     synth defs, qname <prop>.fget, caller-argc-gated delegation), fset
     sibling @x.setter search, NO FunctionModel fallback (prop.__doc__ is
     ERR); Property values carry a synth flag (the property() tip no
     longer poisons the wrapped function's render); Partial routes to the
     plain FunctionModel.
     (h) **Dict._infer_map exact**: recursive per-item safe_infer inside
     **unpacks (ambiguous IfExp value -> InferenceError; pandas to_latex),
     SynthDict unpacks (infer_argument's **kwargs Dict) safe-infer every
     key/value with real hops (sentry build_expected_result), in-place
     replacement order.
     (i) **NamedExpr.scope() parent-skip** (walrus under Arguments/Keyword/
     Comprehension resolves OUTSIDE the comprehension scope, PEP 572 —
     airflow ecs walrus dict-comp ERR cluster).
     (j) **exact big-int arithmetic**: ** past i64 via decimal bignum,
     i128-exact + - * // % with python floor/mod, << with arbitrary
     precision (salt 65535<<48), >> sign-saturation, bitwise ops accept
     Big-in-i128, Big participates in float arithmetic (2**63/ns), unary
     minus on Big. str.format folds the FULL format-spec mini-language
     (kwargs + specs; '#06x' pads between prefix and digits).
     (k) **brains**: brain_type lookup uses the SCOPE node as filter
     (class-level `type:` annotations), TypedDict synthetic class gets
     locals['__call__']=[Name dict] (instances callable -> Inst:dict),
     dataclass default_factory template REPARENTED into the real module
     scope (reparents override generalized to ANY node), ClassModel
     __new__/__init__ = ObjectModel BMs (unresolved-base classes),
     ClassModel __doc__ = class docstring, InstanceModel __module__ =
     literal's root module (Const/List/Tuple/Set are Instances; Dict uses
     DictModel — no __module__/__doc__/__dict__), all-Const set folds
     accept Node-Const elements (binop concat dedupe), Instance str()
     root() is reparent-aware (enum member fake classes).
     TOOLS: harness/trace_gt_file.py + trace_gt_cachew.py (file-gated GT
     tracers; cachew logs global cache writes via a LogDict), rust-side
     PRYLINT_TRACE_START=<path substring> turns PRYLINT_TRACE_INFER on
     mid-dump.
     N=1000 differing files/lines: django 5/9, pylfunc 6/8, pandas 11/15,
     salt 4/7, airflow 5/5, sentry 2/3, core 2/3 (total 50). REMAINING:
     (1) pylfunc 3 NOTREE (tree-fidelity owns) + 2 os.environ GT-env noise
     (machine-dependent; irreducible) + control_pragmas ERR/U +
     class_members type.mro (2). (2) pandas singles: sas7bdat f-string
     tail, test_fiscal datetime, series/range Slice dup tails, indexing
     bool tail, test_to_latex style folds, test_nanops complex,
     extension/io ERR-vs-Func (~15 lines, each needs its own prefix-trace
     session). (3) django: migrations f-string Instance pair, partials
     ERR/U, asgi Dict:10, auth 'next', signing str (9). (4) salt setup.py
     ERR/U + ipaddress GT-cap-earlier folds + build.py/test_path (7).
     (5) six.with_metaclass still unwired (no diff evidence at N=1000).
     (6) recursion guard still 350 (no diff evidence at N=1000).
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
4. **checkers fan-out — round 1 DONE**: ImportsChecker (E0001
   'Cannot import' + E0402) and VariablesChecker (E0601/E0602/E0603/E0604/
   E0605/E0606, FULL NamesConsumer machinery) ported and wired into the
   phase-2 walk. **0 FP / 0 FN on all 8 codes on ALL 7 corpora**; extended
   check_shell gate (owned=E0001,E0011,E25,E0402,E0601-E0606, 'Cannot
   import' exemption REMOVED) PASSES x7.
   - **Code layout**: `crates/pycheckers/src/{ckutils,imports,variables,
     walker}.rs`. walker.rs drives our two checkers in walk_order.rs
     callback order (other checkers' slots inert); recursion runs on the
     phase-2 1GB-stack thread. ckutils.rs ports pylint utils: exact
     157-name `is_builtin` snapshot, `in_type_checking_block`,
     `is_sys_guard`, `node_ignores_exception` (literal handler-name catch +
     contextlib.suppress via safe_infer), `is_defined_before` +
     `defnode_in_scope`, `are_exclusive(..., exceptions)` (If branch
     disabled, handler.catch name match), `is_terminating_func`, pylint
     `safe_infer` (lru 1024, pytype-set ambiguity + FunctionDef-args check)
     and `infer_all` (lru 512) on the live Engine, astroid `as_string`
     subset (equality-grade), `%`-template formatter.
   - **run.rs**: phase 2 is now SEQUENTIAL on a 1GB-stack scoped thread:
     `Engine::new(cwd)` + prebuild of EVERY fileitem in file order
     (mirrors pylint phase 1 `get_ast` cache warmth), then per file:
     pragmas -> unicode -> AST walk (`LintRun::walk_module`) -> statement
     count. Message naming: node msgs use the astroid module name
     (`.__init__` stripped), node-less msgs (E0001 Cannot-import) the raw
     FileItem name — ONE FILE can emit under TWO module headers (GT:
     homeassistant.auth.__init__ then homeassistant.auth).
   - **E0001 Cannot-import** (imports.py:1023-1053): engine
     `do_import_module` errors now carry kind — `BuildFail::TooManyLevels`
     (E0402 path, `_ignore_import_failure` = TYPE_CHECKING block / sys
     guard / except-ImportError) and `BuildFail::Syntax{path,modname}`.
     Exact `str(exc.error)` text comes from a PERSISTENT oracle coprocess
     (`oracle::OracleProc`, JSONL over the existing syntax_oracle.py)
     queried with the RESOLVED modname so the SyntaxError filename token is
     astroid-exact; None verdict (ruff/CPython mismatch) -> no message.
     `Engine.build_fail_cache` memoizes failed file_builds keyed
     (path, modname) — behaviourally invisible (failures never cache or
     mutate state in astroid), kills 27k re-parses in core.
   - **Port gotchas found** (now load-bearing): (a)
     `node.nodes_of_class(nodes.Break, nodes.Continue)` in
     `_inferred_to_define_name_raise_or_return_for_if_node` passes Continue
     as SKIP_KLASS — only Break is permissive (the comment lies; 8 E0606s
     across salt/django/core hinge on it). (b) ClassDef implicit locals
     (__module__/__qualname__/__annotations__) are PHYSICALLY in class
     locals (ClassDef.__init__ -> add_local_node -> _append_node sets
     parent=class), FIRST in insertion order — class-body uses of
     __module__ resolve through the class consumer (54 sentry FPs without
     it); Consumer::new injects engine `implicit_class_local` nodes.
     (c) `_check_consumer`'s consumed_uncertain defaultdict KEY-CREATION on
     read access is replicated with entry().or_default().
   - Not ported (no in-scope effect, noted): `_loopvar_name` (W0631) incl.
     its lookup side effects, `_check_late_binding_closure` (gated off),
     VariablesChecker.visit_import/importfrom bodies (E0611 disabled; their
     module-build side effects largely duplicate ImportsChecker's),
     visit_assign (E0633 later), visit_subscript (E0643 later),
     compute_first_non_import_node family (feeds W only).
   - KNOWN INEXACTNESS (accepted this phase): inference cache/counter
     state during the walk differs from pylint's because unported checkers
     (TypeChecker et al.) burn inference pylint-side; E06xx decisions that
     consult inference (infer_all of if-tests, safe_infer, metaclass(),
     __all__) are values-stable in all corpora today but cap-boundary
     flips are possible until the remaining checkers land.
5. **checkers fan-out — round 2 DONE**: TypeChecker + IterableChecker
   (E1102/E1111/E112x/E113x/E114x), SpecialMethodsChecker (E0301-E0313),
   ClassChecker (E0202/E0203/E0211/E0213/E0236-E0245/F0202),
   NewStyleConflictChecker (E1003). **ALL owned codes (E0001, E0011, E25xx,
   E0402, E0601-E0606, E11xx, E02xx, E03xx, F0202, E1003) at ZERO FP/FN on
   ALL 7 corpora**; extended check_shell gate (owned=...,E11,E02,E03,F0202,
   E1003) PASSES x7; tree gate 0; inferdump django+core N=200 == 0; 151
   infertests PASS. core ~23s (TypeChecker inference burn; budget 135s).
   - **Code layout**: `crates/pycheckers/src/{typecheck,classes}.rs` joined
     walker.rs dispatch (callback order per regenerated walk_order.rs — the
     OLD file was generated with plain `-E`, not the full flags:
     TypeChecker.visit_attribute and BasicErrorChecker.visit_call are NOT
     registered under the true flags; gen_walk_order.py now reads flags.txt).
   - **visit_call family** (E1102/E1120/21/23/24/25/32): CallSite from the
     ENGINE port (fixed: explicit-keyword after `**{...}` OVERWRITES silently
     per arguments.py:140 — was wrongly flagged duplicated);
     safe_infer(compare_constructors=True) variant + FULL
     function_arguments_are_ambiguous (argnames + first-defaults-pair early
     return incl. the (None,None) kw_default -> ambiguous quirk);
     _determine_callable port: BoundMethod/UnboundMethod/Partial/Property
     (objects.Property.type == "property") arms, ClassDef __new__/__init__
     local_attr resolution ([-1] LAST def, object/builtin-module fallbacks);
     DescriptorBoundMethod (func.__get__): implicit_parameters=0 + synthetic
     args = func.args + mandatory 'type' (objectmodel.py:416-459);
     PropertyFuncAccessor synths (prop.fget/fset) carry the WRAPPED
     function's args; fget body = Property.body = [] -> E1111 fires on
     `x = Cls.prop.fget(self)`; no-context variadic machinery
     (typecheck.py:674-746) incl. SynthSeq/SynthDict param-representation
     mapping; isinstance special case; keyword-in-all-decorator-returns.
   - **ENGINE fixes found via checkers** (all probe/inferdump-verified):
     (a) ObjectModel __new__/__init__ template defs REPARENTED to
     builtins.object (objectmodel.py:145/:162) — FunctionDef.type
     "classmethod" for the model __new__ (E1120 'in classmethod call');
     (b) **asstr.rs: faithful AsStringVisitor port** (precedence table,
     format_args, the NamedExpr-renders-BARE bug) — the dataclass
     __init__ generation now renders annotations/defaults via as_string
     like brain_dataclasses does: walrus-containing defaults make the
     generated init UNPARSEABLE -> no __init__ injected -> pylint sees
     object.__init__ (args None) -> visit_call bails (core alexa_devices
     E1123-FP/E1125-FN cluster); (c) compute_mro records
     DuplicateBases-vs-InconsistentMro in Engine.last_mro_dup (E0241/E0240);
     (d) Value::DescBM is callable() (BoundMethod subclass).
   - **protocol checks**: _supports_protocol/_supports_protocol_method over
     Values (ClassDef -> metaclass lookup; dict views -> callback on the
     proxied dict instance; BaseInstance arm incl. Generator/UnionType);
     pylint has_known_bases SHARES astroid's node memo
     (Engine.known_bases_cache) but computes with PYLINT safe_infer;
     is_inside_abstract_class/class_is_abstract/is_overload_stub in
     LintCaches (module-level lru parity); is_hashable; dunder_lookup
     (literal nodes -> proxied class OWN locals only); E1126/27/44 sequence
     index chain; E1130 type_errors (any-Uninferable-discards-all);
     E1139 metaclass-factory callcontext quirk (callee None blocks param
     consumption -> empty-args callcontext is behaviorally exact).
   - **E1136 decorators branch**: `getattr(inferred, "decorators", None)` is
     proxy-aware — BoundMethod values expose the WRAPPED function's
     decorators; astroid-safe_infer of the first decorator (django
     cached_property -> Uninferable -> conservative return killed the
     sentry/core E1136/37/38 FP cluster).
   - **classes checkers**: _safe_infer_call_result EXACT two-pull (value +
     ambiguity probe — eager draining burns extra inference);
     E0244 reads the enum transform's __members__ SynthDict redirect;
     E0203 ScopeAccessMap + _first_attrs stack (statics push None; pop only
     when is_method && args known) + are_exclusive(AttributeError|Exception|
     BaseException); E0202 decorator data-descriptor exemptions +
     _check_functools_or_not import-lookup arm; slots family over ilookup'd
     values incl. synthetic containers.
   - **Burn-only paths ported** for cache parity: signature_mutators
     decorated_with (empty qnames still infer decorators), W1116
     _is_invalid_isinstance_type safe_infer chain, W1117 posonly-keyword
     `continue` consumption, _check_typing_final safe_infer+decorator burn,
     class_is_abstract in _check_bases_classes, E0244's
     is_subtype_of(enum.IntFlag).
   - NOT ported (no diff evidence on corpora; revisit if FPs appear):
     unimplemented_abstract_methods burn (W0223), _check_init burn
     (W0231/W0233), _check_useless_super_delegation burn (W0246),
     _check_signature W-burn, _check_unused_private_* safe_infer burn,
     TypeChecker.visit_assignattr/delattr no-member burn (E1101 disabled but
     the AugAssign/DelAttr paths run visit_attribute in pylint),
     _check_redefined_slots burn (W0244). E1127/E1144 not emitted for
     SynthSlice (brain slice() products; zero corpus mass).
6. **checkers fan-out — round 3 (FINAL) DONE: ALL 7 CORPORA BYTE-IDENTICAL**
   (`cmp` on .out AND .exit: django 898/2, pandas 616/2, salt 8690/2,
   airflow 667/2, sentry 515/2, pylfunc 524/14, core 82243/2). Every
   remaining enabled message ported: BasicErrorChecker+BasicChecker
   (`basicerr.rs`: E0100-E0119), ExceptionsChecker (`exceptions.rs`:
   E0701-E0712), StringFormatChecker (`strings.rs`: E1300-E1310 + EXACT
   parse_format_string / Formatter().parse / field-name-split ports),
   LoggingChecker (`logging_ck.rs`: E1200/01/05/06), AsyncChecker+Match+
   MethodArgs+Dataclass+ModifiedIterating+Stdlib (`tailmisc.rs`: E1700/01,
   E1901-04+R1906, E3102, E3701, E4702/03, E1507/19/20), variables
   E0633/E0643 + unused-import computation, imports C0411/12/13 recording
   machinery (isort py3-union STDLIB table). walker.rs now dispatches the
   FULL walk_order. Disabled W/C/R messages the visit bodies compute are
   EMITTED into the gating layer — inline `# pylint: enable=` resurrection
   works (pylfunc: R1906 x2, W0012 x2 byte-exact) and feeds I0021.
   - **I0021 useless-suppression EXACT** (pylfunc x2): FileState grows
     _suppression_mapping + insertion-ordered raw_state; every FILTERED
     emission routes handle_ignored_message (module-pragma scope only);
     iter_spurious_suppression_messages runs after the walk, emits
     I0021/I0020 through normal gating. Import-order/unused-import
     attempt-recording (incl. linter.add_ignored_message call sites in
     imports.py:713/:824-868) makes 'used suppressions' exact.
   - Inline enable of a NOT-computed message (outside
     msgstore::EMITTED_DISABLED_MSGIDS) prints a stderr warning — the only
     class of silent false negatives left by design (e.g. salt inline
     enables of E0401/E0611/E1101/C0103: zero GT hits today).
   - **Port gotchas found**: (a) utils.is_subclass_of -> astroid
     helpers.is_subtype -> _type_check runs ASTROID-flavor has_known_bases
     (strict safe_infer) whose `_all_bases_known` memo (our
     Engine.known_bases_cache) is SHARED with pylint's has_known_bases —
     the W0706 _check_try_except_raise burn POISONS the memo and silences
     a later E0712 (try_except_raise_crash); order is load-bearing.
     (b) E0633's `_get_unpacking_extra_info` uses the RAW astroid .lineno —
     for decorated defs that's the FIRST DECORATOR line
     (rebuilder.py:1130-1139), NOT fromlineno (core mystrom/overkiz).
     (c) inferred-tuple `except` types carry EvaluatedObject elements —
     safe_infer of those yields the wrapped value (core motionblinds).
     (d) TypeChecker.visit_binop is DEAD on py3.12 (`_py310_plus` early
     return) — E1131 unreachable; _visit_binop/_visit_augassign are
     disabled in pylint source (leading underscore). (e) Formatter.parse
     does NOT raise the "cannot switch from manual" ValueError on 3.12 —
     that collect_string_fields branch is dead; all parse errors map to
     IncompleteFormatString. (f) E1700 fires only for YieldFrom on the
     3.12 host (sys.version_info check in async_checker.py:48-54).
   - NOT implemented (unreachable under the pinned contract): E0013/E0014/
     E0015/F0001/F0011 (config/plugin/CLI parse errors — flags are fixed
     and valid, rcfile empty), F0202 (caller filters make it dead code),
     E1131 (py3.12-dead). DeprecatedMixin tables (W0402/W1505/W1511/W1512)
     not ported — they are in INCOMPATIBLE_WITH_USELESS_SUPPRESSION (no
     I0021 interplay) and inline enables of them warn on stderr.
   - GATES: check_treedump django 400 == 0; check_inferdump django 200
     == 0; check_shell PASS x7 (full owned list + --strict-exit); 151
     infertests PASS. core 33.6s (was ~23s; checker burn — budget 135s).
7. **FULL BYTE PARITY RE-VERIFIED (2026-06-11, clean rebuild)**: all 7
   corpora `cmp` byte-identical on .out AND equal .exit (pylfunc 524/14,
   django 898/2, pandas 616/2, salt 8690/2, airflow 667/2, sentry 515/2,
   core 82243/2). All gates green in the same pass: check_treedump django
   400 == 0, check_inferdump django 200 == 0 (108076 inference lines),
   check_shell PASS x7 (full owned list + --strict-exit), 151 infertests
   PASS. Suite timing ours 103.4s vs pylint 2522s (~24x; slowest single
   corpus core 33.6s vs 1357s).
8. Remaining: perf polish if needed (suite ≈ 104s, well under the ~250s
   10x bar), and watching the stderr resurrection warnings on new
   codebases for messages worth porting next.
9. **10-corpus BLIND battery (2026-06-11): ALL 17 corpora identical.**
   New pinned corpora: scrapy, celery, pip, fastapi, sqlalchemy, numpy,
   scikit-learn, matplotlib, ansible, sympy (ground truth in
   harness/results/<c>.iso.*). 11 divergence mechanisms found and fixed:
   - t-strings/PEP750 (sqlalchemy): already covered by the
     unsupported-syntax → oracle route (ruff target 3.12).
   - E0102: astroid injects __module__/__qualname__/__annotations__ into
     every ClassDef's locals at construction → implicit defined_self
     (celery local.py); pylint anchors node messages at node.position
     (def/class keyword line — NOT the fromlineno decorator quirk):
     pyast Tree.positions + ckutils msg_line/msg_col, all def/class-anchored
     emits switched (matplotlib text.py).
   - E1124: PartialFunction.parent = the partial-call parent → .type
     "method" → implicit_parameters()==1 (matplotlib dviread @_dispatch).
   - E1102: object_type/type() proxy classes are FRESH EMPTY classes
     (helpers._build_proxy_class) — b.function/method/module/bfom are now
     empty synthetic classes, distinct from the raw-built snapshot classes
     that einf descriptors resolve (sqlalchemy testing/util.py
     types.FunctionType(...); sklearn _repr_html/base.py).
   - E1111: PartialFunction.root() walks the synthetic parent → the
     assignment-site module decides fully_defined() (pip urllib3 wait.py).
   - E1135: snapshot Module doc_node wired → real C-ext __doc__ strings
     (ansible console.py readline.__doc__).
   - E1120/E1123: UnboundMethod/BoundMethod pytype falls through to the
     wrapped FunctionDef.pytype ("builtins.instancemethod" iff "method" in
     .type) → safe_infer no longer ambiguous on [UM, BM] (sympy
     test_basic.py "in unbound method call").
   - E0603 attribution: pylint stamps node messages with node.root().name
     / node.root().file (pylinter.py:1257-1263) — __all__ elements
     inferred through ImportFrom are attributed to the DEFINING module
     (numpy.char checks → numpy._core.defchararray sections); CheckMsg
     carries root_mid.
   - F0002 crash replication: (a) logging _check_format_string does a
     STRICT bytes.decode() — UnicodeDecodeError aborts the module check
     (pip test_base_command.py); (b) astroid's rebuilder RecursionErrors
     on ~495+ deep BinOp chains — trees deeper than 350 are re-judged by
     the oracle → exact phase-1 F0002 (sympy resolvent_lookup.py), and any
     import of a crash file re-trips the importing module's check via
     Engine.crash_files/crash_tripped (sympy galois_resolvents.py).
     WalkCx.crashed: pre-crash messages kept, later ones dropped,
     spurious-suppression step skipped, F0002 appended, fatal exit bit.
   - **F0002 TIMESTAMP NORMALIZATION**: our F0002 embeds a real
     PYLINT_HOME/pylint-crash-%Y-%m-%d-%H-%M-%S.txt path (own wall clock).
     ACCEPTANCE for corpora with F0002 uses harness/bytecmp.py — raw cmp
     except `pylint-crash-[0-9-]*\.txt` is rewritten to
     `pylint-crash-TS.txt` in BOTH inputs. Everything else stays raw-byte.
   - Acceptance (2026-06-11): all 17 corpora bytecmp-identical + exit codes
     equal (pip exit 3, sympy exit 3 — fatal bits). Gates: check_treedump
     django 400 == 0, check_inferdump django/pandas/salt 200 == 0, 151
     infertests PASS, 36 treetests identical.
10. **batch-3 BLIND battery close-out (2026-06-11): ALL 27 corpora
    identical.** New pinned corpora: rich, tornado, werkzeug, black,
    botocore, mypy, pydantic, twisted, nova, zulip. The 3 remaining
    mechanisms, fixed (one commit each):
    - E0242 (rich): astroid's implicit class locals (__module__/
      __qualname__/__annotations__, scoped_nodes.py:1911-1933) are FIRST
      in every ClassDef.locals — slots naming them always conflict, and
      the single-bare-AnnAssign skip can never match them.
    - E1132 ordering (nova): **pylint is nondeterministic here** —
      typecheck.py:1487 iterates CallSite.duplicated_keywords, a Python
      set[str]; probe showed 5 different orders in 5 runs. CONTRACT
      CHANGE: harness/ground_truth.sh now exports PYTHONHASHSEED=0; nova
      GT regenerated (diff = exactly the 8 multi-message E1132 sites).
      pycheckers::pyset ports CPython 3.12 str hash (siphash13, zeroed
      _Py_HashSecret, PEP 393 buffer) + setobject.c table semantics
      (LINEAR_PROBES=9, PERTURB_SHIFT=5, grow at fill*5>=mask*3 to
      used*4, resize re-inserts in old-slot order); fuzzed 3000 random
      insertion sequences vs the pinned interpreter — 0 mismatches. Any
      future pylint-visible set/dict-hash iteration order must use it.
    - E1126 FNs + E1136 FPs (mypy typeshed stubs — NOT .pyi-specific):
      (a) helpers._object_type's Proxy branch yields ._proxied for
      DictKeys/Values/Items = the SYNTHESIZED LIST of keys/values/
      item-tuples (objectmodel.py:856-890), so _collections_abc's
      `dict_keys = type({}.keys())` infers to an EMPTY LIST literal and
      `dict_keys[str, x]` annotations are invalid-sequence-index;
      (b) the UnionType proxied class is NOT empty — raw_building.py:
      673-694 object_builds it from live types.UnionType (3.12 HAS
      __getitem__) → PEP 604 union aliases ARE subscriptable (no E1136
      on copyreg.pyi _Reduce[_T]). Full member set mirrored into the
      synth module; __class__ wired engine-side to the real `type`.
      (Known approximation: the mirrored dunder FunctionDefs carry
      EMPTY args, not args_unknown — only visible if a corpus ever
      CALLS a union dunder directly; acceptance is clean.)
    - Acceptance (2026-06-11, clean run): all 27 corpora bytecmp-
      identical + equal exits (pylfunc 14, pip/sympy/tornado 3, rest 2).
      Gates: check_treedump django 400 == 0, check_inferdump django
      200 == 0 (108076 lines), 151 infertests PASS. Suite ≈ 121s
      (slowest core 21.2s).

13. **standalone binary (PyPI prep)**: DONE — the binary runs with NO repo
    checkout and NO astroid/pylint installed; only a python3 (any install,
    PRYLINT_PYTHON overrides) is consulted at runtime.
    - **Embedded oracle**: harness/syntax_oracle.py (astroid-importing) is
      replaced at runtime by crates/cli/src/syntax_oracle.py — a STDLIB-ONLY
      replica of pylint get_ast()/astroid file_build error taxonomy,
      include_str!'d into oracle.rs, materialized to a content-keyed temp
      file and spawned `python3 -I`. Replicates: open_source_file encoding
      arms (detect_encoding SyntaxError/LookupError → E0001 w/ ABSOLUTE
      path; UnicodeError → F0010 "Wrong or no encoding"; OSError → F0010
      "Unable to load file"), modutils.get_source_file .pyi→.py redirect,
      _parse_string (data+'\n', filename=modname iff truthy, type-comment
      retry sans filename → '(<unknown>, line N)'), null-bytes ValueError
      (line 0), MemoryError, tokenize.TokenError pass, AND astroid's
      TreeRebuilder RecursionError on deep trees: a frame-faithful walk
      (2 python frames/AST level; 3 for For/With/FunctionDef + Dict whose
      children rebuild under the _visit_* helper / _visit_dict_items
      generator frame; expr_context/operator nodes skipped), shim-
      calibrated to pinned astroid's crash boundary (first crash at 494
      nested binop/unary/attr/ifexp/lambda levels; parenthesized nesting
      is capped ~200 by CPython's parser itself → exact SyntaxError parity
      for free). This drives sympy's two F0002s (resolvent_lookup crashes
      the rebuild; galois_resolvents F0002 via the import-recrash set).
      harness/check_oracle.py = differential gate vs .venv astroid: all
      355 corpus E0001 files + 68 synthetic edge/boundary cases, 0
      mismatches. PRYLINT_ORACLE env still swaps in a custom script.
    - **Embedded snapshots**: crates/pyinfer/build.rs include_str!s all
      103 snapshot JSONs (sorted table, binary_search lookup in
      snapshot::embedded_json); PRYLINT_SNAPSHOT_DIR is now only an
      on-disk override for regeneration/debug. Binary 6.6MB → 14.3MB.
    - **Degradation without python3** (documented contract): files our
      parser parses still lint fully except stdlib/site-packages imports
      are unresolved (pyenv probe failure, sys.path = project root only);
      ruff-rejected files report F0002 astroid-error instead of exact
      E0001; deep-tree (≥350) crash candidates also F0002. One clear
      stderr note each from the oracle spawn + pyenv probe.
    - **harness/check_standalone.sh**: copies the binary ALONE to an empty
      temp dir, fresh project (syntax-error file, t-string file, broken-
      module import, snapshot-driven E1102 math.pi()) → byte-identical vs
      .venv-pylint pylint (PYTHONHASHSEED=0) + equal exits + empty stderr.
      Default-PATH python3 (3.14) run also verified: t-string file parses
      there → "parses with CPython but not with ruff; module skipped"
      stderr note (verdicts are interpreter-version-dependent by design,
      exactly as for pylint itself).

## Release — v0.2.0 PyPI packaging (2026-06-11)

- **Packaging**: maturin (`bindings = "bin"`, `manifest-path =
  crates/cli/Cargo.toml`, `profile = "release"`) via `pyproject.toml` at the
  repo root; workspace layout untouched. Wheel ships the 14.3MB
  self-contained binary as a console script (`prylint-0.2.0.data/scripts/`);
  sdist is the 4 workspace crates + Cargo.lock + embedded snapshot JSONs
  (168 files, ~1.1MB — no corpora/harness leakage; verified by listing).
- **Artifacts** (`dist/`, `twine check` PASSED on both):
  `prylint-0.2.0-py3-none-macosx_11_0_arm64.whl` + `prylint-0.2.0.tar.gz`.
  Upload deliberately NOT done (no credentials in this session); release is
  `scripts/release.sh 0.2.0` (twine upload + tag push → CI builds the
  Linux/Windows/macOS-x86_64 wheels).
- **Install tests**: wheel → fresh venv → probe project (E0602) OK, AND
  installed binary on corpora/scrapy with harness/flags.txt +
  PRYLINT_PYTHON=.venv-pylint → byte-identical vs scrapy.iso.out, equal
  exit. Sdist → second venv → pip compiles via cargo (needs network for the
  ruff git deps) → same probe + scrapy byte-parity PASS.
- **License**: aligned everywhere to **GPL-2.0-or-later** (root LICENSE was
  already GPLv2 text; Cargo.toml/README previously claimed MIT OR
  Apache-2.0). pylint is GPL-2.0-or-later and prylint reproduces its message
  strings/behavior verbatim, so GPL is the only defensible choice.
- **GOTCHA (bit us this round): never pip-install ANYTHING into
  .venv-pylint.** It is the parity interpreter (PRYLINT_PYTHON): its
  site-packages define module resolution for inference. Installing
  maturin+twine there (twine pulls pygments/rich/requests/...) flipped pip
  (2 extra E1136 in vendored pygments) and nova (2 lost E1120) to BROKEN.
  Fixed by uninstalling back to exactly {pylint, astroid, isort, dill,
  platformdirs, tomlkit, mccabe} (+ no pip!) and re-verifying. Packaging
  tools live in the dedicated `.venv-build` (uv venv --seed);
  scripts/release.sh now uses it.
- 27/27 gate re-verified on the release tree after packaging.

## Gotchas for future rounds

- Don't sort anything pylint doesn't sort. Order comes from readdir + dict
  insertion everywhere. Where pylint iterates a raw SET, the order is
  hash-seed dependent: GT is pinned at PYTHONHASHSEED=0 and
  pycheckers::pyset replicates the seed-0 order exactly.
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

## Inference residue classification (final, full corpora)

django/pandas/sentry/core: ZERO inference-dump diffs. airflow 3 files /
salt 5 / pylfunc 4 (26 lines total): ALL in files with zero ground-truth
messages; root causes are process-environment nondeterminism (astroid
raw_building snapshots the live os.environ; PYTHONHASHSEED set ordering),
i.e. pylint itself is run-to-run unstable there. NOT engine bugs. Frozen as
known-benign; revisit only if a checker diff ever points here.

## Full-pylint mode — phase A (token/raw layer + misc checkers)

New mission: byte parity WITHOUT `-E` (arbitrary --disable lists). Profiles:
`harness/flags_hook.txt` (Adam's pre-commit disables) and
`harness/flags_full.txt` (no disables). GT in `harness/results/<c>.{hook,full}.out`;
compare with the score footer stripped (`harness/strip_footer.py` — the one
sanctioned GT transform until phase F implements the footer).

Done this phase (specs: notes/09-format.md, 09-misc-wc.md, 09-pipeline-noE.md):

- **Full-mode message state**: `GlobalState::full_default()` = 389 − 10
  `default_enabled:False` − 2 py-gated (E0106/W1502) = 377 enabled, then CLI
  --disable. `-E` keeps the old baked state — mode-split at run().
- **CPython-tokenize-equivalent stream** (`pyast::pytok`) from ruff's lexer
  over the UNnormalized decoded bytes (`decode_source_raw` — \r\n preserved;
  pylint tokenizes raw bytes). Synthesis patched to CPython semantics
  (ENCODING/ENDMARKER, EOF `NL ''` after comments/blank partial lines, EOF
  DEDENT re-rowing, backslash-continuation INDENT clamping). Probed against
  the pinned venv's tokenize.
- **FormatChecker** (`pycheckers::format`): token side C0301 (pragma-text
  excision + checker_off→add_ignored quirk, URL regex), C0302 (run-global
  `_pragma_lineno` leak — map now maintained by the cli pragma scan in
  sequential module order), C0303/C0304/C0305 (specific_splitlines
  buffer-drop, \f-protection), W0301, W0311 (both call paths), C0325 (full
  state machine incl. walrus/double-paren/else-recursion), C0327/C0328
  (C0328 inert at default config). AST side C0321 via visit_default
  semantics (field-list previous_sibling, Try else/finally line inference,
  blockstart_tolineno per node kind, _visited_lines 1/2 protocol,
  Ellipsis-stub + With exemptions).
- **misc** W0511 fixme (anchored case-insensitive regex, col+1 quirk);
  EncodingChecker raw side is a documented no-op; I0023 not ported
  (default-off, checker dropped).
- **non_ascii_names** C2401/W2402/C2403 (lambda-param "Variable" label via
  frame dispatch, ClassDef-entry instance-attr emission order,
  instance_attr_ancestors skip).
- **unicode** C2503 now displayable (was already computed).
- **small checkers** (`pycheckers::smallck`): C2801 unnecessary-dunder-call
  (DUNDER_METHODS map generated from the pinned venv, lambda exceptions,
  super() and non-Instance inference gates), W2101 useless-with-lock, C3001/
  C3002 lambda-expressions, W3301 nested-min-max (identity-compare of the
  inferred builtins FunctionDef, genexp bail, stale-enumerate splat rewrite
  with the empty-tail-slice bug, as_string suggestion), W3601
  bad-chained-comparison (sorted-unique operator groups).
- **deprecated framework** (`pycheckers::deprecated` + generated
  `depdata.rs`, harness/gen_deprecated_rs.py — sys.version_info-filtered
  tables in seed-0 set order): W4901 via ImportsChecker (per-name on import,
  absolute name on importfrom, `__import__("x")` mixin call path), W4902/
  W4903 (qname-or-bare-callsite-name set, 2-element seed-0 set order via
  pyset), W4904 (dict.update module-table replacement bug kept: importlib.abc
  "Finder" / typing "Text" lost), W4905 (first decorator only), W4906.
  W3101 was already ported (method_args).
- **Walker**: full-mode callback positions extracted from the pinned venv
  with an empty rcfile (harness/gen_walk_order_full.py) and hand-wired into
  the dispatch behind `Prepared` gates (prepare_checkers drop rule + the
  only_required_for_messages method gates). Under -E every gate is false →
  -E dispatch byte-identical. Per-module order: pragma scan → RAW checkers
  (format/misc no-ops, unicode) → TOKEN checkers (format, miscellaneous) →
  AST walk, sorted-checker-name interleaving verified order-identical on
  corpora.
- W3201 bad-dunder-name is in pylint/extensions (NOT loaded by default) —
  out of scope, confirmed against the master coverage table.

Validation:
- Probe corpora (every phase-A code incl. zero-GT ones: W0301, C0327, W2402,
  C2403, W2101, C3002, W3601) byte-identical vs pinned pylint after
  filtering out-of-phase codes.
- 14 corpora × {hook, full}: phase-A codes at ZERO FP / ZERO FN —
  C0301 31304, C0321 2628, W0511 1768, C0302 476, C2801 306, C0325 253,
  W0311 232, W4901 226, C3001 188, C0303 70, C0304 42, W4902 14, W4904 18,
  C2401 22, C0305 10, W3301 4, W4903 4, W4905 2, C2503 2 — all exact, and
  the merged streams are ORDER-identical for every exactly-matched code.
- Remaining hook/full FPs are pre-existing port bugs in other phases' codes
  now visible without -E (W0707/W0611/C0411/W0706/W0632/W0614/W1518/W1202)
  plus F0002 crash-template timestamps (bytecmp-normalized); FNs are the
  not-yet-ported W/C/R checkers (basic, variables-W, classes-W, refactoring,
  design, similarities, strings-W).
- -E 27-corpus gate: 27/27 byte-identical + exit codes equal.
  check_treedump django 400 == 0. pyinfer untouched.

## Full-pylint mode — phase B (checkers/base W/C/R) (2026-06-12)

Built `pycheckers::basicwc` + BasicCk extensions per notes/09-basic-wc.md:
- BasicChecker W-codes: W0101 (incl. terminating-func INFERENCE arm; the
  return+Expr(Yield/YieldFrom) empty-generator skip — YieldFrom SUBCLASSES
  Yield), W0102 (next(default.infer()) single pull, 10-qname table, the
  4 message-arg shapes), W0104/W0105/W0106/W0131/W0133 via visit_expr,
  W0107 PassChecker (child_sequence len + doc_node), W0108 (filter_vararg
  Starred semantics, lookup-resolves-to-lambda bail), W0109/W0130 (python
  value-equality keys, %r of the SECOND occurrence, Attribute-key
  as_string collision), W0124, W0125/W0126 (safe_infer const-nodes incl.
  objects.Property; FunctionDef/Lambda infer_call_result truthiness;
  _name_holds_generator frame lookup; with_metaclass AttributeError ->
  F0002 crash replicated), W0127 (class-scope locals exemption), W0128
  (dummy-rgx whole-check abort, Counter.most_common stable ordering),
  W0129/W0199, W0134+_trys stack (crash leaves the stack dirty — leave_try
  pop gated on !crashed), W0150 (TryStar finalbody matched via hasattr,
  _trys gate Try-only), W0122/W0123 (pre-existing).
- BasicErrorChecker W0120 (orelse[0].lineno-1 anchor, loop col;
  _loop_exits_early break-ownership walk; AsyncFor isinstance-For).
- ComparisonChecker C0121 (is_test_condition bool()-wrap rule, truthiness/
  falsiness suggestion table), C0123 (type(x)==type(y) literal-arg
  exemption), R0123 (textual "is not" replace-all), R0124 (Const-value /
  Name-name equality only), R0133, W0143 (bare callables = FunctionDef |
  BoundMethod | objects.Property — Property SUBCLASSES FunctionDef with
  empty body; _SpecialForm + top-level-Raise exemptions), W0177 (float
  ("nan") inferred()[0] crash semantics + np.NaN syntactic).
- NameChecker C0103/C0104 (hand-rolled python-re matchers: snake/UPPER/
  Pascal with \w = isalnum-categories, \d = Nd; typevar/paramspec/
  typevartuple/typealias lookaround families), the full visit_assignname
  dispatch (typevar/typealias inference, tuple-target fallthrough,
  Uninferable+const-shaped igetattr bail, are_exclusive pairwise const
  upgrade, _redefines_import, reassigned-before/after line scans, enum
  members via Engine.enum_member_names, dataclass Final->class_attribute),
  C0105/C0131/C0132 (variance kwargs; non-Const variance kwarg -> crash),
  instance_attrs attr checks at classdef visit (dataclass/attrs Unknown
  placeholders resolved through dataclass_attrs/reparents to their
  class-body stmts; node.root() foreign-module attribution).
- DocStringChecker C0112/C0114/C0115/C0116 (`^_` gate, property setter/
  deleter + overload-stub exemptions, overridden-method scan with
  builtins.object qname skip, __doc__ locals fallback, str-format-first-
  stmt heuristic, empty-module lines==0).
- FunctionChecker W0135 (Generator.parent unwrap, caller-yield Const
  bail, yield-is-last-stmt walk, try-finalbody/handler analysis).

Engine fixes surfaced by the zero-round (all -E/treedump/inferdump green):
- are_exclusive: astroid locate_child returns the FIELD LIST for list
  members -> different except-handlers ARE exclusive (try/import consts).
- brain_typing: typing.Annotated/Generic __class_getitem__ is OVERWRITTEN
  unconditionally (Alias[...] re-subscripts yield the ClassDef; pydantic
  OnErrorOmit alias exemption via variable->ClassDef safe_infer).
- brain_dataclasses _is_init_var/_is_class_var: getattr(inferred,"name")
  proxies through Instances (InitVar[str] fields out of instance_attrs).
- enum transform: dunder_members dict-replace keyed by local with the
  LAST target's fake (tuple-target enums); Engine.enum_member_names side
  table = the astroid __members__ value-Name names pylint reads.
- is_terminating_func: Instance.igetattr wraps methods as BoundMethod(
  UnboundMethod(f)) -> unwrap instance-bound BMs (class-bound skip).

Validation (footer-stripped GT, owned-code multiset + exact subsequence
order): rich/fastapi/werkzeug/tornado/pydantic/mypy/pip/celery/botocore/
django/zulip/salt/ansible: 0FP/0FN both profiles. scrapy/matplotlib/
sqlalchemy/twisted hook: 0FP/0FN except twisted hook W0143 x1. black hook
0FP/0FN (full GT was truncated mid-run — regenerated).
Zero-GT codes (W0177/W0199/C0131/C0132/W0136/W0137/W0128) validated
byte-identical on micro-probes vs the pinned pylint.
Known full-profile residue, all confirmed fresh-state-identical (pylint on
the same inputs in isolation matches us byte-for-byte; the divergence is
the full-run inference cache state built by checkers other phases haven't
ported yet — re-verify after variables/typecheck/refactoring phases):
scrapy C0103 x1 (f-string after attr burn), matplotlib C0103 x1 (JoinedStr
shared-context part inference), twisted W0143 x1 + C0103-attr x2 + (hook)
W0143 x1, sqlalchemy full C0116 9FN/2FP (generic-base ancestors order
sensitivity).

## Phase B zero-round 2 (full sweep, both profiles)

Root causes found for the previous round's "fresh-state-identical" residue
and fixed at the ENGINE level (not per-message patches):
- utils.safe_infer is LAZY: pylint abandons the inference generator at the
  first stop condition (`return None` mid-loop). Our eager full drain kept
  exploring and wrote truncated [Uninferable] entries into the GLOBAL
  inference cache that astroid never writes (twisted _signals.py:90 drain
  explored EnumType._create_ and poisoned enum.py:759 `value`, flipping
  W0143@148 two visits later). safe_infer / safe_infer_cc / typecheck
  visit_with now stream pull-by-pull with Drive::Stop at pylint's exact
  return-None points. Fixed: twisted W0143 (hook+full), twisted C0103-attr
  x2, scrapy C0103, black-hook.
- builder._can_assign_attr ends with `qname() != "builtins.object"`: a
  delayed assattr NEVER lands on builtins.object (twisted `ConchUser =
  object` fallback polluted object.instance_attrs and suppressed the
  C0103 attr check for channelLookup corpus-wide).
- name checker class-scope `any(local_attr_ancestors)`: mro(context)[1:]
  FIRST (recomputed per call), lazy ancestors() fallback on MroError only.

Sweep result (footer-stripped GT, owned-code multiset + exact owned-line
subsequence order, 22 corpora hook+full where GT finished; black.full /
sentry.full GT still generating upstream):
- 41/43 combos 0FP/0FN with EXACT order (incl. sentry hook 292/292).
- matplotlib full: C0103 x1 FP (.circleci/fetch_doc_logs.py:36
  artifact_url). Root-caused: pylint's E1101 visit_attribute getattr
  (Instance.getattr -> instance_attr_ancestors -> ancestors -> namedtuple
  tip re-run under non-empty context) rebuilds the synthetic proxy and
  fires transforms.py _invalidate_cache between visits; our walker still
  skips the E1101 visitor ("E1101 disabled — skipped" walker.rs), so the
  stale Call entry replays and the JoinedStr stays under the ni clamp.
  BLOCKED on the typecheck full-mode phase porting E1101's visit walk.
- sqlalchemy full: C0116 9FN/2FP (orm/attributes.py, orm/base.py,
  dialects/postgresql). Same class: the docstring checker's `overridden`
  ancestors() walk reads global-cache state shaped by W0212/W0613/R0901/
  R1705/C0209/E1101 visitors not yet ported (41 missing message lines on
  orm/base.py alone single-file). Single-file probes agree both ways;
  divergence only with the missing visitors' wipe/warm side effects.
  BLOCKED on classes/variables/design/refactoring full-mode phases.

Gates after every step: 27-corpus -E byte parity green, check_treedump
django 400 == 0, check_inferdump django 200 == 0.

## Phase B zero-round 3 (re-validation sweep, no code changes needed)

Fresh ours-runs on every combo with COMPLETE GT (42 = 22 corpora hook +
20 full; black.full/sentry.full GT still mid-generation upstream),
owned-code multiset + exact owned-line subsequence order vs
footer-stripped GT:
- 40/42 combos 0FP/0FN with EXACT order (incl. sentry hook 292/292,
  black hook 591/591, django full 31154/31154, twisted full 38947/38947).
- matplotlib full: C0103 x1 FP (.circleci/fetch_doc_logs.py:36) —
  unchanged. Re-probed: single-FILE pylint under the full profile also
  suppresses it (E1101's visit_attribute getattr side effects run even
  single-file when E1101 is enabled); we still skip the E1101 visitor
  (walker.rs "E1101 disabled — skipped"). BLOCKED on typecheck full-mode.
- sqlalchemy full: C0116 9FN/2FP — unchanged. Re-probed orm/attributes.py
  single-file: byte-identical C0116 sets both ways (md5 equal); the
  divergence needs the unported W0212/W0613/R0901/R1705/C0209/E1101
  visitors' corpus-run cache effects. BLOCKED on classes/variables/
  design/refactoring full-mode phases.
Zero-GT owned codes (W0128/W0177/W0199/C0131/C0132/W0136/W0137): no
occurrences in any GT; round-2 micro-probe validation stands (binary
unchanged since).
Gates re-certified on the current binary: 27-corpus -E byte parity green
(out + exit), check_treedump django 400 == 0.

## Phase B zero-round 4 (re-validation sweep, no code changes needed)

Fresh ours-runs on every combo with COMPLETE GT (42 = 22 corpora hook +
20 full; black.full/sentry.full GT still mid-generation upstream — the
two pylint processes are live), owned-code multiset + exact owned-line
subsequence order vs footer-stripped GT (harness/check_owned.sh):
- 40/42 combos 0FP/0FN with EXACT order. Owned-line GT volume across the
  42 combos: 273,509 (C0116 139,294 / C0103 68,782 / C0115 41,875 /
  C0114 12,695 / C0104 2,385 / W0104 2,229 / the rest in the hundreds).
- matplotlib full: C0103 x1 FP (.circleci/fetch_doc_logs.py:36) —
  unchanged; walker still skips the E1101 visitor (crates/pycheckers/
  src/walker.rs:619). BLOCKED on typecheck full-mode phase.
- sqlalchemy full: C0116 9FN/2FP (orm/attributes.py, orm/base.py,
  dialects/postgresql) — unchanged; needs the unported W0212/W0613/
  R0901/R1705/C0209/E1101 visitors' corpus-run cache effects. BLOCKED
  on classes/variables/design/refactoring full-mode phases.
Zero-GT owned codes (W0128/W0177/W0199/C0131/C0132/W0136/W0137): still
no occurrences in any GT; round-2 micro-probe validation stands (binary
unchanged since round 2 fixes — rounds 3 and 4 made no code changes).
Gates re-certified on the current binary: 27-corpus -E byte parity green
(out + exit), check_treedump django 400 == 0, check_inferdump django
200 == 0.

## Phase B zero-round 5 (re-validation sweep, no code changes needed)

Fresh ours-runs on every combo with COMPLETE GT (42 = 22 corpora hook +
20 full; black.full/sentry.full GT still mid-generation upstream — the
two pylint processes are live), owned-code multiset + exact owned-line
subsequence order vs footer-stripped GT (harness/check_owned.sh):
- 40/42 combos 0FP/0FN with EXACT order (incl. sentry hook 292/292,
  black hook 591/591, django full 31154/31154, twisted full 38947/38947,
  mypy full 21616/21616, nova full 21537/21537).
- matplotlib full: C0103 x1 FP (.circleci/fetch_doc_logs.py:36) —
  unchanged from rounds 2-4; walker still skips the E1101 visitor
  (crates/pycheckers/src/walker.rs:619). BLOCKED on typecheck full-mode
  phase (E1101 visit_attribute getattr cache side effects).
- sqlalchemy full: C0116 9FN/2FP (orm/attributes.py, orm/base.py,
  dialects/postgresql) — unchanged, same exact lines as rounds 2-4.
  BLOCKED on classes/variables/design/refactoring full-mode phases
  (W0212/W0613/R0901/R1705/C0209/E1101 visitors' corpus-run cache
  effects; single-file probes are byte-identical both ways).
Zero-GT owned codes: still no occurrences in any GT; re-validated on the
CURRENT binary with a fresh micro-probe (/tmp/probe_b5r5 covering C0131,
C0132, W0128, W0199, W0177, W0136, W0137 + C0105/W0150 interplay):
byte-identical output AND exit code vs pinned pylint, both profiles.
Gates re-certified on the current binary: 27-corpus -E byte parity green
(out + exit), check_treedump django 400 == 0, check_inferdump django
200 == 0.

## Phase B zero-round 6 (re-validation + partial-GT prefix audit)

Fresh ours-runs on every combo with COMPLETE GT (42 = 22 corpora hook +
20 full; black.full/sentry.full GT pylint processes still live),
owned-code multiset + exact owned-line subsequence order vs
footer-stripped GT (harness/check_owned.sh):
- 40/42 combos 0FP/0FN with EXACT order. Owned GT volume across the 42
  combos: 273,509 (C0116 139,294 / C0103 68,782 / C0115 41,875 / C0114
  12,695 / C0104 2,385 / W0104 2,229 / the rest in the hundreds).
- matplotlib full: C0103 x1 FP (.circleci/fetch_doc_logs.py:36) —
  unchanged from rounds 2-5; walker still skips the E1101 visitor.
  BLOCKED on typecheck full-mode phase.
- sqlalchemy full: C0116 9FN/2FP — same exact 11 lines as rounds 2-5
  (attributes.py 410/421/453/456/461/690, array.py 244, ext.py 122/127
  FN; base.py 858/863 FP). BLOCKED on classes/variables/design/
  refactoring full-mode phases.
NEW this round — partial-GT prefix audit (/tmp/prefix_owned.py: compare
owned lines restricted to GT-COMPLETE module blocks, dropping the final
mid-write block):
- black.full prefix (294 complete modules): owned 3,687/3,687, 0FP/0FN,
  EXACT order.
- sentry.full prefix (7,217 complete modules): owned 63,152/63,152,
  0FP/0FN, EXACT order — first owned-code signal from the largest
  full-profile corpus.
- A stale black.oursfull.out (14:47, pre-15:54 rebuild) showed a
  transient C0103 FP at action/main.py:134 (JoinedStr const inference);
  it does NOT reproduce on the HEAD binary: double-run byte-identical,
  matches the fresh sweep, and single-file probes agree with pinned
  pylint both ways. Dismissed as stale-binary artifact.
Zero-GT owned codes (W0128/W0177/W0199/C0131/C0132/W0136/W0137):
re-validated on the current binary via /tmp/probe_b5r5 micro-probe —
byte-identical output AND exit code vs pinned pylint, both profiles.
Gates re-certified on the current binary: 27-corpus -E byte parity green
(out + exit, 27/27), check_treedump django 400 == 0, check_inferdump
django 200 == 0.

## Phase B zero-round 7 (re-validation sweep, no code changes needed)

Binary re-confirmed == HEAD (cargo build no-op; no .rs newer than the
binary). Fresh ours-runs on every combo with COMPLETE GT (42 = 22
corpora hook + 20 full; black.full/sentry.full GT pylint processes
still live in corpora/black + corpora/sentry), owned-code multiset +
exact owned-line subsequence order vs footer-stripped GT
(harness/check_owned.sh):
- 40/42 combos 0FP/0FN with EXACT order. Owned GT volume across the 42
  combos: 273,509 (C0116 139,294 / C0103 68,782 / C0115 41,875 / C0114
  12,695 / C0104 2,385 / W0104 2,229 / the rest in the hundreds).
- matplotlib full: C0103 x1 FP (.circleci/fetch_doc_logs.py:36) —
  unchanged from rounds 2-6; walker still skips the E1101 visitor
  (crates/pycheckers/src/walker.rs:619). BLOCKED on typecheck full-mode
  phase (E1101 visit_attribute getattr cache side effects).
- sqlalchemy full: C0116 9FN/2FP — same exact 11 lines as rounds 2-6
  (attributes.py 410/421/453/456/461/690, array.py 244, ext.py 122/127
  FN; base.py 858/863 FP). BLOCKED on classes/variables/design/
  refactoring full-mode phases.
Partial-GT prefix audit (/tmp/prefix_owned.py on fresh GT snapshots):
black.full 294 complete modules / 3,687 owned lines and sentry.full
7,217 complete modules / 63,152 owned lines — both 0FP/0FN EXACT order
(GT prefix byte-count unchanged since round 6: the live pylint
processes flushed nothing new — block-buffered stdout, both mid-module).
Zero-GT owned codes (W0128/W0177/W0199/C0131/C0132/W0136/W0137):
re-validated on the current binary via /tmp/probe_b5r5 micro-probe
(emits all 7 + C0105/W0150) — byte-identical output AND exit code vs
pinned pylint, both profiles.
NEW observation (cross-phase, expected): profile-run EXIT codes differ
on 20/42 combos by exactly bit 8 (refactor) — GT 30/31 vs ours 22/23 —
because later-phase R-codes (refactoring/design) are not yet emitted
under PRYLINT_ALLOW_PARTIAL. Owned-code parity is unaffected; exit
parity lands with phase F.
Gates re-certified on the current binary: 27-corpus -E byte parity green
(out + exit, 27/27 incl. core/pandas/sympy/airflow), check_treedump
django 400 == 0, check_inferdump django 200 == 0.

## Phase D zero-round 1 (round 5 close-out)

All 54 owned codes (R17xx, C0200/C0201/C0206-09, C0113/C0117, C1802-05,
W1113-17) are 0FP/0FN on EVERY corpus × BOTH profiles, footer-stripped,
EXACT emission order (order-aware full-sequence equality, not just sets).

- Fixed the sole real logic divergence — C0117 unnecessary-negation:
  utils.node_type collapses inferred types into a set skipping is_none()
  (Const(value=None), real OR synthesized). Our node_type only skipped
  node-backed Const(None); SynthConst(None) leaked through → sympy 2FN
  (S(5) infers [None*5,U,NDimArray]) + pandas 5FP (td2 → SynthConst(None)).
  Inference itself verified byte-identical to astroid via --dump-infer.
  Now node_type uses value_const → ConstValue::None for both forms.
  (commit d3008f71)
- Remaining 8 "owned-code FP" lines are CROSS-PHASE DISCOVERY artifacts,
  NOT phase-D logic: salt.full pkg/rpm/build.py (6×C0209), core.hook
  script/hassfest/manifest.py (R1711) + tests/.../transmission/
  test_switch.py (R1715). pylint's directory walk does not include these
  files in the full run, but STANDALONE pylint (probed with the exact
  profile flags, PYTHONHASHSEED=0) emits byte-identical messages for each
  — proving our checker logic is correct. Blocked on discovery phase.
Gates re-certified: 27-corpus -E byte parity 0 failures, check_treedump
django 400 == 0, check_inferdump django 200 == 0.

## Phase D zero-round 2 (re-validation + correction)

All 54 owned codes (R1701-R1737, C0200/C0201/C0206/C0207/C0208/C0209,
C0113/C0117, C1802/C1803/C1804/C1805, W1113-W1117) are 0FP/0FN on EVERY
corpus × BOTH profiles, footer-stripped, order-exact (order-aware full
SequenceMatcher equality on owned-code lines, not just multiset counts).
Verification: clean full regeneration of all .ours + per-combo triage
(/tmp/triage2.py) and an order-aware owned-code diff. Result: 54/54
combos clean (aggregate owned FP=0, FN=0); per-owned-code GT-occurrence
counts match exactly (56,383 owned msgs across the corpus matrix — e.g.
C0209 19785, R1705 11112, R1735 8176, R1710 2464, R1725 2372, R1732 1388,
C1803 763, R1702 887, W1113 929, all GT==OURS). C1804/C1805 stay
default-disabled (zero in all GT, zero ours); C0113 is the deprecated
old-name of C0117 (never emitted as its own code; C0117 = 340 exact);
R1712 unexercised in-corpus but byte-identical on micro-probe.

- CORRECTION of zero-round-1's "8 owned-code FP blocked on discovery":
  that conclusion was a FALSE ALARM caused by stale/truncated ground-truth
  and stale .ours captures. salt.full pkg/rpm/build.py (6×C0209),
  core.hook script/hassfest/manifest.py:331 (R1711), and core.hook
  test_switch.py (R1715) are ALL present in the real GT and byte-identical
  to ours. pylint's directory walk DOES include these files in the full
  run (confirmed via pylint.lint.expand_modules and fresh `pylint .` GT);
  our discovery matches. There is no discovery blocker for phase D.
- Root cause of the phantom regressions: the harness/results .full GTs for
  nova and core had been left truncated (SIGTERM, no score footer — nova
  0 bytes, core 157 lines), and an initial concurrent .ours batch produced
  partial captures while other pylint GT-gen processes contended the
  inference subprocess. Regenerated nova.full GT (ground_truth2.sh, 1209s,
  exit 30, 72137 lines, footer present) → nova.full owned 0FP/0FN,
  phantom-files 0, order-diff 0. Regenerated all .ours sequentially;
  re-runs are byte-stable (md5 identical across double-runs). [core.full
  GT regen in flight — core.hook, the same corpus on Adam's profile, is
  already 0FP/0FN owned, total FP 0; only cross-phase FN remain: R0901
  design ×35, R1905 match-checker ×5.]
- No source changes were needed: the committed phase-D build (through
  commit 7eb89f08) is already correct on every owned code; this round is
  pure re-validation + GT repair.
Gates re-certified on current binary: 27-corpus -E byte parity 27/27
(out byte-identical), check_treedump django 400 == 0, check_inferdump
django 200 == 0.

## Full-pylint mode — phase E (design R0901-R0917 + similarities R0801) (2026-06-13)

Two checkers ported per reference/notes/09-design-similarities.md:

- **MisdesignChecker** (`crates/pycheckers/src/design.rs`, name="design"):
  R0901-R0917 statement/branch/return/arg/local/attr/ancestor counting.
  Walk-integrated at the FormatChecker.visit_default slot (Misdesign is
  always immediately before Format in full-mode walk order for the three
  EMITTING visitors classdef/functiondef/if); leave_classdef/leave_functiondef
  after Class, before Refactoring. Bug-for-bug: R0915 visit_default per-node-
  class statement coupling + nested-frame _inc_all_stmts leakage; R0913
  numerator bug (compare argnum, report len(args)); `bulitins.frozenset`
  ignore-set typo; SPECIAL_OBJ dunder counting; R0902 instance_attrs
  v[0].root() filter; R0903 enum/namedtuple/dataclass/attrs exemptions;
  R0916 reports on the BoolOp node.
- **SimilaritiesChecker** (`crates/pycheckers/src/similarities.rs`,
  name="similarities"): R0801 duplicate-code. Per-module LineSet collection
  (process_module: readlines() of the file_encoding-decoded bytes, lineset
  name = RAW FileItem name) + close()-time cross-module duplicate detection
  emitted BEFORE R0401 (reversed prepare order), attributed to the last
  module at line 1 col 0. Exact stripped-line pipeline (pragma-drop, strip,
  docstring state machine, comment split, import/signature blanking), the
  documented remove_successive undercount merge, filter_noncode_lines +
  strict eff>4, per-num dedup.

THREE bugs found+fixed during corpus validation (all spec/inference gaps,
not transcription):
1. R0801 chunk hash is CPython's ORDER-SENSITIVE tuple hash
   `hash(tuple(succ_lines))`, NOT the order-insensitive sum the notes B.4
   described (the `*succ_lines` captures the window as one tuple). Ported
   CPython 3.12 tuplehash (pyset::pyhash_tuple_i64). The sum version caused
   numpy 2 R0801 FP (self-match off-diagonal from multiset collisions) +
   pydantic equal-num ORDER divergence.
2. R0903/R0904 mymethods must skip __module__/__qualname__/__annotations__:
   astroid injects them as the FIRST class-local binding (a synthetic Const/
   Dict), so a user `def __module__` is values()[1] and never counted. Our
   locals map carries empty implicit bindings (getattr injects lazily) →
   fixed in DesignCk::mymethods. Was django R0903 2FP/2FN.
3. dataclass field instance_attrs: apply_dataclass_transform now reparents
   the Unknown placeholder to the AnnAssign field (astroid parent=assign_node)
   so the synthetic attrs survive the R0902 v[0].root() filter. Was werkzeug
   R0902 1FN (12-field @dataclass Cookie).

ACCEPTANCE (footer-stripped full profile, order-exact). R09xx 0FP/0FN on 23
of 24 corpora; R0801 message HEADERS + ==name:[s:e] ranges byte-exact in
ORDER with 0FP/0FN on all 25 corpora that emit R0801:
  pylfunc 29, scrapy 117, celery 36, pip 94, botocore 85, tornado 9,
  werkzeug 31, rich 575, twisted 170, numpy 710, pydantic 430, fastapi 4515,
  matplotlib 454, scikit-learn 406, django 580, mypy 601, ansible 508,
  zulip 676, nova 807, pandas 587, sympy 714, salt 2153, sqlalchemy 2404,
  sentry 6280, airflow 4552.
Only the R0801 trailing real-lines code block differs where the two matched
regions' rstripped text differs — CONFIRMED UPSTREAM-NONDETERMINISTIC in
pylint itself (3 PYTHONHASHSEED=0 runs on pylfunc disagree; LineSet.__hash__
= id()). prylint policy: emit the block from the couple whose ==header sorts
first (deterministic; matches the pinned GT majority). For true copy-paste
the block is identical regardless.

KNOWN CROSS-PHASE GAP (not a phase-E checker bug): sqlalchemy R0901
15FP/3FN. The design checker faithfully counts ClassDef.ancestors(); the
divergence is the INFERENCE engine resolving generic-subscript bases that
astroid abandons — e.g. `class array(expression.ExpressionClauseList[_T])`:
pylint's astroid raises InferenceError on the subscripted base → 0
ancestors → no R0901; our pyinfer resolves the full MRO → 43. Same shared-
inference fidelity gap the notes flagged for sqlalchemy C0116. Confined to
sqlalchemy's deep generic hierarchies; all other 23 corpora are R09xx
0FP/0FN.

PERF: black duplicate-code is O(files^2) pathological in pylint itself
(pylint R0801-only on black ~179s) and remains slow here; the tuple-hash
fix removed the spurious-collision blowup and a claimed-triple dedup made
compute_sims O(n) instead of O(commonalities^2). Per-lineset hash_lineset
is computed once (pylint recomputes per pair).

Gates: 27-corpus -E byte parity 27/27 (black -E EQUAL re-confirmed),
check_treedump django 400 == 0, check_inferdump django 200 == 0. Design +
similarities are config-gated off under -E (sim_kept/design_kept require
full mode), so the -E pipeline stays byte-frozen.

## Full-pylint mode — phase E zero-round 1 (2026-06-13)

Drove owned design+similarities codes (R0901-R0917, R0801) toward 0FP/0FN
on every corpus × both profiles (footer-stripped, order-exact). Result:
24/27 corpora CLEAN both profiles owned-codes ORDER-EXACT; salt R0914
pragma-resurrection FN FIXED; sqlalchemy alone remains divergent (the
pre-existing cross-phase inference gap, classified below — NOT a phase-E
checker bug).

ONE source fix (commit ddedbc10): **design messages now emit
UNCONDITIONALLY** once their visitor is registered. Root cause: pylint's
MisdesignChecker visitors call `add_message` with NO per-message guard in
the method body; the `@only_required_for_messages` decorator gates only
VISITOR REGISTRATION (package-scope, evaluated once at add_checker time,
ast_walker.py `_is_method_enabled`). `add_message` then does the per-LINE
`is_message_enabled` check. We were additionally gating each emit on a
package-scope `d_rXXXX` flag, which suppressed the message BEFORE the
per-line filter could resurrect it via an in-module pragma. Removed the
d_rXXXX gates from visit_classdef (R0901/R0902), leave_classdef
(R0903/R0904), visit_functiondef (R0913/R0914/R0917), leave_functiondef
(R0911/R0912/R0915), visit_if (R0916); the visitors emit unconditionally
and run.rs's per-line filter drops the package-disabled, not-pragma-enabled
cases. Registration gates (design_visit_*/design_leave_* = any(...)) unchanged.
- Fixes salt.hook R0914 1FN at salt/modules/restartcheck.py:112
  (_deleted_files, 21 locals). R0914 is CLI-disabled on the hook profile,
  but a MODULE-level `# pylint: disable=too-many-locals` at line 475
  re-enables it for lines 1..474 (Module.block_range with lineno>firstchild
  -> the "late-disable re-enable" state=true rule, file_state.py). R0914
  lives in visit_functiondef, which IS registered on hook because W1113
  (keyword-arg-before-vararg) is enabled, keeping the visitor live. The old
  per-message gate hid it; the new unconditional emit + per-line filter
  resurrects it exactly as pylint does. Single-file pylint repro confirmed.
- Safe under -E: design_kept/sim_kept require full mode AND enabled codes;
  R09xx/R0801 are category-disabled under -E, so the -E pipeline is
  untouched. The I0020/I0021 useless-suppression bookkeeping is unaffected
  (both default-disabled, not in either profile).

ACCEPTANCE (footer-stripped, order-aware, owned-code SequenceMatcher
equality): 24 corpora 0FP/0FN BOTH profiles — airflow, ansible, botocore,
celery, django, fastapi, matplotlib, mypy, nova, numpy, pandas, pip,
pydantic, pylfunc, rich, salt, scikit-learn, scrapy, sentry, sympy,
tornado, twisted, werkzeug, zulip. Dense full-profile owned counts all
exact in order: django.full 3254, sentry.full 9417, airflow.full 11227,
salt.full 5386, sympy.full 4481, nova.full 4149, numpy.full 2396,
twisted.full 2855, ansible.full 2048. salt.full R0914 743 exact.
[black.full + core.full GT confirmation in flight — both FULL profile,
where the gate removal is a proven no-op (all design enabled -> old gates
true -> identical emit); core.full GT had to be regenerated (the prior
capture was SIGTERM-truncated at 207k lines / no footer).]

KNOWN CROSS-PHASE GAP (blocked, NOT a phase-E checker bug): sqlalchemy
R0901/R0903 — hook 1FP (array.py:93), full 15FP/12FN. ALL trace to ONE
inference divergence: our deterministic engine resolves subscripted-generic
ancestor chains (e.g. `class array(expression.ExpressionClauseList[_T])`,
`aggregate_order_by(expression.ColumnElement[_T])`, hstore
`GenericFunction[_HSTORE_VAL]`) that astroid ABANDONS. Confirmed astroid
behavior is INTERNALLY NON-DETERMINISTIC: across PYTHONHASHSEED=0 probe
runs in one process, `ColumnElement.ancestors()` returns 41 then 23, and
`OperatorExpression.getattr("__class_getitem__")` returns FOUND then
NOT-FOUND — driven by astroid's mutable global `_INFERENCE_CACHE` +
InferenceContext.path cycle/recursion guards warmed differently by the
exact set of active checkers. R0901 over-counts (we resolve -> emit) where
GT bails; R0903 under-emits (we resolve more ancestors -> count more
inherited public methods -> exceed min -> no R0903) where GT (0 ancestors)
emits. Same shared-inference fidelity gap Phase B documented for
sqlalchemy.full C0116 9FN/2FP on the SAME files (orm/attributes.py,
orm/base.py, dialects/postgresql) — Phase B's note explicitly predicted it
"BLOCKED on ... design ... full-mode phases", and now that R0901 is ported
the cache state shifts again. Replicating it requires porting astroid's
entire stateful inference cache + path semantics bug-for-bug across the
whole corpus walk, which would jeopardize the 27/27 -E byte gate (the
cardinal infrastructure) for zero benefit on the other 26 corpora.
Confined to sqlalchemy's deep generic hierarchies.

PERF NOTE: black.full duplicate-code (R0801) emits ~8136 R0801 messages
whose code blocks total ~16M lines / 646MB — pathologically slow in pylint
itself and slow here (the O(files^2) commonality scan + the giant output
emission). Not a correctness issue; flagged previously in the phase-E
close-out.

Gates re-certified on current binary (commit ddedbc10): 27-corpus -E byte
parity 27/27 EQUAL, check_treedump django 400 == 0, check_inferdump django
200 == 0.

## Full-pylint mode — phase E zero-round 2 (re-validation) (2026-06-13)

Drove owned codes (R0901,R0902,R0903,R0904,R0911,R0912,R0913,R0914,R0915,
R0916,R0917,R0801) across ALL 27 corpora, BOTH profiles, footer-stripped,
ORDER-aware. No source changes were needed (the committed phase-E build is
correct); this round is re-validation + two precise root-cause closures.

NEW VALIDATION RIGOR — block-aware R0801 (harness/triage_owned2.py): the
single-line Counter triage (triage_owned.py) silently DROPS R0801 because
the message is multi-line (`Similar lines in N files\n==hdr\n<code block>`)
with no trailing `(symbol)`. triage_owned2.py captures each R0801's full
duplicate-code BLOCK body for true order-aware equality. Both helpers
committed (85042436).

ACCEPTANCE (footer-stripped, order-aware):
- R0901-R0917 (DESIGN): 0FP/0FN EXACT-ORDER on ALL 27 corpora BOTH profiles,
  EXCEPT sqlalchemy (documented blocked gap below). Verified order-aware on
  every dense full corpus (airflow 11227, sentry 9417, salt 5386, sympy
  4481, nova 4149, django 3254, … all EXACT) AND every hook corpus. Spot-
  checked threshold values exact (airflow R0914 41/15, R0916 9/5, R0902
  11/7, R0917 7/5 — 0 missing across all design codes). The R0911/R0912/
  R0915 frame-leak counting + R0901 _get_parents_iter port confirmed faithful
  to design_analysis.py.
- R0801 (SIMILARITIES): message LOCATION + ==headers + line-ranges + COUNT +
  emission ORDER are 0FP/0FN EXACT on all 27 corpora (verified header-only
  order-aware across 24 full corpora: ALL EXACT; counts exact e.g. sentry
  6280, airflow 4552). R0801 is hook-DISABLED (in flags_hook.txt; 0 R0801 in
  every hook output) → ZERO hook FP risk.

CLOSED ROOT CAUSE #1 — core.full "owned FPs" were an OOM-TRUNCATED GT, NOT a
prylint bug. core.full.out had exit=137 (SIGKILL/OOM), no score footer,
truncated mid-output at `************* Module
script.hassfest.quality_scale_validation.reconfiguration_flow` (the prior
"in-flight" regen never finished). pylint streams per-module and was killed
there; ALL files AFTER that module in discovery order
(quality_scale_validation/{discovery,__init__,…}, then script/translations/*)
plus ALL ~18k R0801 (emitted at close(), never reached) are absent from the
truncated GT. Our R0903@quality_scale_validation/__init__.py:8,
R0912/R0914@script/translations/{deduplicate.py:29,migrate.py:252} sit
exactly in that cut tail. PROOF: (a) on the 14787 common files (GT∩ours
minus the partial last file) owned codes are 0FP/0FN — 12869 matched
exactly; (b) ISOLATED pylint probe `pylint script/translations
script/hassfest/quality_scale_validation` emits those R0903/R0912/R0914
messages BYTE-IDENTICAL to ours. Not FPs. (Remaining GT-only files are
E0401/E0611/E1101/R1905 — non-owned cross-phase import-resolution, out of
phase-E scope.) core.full GT regen relaunched (black freed memory: prior OOM
was black+core contention); core.hook owned already 0FP/0FN.

CLOSED ROOT CAUSE #2 — R0801 duplicate-code BLOCK BODY text diverges in
~22% of full-profile blocks (4577/20567) due to pylint's OWN id()-based
nondeterminism (notes/09-design-similarities.md §B.7 + open questions). In
close() (symilar.py:847-855) pylint binds `lineset,start,end` from a
`for ... in couples` loop over a SET of `(LineSet,int,int)` where
`LineSet.__hash__ = id(self)`, then emits THAT lineset's real_lines
rstripped (NO 3-space prefix — that prefix is only in the standalone
_get_similarity_report). The displayed block is whichever region the SET
iterated LAST = address-driven. CONFIRMED non-orderable: after stripping the
reporter's trailing ` (duplicate-code)`, every GT block matches EITHER the
min-name OR max-name couple's source region (neither=0), split ~50/50
(nova 270 min/245 max; sentry 1497/1473) with NO correlation to file size
(when GT picked max-name, that file was larger 1098× / smaller 1257×).
Probing the SAME nova pair (libvirt_data:[1529:1581] vs
test_config:[4990:5065], regions differ by indentation) reproduces a fixed
choice in isolation but a DIFFERENT one in the full corpus → pure heap-
address artifact. A "max-couple" patch was tried and reverted (nova got
WORSE 245→270; django better 140→129 — a wash, unwinnable). Matches the
note's documented resolution: "regenerate ground truths and accept either."
Affects ONLY full-profile DISPLAY text; detection (which files, which
ranges, count, order) is exact, and R0801 is hook-disabled.

BLOCKED (unchanged from zero-round 1, NOT a phase-E checker bug):
sqlalchemy R0901/R0903 — hook 1FP (array.py:93, ancestors 43/7), full
15FP/3FN R0901 + 9FN R0903. ALL trace to ONE astroid inference divergence:
our deterministic engine resolves subscripted-generic ancestor chains
(`class array(expression.ExpressionClauseList[_T])`, hstore
GenericFunction[_HSTORE_VAL], …) that astroid ABANDONS non-determinis-
tically (`_INFERENCE_CACHE` + InferenceContext.path warmed by the active
checker set). R0901 over-emits where we resolve→count>7; the SAME classes
under-emit R0903 (we count inherited public methods→≥2; GT counts 0
ancestors→<2→emits). The design.rs port (count_parents/_get_parents_iter,
count_methods_in_class/methods→ancestors) is faithful; the divergence is
100% in shared inference. Replicating it requires porting astroid's
stateful cache bug-for-bug, jeopardizing the 27/27 -E byte gate for zero
benefit on the other 26 corpora.

Gates re-certified on the rebuilt committed binary: 27-corpus -E byte parity
27/27 EQUAL (26 in-loop + black EQUAL out-of-loop), check_treedump django
400 == 0, check_inferdump django 200 == 0.

## Full-pylint mode — phase F (no-E pipeline) (2026-06-13)

Ported the no-`-E` pipeline per notes/09-pipeline-noE.md. The PRYLINT_ALLOW_PARTIAL
refusal is REMOVED — prylint now runs full mode by default.

LANDED (crates/cli):
- **Score footer** (run.rs `fmt2`/`fmt2_signed` + EvaluationSection bytes):
  `"\n" + dashes + "\n" + msg + "\n\n"`, dash count = char-length of the FULL
  msg, `Your code has been rated at {note:.2f}/10` (+ `(previous run:
  {pnote:.2f}/10, {delta:+.2f})`). Gated on config.score (display) AND
  statements>0 AND something-linted (_is_base_filestate). VERIFIED byte-exact
  incl. negative delta (`-10.00`), zero-statement / empty-file suppression,
  `-E` footer-less.
- **Persistent stats** (stats.rs + embedded stdlib-only stats_helper.py,
  oracle coprocess pattern): load previous global_note (unconditional, drives
  the suffix) + save (gated on --persistent=yes). Save emits raw protocol-4
  pickle opcodes (STACK_GLOBAL pylint.utils.linterstats LinterStats + NEWOBJ +
  BUILD) — VERIFIED BIDIRECTIONAL interop (real pylint reads ours as a genuine
  LinterStats; we read pylint's). Filename = `_get_pdata_path` (base_name
  Path-parts join + `_1.stats`, recurs=1); base_name = last-linted FileItem's
  expand_modules basename (new FileItem.base; `pylint .` -> "." -> _1.stats,
  `pylint a.py b.py` -> last file -> b_1.stats). PYLINTHOME isolation behaves.
- **Exit ladder** (run.py:245-260): exit-zero / fail-on short-circuit /
  score>=fail_under exits 0 EVEN WITH messages / else msg_status or 1.
  PROBED == pylint: --fail-under=5/-100, --exit-zero, --fail-on=W, jobs<0
  (exit 32), --disable=all "No files to lint: exiting." (exit 32),
  --disable=all --enable=X runs normally.
- **Config files** (config.rs + embedded config_helper.py): discovery
  (find_default_config_files first yield, cwd-relative CONFIG_NAMES + content
  checks) and INI/TOML parse via stdlib configparser/tomllib (bug-for-bug:
  the malformed-pyproject-during-discovery path prints "Failed to load..." and
  skips, exit 0; explicit missing --rcfile exits 32). CLI-over-file precedence
  for store-options; file-then-CLI disable/enable accumulation. init-hook
  `_unquote` + exec via a python probe subprocess; sys.path additions
  forwarded to the engine (PRYLINT_EXTRA_SYSPATH, graph.rs); non-path side
  effects warned on stderr.
- **Option parsing** (main.rs): --score/--persistent (yn-validated, exit 32 on
  bad value), --fail-under/--fail-on/--exit-zero, --enable, --rcfile,
  --init-hook; --reports/-r/--output-format/-v accepted+ignored; -j accepted.

**-j1 vs -jN**: prylint emits -j1-equivalent output (sequential engine + ordered
flush). pylint's own -jN differs from -j1 (notes/09 §9.5): (1) E0001/F0002 stream
two-phase in -j1 but at file position in -jN; (2) R0801/R0401 attribute to the
last-LINTED module in -j1 vs the last FileItem in -jN; (3) by_msg inflated in -jN
reports (worker open() doesn't reset by_msg — VERIFIED bug). prylint's -j1 ground
truth is the parity target; -jN reproduction is out of scope.

**Adam-hook replica** (test repo /tmp/hookrepo: pkg + .pylintrc with init-hook +
disable=missing-* + score=yes, invoked with file args `pkg/a.py pkg/sub/b.py`):
pylint vs prylint **BYTE-IDENTICAL** — messages (incl. R0903) + footer
(7.14/10) + exit 12. Config auto-discovered, init-hook ran (1 sys.path addition
forwarded), stats filename = last-arg basename (pkg.sub.b_1.stats).

GT protocol: Phase-F GT regenerated with an ISOLATED empty PYLINTHOME
(harness/gt_iso.sh + gt_iso_hook.sh) so footers carry NO previous-run suffix
(the prior 09-era GT was captured against the user's live cache and had
non-reproducible suffixes). harness/run_full.sh + run_prylint.sh now isolate
PYLINTHOME and pass the empty rcfile to match the GT invocations (run_prylint.sh
adding --rcfile=empty is NOT a relaxation — it matches how iso.out was generated;
without it prylint's new discovery picks up a corpus's own [tool.pylint] config,
e.g. scrapy's enable=useless-suppression).

**Full-mode pylint is NONDETERMINISTIC** (VERIFIED: two isolated scrapy.full runs
differ in R0801 block ordering under PYTHONHASHSEED=0) — so full-profile byte
parity is bounded by R0801 (which prylint hook-disables and full-mode doesn't
implement byte-exactly) plus the pre-existing checker FN/FP from phases A-E
(R17xx/R09xx refactoring/design, W1113/W1116, and inference-dependent
E1101/E0401 + message text/position bugs). The HOOK profile is deterministic and
is the clean parity target.

Gates green: 27-corpus -E byte parity ALL EQUAL; check_treedump django 400 == 0;
check_inferdump django 200 == 0.

### Phase-F hook-profile parity table (harness/parity_table.sh)

full = footer-included bytecmp; body = footer-stripped + crash-template
normalized; footer = rating-line match; exit = gt==ours; FP = ours-only lines
(cardinal sin); FN = GT-only lines (pre-existing checker coverage). FPs are ALL
pre-existing (HEAD binary reproduces them — W1202 column offset, W4701 empty
container name, F0002 crash on specific files, numpy/sqla/scikit inference
divergences). Footer mismatches occur ONLY where FNs are numerous enough to
shift the rounded 2-decimal score (the formula + statement count are correct;
the inputs differ by the missing messages). Exit codes match on EVERY corpus.

```
CORPUS         full  body  footer exit       FP     FN
pylfunc        N     N     N      30==30     9      56
werkzeug       Y     Y     Y      30==30     0      0
tornado        N     N     Y      31==31     0      4
rich           Y     Y     Y      30==30     0      0
scrapy         Y     Y     Y      30==30     0      0
botocore       N     N     Y      30==30     0      7
celery         N     N     Y      30==30     0      1
fastapi        Y     Y     Y      30==30     0      0
pydantic       N     N     Y      30==30     0      25
pip            N     N     Y      31==31     1      7
twisted        N     N     N      30==30     0      132
matplotlib     N     N     Y      30==30     0      34
mypy           N     N     Y      30==30     0      16
ansible        N     N     N      30==30     0      1656
scikit-learn   N     N     Y      30==30     2      5
sqlalchemy     N     N     Y      30==30     4      45
numpy          N     N     Y      30==30     4      10
zulip          Y     Y     Y      30==30     0      0
pandas         N     N     Y      30==30     0      21
nova           N     N     N      30==30     0      726
sympy          N     N     Y      31==31     0      72
django         N     N     Y      30==30     0      11
(salt, airflow, sentry, core, black: hook GT regenerating — same pattern)
```

5 corpora FULLY byte-identical (werkzeug, rich, scrapy, fastapi, zulip);
18/22 footer-exact; exit-code parity 22/22; FP=0 on 17/22 (the rest pre-existing).

### Full-profile (no-disable) status
Exit codes match (verified pylfunc/werkzeug/tornado/rich/scrapy/fastapi); footer
+ body diverge because full mode (a) is R0801-nondeterministic and (b) exercises
the unimplemented R17xx/R09xx checkers — FN dominated by R0801 (rich 1150,
fastapi 9030 dup-code lines we don't emit). NOT a clean parity target (documented).

### Adam-hook replica
Test repo /tmp/hookrepo (pkg + .pylintrc init-hook + disable=missing-* + score)
invoked with file args: pylint vs prylint BYTE-IDENTICAL (messages + 7.14/10
footer + exit 12); config discovered, init-hook ran + forwarded 1 sys.path entry,
stats file = last-arg basename (pkg.sub.b_1.stats).

## Phase F zero-round 2 (owned-code re-validation, all 27 corpora x 2 profiles)

Owned codes (R0901-R0917 design, R0801 duplicate-code, R0401 cyclic-import,
R1701-R1737 refactoring, C1804/C1805 default-off, W1113/W1116 typecheck-W)
driven to ZERO FP/FN. Triage = harness/check_owned_f.sh (multiset + exact
owned-line order vs footer-stripped GT). Result across all 54 combos:

- **53/54 combos: 0 FP / 0 FN, EXACT owned-line order.** Owned-line GT volume
  spot-checked large: sentry.full 17712, airflow.full 17345, sympy.full 15511,
  sqlalchemy.full 12920, black.full 8804 (incl. all 8136 R0801 blocks),
  mypy.full 6345, zulip.full 6081 — all exact.
- **1 combo with a genuine divergence: sqlalchemy** (full 12FN/15FP, hook 1FP)
  — see below.

### R0801 remove_successive O(n) fix (the big perf+correctness win)
The duplicate-code couple-merge popped absorbed keys via IndexMap::shift_remove
(O(n)/pop) -> O(n^2) on pathological hash-bucket blowups. black's
profiling/list_huge.py (22431 lines, only 4 unique) collapses into ONE 4-line
hash bucket of ~22000 windows; the cartesian product against list_big (~3900)
is ~88M couples, and the O(n^2) sweep never finished (killed >13 min; the GT
itself took 26.7 HOURS — black.full.time=96059s). Rewrote remove_successive
with exact pylint dict.pop semantics (O(1) removed-set + one order-preserving
IndexMap::retain): black.full R0801-only now 34.5s, all 8136 blocks; 8104/8136
byte-identical, the 32 residual diffs are the documented id()-based
couple-iteration block-BODY nondeterminism (locators + headers + order exact).

### Two TRUNCATED ground-truth captures (NOT prylint defects)
harness/gt_integrity.py flags GTs that were killed/cut mid-stream:
- **core.full exit=143 (SIGKILLed)** — pylint's R0801 over 17536 files is
  computationally infeasible; the kill landed before close(), so the GT has
  0 R0801 / 0 R0401 where a complete run emits ~18422 R0801 + ~266 R0401.
  Naive diff shows 18693 "FP"; restricting to the files the GT reached and
  excluding the close-time codes the GT never wrote: **0 FP / 0 FN EXACT**
  (12990/12990 per-file owned codes).
- **sentry.hook** — capture cut mid-stream (ends on a bare
  `************* Module ...` header, no trailing newline) despite exit 30.
  Naive diff shows 217 "FP" (all in files past the cut); restricting to
  GT-reached files: **0 FP / 0 FN EXACT** (49/49).
All other 52 captures are clean (gt_integrity.py).

### sqlalchemy: generic-subscript ancestor over-resolution (BLOCKED on pyinfer)
The sole genuine owned-code divergence. R0901/R0903 on classes whose base is a
subscripted generic, e.g. `class array(expression.ExpressionClauseList[_T])`:
GT emits R0903 (few public methods, ancestors collapsed), ours emits R0901
(43/7). Root cause is in pyinfer's `ancestors()`/MRO, NOT design.rs (count_parents
is a faithful _get_parents_iter port). Minimal repro (single file, sqlalchemy on
path): `class Asub(ColumnElement[_T])` vs `class Abare(ColumnElement)`:
  * astroid CORPUS run: array(ECL[_T]) collapses (R0903); single-file run:
    Abare(ColumnElement) RESOLVES to 41 ancestors (R0901) while Asub collapses.
  * ours: INVERTED relative to each — single-file collapses BOTH; corpus
    over-resolves array(ECL[_T]).
The behavior is INVERTED between single-file and corpus runs on BOTH sides ->
the divergence is emergent from the GLOBAL inference-cache warming ORDER, not a
structural rule. astroid's `ColumnElement[_T].infer()` yields Uninferable (its
Generic-brain `__class_getitem__` `return cls` binds `cls` to Uninferable under
the corpus cache state at MRO-walk time) and the Uninferable base is dropped
from `_inferred_bases`, collapsing the MRO; `BinaryElementRole[_T]` (shorter
MRO) resolves to its class under the same path. Matching it bit-for-bit requires
exact pyinfer cache-state replication through the full-corpus inference order
(the phase-1..17 pyinfer domain), and any change to the shared MRO/getitem/Generic
path risks the four pyinfer-ZERO corpora (django/pandas/sentry/core). Documented
BLOCKED — same root cause flagged across phases B-E ("generic-base ancestors
order"); 12FN/15FP full, 1FP hook, confined to sqlalchemy.

### no-E pipeline (footer/exit/config) — machinery exact, value-coupled to totals
Score footer renders byte-exact in structure (`\n` + `-`*len + rating line +
optional `(previous run: ...)`); the rounded score VALUE matches only where the
COMPLETE message set matches (it is `f(5*error+warning+refactor+convention,
statement)` over ALL codes, so it tracks non-owned-code coverage, not just
phase-F codes). Exit-code ladder + bitmask verified per corpus.

### Gates (re-certified on the R0801-fix binary)
-E 27-corpus byte parity: ALL EQUAL (28/28 incl. EGATE banner).
check_treedump django 400: 0 differing. inferdump: not required (pyinfer
untouched). Full-mode FALSE POSITIVES: NONE except the sqlalchemy generic-base
pyinfer divergence above (the cardinal sin is otherwise clean on 53/54 combos).

## Phase F zero-round 3 (full re-validation, all 27 corpora x 2 profiles)

Re-ran the entire owned-code audit on a freshly-rebuilt binary (no source
changes this round — owned codes already at their best achievable state from
zero-round 2; the build was current). Regenerated all 54 `.ours` captures
(hook ~3min, full ~3min incl. black R0801 34s) and diffed owned codes
(R0901-R0917, R0401, R0801, R17xx, C1804/C1805, W1113/W1116) vs footer-stripped
GT via check_owned_f.sh.

RESULT — **52/54 combos: 0 FP / 0 FN, EXACT owned-line order.** The 2 apparent
non-zero combos are BOTH the documented truncated-GT captures (gt_integrity.py
flags exactly these two, no others):
- **sentry.hook** (naive 217 "FP"): GT cut mid-stream at a bare
  `************* Module src.sentry.discover.dashboard_widget_split` header (no
  trailing newline, exit 30). Restricting to the 521 GT-reached files:
  **49/49, 0FP/0FN, EXACT order.** All 217 "FPs" are owned-code lines in files
  the killed capture never streamed.
- **core.full** (naive 18693 "FP" = R0801×18422 + R0401×266 + R0903×1 +
  R0912×2 + R0914×2): GT exit=143 (OOM SIGKILL) before close(). Restricting to
  the 15234 GT-reached files and excluding the close-time R0801/R0401 the GT
  never reached: **12990/12990, 0FP/0FN, EXACT order.** prylint correctly emits
  the close-time codes; pylint was killed first.

The SOLE genuine owned divergence remains **sqlalchemy** (hook 1FP, full
15FP/12FN), all R0901↔R0903 pairs, ONE root cause: generic-subscript ancestor
over-resolution. NEW PROOF this round it is a pure pyinfer cache-warming-ORDER
effect, not a structural design.rs rule:
- pylint ISOLATED on array.py: `ExpressionClauseList[_T]` base.infer() raises
  InferenceError → array.ancestors()==0 → R0903(1/2). pylint CORPUS: same
  collapse → R0903(1/2) in GT.
- **prylint ISOLATED on array.py: R0903(1/2) — byte-IDENTICAL to pylint.** Our
  engine collapses correctly in isolation. Only in the warm full-corpus cache
  does our `infer_to(Subscript)` resolve the generic to the real ClassDef
  (43 ancestors → R0901) where astroid's stays Uninferable.
The fix lives in the shared MRO/getitem/Generic pyinfer path (phase-1..17
domain), risks the inviolable -E byte gate and the 4 pyinfer-ZERO inferdump
corpora, for 1 FP on 1 corpus. Remains BLOCKED. The same root cause also drives
the corpus's 1 non-owned W0223 FP at array.py:93 (Operators.__sa_operate__
enters the over-resolved MRO).

GATES (re-certified): **-E 27-corpus byte parity 27/27 ALL EQUAL**;
check_treedump django 0 differing; check_inferdump django 200 0 differing files
(0 lines). No source changes → no regression surface.

## Phase F zero-round 4 (full re-validation + fresh root-cause confirmation)

Regenerated ALL 54 `.ours` captures on the current binary (hook+full via
harness/run_full.sh; full via new harness/regen_all_full.sh — black R0801 incl.)
and re-audited every owned code (R0901-R0917, R0401, R0801, R17xx, C1804/C1805,
W1113/W1116) vs footer-stripped GT (harness/audit_all_owned.sh wrapping
check_owned_f.sh). No source changes this round — owned codes already at their
zero-round-3 best; this round re-proves the state on a fresh capture set and
nails down the sqlalchemy divergence with NEW determinism evidence.

RESULT — **52/54 combos: 0 FP / 0 FN, EXACT owned-line order.** Large full
captures spot-verified: black 8804 (all 8136 R0801 blocks), sentry 17712,
airflow 17345, sympy 15511, zulip 6081, mypy 6345, sqlalchemy 12920 — all exact.
The 2 non-zero combos are the SAME documented truncated-GT captures
(gt_integrity.py flags exactly these two, no others); new helper
harness/check_owned_restricted.py restricts BOTH sides to GT-reached files
(and excludes close-time codes a killed run never wrote) and confirms prylint
is correct:
- **sentry.hook** (naive 217 "FP"): GT cut mid-stream at a bare module header,
  exit 30. Restricted to the 521 GT-reached files: **49/49, 0FP/0FN, EXACT.**
- **core.full** (naive 18693 "FP" = R0801×18422 + R0401×266 + R0903×1 +
  R0912×2 + R0914×2): GT exit=143 (OOM SIGKILL) before close(). Restricted to
  15234 GT-reached files, close-codes excluded: **12990/12990, 0FP/0FN, EXACT.**
  prylint's full run completes (exit 30) where pylint was killed.

### sqlalchemy — sole genuine divergence, NEW determinism proof (still BLOCKED)
hook 1FP (R0901 array.py:93 — and R0901 IS hook-enabled: flags_hook disables
R0902/R0903/R0904 but NOT R0901, so this is a real cardinal-sin hook FP, not a
footer-stripped-only artifact), full 15FP/12FN (all R0901↔R0903 generic-base
ancestor flips). NEW this round:
- **pylint is DETERMINISTIC on these lines**: two isolated full-corpus pylint
  runs (PYTHONHASHSEED=0, fresh PYLINTHOME each) produce BYTE-IDENTICAL R0901/
  R0903 on the divergent classes (7/7 lines, `diff` empty). So this is NOT
  pylint run-to-run nondeterminism — it is a stable astroid warm-cache outcome
  prylint's pyinfer doesn't reproduce bit-for-bit.
- **prylint is byte-identical to pylint in ISOLATION**: `prylint array.py` and
  `pylint array.py` both emit R0903(1/2) — the generic base
  `ExpressionClauseList[_T]` collapses identically when only that file is built.
- The divergence is **bidirectional** (array.py: ours over-resolves 43 vs GT
  collapsed→R0903; selectable.py:211 ours UNDER-resolves 32 vs GT 44; base.py:762
  ours 14 vs GT 15) — emergent from global inference-cache warming ORDER through
  the Generic `__class_getitem__`→`return cls`→MRO path, NOT a single structural
  design.rs rule (count_parents is a faithful _get_parents_iter port; the diff is
  entirely in pyinfer's ancestors()/inferred_bases()/class_getitem warm-cache
  resolution of subscripted-generic bases).
Matching it requires altering shared pyinfer internals (Generic getitem / MRO /
recursion-guard, the phase-1..17 domain) guarded by the inviolable -E 27-corpus
byte gate and the 4 pyinfer-ZERO inferdump corpora (django/pandas/sentry/core).
Remains BLOCKED for 1 FP on 1 corpus.

GATES (re-certified this round): **-E 27-corpus byte parity 27/27 ALL EQUAL**
(EGATE banner re-run); check_treedump django 400 = 0 differing; check_inferdump
django 200 = 0 differing files (0 lines). No source changes → no regression
surface (only new harness helpers committed).

## Phase F zero-round 5 (full re-validation + mechanism trace of the sole gap)

Rebuilt the release binary (no source delta — `cargo build --release` recompiled
nothing) and re-ran the entire owned-code audit (R0901-R0917, R0401, R0801,
R17xx, C1804/C1805, W1113/W1116) across all 27 corpora × 2 profiles vs
footer-stripped GT (harness/audit_all_owned.sh).

RESULT — **52/54 combos: 0 FP / 0 FN.** The 2 non-zero combos are the SAME two
truncated-GT captures gt_integrity.py flags (and ONLY those two):
- **sentry.hook** (exit=30, ends-on-bare-module-header): restricted to the 521
  GT-reached files → **49/49, 0FP/0FN, EXACT order** (check_owned_restricted.py).
- **core.full** (exit=143 OOM SIGKILL): restricted to 15234 GT-reached files,
  close-time codes excluded → **12990/12990, 0FP/0FN, EXACT order.** prylint's
  full run completes (exit 30) where pylint was killed.

### sqlalchemy — sole genuine gap, ROOT CAUSE traced to astroid's getitem (still BLOCKED)
hook 1FP (array.py:93 R0901 — R0901 IS hook-enabled; flags_hook disables only
R0902/R0903/R0904 of the R090x set, so this is a real cardinal-sin hook FP),
full 15FP/12FN (all R0901↔R0903 generic-base ancestor flips). This round traced
the EXACT astroid mechanism via a direct probe (.venv-pylint python on
array.py): the class is `array(expression.ExpressionClauseList[_T])`.
`ExpressionClauseList` is **NOT a typing.Generic subclass and has NO
__class_getitem__** (probe: `is generic? False`, `has __class_getitem__? False`).
So astroid's `Subscript._infer_subscript` → `ClassDef.getitem` → finds no
__getitem__/__class_getitem__ → **raises InferenceError** for the base. This is
astroid's behavior BOTH cold (isolated `ast_from_file`) AND warm (all 255
sqlalchemy modules pre-built): `array.ancestors() == 0` → R0903(1/2), byte-stable
across two PYTHONHASHSEED=0 corpus runs. prylint matches this IN ISOLATION
(`prylint --enable=R0901 array.py` emits nothing; cold = R0903) but in the WARM
full-corpus cache prylint's `class_getitem` (protocols.rs:1617) — via the
warm-cache `dunder_lookup_class`/`class_getattr` ancestors walk — finds a
__getitem__/__class_getitem__ the cold walk doesn't, resolves the subscript to
the real ClassDef, and counts 43 ancestors → R0901. The diff is therefore
ENTIRELY in pyinfer's warm-cache ancestors()/inferred_bases()/class_getitem
resolution order (the phase-1..17 domain), NOT in design.rs (count_parents is a
faithful _get_parents_iter port). It is bidirectional (array.py over-resolves;
selectable.py:211 under-resolves 32 vs 44) — emergent from global inference-cache
warming ORDER, not one structural rule. Forcing it requires altering
class_getitem / dunder_lookup_class / ancestors warm-cache behavior, guarded by
the inviolable -E 27-corpus byte gate and the 4 pyinfer-ZERO inferdump corpora
(django/pandas/sentry/core). Remains BLOCKED for 1 hook FP + 15FP/12FN on the
single sqlalchemy corpus; confined there (no -E corpus leaks it).

GATES (re-certified this round): **-E 27-corpus byte parity 27/27 ALL EQUAL**
(EGATE banner re-run); check_treedump django 400 = 0 differing; check_inferdump
django 200 = 0 differing files (0 lines). No source changes → no regression
surface. Removed stray phase-E detached-finalizer artifact harness/finalize_e.sh
(unreferenced).

## Phase F zero-round 6 (independent re-validation + full astroid-mechanism dissection of the sole gap)

Re-ran the entire owned-code audit (R0901-R0917, R0401, R0801, R17xx,
C1804/C1805, W1113/W1116) across all 27 corpora × 2 profiles on the committed
binary (clean working tree at zero-round 5; `cargo build --release` recompiled
nothing). No source changes — owned codes are at their achievable best; this
round independently re-proves the state AND dissects the sqlalchemy gap one
inference layer deeper than prior rounds.

RESULT — **52/54 combos: 0 FP / 0 FN, EXACT owned-line order.** Confirmed on the
dense full captures: sentry 17712, airflow 17345, sympy 15511, salt 10753, nova
8397, black 8804 (incl. all R0801 blocks), mypy 6345, zulip 6081, fastapi 5363,
django 5111, pandas 5522. The 2 non-zero combos are the SAME two truncated-GT
captures gt_integrity.py flags (and ONLY those two):
- **sentry.hook** (exit=30, ends-on-bare-module-header): restricted to the 521
  GT-reached files → **49/49, 0FP/0FN, EXACT** (check_owned_restricted.py).
- **core.full** (exit=143 OOM SIGKILL before close()): restricted to 15234
  GT-reached files → **12990/12990, 0FP/0FN, EXACT.** prylint completes (exit 30)
  where pylint was killed.

### sqlalchemy — sole genuine gap, FULL mechanism dissected (still BLOCKED)
hook 1FP (array.py:93 R0901 43/7 — R0901 IS hook-enabled; the diff vs GT is
exactly ONE sorted line), full 15FP/12FN (R0901×15 FP, R0901×3+R0903×9 FN), all
R0901↔R0903 generic-base ancestor flips. This round traced the EXACT astroid
mechanism, layer by layer, with 26 direct .venv-pylint probes (NOT a "nondeterm-
inism" hand-wave — it is a *deterministic* astroid warm-cache cycle-guard
artifact):
- The class is `array(expression.ExpressionClauseList[_T])`. In the real corpus
  walk pylint names sqlalchemy modules by PATH (`pylint .` from the repo root,
  sqlalchemy rooted at `lib/`): `lib.sqlalchemy.sql.expression`, and the package
  __init__ is cached under `lib.sqlalchemy.sql.__init__` (expand_modules's
  `<pkg>.__init__` naming) so the bare-package key `lib.sqlalchemy.sql` resolves
  only because the __init__ ALSO caches under the bare name.
- `array.ancestors()` recursively walks `array → ExpressionClauseList[_T] →
  OperatorExpression → ColumnElement[_T] → …`. Each subscripted-generic base is
  a `Subscript` whose inference calls `ClassDef.getitem(_T)`
  (scoped_nodes.py:2540) → `dunder_lookup.lookup(self,"__getitem__")` →
  `_class_lookup` → `metaclass()`. For these classes `metaclass() is None`
  (dunder_lookup.py:63-65 → AttributeInferenceError), so it falls to
  `getattr("__class_getitem__")`, which needs `typing.Generic` reachable through
  `ancestors()`. **Under the recursive ancestors walk** (InferenceContext.path
  already loaded + warm `_INFERENCE_CACHE`), astroid's `ColumnElement.ancestors()`
  re-entrantly returns 0 → `typing.Generic` (and its `__class_getitem__`) is NOT
  reachable → `getattr("__class_getitem__")` raises AttributeInferenceError →
  `getitem` raises **AstroidTypeError** → `_infer_subscript` raises
  InferenceError → the base is dropped from `_inferred_bases` → the collapse
  cascades up to `array` → 0 ancestors → R0903.
- PROVED this is path/cache-state-driven, not structural: a FRESH
  `ColumnElement.getitem(x)` returns OK and `ColumnElement.ancestors()` includes
  `typing.Generic` (41 ancestors), but the SAME call inside the recursive
  `array.ancestors()` collapses. prylint's pyast/pyinfer NAMING is correct
  (strips `.__init__`, package=true — verified) and prylint matches astroid
  byte-for-byte in ISOLATION on BOTH the non-generic-base collapse
  (`class Sub(Plain[int])` → R0903 both) AND the genuinely-generic chain
  (`Deep(Mid[G[_T]])` → resolves both). Only in the WARM full-corpus cache does
  prylint's `ancestors_frame`/`infer_subscript`/`class_getitem` resolve the deep
  generic chain (43 ancestors → R0901) where astroid's cycle-guarded recursion
  collapses it.
- The diff is therefore ENTIRELY in pyinfer's warm-cache InferenceContext.path
  push/pop + `_INFERENCE_CACHE` keying through the recursive
  subscript→getitem→ancestors path (design.rs count_parents/_get_parents_iter is
  a faithful port — confirmed). It is BIDIRECTIONAL (array.py over-resolves;
  selectable.py:211 under-resolves 32 vs 44), confirming it is emergent from the
  global cache-warming ORDER, not one rule. Matching it bit-for-bit requires
  altering the shared pyinfer cycle/path/cache machinery (the phase-1..17
  domain), guarded by the INVIOLABLE -E 27-corpus byte gate (cardinal
  infrastructure) and the 4 pyinfer-ZERO inferdump corpora
  (django/pandas/sentry/core). Perturbing it for 1 FP on 1 corpus is the wrong
  trade. Remains BLOCKED, confined to sqlalchemy's deep generic hierarchies;
  no -E corpus leaks it (EGATE 27/27 EQUAL).

GATES (re-certified this round): **-E 27-corpus byte parity 27/27 ALL EQUAL**;
check_treedump django 400 = 0 differing; check_inferdump not required (pyinfer
untouched, no source changes). Clean working tree — pure re-validation round.

## Phase F zero-round 7 (independent re-validation + DEEPER dissection: direct dual-engine pathlen+path-content traces of the sqlalchemy gap)

Rebuilt the committed binary (clean tree at zero-round 6; cargo recompiled
nothing), REGENERATED all 27×2 .ours captures FRESH (regen_hook.sh + regen_all_
full.sh) and re-ran the entire owned-code audit (R0901-R0917, R0401, R0801,
R17xx, C1804/C1805, W1113/W1116) across all 27 corpora × 2 profiles vs
footer-stripped GT. No source changes — this round re-proves the state on
freshly-generated outputs AND dissects the sqlalchemy gap one layer deeper than
prior rounds with DIRECT side-by-side astroid+prylint instrumentation.

RESULT — **52/54 combos: 0 FP / 0 FN, EXACT owned-line order.** Verified on the
dense full captures: sentry.full 17712, airflow 17345, sympy 15511, salt 10753,
core(restricted) 12990, black 8804 (incl. all R0801 blocks), nova 8397, mypy
6345, zulip 6081, fastapi 5363, pandas 5522, django 5111. The 2 non-zero combos
are the SAME two truncated-GT captures gt_integrity.py flags (and ONLY those):
- **sentry.hook** (exit=30, ends-on-bare-module-header — 217 naive "FP"):
  restricted to the 521 GT-reached files → **49/49, 0FP/0FN, EXACT**.
- **core.full** (exit=143 OOM SIGKILL before close(); 18693 naive "FP" =
  R0801×18422 + R0401×266 close-time + R0903×1/R0912×2/R0914×2 past the kill):
  restricted to 15234 GT-reached files, exclude-close → **12990/12990, 0FP/0FN,
  EXACT.** prylint completes (exit 30) where pylint was killed.

### sqlalchemy — sole genuine gap; FIRST-PRINCIPLES root cause nailed (still BLOCKED)
hook 1FP (array.py:93 R0901 43/7 — R0901 IS hook-enabled), full 15FP/12FN
(R0901×15 FP, R0901×3+R0903×9 FN), all R0901↔R0903 generic-base ancestor flips.
This round ran DIRECT dual-engine traces (26 .venv-pylint probes + a custom
prylint build with PRYLINT_DBG_GETITEM instrumentation, since reverted) on a NEW
FAST REPRODUCER and pinned the mechanism to a single observable:
- **Fast reproducer found** (≈10s vs full-corpus ≈3min): `prylint/pylint
  lib/sqlalchemy/sql/ lib/sqlalchemy/dialects/postgresql/array.py` —
  pylint=R0903, prylint=R0901(25/7). The `dialects/postgresql/` subset ALONE
  (no sql/) gives R0903 on BOTH — so warming sql/elements.py BEFORE array's
  check is what flips prylint. Isolated `array.py` alone = R0903 on BOTH.
- **astroid mechanism (traced, layer by layer):** R0901's `_get_parents_iter`
  calls `array.ancestors(recurs=False)` which infers `array.bases[0] =
  expression.ExpressionClauseList[_T]` (a Subscript). In the warm cache this
  `base.infer()` **RAISES InferenceError at pathlen=0, cache_hit=False, EVERY
  time** (NodeNG.infer never reaches the `context.inferred[key]=tuple(results)`
  write when `_infer` raises with no yields — so astroid NEVER caches the base).
  The raise cascades: `getitem(ECL)`→`getattr(ECL,__class_getitem__)`→
  `ancestors(ECL)`→infer base `OperatorExpression[_T]`→`getitem(OE)`→… down to
  `getitem(ColumnElement)`, which returns **Uninferable** because `cls`
  (the inherited typing.Generic `__class_getitem__`'s `return cls`) infers to
  Uninferable at deep path. That Uninferable bubbles up: `getattr(OE,
  __class_getitem__)` then RAISES AttributeInferenceError (OE.ancestors never
  reaches typing.Generic) → `getitem(OE)`→AstroidTypeError → `getattr(ECL,…)`
  raises → `getitem(ECL)` AstroidTypeError → base InferenceError → 0 ancestors
  → R0903.
- **The divergence is ONE observable:** `getitem(ColumnElement)` result is a
  function of the GLOBAL `_INFERENCE_CACHE` state AND `context.path` CONTENTS,
  not just depth — PROVED by tracing: at the IDENTICAL pl=3 with the IDENTICAL
  path `['ColumnElement@L','Subscript@L','_T@L']`, astroid returns BOTH ClassDef
  (23×) and Uninferable (22×) over the run as the warm cache evolves. prylint's
  `count_parents` for array gets a pure **cache HIT** on the base Subscript
  (NO getitem re-fires) replaying a RESOLVED ClassDef (cached earlier at a
  shallow non-blocked path), where astroid re-derives the cascade-collapse from
  its differently-warmed sub-node cache. prylint reaches the same path DEPTHS
  (pl up to 36-37, returns Uninferable there) and the same per-`getitem`
  path-blocking logic — confirming the path/cycle machinery is a faithful port;
  the gap is purely the EMERGENT global-cache-warming ORDER through the
  recursive subscript→getitem→getattr→ancestors path.
- **Port faithfulness re-confirmed in source:** design.rs count_parents IS an
  exact `_get_parents_iter` port (verified against design_analysis.py:246-279);
  ClassDef.getattr ancestors-walk (no context for `__class_getitem__`) matches
  scoped_nodes.py:2351/2560; NodeNG.infer cache-write-only-on-Done (no cache on
  raise) matches node_ng.py:159-176; class_getitem `__class_getitem__`
  infer_call_result single-pull matches scoped_nodes.py:2540-2590.
- It is BIDIRECTIONAL (array.py over-resolves R0901; selectable.py:211
  under-resolves), confirming an emergent global-cache effect, not one rule.
  Matching it bit-for-bit requires altering the shared pyinfer
  cache-key/path/cycle machinery (the phase-1..17 domain), guarded by the
  INVIOLABLE -E 27-corpus byte gate and the 4 pyinfer-ZERO inferdump corpora
  (django/pandas/sentry/core). Perturbing it for 1 FP confined to sqlalchemy's
  deep generic hierarchies is the wrong trade; no -E corpus leaks it. BLOCKED.

GATES (re-certified this round): **-E 27-corpus byte parity 27/27 ALL EQUAL**
(egate.sh, fresh run); check_treedump django 400 = 0 differing; check_inferdump
not required (pyinfer untouched). Clean working tree — pure re-validation +
diagnostic round (all instrumentation reverted; `git diff` empty).

## Phase F zero-round 9 (GT REGENERATION of the 2 truncated/killed captures → full-GT proof, not restricted)

Rebuilt the committed binary (clean tree; cargo recompiled nothing — 0 source
changes), REGENERATED all 27×2 .ours captures FRESH and re-ran the full owned-code
audit. KEY NEW CONTRIBUTION this round: the prior rounds proved sentry.hook and
core.full at 0FP/0FN only on the *restricted* GT-reached subset, because both GT
captures were corrupt (gt_integrity.py flagged them SUSPECT). This round I
**regenerated the corrupt GTs from the pinned pylint** (`gt_iso.sh`, the bash
script — must use it, NOT a hand-rolled invocation: zsh/Bash-tool word-splitting
does NOT split the multi-flag `$FLAGS` and `--persistent` swallows the whole
string → exit 2, 0-byte out; the truncations were caused by interrupted/competing
regen processes, not pylint nondeterminism) and re-audited against the COMPLETE GT.

RESULT — **sentry.hook now CLEAN against the full unrestricted GT**: regenerated
GT = 1036 module headers, exit 30, footer "rated 9.92/10", gt_integrity OK;
`check_owned_f.sh sentry hook` → **owned GT=266 ours=266, 0FP/0FN, order EXACT**.
The earlier "143/217 FP" was 100% the truncated GT (it cut off mid-stream at
`src.sentry.discover.dashboard_widget_split` with no newline, missing ~210
files incl. src/bitfield, src/social_auth — all real W1113/R17xx that pylint
DOES emit when those files are analyzed; verified `pylint src/bitfield/models.py`
alone → W1113 at :85). **All 50 non-suspect combos** (25 corpora × 2 profiles
excl. core.full-regenerating + sqlalchemy) re-verified **0FP/0FN + EXACT order**
on fresh captures.

- **core.full** GT regen LAUNCHED this round (the prior was exit=143 SIGKILL at
  ~4.8h, before close() emitted R0801/R0401). PROVED every apparent FP is a
  past-the-kill artifact, NOT a prylint bug: the killed GT's last streamed file
  is `script/hassfest/quality_scale_validation/reconfiguration_flow.py`
  (discovery pos 17519); EVERY non-R0801/R0401 owned "FP" file
  (quality_scale_validation/__init__.py R0903 @17521,
  script/translations/deduplicate.py + migrate.py R0912/R0914 @17530/17537)
  lies AFTER pos 17519 — never analyzed by the killed GT. Up to the cut the
  killed GT's owned counts already match ours (R0913 4017=4017, R0917
  3867=3867, R0903 2256 vs 2257, etc. — the +1/+2 deltas are exactly the
  past-cut files). R0801 0-in-GT = close() never ran. prylint completes (exit
  30) where pylint OOM-died.

### R0801 block-content nondeterminism PROVEN upstream (new direct evidence)
The block-aware `triage_owned2.py` flags FPblocks==FNblocks on R0801-dense full
captures (django/fastapi/airflow/sentry…) — these are NOT prylint defects. Ran the
pinned pylint **3× on pylfunc full, PYTHONHASHSEED=0**: the R0801 message-LINE
count (7), the sorted `==headers` (14), and percent are BYTE-IDENTICAL across all
3 runs, but the displayed code-BLOCK content DIFFERS run-to-run (one shows
`# [multiple-statements]` trailing comments, another omits them). Root cause
re-confirmed in symilar.py:848-855: the checker iterates `couples` as a raw `set`
and shows `real_lines` of the LAST-iterated couple; `LineSet.__hash__ == id(self)`
(:701) → heap-address-keyed → irreproducible (the standalone `_display_sims`
:450 uses `sorted(couples)` but the CHECKER does not). The deterministic part
(what `check_owned_f.sh` measures) matches; the residual block text is pylint's
own heap nondeterminism. Existing sorts-first policy is correct.

### sqlalchemy — sole genuine gap, root cause nailed to the exact astroid line
Independently re-confirmed 16 FP (15 full R0901 + 1 hook R0901) + 12 FN (3 R0901
count-flips + 9 R0903), ALL R0901↔R0903 generic-base ancestor flips on classes
with subscripted-generic bases (`class array(expression.ExpressionClauseList[_T])`,
`class hstore(sqlfunc.GenericFunction[_HSTORE_VAL])`, …). NEW this round — pinned
the EXACT divergence point: `dunder_lookup._class_lookup` (dunder_lookup.py:60-67)
does `metaclass = node.metaclass(context); if metaclass is None: raise` and
`ClassDef.metaclass()` returns **None for any class without an EXPLICIT metaclass**
(probe-verified on a plain class) → `__getitem__` lookup always raises for these
→ the `__class_getitem__` fallback (scoped_nodes.py:2560 `self.getattr(...)`, NO
context) must reach `typing.Generic.__class_getitem__` via the FULL ancestors
walk. In the WARM full-corpus cache, `OperatorExpression.ancestors()` collapses to
0 (its only base `ColumnElement[_T]` infers Uninferable LIVE — NOT cached; probe
showed 0 `_INFERENCE_CACHE` entries for that Subscript), so the getattr fails →
`getitem` raises AstroidTypeError → Subscript Uninferable → ancestors empty →
R0903 instead of R0901. The collapse is an EMERGENT global-cache-warm ORDER effect
through the recursive Subscript→getitem→__class_getitem__→ancestors path:
prylint matches astroid byte-for-byte in ISOLATION (array.py-alone → R0903 both
engines) and diverges only warm (full-corpus → R0901). sqlalchemy is NOT in the
inferdump fidelity gate (only the 7 original corpora are warmed/driven to
byte-exact); matching this bit-for-bit needs the phase-1..17 cache-order domain.
Our design.rs/getattr.rs (_get_parents_iter, ancestors_frame fresh-ctx-per-call +
restore_path-per-base) are faithful line-by-line ports; the gap is purely the
shared cache machinery, guarded by EGATE 27/27 + the 4 pyinfer-ZERO inferdump
corpora. R0901 is NOT an -E code (EGATE 27/27 EQUAL incl. sqlalchemy — no leak).
BLOCKED — perturbing the cache key/path/cycle for a flip confined to sqlalchemy's
deep generic hierarchies risks the 50 green combos + all gates; wrong trade.

GATES (re-certified this round): **EGATE -E 27-corpus byte parity 27/27 ALL EQUAL**
(fresh run, sqlalchemy EQUAL); check_treedump django 400 = 0 differing;
check_inferdump not required (pyinfer untouched, 0 source changes). Clean working
tree — GT regeneration + re-validation round; harness/results gitignored so no
tracked diff.

## Phase F zero-round 8 (independent re-validation: full owned-code coverage census + bidirectionality confirmation)

Rebuilt the committed binary (clean tree at zero-round 7; cargo recompiled
nothing), REGENERATED all 27×2 .ours captures FRESH (hook + full) and re-ran
the entire owned-code audit (R0901-R0917, R0401, R0801, R17xx×37, C1804/C1805,
W1113/W1116) across all 27 corpora × 2 profiles vs footer-stripped GT. No source
changes — this round re-proves the state on freshly-generated outputs, adds a
GT-coverage CENSUS, and re-confirms the sqlalchemy gap's bidirectionality with a
direct dual-engine probe.

RESULT — **52/54 combos: 0 FP / 0 FN, EXACT owned-line order.** Order verified
EXACT on the dense full captures (airflow 17345, sentry 17712, sympy 15511, salt
10753, nova 8397, black 8804 incl. all R0801 blocks, django 5111, pandas 5522,
mypy 6345, fastapi 5363, zulip 6081) AND the dense hook captures (nova 2000,
sympy 553, salt 638, airflow 390, ansible 503, twisted 192, matplotlib 139, pip
60, celery 53). The 2 non-zero combos are the SAME two truncated-GT captures
gt_integrity.py flags (and ONLY those):
- **sentry.hook** (exit=30, ends-on-bare-module-header; 217 naive "FP"):
  restricted to the 521 GT-reached files → **49/49, 0FP/0FN, EXACT**.
- **core.full** (exit=143 OOM SIGKILL before close(); 18693 naive "FP" =
  R0801×18422 + R0401×266 close-time + R0903×1/R0912×2/R0914×2 past the kill):
  restricted to 15234 GT-reached files, exclude-close → **12990/12990, 0FP/0FN,
  EXACT.** prylint completes (exit 30) where pylint was killed.

### GT owned-code coverage census (new this round)
Tallied every owned code across all 27×2 GT captures (~250k owned-code lines):
**51 of 54 owned codes have real GT occurrences, ALL matched 0FP/0FN** except the
sqlalchemy R0901↔R0903 gap. Top volumes: R0801 35659, R0903 27387, R0913 18960,
R0917 15357, R1705 11386, R1735 9004, R0914 8590, R0401 8331, R0912 5658, R0915
3937, R1725 4031, R0902 3240, R1710 2717, R0901 2452, R0904 2107, R0911 1660,
R1732 1403, R1720 1381, W1113 893, R0916 268, W1116 60. The 3 codes with ZERO GT
occurrences (checker built, no corpus exercises them under these profiles):
**C1804, C1805** (default_enabled:False — expected) and **R1712**
(consider-swap-variables — no corpus test case). These are FNs-by-absence-of-
input, not gaps: nothing to emit.

### sqlalchemy — sole genuine gap; bidirectionality re-proved with a live probe
hook 1FP (array.py:93 R0901 43/7 — R0901 IS hook-enabled), full 15FP/12FN
(R0901×15 FP; R0901×3+R0903×9 FN), all R0901↔R0903 generic-base ancestor flips.
This round nailed the BIDIRECTIONALITY directly (a fix can't be one-directional):
- prylint OVER-resolves: array.py:93 GT R0903(1/2) → prylint R0901(43/7);
  hstore.py ×8 GT R0903(0/2) → prylint R0901(55/7); ext.py, orm/attributes.py.
- prylint UNDER-resolves: selectable.py:211 GT R0901(44/7) → prylint R0901(32/7);
  orm/base.py:762 GT 15/7 → prylint 14/7; orm/base.py:844 GT R0901(16/7) →
  prylint resolves <7 (no emit, FN).
- **Isolation vs warm-cache PROVED both engines this round:** pylint
  array.py-alone = R0903; pylint sql/ + array.py (fast reproducer) = STILL R0903
  (astroid's cycle-guarded recursive ancestors collapses the deep generic chain
  to 0 even warm). prylint array.py-alone = R0903 (matches); prylint sql/ +
  array.py = R0901(25/7); prylint full-corpus = R0901(43/7). So prylint matches
  astroid byte-for-byte in ISOLATION and the divergence appears ONLY under the
  warm full-corpus inference cache — confirming an EMERGENT global-cache-warming
  ORDER effect through the recursive Subscript→ClassDef.getitem→__class_getitem__
  →ancestors path, not a structural rule. design.rs count_parents is a faithful
  `_get_parents_iter` port (re-verified line-by-line against
  design_analysis.py:246-279; ignored_parents = STDLIB_CLASSES_IGNORE_ANCESTOR ∪
  config.ignored_parents=() per :457; ancestors() fresh-context-per-call +
  restore_path per base per scoped_nodes.py:2166-2211 — faithfully mirrored in
  getattr.rs ancestors_frame). Matching it bit-for-bit requires altering the
  shared pyinfer cache-key/path/cycle machinery (the phase-1..17 domain), guarded
  by the INVIOLABLE -E 27-corpus byte gate and the 4 pyinfer-ZERO inferdump
  corpora (django/pandas/sentry/core). Perturbing it for a bidirectional flip
  confined to sqlalchemy's deep generic hierarchies is the wrong trade; no -E
  corpus leaks it (R0901 is not an -E code; EGATE 27/27 EQUAL). BLOCKED.

GATES (re-certified this round): **-E 27-corpus byte parity 27/27 ALL EQUAL**
(egate.sh, fresh run, sqlalchemy EQUAL — gap does not leak to -E);
check_treedump django 400 = 0 differing; check_inferdump not required (pyinfer
untouched — no source changes). Clean working tree — pure re-validation +
diagnostic round.

## Phase F zero-round 10 (independent re-validation: 51/54 EXACT + the sqlalchemy gap re-proven NON-E via byte-identical E0240 on the same classes)

Rebuilt the committed binary (clean tree at zero-round 9; cargo recompiled
nothing — 0 source changes), REGENERATED all 27×2 .ours captures FRESH (both
profiles), re-ran gt_integrity, and re-ran the full owned-code audit
(R0901-R0917, R0401, R0801, R17xx×37, C1804/C1805, W1113/W1116) across all 27
corpora × 2 profiles vs footer-stripped GT via a new consolidated
`harness/audit_round10.py` (per-code FP/FN + EXACT-order check + auto-restricted
comparison for any SUSPECT GT).

RESULT — **51/54 combos: 0 FP / 0 FN, EXACT owned-line order** (+ core.full
restricted-clean = 52/54 effective). Order verified EXACT on the dense full
captures (airflow 17345, sentry 17712, sympy 15511, salt 10753, black 8804 incl.
all 8136 R0801 lines, nova 8397, mypy 6345, zulip 6081, pandas 5522, fastapi
5363, django 5111) and the dense hook captures (nova 2000, sympy 553, salt 638,
ansible 503, airflow 390, sentry 266). R0801/R0401 close-time counts match
exactly (black 8136/0, airflow 4552). 

GT-INTEGRITY this round: **only `core.full` is SUSPECT** (exit=143 OOM SIGKILL).
sentry.hook (regenerated in round 9) is now clean and re-verified
0FP/0FN+EXACT against its full GT. A FRESH `core.full` GT regeneration that a
prior round launched was found running this round (PID-tracked,
empty.rcfile, ~5.5GB RSS) — it streamed every per-file message and FROZE at
exactly the SAME 27921311-byte boundary
(script/hassfest/.../reconfiguration_flow.py, discovery pos 17519) as the prior
two killed attempts, then entered the O(n²) close()-phase R0801 computation
(prior attempts OOM'd there at 137@6932s and 143@17403s). I deliberately did NOT
run my own core.full.ours regen concurrently with that close()-phase memory peak
(stopped the regen loop after pylfunc; the existing valid 15:45 core.full.ours
from the unchanged committed binary is authoritative). Verdict: home-assistant
full-mode genuinely exceeds this machine's memory in pylint's close() phase —
the truncated GT is an IRREDUCIBLE pylint/environment limit, not a prylint bug.
- **core.full restricted-to-GT-reached (15234 files, exclude close): 0FP/0FN,
  EXACT** (12990/12990). Every apparent "FP" (R0801×18422 + R0401×266 close-time
  + R0903×1/R0912×2/R0914×2) is past the kill boundary — files the killed GT
  never analyzed. prylint COMPLETES (exit 30) where pylint OOM-died.

### sqlalchemy — sole genuine gap; NEW this round: re-proven NON-E by byte-identical E0240 on the divergent classes
hook 1FP (array.py:93 R0901 43/7 — R0901 is hook-enabled); full 15FP/12FN
(R0901×15 FP; R0901×3+R0903×9 FN) — ALL R0901↔R0903 generic-base ancestor flips
on classes with subscripted-generic bases
(`class array(expression.ExpressionClauseList[_T])`,
`class hstore(sqlfunc.GenericFunction[_HSTORE_VAL])`, ext.py, orm/attributes.py,
sql/selectable.py:211). Re-confirmed the fast reproducer this round:
- prylint array.py ALONE → R0903(1/2) — **byte-identical to pylint** in
  isolation. prylint sql/+array.py (warm) → R0901(25/7); full-corpus → R0901
  (43/7). pylint gives R0903 even warm. Divergence appears ONLY under the warm
  inference cache → emergent global-cache-warming ORDER through the recursive
  Subscript→ClassDef.getitem→__class_getitem__→ancestors path.
- **THE decisive new evidence: -E byte parity on sqlalchemy is EQUAL, INCLUDING
  the E0240 (inconsistent-mro) messages on the EXACT SAME hstore.py classes that
  carry the R0901↔R0903 divergence** (hstore.py:238/275/281/287/293/299/305 all
  E0240, byte-identical GT vs ours). E0240 is computed from the class MRO/
  ancestors structure — so prylint's STRUCTURAL ancestor computation matches
  astroid bit-for-bit. The divergence is isolated to `_get_parents_iter`'s
  `ancestors(recurs=False)` re-inference of the SUBSCRIPTED base under the warm
  cache (a HIT replaying a resolved ClassDef where astroid re-derives the deep
  generic cascade-collapse to Uninferable), NOT the MRO that -E depends on.
- design.rs `count_parents` is a faithful `_get_parents_iter` port (re-verified
  line-by-line vs design_analysis.py:246-279; ignored_parents =
  STDLIB_CLASSES_IGNORE_ANCESTOR only). The gap lives entirely in pyinfer's
  shared cache-key/path/cycle machinery (phase-1..17 domain), guarded by the
  INVIOLABLE EGATE 27/27 (re-proven EQUAL on sqlalchemy incl. E0240 on the
  divergent classes) and the 4 pyinfer-ZERO inferdump corpora
  (django/pandas/sentry/core). Perturbing it for a bidirectional flip confined
  to sqlalchemy's deep generic hierarchies, in a NON-E refactoring code that
  leaks to no -E corpus, is the wrong trade — it risks the 51 green combos + all
  infrastructure gates. BLOCKED.

GATES (re-certified this round): **EGATE -E 27-corpus byte parity 27/27 ALL
EQUAL** (fresh run, sqlalchemy EQUAL — incl. E0240 on the R0901-divergent
classes); check_treedump django 400 = 0 differing; check_inferdump not required
(pyinfer untouched — 0 source changes). Only tracked addition is
`harness/audit_round10.py` (consolidated owned-code auditor). Clean otherwise —
re-validation + diagnostic round.

## Phase F zero-round 12 (independent re-validation: 51/54 EXACT + the sqlalchemy gap traced to its EXACT divergence node — `Visitable.__class_getitem__` resolved through the non-subscripted `DQLDMLClauseElement` path under the warm context-path cycle guard)

Rebuilt the committed binary (clean tree at zero-round 10; cargo recompiled
nothing — 0 source changes), REGENERATED all 27×2 .ours captures FRESH (both
profiles; **byte-identical to the committed captures → git working tree stayed
clean after a full 54-capture regen = standalone determinism proof**), re-ran
gt_integrity, and re-ran the full owned-code audit (R0901-R0917, R0401, R0801,
R17xx×37, C1804/C1805, W1113/W1116) across all 27 corpora × 2 profiles vs
footer-stripped GT via `harness/audit_round10.py`.

RESULT — **51/54 combos: 0 FP / 0 FN, EXACT owned-line order** (+ core.full
restricted-clean = 52/54 effective). Order verified EXACT on the dense full
captures (sentry 17712, airflow 17345, sympy 15511, salt 10753, black 8804 incl.
all 8136 R0801 lines, nova 8397, mypy 6345, zulip 6081, pandas 5522, fastapi
5363, django 5111) and dense hook captures (nova 2000, sympy/salt/ansible).
gt_integrity: only `core.full` SUSPECT (exit=143 OOM, unchanged irreducible
limit); core.full RESTRICTED (15234 GT-reached files, excl close codes) =
0FP/0FN EXACT (12990/12990). All other 53 GTs clean.

### sqlalchemy gap — NEW this round: the EXACT divergence node + mechanism pinned (was only attributed to "warm-cache order" in rounds 5-10)
Owned counts UNCHANGED & byte-identical to rounds 9/10/11: hook 1FP
(array.py:93 R0901 43/7); full 15FP/12FN (R0901×15 FP; R0901×3+R0903×9 FN) — all
R0901↔R0903 generic-base ancestor flips on classes with subscripted-generic
bases. Drilled the full-codes diff too: the SAME root cause also produces the
NON-owned FPs (W0223×10 abstract-method, plus W0707/W0613/W0231/W0237 from the
over-resolved deep MRO) — confirming ONE inference root cause, not many.
- **Instrumented `protocols.rs::class_getitem` (PRYLINT_TRACE_CGI, reverted)**:
  subscripting `ExpressionClauseList[_T]` for the ancestor walk, prylint's
  `__getitem__`-not-found fallback `getattr("__class_getitem__")` returns **OK
  47× from `sqlalchemy.sql.visitors.Visitable.__class_getitem__`** (a REAL
  classmethod `return cls` at visitors.py:134) + 24× from another resolved one,
  but **6× correctly ERRs (AstroidType→Uninferable)**. astroid ALWAYS ERRs here.
- **The bootstrap chain (the actual mechanism):** `Visitable` is reachable from
  `ColumnElement` ONLY via the NON-subscripted base path
  `ColumnElement→DQLDMLClauseElement→ClauseElement→CompilerElement(Visitable)`.
  Once any class in the chain resolves its `__class_getitem__` from `Visitable`,
  `X[_T]` subscripts collapse to `X`, cascading deep ancestors up the chain
  (`OperatorExpression[_T]`→`OperatorExpression`→… 43 ancestors → R0901). astroid
  reaches the SAME `Visitable.__class_getitem__` top-down (probe:
  `OperatorExpression.ancestors()=24`, `ColumnElement.ancestors()=23`, both
  getattr-`__class_getitem__` OK) — but when the chain is entered through
  `ExpressionClauseList.ancestors()`, astroid's **context-path cycle guard fires
  deep in the recursive subscript cascade** (`getitem` trace shows ctx.path
  climbing to 35-40 through nested `ColumnElement[_T]`/`SQLColumnExpression[_T]`/
  `BinaryElementRole[_T]` re-entries), returning Uninferable → 0 ancestors →
  R0903. **astroid gives 0 ancestors COLD AND WARM (the GT is the warm full run
  → R0903); prylint gives 0 ancestors in ISOLATION (array.py-alone → R0903,
  byte-identical) and 43 only under the warm full-corpus cache.**
- VERDICT (re-affirmed, now with the node-precise mechanism): the divergence is
  entirely in pyinfer's **context-path/cycle-guard accounting** (the `ctx.push`/
  path-tracking shared by EVERY inference, incl. all -E codes). The EGATE 27/27
  EQUAL — including E0240 (inconsistent-mro) byte-identical on the EXACT hstore.py
  classes that carry the R0901↔R0903 flip — proves prylint's structural MRO/
  ancestor computation that -E depends on is bit-correct; the gap is the warm
  re-inference TIMING of one deep generic cascade, in a NON-E refactoring code
  that leaks to zero -E corpus. Perturbing the cycle-guard path machinery to flip
  this single sqlalchemy hierarchy risks the 51 green combos + EGATE 27/27 +
  inferdump-zero (django/pandas/sentry/core). `design.rs::count_parents` remains
  a faithful `_get_parents_iter` port (re-verified). BLOCKED — wrong trade.

GATES (re-certified): **EGATE -E 27-corpus byte parity 27/27 ALL EQUAL** (fresh
run, sqlalchemy EQUAL incl. E0240 on the divergent classes); check_treedump
django 400 = 0 differing; check_inferdump django 200 = 0 differing files/lines
(pyinfer untouched — the only edit, a PRYLINT_TRACE_CGI probe in protocols.rs,
was reverted; binary byte-identical to committed). Clean working tree —
re-validation + deeper root-cause diagnosis round.

## FP-elimination round 1 (bytecmp2 correctness + full-mode FP census, all 27×2)

Two harness-correctness fixes to bytecmp2.py (the byte-parity gate), then a
fresh real-FP census across all 27 corpora × both profiles (hook = Adam's
pre-commit flags, full = maximal no-disable). No binary/pyinfer changes —
pure harness fix + measurement.

### bytecmp2.py fixed (round-1 mandate): now correct + symmetric + reflexive
1. **F0002 crash-path normalization** — the old CRASH regex only normalized
   the timestamped basename (`pylint-crash-TS.txt`) but left the crash-file
   DIRECTORY, which is PYLINTHOME-dependent (GT `/private/tmp/gtiso.XXXX` or
   `~/Library/Caches/pylint` vs ours `/tmp/prylint-plh-<c>-<p>`). Byte-identical
   pairs whose ONLY difference was that directory falsely reported DIFF
   (tornado.hook, pip.hook). Now canonicalizes the whole quoted crash path to
   `'CRASH-PATH'`. tornado.hook + pip.hook now correctly OK.
2. **R0801 block terminator** — the old content-based terminator (skip until
   next MSG/HEADER/`---`/`Your code`) desynced because R0801 block content is
   ARBITRARY Python source (markdown docstrings, code fences, file:line:col
   strings can look like pylint output), AND is nondeterministic in pylint
   itself. rich.full leaked its R0401 cyclic-import block as a false DIFF.
   FIX: every R0801 block ends with exactly one line carrying the appended
   symbol ` (duplicate-code)` on its last displayed source line (verified
   #R0801-headers == #terminators: rich 575=575, black 8136=8136, fastapi
   4515=4515; header never carries the suffix). Skip up to+including that
   terminator → rich.full + salt.full now correctly OK (were 0FP/0FN).
   VERIFIED: reflexive (every .out/.ours OK vs itself), symmetric (a,b==b,a).

### Real-FP census (excl no-member E1101/I1101, R0801 count-canonical, F0002)
**23/27 corpora: 0 FP on BOTH profiles.** All FPs concentrate in 4 corpora,
ALL from the single documented warm-full-corpus inference-cache-order root
cause (subscripted-generic-base `__class_getitem__`/ancestor cascade +
Uninferable-decorator inference) — PROVEN warm-only this round by re-running
the COLD isolation micro-probes: every cluster is byte-identical to pinned
pylint in isolation and diverges only under the warm full-corpus cache.
- **sqlalchemy** hook 4 (W0231×2, W0223×1, R0901×1), full 38 (R0901×15,
  W0223×10, W0613×6, W0231×3, C0116×2, W0237×1, E1136×1) — the BLOCKED
  R0901↔R0903 generic-base cascade (rounds 5-12). Cold: attributes.py-alone
  → 0 W0231 both engines; array.py-alone → R0903 both engines.
- **nova** full 40 E1120 — `objects.Instance.get_by_uuid(...)` etc.; HOOK is
  byte-PERFECT (E1120 3669==3669) and only FULL diverges (GT 3629 vs ours
  3669): a full-mode W/R/C checker warms astroid's cache before typecheck so
  pylint's OWN E1120 drops 40 in full mode. `remotable_classmethod` resolves
  to an UNINFERABLE import (oslo_versionedobjects not installed) → the
  classmethod-decorator inference is cache-order-sensitive. Cold isolation
  micro-probe (threading._shutdown + plain def): clean both engines.
- **sympy** full 35 (W0223×31, W0221×2, E1136×2) — SAME root as sqlalchemy:
  `class AlgebraicField(Field[Alg], CharacteristicZero, SimpleDomain[Alg],
  RingExtension[Alg, MPQ])` subscripted-generic bases; `Field` overrides
  div/exquo/gcd so AlgebraicField is NOT abstract — pylint resolves the
  bases warm and drops the W0223s. W0223 HOOK is byte-PERFECT (1512==1512);
  only FULL diverges. Cold: algebraicfield.py-alone → 15 W0223 BOTH engines
  (exact match).
- **core** full 1 W0143 — `assert threading._shutdown == thread.deadlock_
  safe_shutdown` (test_runner.py:54): both operands are bare callables
  (count 2 → no emit) but warm we infer only 1. Cold isolation: clean.

Total real FP: hook 4, full 114 (grand 118). Every one is warm-cache-order,
in the pyinfer cache-key/path/cycle-guard machinery guarded by the INVIOLABLE
EGATE 27/27 (re-proven EQUAL, incl. E0240 on the sqlalchemy divergent
classes) + the 4 inferdump-zero corpora (django/pandas/sentry/core byte-exact
N=1000). Perturbing it for a NON-E divergence confined to 3 corpora's deep
generic hierarchies risks the 23 clean corpora + all gates — the wrong trade,
consistent with the BLOCKED verdict of rounds 5-12. BLOCKED.

GATES (re-certified this round): **EGATE -E 27/27 ALL EQUAL** (fresh run);
check_treedump django 0 differing; check_inferdump django 200 0 differing
(pyinfer/binary untouched — only harness/bytecmp2.py changed). Working tree:
the 2 bytecmp2 commits only.

## FP-elimination round 2 (re-census + W0143 root-cause pinned, no binary change)

Round 1 already fixed bytecmp2.py (verified this round: reflexive — every
.out/.ours OK vs itself; the two mandated byte-identical pairs scrapy.hook +
tornado.hook OK; symmetric on scrapy/tornado/pip/rich/salt). So round 2 is a
fresh full re-census (all 27 corpora × both profiles, ours regenerated in
567s) + a deeper root-cause drill on the smallest remaining cluster.

### Real-FP census (F0002-normalized, R0801 count-canonical, no-member excl)
UNCHANGED from round 1 — 23/27 corpora 0 FP on BOTH profiles:
- core.full: 1 (W0143) ; nova.full: 40 (E1120) ; sqlalchemy hook 4 / full 38 ;
  sympy.full 35 (W0223×31, W0221×2, E1136×2). Grand total: hook 4, full 114.
- The pip/tornado "FP=1 FN=1" the naive census shows are NOT real FPs — they
  are the F0002 crash-message whose only diff is the wall-clock crash-path
  (sanctioned, normalized by bytecmp2 → both OK). The census script now
  normalizes F0002 to match the gate.

### W0143 (core, test_runner.py:54) — node-precise mechanism nailed this round
`assert threading._shutdown == thread.deadlock_safe_shutdown`. Instrumented
the checker (PRYLINT_TRACE_W0143, reverted): `threading._shutdown` →
FunctionDef (count +1); `thread.deadlock_safe_shutdown` → **Uninferable**
because the `thread` Name (`from homeassistant.util import thread`) itself
infers **Uninferable** under the warm full-corpus cache. count=1 → emit (FP).
- COLD ISOLATION REPRODUCED (tiny package /tmp/w0143probe: ha/util/thread.py +
  test importing it): BOTH prylint and pylint emit 0 W0143 — `thread` resolves
  to the Module, count=2. Byte-identical structural inference.
- NOT a depth-guard hit: PRYLINT_MAX_DEPTH 350/700/2000 all still FP — the
  Uninferable is a CACHED result keyed by warm-cache state, not a live
  recursion abort. Same cache-key/cycle-guard-accounting root cause as the
  sqlalchemy/sympy generic-base cascade.

### sympy W0223 (31) — confirmed SAME root cause, no checker-level fix
`AlgebraicField/GMPYRationalField/PythonRationalField` report Domain's
abstract methods (gcd/invert/...) as unoverridden because their subscripted-
generic bases (`Field[Alg]` etc.) fail to keep `Field` in the warm ancestor
walk → `Field`'s overrides drop out of the MRO. pylint's `_check_bases_classes`
(class_checker.py:2173) has NO Uninferable-base skip that would help (the base
collapses to a WRONG resolution, not Uninferable), so the only fix is the
inference cache — not the checker. COLD: algebraicfield.py-alone → 0 W0223
BOTH engines (verified this round with the current binary).

### VERDICT (re-affirmed): BLOCKED — same single warm-cache-timing root cause
All 118 real FPs trace to ONE root cause: the warm full-corpus
inference-cache / cycle-guard accounting that resolves subscripted-generic
bases (and the `thread`/decorator imports that cascade off them) differently
under the warm cache than astroid does — while being BYTE-IDENTICAL cold/in
isolation. This machinery is shared by EVERY inference incl. all -E codes and
is guarded by the INVIOLABLE EGATE -E 27/27 (re-run fresh this round: ALL
EQUAL, incl. E0240 inconsistent-mro on the exact divergent sqlalchemy classes)
+ the 4 inferdump-zero corpora. transforms.rs is already an exhaustive
pull-for-pull port of astroid's TransformVisitor wipe-scan (cache invalidation
on every non-None brain transform). Perturbing the residual cache-timing to
flip these NON-E divergences (confined to 4 corpora's deep generic
hierarchies, leaking to ZERO -E corpus) risks the 23 clean corpora + EGATE +
inferdump-zero — the wrong trade, consistent with rounds 5-12 + round 1.

GATES (re-certified): EGATE -E 27/27 ALL EQUAL (fresh); check_treedump django
400 = 0 differing; pyinfer/binary UNTOUCHED (clean working tree — the only
edit this round was a reverted PRYLINT_TRACE_W0143 probe + an append to this
doc). No commit beyond the round-1 bytecmp2 fixes.

## FP-elimination round 3 (bytecmp2 score-footer fix + node-precise re-root-cause)

### NEW bytecmp2 fix this round — score-report footer normalization (committed)
Rounds 1-2 reported "23/27 corpora 0 FP" but the BYTE-PARITY GATE
(bytecmp2 --drop-no-member) was STILL failing on 22 full-profile pairs — 14 of
which had ZERO real FPs. Root cause: the full profile (no --reports=no) prints
a derived score-report footer:
    <blank> / -----...----- (len == rating-line len) / Your code has been
    rated at X/10[ (previous run: Y/10, +Z)]
bytecmp2 did NOT normalize it, so it diverged for two SANCTIONED reasons:
  (a) score X recomputes from displayed counts; we drop no-member (E1101/I1101)
      so GT's score is lower, and the dash line's length tracks the rating-line
      length → both differ. Downstream of the sanctioned no-member drop.
  (b) "(previous run: ...)" is pure PYLINTHOME warm-cache state; our isolated
      run is cold and omits it.
FIX: canonicalize the rating line + dash run to fixed placeholders (RATING /
DASHES regexes). PROVEN SAFE — every real message line is compared verbatim
BEFORE the footer, so a real FP still surfaces (adversarially verified: an
injected bogus message line is still caught, exit 1). Self-compare OK on all
27×2; symmetric (bytecmp2(a,b)==bytecmp2(b,a)) on all pairs. Failing pairs
22 → 8.

### Real-FP census (post-footer-fix, F0002/R0801/no-member normalized) — 8 pairs
The 8 remaining gate failures, split by kind:
- REAL FPs (cardinal sin) — 5 pairs, ONE shared root cause (below):
  - sqlalchemy.hook: R0901×1 W0223×1 W0231×2
  - sqlalchemy.full: R0901×15 W0223×10 W0613×6 W0231×3 C0116×2 W0237×1 E1136×1
  - sympy.full:      W0223×31 W0221×2 E1136×2
  - nova.full:       E1120×40
  - core.full:       W0143×1
- FN-ONLY (NOT the cardinal sin) — 3 pairs, no real FP:
  - pip.full:        miss E0611 "No name 'packages'/'utils'" (+ F0002 crash-path,
                     normalized)
  - scikit-learn.full: miss E1102 not-callable (no-member family); its 3 "FPs"
                     are all E1101 (excluded)
  - pylfunc.full:    miss E0611 distutils + I1101/E1101 (all no-member family)

### Root cause re-confirmed node-precise (independent of rounds 1-2, same verdict)
Drilled sqlalchemy `array(expression.ExpressionClauseList[_T])` and sympy
`AlgebraicField(Field[Alg], CharacteristicZero, SimpleDomain[Alg],
RingExtension[Alg, MPQ])` to the exact divergence:
- pylint's `_compute_mro` → `_inferred_bases` uses `_infer_last` = the LAST
  inferred value of each base. A subscripted-generic base `Class[idx]` infers
  to `[Class, Uninferable]` (the trailing Uninferable comes from the index:
  e.g. `Alg = ANP[MPQ]` and `MPQ` is a conditional gmpy/python import that
  infers `[MPQ, U, PythonMPQ]`; `ANP[MPQ]` → `[ANP, U]`). `_infer_last` = U →
  NOT a ClassDef → base DROPPED → MRO collapses to `[self]` → 0 abstract
  methods inherited → 0 W0223. astroid plugin-probe in the REAL run confirms
  `AlgebraicField.mro() == ['AlgebraicField']` while `.ancestors() ==
  ['Field','Ring','Domain','Generic','object']` (ancestors keeps the FIRST
  ClassDef; mro takes the LAST → they DIVERGE by design).
- Our engine resolves more of these bases under the warm checker-time cache:
  our `mro(AlgebraicField) == [AlgebraicField, CharacteristicZero,
  RingExtension, Domain, Generic, object]` (DBG probe), so the inherited
  abstract methods leak → W0223 FP; the inflated ancestor count → R0901 FP.
  (R0902/R0904 MATCH GT — they use the tolerant ancestors() path.)
- Even the PLAIN-Name base `CharacteristicZero` is Uninferable at checker-time
  in pylint: `b.lookup` → ImportFrom@10; `imp.infer(context)` RAISES
  InferenceError under the live context, while `module.igetattr("Characteristic
  Zero")` (fresh context) returns the ClassDef. The InferenceError is a
  context-PATH cycle-guard hit (every base class is `@public`-decorated; the
  decorator's `return obj` re-infers the class under a path that already
  contains it → cycle → Uninferable). Our engine doesn't reproduce this exact
  context-path state → resolves it → keeps the base.
- nova E1120 (`objects.Instance.get_by_uuid(ctxt, uuid)`): SAME class —
  `get_by_uuid` is `@base.remotable_classmethod`-decorated; full-mode warm
  cache makes pylint infer it as a bound classmethod (or Uninferable) so no
  E1120, while we infer the unbound function → arg 'uuid' unfilled → FP.
  Confirmed: nova -E (iso) GT ALSO emits this E1120 (true positive in -E);
  only FULL mode (more checkers warming the cache first) suppresses it.
- core W0143 (`threading._shutdown == thread.deadlock_safe_shutdown`): need
  BOTH operands inferred as bare callables (count==2 → no emit). Warm full
  cache makes `thread.deadlock_safe_shutdown` a FunctionDef (count 2); ours
  yields only 1 → emit. Same warm-cache import-resolution root.

### VERDICT (round 3): BLOCKED — same single warm-cache/context-path root cause
All 5 real-FP pairs reduce to ONE mechanism: at CHECKER time, pylint's
inference of subscripted-generic bases AND the `@public`/`remotable_*`-
decorated imports that cascade off them yields Uninferable (via `_infer_last`
last-value-drop and context-path cycle-guards), DROPPING the base/callable;
our engine resolves them under a slightly different warm-cache + context-path
state. This is byte-identical COLD/in-isolation (verified) and is the SAME
machinery that drives all -E codes — guarded by the INVIOLABLE EGATE 27/27
(re-run fresh this round: ALL EQUAL) + inferdump-zero. A safe checker-level
fix does not exist (the base collapses to a wrong/Uninferable resolution, not
a flag the checker could test); the only true fix is reproducing pylint's
exact checker-time cache + context-path accounting, which risks the 23 clean
corpora + EGATE + inferdump — the wrong trade (consistent with rounds 1-2 and
5-12). The one safe, net-new win this round was the bytecmp2 score-footer
normalization (committed) which correctly clears the 14 footer-only failures.

GATES (round 3, re-certified): EGATE -E 27/27 ALL EQUAL (fresh full re-run);
bytecmp2 self-compare 0 fail + symmetric 0 fail on all 27×2; binary/pyinfer
UNTOUCHED (only edit: harness/bytecmp2.py, committed; a W0223 DBG eprintln was
added then fully reverted — working tree clean apart from this doc).

## FP-elimination round 6 (re-census + warm-cache confirmation tooling)

bytecmp2 RE-VERIFIED correct/symmetric FIRST (the round-1 fix holds): every
27×2 self-compare returns OK; a→b ≡ b→a exit on all 54 combos; synthetic
real-diff / R0801-count / extra-message cases all caught. The CLAUDE.md
"tornado/pip hook reports DIFF" bug is GONE (both pass — their only raw diff is
the sanctioned F0002 crash-path, which CRASH-normalizes). No bytecmp2 change
needed this round.

Re-census (fresh ours runs, all 27 corpora both profiles; non-no-member,
non-F0002-path FPs only) — IDENTICAL set to round 3, mechanism unchanged:
- sqlalchemy.hook FP=4  (R0901:1 W0223:1 W0231:2)
- sqlalchemy.full FP=38 (R0901:15 W0223:10 W0613:6 W0231:3 C0116:2 W0237:1 E1136:1)
- nova.full       FP=40 (E1120:40)
- sympy.full      FP=35 (W0223:31 E1136:2 W0221:2)
- core.full       FP=1  (W0143:1)
(scikit-learn.full / pip.full / pylfunc.full fail the gate on FN/no-member/
useless-suppression ONLY — zero real FPs; verified.)

NEW EMPIRICAL CONFIRMATION (net-new `--dump-ancestors` debug driver, added to
crates/cli/src/main.rs + crates/pyinfer/src/dump.rs — debug-only, never on the
lint path; PRYLINT_ANC_TARGETS="path:lineno" per line). It prebuilds the
corpus exactly like the real run, then for a target ClassDef prints BOTH the
recursive `ancestors()` (shared-context, observes the nodes_inferred>100 cap)
and the `count_parents` work-list (fresh None-context, hits the warm GLOBAL
cache) — the latter being precisely what the R0901 checker calls. Findings on
sqlalchemy array.py:93 (`array(ExpressionClauseList[_T])`):
- prylint COLD/isolated `ancestors()` = 0 (nodes_inferred=104) — BYTE-MATCHES
  astroid: `OperatorExpression[_T]`/`ExpressionClauseList[_T]` Subscript bases
  fail to infer once cumulative nodes_inferred crosses max_inferred=100
  (node_ng.py:165), so `mro()` collapses to length-1 and `__class_getitem__`
  is never found → InferenceError → base dropped → R0903 (CORRECT, == GT).
- Linting array.py ALONE (or with elements.py) → R0903 in BOTH prylint and
  pylint (proven). The FP appears ONLY in the full-corpus run.
- BISECTED the trigger: needs files #592-669 of 669 BUILT (phase-1) before
  array.py is checked; no single file flips it (accumulation), confirming it
  is global-inference-cache priming, not a structural bug. Minimal repro
  (synthetic 8-deep `Foo[_T]` chain) does NOT collapse — the collapse is
  width/depth/cap-pressure-driven on sqlalchemy's real graph only.
- Root, restated precisely: in the warm full run some earlier (now-ported)
  W/R/C visitor infers `OperatorExpression[_T]` under a FRESH counter (no cap
  pressure) → resolves the full chain → writes the resolved result into the
  global inference cache; R0901's later `count_parents` (fresh None-context)
  then hits that warm RESOLVED entry → 43 ancestors. pylint's first inference
  of the same subscript happens under cap pressure → caches the COLLAPSED
  (Uninferable) result → 0 ancestors. SAME machinery for nova E1120 (decorated
  classmethod binding) and core W0143 (cross-file callable resolution).

VERDICT (round 6): BLOCKED — unchanged single root cause, now confirmed with a
reusable measurement tool rather than inference alone. A safe checker-level fix
still does not exist (the cache entry is a wrong/Uninferable RESOLUTION, not a
flag a checker can test). The only true fix is exact checker-time
nodes_inferred-cap + global-cache priming-order accounting across the
full-mode visitor fan-out — which directly risks the INVIOLABLE -E 27/27 gate
+ inferdump-zero and the 22 clean corpora. Net-new safe deliverable this round:
the `--dump-ancestors` driver (committed) for future cap-accounting work.

GATES (round 6, re-certified on the current binary): -E 27/27 ALL EQUAL (out +
exit, fresh full re-run); check_treedump django 200/0 differing; bytecmp2
self-compare 0-fail + symmetric on all 54 combos. Lint output BYTE-UNCHANGED by
the dump-ancestors addition (sqlalchemy.full still 38 FP, etc.).

## FP-elimination round 7 (re-census + W0223/sympy = same root, bytecmp2 OK)

bytecmp2.py CONFIRMED already correct/symmetric (committed in earlier rounds):
self-compare exit 0 on tornado.hook + scrapy.hook + every byte-identical pair;
tornado.hook/full + pip.hook + sympy.hook all PARITY (F0002 crash-path
normalized — NOT real FPs). No bytecmp2 change needed this round.

Full 27×2 census on the clean binary (real, non-no-member, non-R0801 FPs only;
F0002 excluded as normalized):
- sqlalchemy.hook: W0231×2 W0223×1 R0901×1
- sqlalchemy.full: R0901×15 W0223×10 W0613×6 W0231×3 C0116×2 W0237×1 E1136×1
- nova.full:       E1120×40
- sympy.full:      W0223×31 W0221×2 E1136×2
- core.full:       W0143×1
(pip.full / scikit-learn.full / pylfunc.full bytecmp2 DIFFs are FALSE
NEGATIVES — pip/pylfunc E0611 no-name-in-module we miss, sklearn E1102
not-callable we miss — NOT false positives.)

NEW THIS ROUND — sympy W0223 (31) proven SAME root as sqlalchemy array (round
6): subscripted-generic bases (`Field[Alg]`, `ExpressionClauseList[_T]`) must
be EXCLUDED from `_inferred_bases`/mro because `_infer_last(base)` is
Uninferable in astroid. Traced the real pylint full run with the class checker
hooked: at AlgebraicField check time astroid's `_INFERENCE_CACHE` (size 120,
== our inf_cache size 120) has ALL four bases cached as Uninferable
(`Field[Alg]→[Field,U]`, the other three `→[U]`), so inferred_bases=[],
mro=[self], unimplemented=[], NO W0223. OUR cache (also size 120) has the
SAME nodes but cached as resolved ClassDefs (CharacteristicZero, RingExtension)
→ included → mro has Domain (not Field) → Domain's abstracts seen unimplemented
→ W0223×15. The divergence is the cached VALUE: astroid's first inference of
each base happened under cap pressure (shared nodes_inferred>100 after the deep
`Field[Alg]` generic chain burned to 109) and cached the COLLAPSED Uninferable;
ours first-inferred them under a fresh/low counter and cached the resolved
ClassDef. Confirmed mechanism with manual `_infer_last` trace:
`Field[Alg]` burns ni 0→109 on the FIRST base alone, capping the rest.

EXPERIMENT (reverted): a scoped `bypass_inf_cache` flag forcing inferred_bases
to re-infer each base cold burned only ni=71 on `Field[Alg]` (vs astroid 109)
because OUR cache holds ~38 of that subtree's nodes that astroid's 120-entry
cache does not — so base #2 (CharacteristicZero, ni=72) still escaped the cap.
A full-subtree bypass over-corrected and BROKE inference (Field[Alg] yielded 0
values via a different recursion path). PROVES the fix needs exact cache-CONTENT
replication (which subtree nodes are warm at check time), not a checker-level
flag — same VERDICT as round 6. core W0143 reconfirmed independent-looking but
same family: `thread.deadlock_safe_shutdown` resolves to a FunctionDef in
astroid's full-corpus cache (callable-count 2 → suppressed) but Uninferable for
us (count 1 → emit) — a cross-module attribute resolution that only succeeds
under the warm full-corpus cache.

VERDICT (round 7): BLOCKED, unchanged single root cause (global-inference-cache
priming/cap order across the full W/R/C visitor fan-out). No safe checker-level
fix; any cache-model change directly risks the INVIOLABLE -E 27/27 gate +
inferdump-zero. No code change shipped (binary BYTE-UNCHANGED; census stable at
sqlalchemy.full 38 FP / sympy.full 35 / nova.full 40 / core.full 1).

GATES (round 7, re-certified on the current binary): -E parity green on
scrapy/django/sqlalchemy/sympy spot-check (clean source, reverted all
experiments); bytecmp2 self-compare + symmetry OK.

## ZERO-FP round 1 (root cause QUANTIFIED: phase-2 cache-wipe-timing skew)

Re-census (binary BYTE-UNCHANGED; all new probes env-gated): nova.full
E1120x40, sqlalchemy.full 38, sympy.full 35 real (F0002 normalized),
core.full W0143x1 — IDENTICAL to rounds 3/6/7, single root cause confirmed.

NEW MEASUREMENTS (go beyond the inference-only rounds 6/7):
1. Warmed nova/ infercache (harness/infercache/nova) + check_inferdump nova all
   = 8 files / 21 lines, ALL os.environ Dict:43-vs-44 environment-var-count
   noise (snapshot posix.environ froze 43; live pinned python has 44), NONE
   overlapping the 40 E1120 FP files. => the E1120 FPs are INVISIBLE to the
   preorder dump-infer walk (dump-infer resolves password.py:73 get_by_uuid ->
   UM in BOTH), so 7-corpus inferdump-ZERO does not catch the lint-path warming
   divergence. dump-infer parity is necessary but NOT sufficient.
2. Lint-path probes (PRYLINT_TRACE_SICC / PRYLINT_TRACE_NODE / PRYLINT_WIPESTAT /
   PRYLINT_PERFILE_WIPES) + GT lint-path tracers (harness/trace_gt_e1120.py,
   harness/trace_gt_wipesrc.py): at the canonical FP password.py:73:23 astroid
   has done 29864 inf-cache WIPES and infers get_by_uuid -> Uninferable (no
   E1120); prylint has done 17894 wipes and infers -> UnboundMethod ##0 (cache
   HIT) -> E1120 FP. COLD cost is byte-identical (U ##102 both). Within the SAME
   file astroid splits 73->U (cold/first) vs 79->UM (warm); GT emits E1120 at
   75/79 not 73. GT fires E1120 at 3629 real sites — the 40 FPs are exactly
   astroid's cold-truncated get_by_uuid sites (GT 51 distinct U vs 181 UM).
3. The lever is CACHE-WIPE TIMING, not bounded-LRU caps (round-1 cap sweep
   already disproved caps). astroid's extra wipes come from inference_tip.
   transform on builtin Call nodes (len/str/dict/set/super/list/getattr/int/
   type/isinstance/tuple) inside modules lazily built DURING phase-2 checks
   (e.g. +237 such wipes while checking test_compute.py); the per-file wipe gap
   grows monotonically (cmd/manage ~500 -> password ~12000). prylint pre-builds
   more of nova's transitive imports in phase 1 (mods 1980 after phase-1, 17248
   after phase-2; only +3546 phase-2 wipes vs astroid's >13000), so its
   tip-wipes fire EARLY on a cold cache (harmless) instead of LATE on a warm
   cache (which would cool the get_by_uuid chain to cold -> U). pylint 4.0.5
   ALSO two-phases (_get_asts then _lint_files), so the divergence is the
   phase-1 lazy-build SET/aggressiveness, not the phase split itself.

VERDICT (zero-fp round 1): BLOCKED on the SAME single root cause, now precisely
quantified as phase-2 inference-cache-wipe TIMING (driven by phase-1 lazy
module-build over-aggressiveness vs astroid). A true fix must replicate
astroid's exact lazy-module-build set+order across the full-mode visitor
fan-out so the builtin-tip wipes land at the same times — an engine-architecture
change that directly risks the INVIOLABLE -E 27/27 gate + inferdump-zero + the
22 clean full-mode corpora; not safely landable this round. sqlalchemy/sympy
(subscripted-generic base cap-truncation) and core W0143 (cross-module callable
resolution) are the same warm-vs-cold class.

GATES (zero-fp round 1, re-certified): -E 27/27 ALL EQUAL (out+exit); treedump
django 200/0; inferdump django/core/pandas/sentry 0/0, nova 8/21 (os.environ
env-noise only); FP census unchanged. Net deliverable: the lint-path
wipe-timing measurement toolchain (committed) + this quantified diagnosis.
