# prylint architecture (Rust port of pylint 4.0.5 -E subset)

Goal: byte-identical output to `pylint . -E --disable=<list>` (pylint 4.0.5 /
astroid 4.0.4 / CPython 3.12.12), ≥10x faster. Ground truth and diff harness
live in `harness/`; spec notes in `reference/notes/01..08`.

## Crates

- `pyast` — astroid-equivalent tree (DONE-ish; tree-dump differential vs
  astroid driven to zero by the tree-fidelity workflow). Arena `Tree` per
  module, `NodeKind` per astroid taxonomy, astroid positions, locals maps,
  brain tree-transforms that affect structure (dataclasses, …).
- `pyinfer` — inference engine + module graph (this doc).
- `pycheckers` — ported checkers, single shared AST walk.
- `cli` (bin `prylint`) — discovery (DONE, byte-identical), orchestration,
  pragma handling, reporter, exit codes.

## Pipeline (mirrors pylint exactly; see notes/02)

1. Discovery → ordered `FileItem` list (DONE).
2. Phase B: parse+build ALL target files (rayon parallel), in deterministic
   collection: results inserted into ModuleGraph in file order (later same-name
   modules overwrite earlier, like astroid_cache).
   - Files that fail ruff parse (incl. UnsupportedSyntaxError at target 3.12)
     go to the **syntax oracle**: one batched `python` subprocess running
     pinned astroid `_parse_string` per file, returning exact
     (lineno, offset, str(error), retried_without_type_comments) → E0001
     messages formatted like pylinter.get_ast. The oracle uses
     `.venv-pylint` python in dev; in production any python3.12 works
     (configurable, auto-detect).
   - E0001/F0010/F0002 messages from this phase are emitted FIRST, in file
     order (two-phase, notes/02).
3. Phase C: per-file checking (rayon parallel, but OUTPUT buffered per file
   and flushed in file order):
   tokenize-pragmas (E0011 etc) → skip-file? → raw checker (unicode E25xx) →
   AST walk with callbacks in prepared-checker order (extracted empirically,
   see harness order dump) → leave_module checks.
   Message enablement: `MsgState` per file: config state (-E + --disable) +
   pragma line/block states (port of FileState._set_state_on_block_lines,
   needs Tree.block_range).
4. Reporter: stream per file buffer; module header on first message per
   module NAME (global set); template
   `{path}:{line}:{column}: {msg_id}: {msg} ({symbol})`; exit = msg_status
   bits (F=1,E=2,W=4,R=8,C=16) for DISPLAYED messages; score path: exit 0
   when zero displayed messages.

## pyinfer design

### Identity

- `ModId(u32)`, `GNode = (ModId, NodeId)`.
- `Value` (≈ astroid InferenceResult):
  - `Node(GNode)` — Const/List/Dict/ClassDef/FunctionDef/Module/… infer to
    themselves. Const/containers count as Instances of builtin classes
    (astroid: Const inherits Instance; `_proxied` = builtins class by value
    type).
  - `Instance { cls: GNode }` — instance of a ClassDef.
  - `BoundMethod { func: GNode, bound: Box<Value> }`
  - `UnboundMethod { func: GNode }`
  - `Generator { func: GNode, is_async: bool }`
  - `Property { func: GNode }`, `PartialFunction {…}`, `Super {…}`,
    `UnionType {…}`, `ExceptionInstance { cls: GNode, handler_ctx }`,
    `DictItems/Keys/Values(GNode)`, `FrozenSet {…}`
  - `Uninferable` — falsy, attribute access yields itself.
- Identity semantics: path-set and caches key by (GNode, lookupname). Proxies
  never enter the path set (they infer to themselves).

### ModuleGraph

- `DashMap<String /*modname*/, ModuleSlot>` with states Loading/Ready/Failed
  (failed records AstroidImportError vs AstroidSyntaxError + error text for
  imports.py E0001 "Cannot import 'X' due to '…'").
- Sources, in astroid resolution order (port of modutils spec.find_spec —
  notes/01/07): target-file map first? NO: astroid resolves by sys.path
  ([cwd-package-path] + venv sys.path); target files are reachable because
  the package path is prepended. Implement find_spec over: corpus root(s),
  stdlib dir, site-packages dir(s), + embedded synthetic modules for
  C-extensions/builtins.
- Synthetic modules: generated snapshot (see below).
- Cycle handling: astroid tolerates import cycles via cache-before-postbuild;
  replicate: insert slot as Loading, then build; importers during build see
  partially-built module? astroid inserts the Module into cache BEFORE
  delayed import processing; for us trees are built atomically, cycles only
  matter for delayed star-import expansion — match astroid's order there.

### Builtins / C-extension snapshot

`harness/gen_snapshot.py` (pinned venv): builds astroid's view of `builtins`
and every importable C-extension stdlib module (sys.builtin_module_names +
binary stdlib .so), AFTER brains, then serializes trees to JSON
(`crates/pyinfer/snapshot/*.json`, embedded via include_bytes!): every node:
kind, name(s), positions (raw_building uses 0s), const values, args/defaults,
bases, decorators, doc, locals, instance_attrs. Loader reconstructs `Tree`s.
Pure-python stdlib modules are parsed from the real stdlib directory on disk
(path from config/auto-detect; harness pins it to the venv's 3.12 stdlib).

### Inference core (port order)

1. `scope_lookup`/`_filter_stmts` (notes/07 §7) — also used by checkers
   directly (is_defined_before).
2. `infer_name` → `_infer_stmts` + assigned_stmts protocol (§9) for
   Assign/For/With/Arguments/Comprehension/Except/Match/Starred.
3. Import/ImportFrom inference via ModuleGraph (+ relative resolution,
   E0402 condition).
4. Call inference: ClassDef→Instance, FunctionDef.infer_call_result (return
   stmts; generators; implicit-None rules §11), BoundMethod binding,
   metaclass __call__ bail-outs.
5. Attribute access: Instance.getattr/igetattr order (§12: special attrs,
   instance_attrs from AssignAttr collection at build time, class locals,
   MRO walk, descriptors property/classmethod/staticmethod, __getattr__
   fallback conservatism).
6. MRO C3 (§13) incl. error modes for E0240/E0241; ancestors() fallback.
7. Operator protocols (§14) for E1130/E1131; subscript (§15) for
   E1126/E1136; iteration protocol for E1133; membership (§ for E1135);
   context manager protocol for E1129/E1701.
8. Inference tips/brains as required by diffs: builtin_inference
   (str/list/dict methods source-backed), namedtuple, enum, typing,
   functools.partial, super, property, dataclasses field calls.
9. Constraints system (`if x is not None` narrowing) — astroid 4 applies it
   in _infer_stmts; port (notes/07).

Caches: per-run global `DashMap<(GNode, Option<Sym>), Arc<[Value]>>` for the
common (node, name, None, None) key; per-context path sets. max_inferred=100
and recursion semantics per notes/07 §3. StopIteration→InferenceError edges
must map to "no results" identically.

### Threading

Phase C parallel per file; ModuleGraph lazy-load guarded; inference cache
sharded. Any divergence vs pylint's single-thread shared-cache order is
acceptable ONLY if the output diff is empty on all corpora — else fall back
to deterministic sharing (worst case: single-threaded inference with
parallel parse, still >>10x because astroid's cost is dominated by Python
interpretation, not parallelism).

## Fidelity workflow (the loop that matters)

For each checker/feature PR:
1. `cargo build --release`
2. `harness/run_prylint.sh <corpus>` → out file
3. `harness/diffmsg.py results/<corpus>.iso.out <ours>` → FP/FN per code
4. Fix → repeat. Zero FP/FN on: django, pandas, salt, airflow, core, sentry,
   pylfunc. Then byte-compare including order/headers.

## Performance budget (pylint baselines, single-thread)

django 109s · salt 183s · pandas 259s · airflow 286s · sentry 323s ·
core 1357s · pylfunc 5s. 10x target = whole-suite ≤ ~250s; expect Rust
parse+check ≪ that; watch inference blowups (memoize; astroid's own caches
are the model).
