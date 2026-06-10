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
