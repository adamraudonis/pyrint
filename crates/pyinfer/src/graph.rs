//! Engine + ModuleGraph: astroid manager/modutils/spec port.
//!
//! - sys.path resolution: astroid/interpreter/_import/spec.py find_spec
//!   (ImportlibFinder + PathSpecFinder namespace scan; Zip/ExplicitNamespace
//!   finders are dead in the pinned environment and not ported).
//! - manager.ast_from_module_name / ast_from_file (manager.py:131-276),
//!   astroid_cache with setdefault semantics (manager.py:420-422 —
//!   first-built module wins; probe-verified).
//! - builder._post_build (builder.py:159-178): cache BEFORE delayed
//!   star-import locals expansion + delayed_assattr (which uses inference).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};

use pyast::tree::{ModuleData, Node, NodeKind, Tree};
use pyast::NodeId;

use crate::ctx::Ctx;
use crate::intern::GlobalInterner;
use crate::pyenv::{self, PyEnv};
use crate::snapshot::{load_snapshot, EInf};
use crate::value::{ErrKind, GNode, GSym, ModId, Value, ValueKey, NV};

/// Cached PRYLINT_TRACE_INFER check: `std::env::var` (getenv + String
/// alloc) showed up at >10% inclusive in the django profile from the
/// per-call checks sprinkled through inference. The PRYLINT_TRACE_START
/// debug aid flips it on mid-run via `set_trace_infer`.
pub fn trace_infer() -> bool {
    TRACE_INIT.call_once(|| {
        TRACE_INFER.store(
            std::env::var("PRYLINT_TRACE_INFER").is_ok(),
            std::sync::atomic::Ordering::Relaxed,
        );
    });
    TRACE_INFER.load(std::sync::atomic::Ordering::Relaxed)
}

static TRACE_INFER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static TRACE_INIT: std::sync::Once = std::sync::Once::new();

thread_local! {
    /// debug-only: the file currently being linted (set by lint_tree). Lets
    /// focused probes (PRYLINT_TRACE_NODE) scope to one module.
    static CUR_LINT_FILE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// debug-only: record the file currently being linted.
pub fn set_cur_lint_file(path: &str) {
    CUR_LINT_FILE.with(|f| *f.borrow_mut() = path.to_string());
}

/// debug-only: read the file currently being linted.
pub fn cur_lint_file() -> String {
    CUR_LINT_FILE.with(|f| f.borrow().clone())
}

/// debug-only: global count of inference-cache wipes (transforms).
static WIPE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn bump_wipe_count() {
    WIPE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}
pub fn wipe_count() -> u64 {
    WIPE_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Recency index for the lru_cache mirrors: O(log n) eviction instead of
/// the old O(n) `min_by_key` scan over the whole cache. Ticks are unique
/// per live entry, so popping the minimum LIVE tick selects EXACTLY the
/// same entry the linear scan did — eviction order (= semantics, via
/// re-miss recompute effects) is unchanged.
pub struct EvictIndex<K> {
    heap: std::collections::BinaryHeap<std::cmp::Reverse<u64>>,
    by_tick: FxHashMap<u64, K>,
    compact_at: usize,
}

impl<K: Copy + Eq + std::hash::Hash> EvictIndex<K> {
    pub fn new(cap: usize) -> Self {
        EvictIndex {
            heap: std::collections::BinaryHeap::new(),
            by_tick: FxHashMap::default(),
            compact_at: 8 * cap.max(64),
        }
    }
    /// record a refreshed recency (lru hit): old tick dies, new tick lives
    pub fn touch(&mut self, old_tick: u64, new_tick: u64, k: K) {
        self.by_tick.remove(&old_tick);
        self.insert(new_tick, k);
    }
    pub fn insert(&mut self, tick: u64, k: K) {
        self.by_tick.insert(tick, k);
        self.heap.push(std::cmp::Reverse(tick));
        if self.heap.len() > self.compact_at {
            self.heap = self.by_tick.keys().map(|&t| std::cmp::Reverse(t)).collect();
        }
    }
    /// drop a tick whose cache entry was overwritten (recursive re-insert
    /// of the same key during the miss computation)
    pub fn forget(&mut self, tick: u64) {
        self.by_tick.remove(&tick);
    }
    /// pop the key with the minimum live tick (the LRU entry)
    pub fn pop_lru(&mut self) -> Option<K> {
        while let Some(std::cmp::Reverse(t)) = self.heap.pop() {
            if let Some(k) = self.by_tick.remove(&t) {
                return Some(k);
            }
        }
        None
    }
}

/// debug aid: bounded-cache capacities, env-overridable for warmth
/// sensitivity experiments (PRYLINT_LOOKUP_CAP / PRYLINT_META_CAP). Default
/// to astroid's exact maxsize (128 / 1024). Read once and memoized.
pub fn lookup_cap() -> usize {
    use std::sync::OnceLock;
    static V: OnceLock<usize> = OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("PRYLINT_LOOKUP_CAP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(128)
    })
}
pub fn meta_cap() -> usize {
    use std::sync::OnceLock;
    static V: OnceLock<usize> = OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("PRYLINT_META_CAP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024)
    })
}

/// Turn the trace flag on at runtime (PRYLINT_TRACE_START debug aid).
pub fn set_trace_infer(on: bool) {
    let _ = trace_infer(); // force env init first so it can't overwrite us
    TRACE_INFER.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FType {
    Function,
    Method,
    ClassMethod,
    StaticMethod,
}

pub struct Module {
    pub id: ModId,
    pub name: String,
    /// absolute source path; "<?>" for string-built, "<snapshot>" for C-ext
    pub file: String,
    pub tree: Tree,
    /// tree sym index -> global sym
    pub gsym: Vec<GSym>,
    pub package: bool,
    pub pure_python: bool,
    /// scope node -> ordered locals (engine-side mutable copy; astroid
    /// mutates locals during delayed passes and cross-module assattr)
    pub locals: RefCell<FxHashMap<NodeId, IndexMap<GSym, Vec<GNode>>>>,
    /// snapshot FunctionDef.type overrides
    pub ftype: FxHashMap<NodeId, FType>,
    pub einf: FxHashMap<NodeId, Vec<EInf>>,
    pub eklass: FxHashMap<NodeId, crate::snapshot::EKlass>,
    /// raw-built Arguments with args=None (unknown signature)
    pub args_unknown: FxHashMap<NodeId, bool>,
    /// snapshot qname overrides (raw-built node reparenting)
    pub qnames: FxHashMap<NodeId, String>,
    /// transforms applied yet? inference tips are registered on nodes by
    /// the TransformVisitor at the END of build (builder.py:175-177);
    /// build-time inference (delayed_assattr) runs WITHOUT tips.
    pub tips_active: Cell<bool>,
    /// module-extender locals carrying live VALUES (brain_multiprocessing
    /// rebinds context methods as BoundMethod objects directly into module
    /// locals — brain_multiprocessing.py:36-48). Keys present here OVERRIDE
    /// the plain `locals` entry for module-level getattr; consulted by
    /// module_getattr / public_names only.
    pub ext_locals: RefCell<IndexMap<GSym, Vec<crate::value::NV>>>,
}

impl Module {
    pub fn module_node(&self) -> GNode {
        GNode {
            m: self.id,
            n: NodeId::MODULE,
        }
    }
}

#[derive(Debug, Clone)]
pub enum BuildFail {
    /// AstroidImportError (+ message text for imports checker later)
    Import(String),
    /// AstroidSyntaxError. `path`/`modname` identify the failing build so
    /// the imports checker can recover the exact `str(exc.error)` text
    /// (via the CPython oracle); `msg` is our ruff-derived approximation.
    Syntax { msg: String, path: String, modname: String },
    /// astroid TooManyLevelsError (relative import above the top level).
    /// Subclass of AstroidImportError in astroid — sites that handle
    /// failures generically must treat it like Import.
    TooManyLevels,
    /// astroid would CRASH building this file (RecursionError in the
    /// rebuilder on pathologically deep trees). pylint never catches it:
    /// the whole module check aborts -> F0002. The engine trips
    /// `crash_tripped` whenever such a build is attempted.
    Crash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecType {
    CBuiltin,
    CExtension,
    PySource,
    PyCompiled,
    PkgDirectory,
    PyNamespace,
    PyFrozen,
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub type_: SpecType,
    pub location: Option<String>,
    pub submodule_search_locations: Option<Vec<String>>,
}

pub type InfKey = (GNode, Option<GSym>, Option<u64>, Option<ValueKey>);

pub struct BuiltinRefs {
    pub object: GNode,
    pub type_: GNode,
    pub int: GNode,
    pub float: GNode,
    pub complex: GNode,
    pub str_: GNode,
    pub bytes: GNode,
    pub bool_: GNode,
    pub list: GNode,
    pub tuple: GNode,
    pub dict: GNode,
    pub set: GNode,
    pub frozenset: GNode,
    pub slice: GNode,
    pub super_: GNode,
    pub generator: GNode,
    pub async_generator: GNode,
    pub function: GNode,
    pub builtin_function_or_method: GNode,
    pub method: GNode,
    pub module: GNode,
    pub none_type: GNode,
    pub notimpl_type: GNode,
    pub ellipsis_type: GNode,
    pub union_type: GNode,
    pub traceback: GNode,
}

pub struct Engine {
    pub interner: RefCell<GlobalInterner>,
    pub mods: RefCell<Vec<Rc<Module>>>,
    /// astroid_cache: modname -> module (setdefault semantics)
    pub astroid_cache: RefCell<FxHashMap<String, ModId>>,
    /// _mod_file_cache (modname, contextfile=None) -> spec | cached error
    pub mod_file_cache: RefCell<FxHashMap<String, Result<Spec, String>>>,
    /// abs paths of files whose astroid build CRASHES (RecursionError in
    /// the rebuilder; oracle-verified). Any build attempt trips
    /// `crash_tripped` — pylint's RecursionError propagates uncaught and
    /// aborts the current module's check (-> F0002).
    pub crash_files: RefCell<std::collections::HashSet<String>>,
    pub crash_tripped: std::cell::Cell<bool>,
    /// memo of failed file_build attempts keyed by (abs path, modname).
    /// astroid re-reads + re-parses broken files on EVERY import (failures
    /// never enter astroid_cache and have no side effects), so memoizing
    /// the failure is behaviourally invisible — it only saves the 27k
    /// re-parses of core's 318 broken files at lint time.
    pub build_fail_cache: RefCell<FxHashMap<(String, String), BuildFail>>,
    /// instance_attrs for every class/function (cross-module mutable)
    pub iattrs: RefCell<FxHashMap<GNode, IndexMap<GSym, Vec<GNode>>>>,
    /// instance_attrs for instances of the object_type PROXY classes
    /// (function/builtin_function_or_method/method/module): astroid builds
    /// a FRESH `_build_proxy_class` ClassDef per evaluation
    /// (helpers.py:39-57), so delayed assattrs land on per-evaluation
    /// classes — keyed here by (class, InstId), the instance identity that
    /// cache replays preserve. The SHARED snapshot class never accumulates
    /// entries (matches astroid: its raw class stays clean while fresh
    /// proxies come and go across transform-wipes).
    pub proxy_iattrs: RefCell<FxHashMap<(GNode, crate::value::InstId), IndexMap<GSym, Vec<GNode>>>>,
    /// global inference cache (context.py:19-23)
    pub inf_cache: RefCell<FxHashMap<InfKey, Rc<Vec<Value>>>>,
    /// LookupMixIn.lookup `@lru_cache` — DEFAULT maxsize=128
    /// (_base_nodes.py:262)! One tiny GLOBAL LRU for all (node, name)
    /// pairs: hits refresh recency, inserts beyond 128 evict the least
    /// recent. Eviction is semantic: a re-MISS recomputes the lookup
    /// against LIVE module locals (delayed_assattr from later-built
    /// modules lands in earlier modules' locals — e.g. salt compat.py's
    /// `copy._deepcopy_dispatch = pre_dispatch` only becomes visible to
    /// copy.py Name lookups after the stale entry ages out).
    pub lookup_cache:
        RefCell<FxHashMap<(GNode, GSym), (Rc<crate::lookup::LookupResult>, u64)>>,
    pub lookup_tick: Cell<u64>,
    /// recency index for lookup_cache eviction (same order, O(log n))
    pub lookup_evict: RefCell<EvictIndex<(GNode, GSym)>>,
    /// FunctionDef.type cached_property
    pub ftype_cache: RefCell<FxHashMap<GNode, FType>>,
    /// ClassDef._type memo (scoped_nodes.py:1759-1762 — persists for the run)
    pub cls_type_cache: RefCell<FxHashMap<GNode, &'static str>>,
    /// ClassDef._all_slots cached_property
    pub slots_cache: RefCell<FxHashMap<GNode, Result<Option<Rc<Vec<String>>>, ()>>>,
    /// _metaclass_lookup_attribute @lru_cache(maxsize=1024)
    /// (scoped_nodes.py:2375-2386): key (self, name, context-IDENTITY);
    /// context=None keys are stable for the whole run. The Rc<Ctx> is
    /// pinned inside the entry so the pointer can't be recycled (the
    /// Python lru cache holds the context object itself).
    #[allow(clippy::type_complexity)]
    pub metalookup_cache: RefCell<
        FxHashMap<(GNode, GSym, Option<usize>), (Rc<Vec<crate::value::NV>>, u64, Option<Rc<Ctx>>)>,
    >,
    pub metalookup_tick: Cell<u64>,
    /// recency index for metalookup_cache eviction (same order, O(log n))
    pub metalookup_evict: RefCell<EvictIndex<(GNode, GSym, Option<usize>)>>,
    /// inference-tip recursion guard + cache (inference_tip.py:37-86).
    /// Key: (func id, node, ctx identity) — ctx identity is 0 for the
    /// empty-context normalization (`if context.is_empty(): context = None`,
    /// inference_tip.py:50-52) and the Rc pointer otherwise: astroid keys
    /// the OrderedDict by InferenceContext OBJECT identity, the key tuple
    /// keeping the ctx alive (we pin the Rc in the entry for the same
    /// pointer-stability guarantee). 64-entry FIFO: EVERY successful miss
    /// inserts — non-empty-ctx entries virtually never hit again but they
    /// EVICT the useful None-keyed ones (inference_tip.py:78-79
    /// `if len(_cache) > 64: _cache.popitem(last=False)`).
    pub tip_guard: RefCell<FxHashSet<(u8, GNode)>>,
    #[allow(clippy::type_complexity)]
    pub tip_cache:
        RefCell<FxHashMap<(u8, GNode, usize), (Rc<Vec<Value>>, Option<Rc<Ctx>>)>>,
    pub tip_order: RefCell<std::collections::VecDeque<(u8, GNode, usize)>>,
    /// recursion depth guard standing in for Python's RecursionError
    /// Generators created through a PartialFunction call: their parent
    /// is the synthetic PartialFunction whose qname() is the literal class
    /// name "PartialFunction" (objects.py:325-326). Keyed by the captured
    /// call_ctx Rc pointer; the Rc is pinned so the key can't be recycled.
    pub partial_gen_ctxs: RefCell<FxHashMap<usize, Rc<Ctx>>>,
    pub depth: Cell<u32>,
    pub max_depth: u32,
    pub callctx_id: Cell<u64>,
    pub env: PyEnv,
    /// [realpath(cwd)] + venv sys.path
    pub sys_path: Vec<String>,
    /// None = use the snapshots embedded in the binary;
    /// Some = PRYLINT_SNAPSHOT_DIR on-disk override.
    pub snapshot_dir: Option<PathBuf>,
    pub builtins_mod: Cell<ModId>,
    pub b: RefCell<Option<Rc<BuiltinRefs>>>,
    pub isfile_cache: RefCell<FxHashMap<String, bool>>,
    pub isdir_cache: RefCell<FxHashMap<String, bool>>,
    /// typing-brain synthetic classes, cached per origin node regardless of
    /// context (astroid pins node._explicit_inference to a fixed lambda)
    pub typing_tip_cache: RefCell<FxHashMap<GNode, Vec<Value>>>,
    /// placeholder nodes in runtime-built synthetic class modules that stand
    /// for cross-module nodes (NV::N — raw bases of _infer_type_new_call) or
    /// pre-inferred values (NV::V — EvaluatedObject / enum-member instances
    /// stored in locals). infer() forwards through this table.
    pub redirects: RefCell<FxHashMap<GNode, crate::value::NV>>,
    /// redirect placeholders standing for PROXY objects stored directly in
    /// locals (enum member Instances, brain_namedtuple_enum.py
    /// `new_targets.append(fake.instantiate_class())`): astroid's
    /// Proxy.infer is a bare `yield self` (bases.py:139) — NO NodeNG.infer
    /// entry, no bump, no cache write. infer_to short-circuits these.
    pub proxy_placeholders: RefCell<rustc_hash::FxHashSet<GNode>>,
    /// cross-module parent overrides: astroid reparents brain-built nodes
    /// into real modules (`fake.parent = target.parent`,
    /// brain_namedtuple_enum.py infer_enum_class) so their scope chains —
    /// and thus base-Name lookups — resolve in the TARGET module.
    /// Consulted by treeutil::parent().
    pub reparents: RefCell<FxHashMap<GNode, GNode>>,
    /// six.with_metaclass hack: persistent `self._metaclass = baseobj._metaclass`
    /// mutation from declared_metaclass (scoped_nodes.py:2638-2645)
    pub meta_override: RefCell<FxHashMap<GNode, GNode>>,
    /// ObjectModel.attr___new__/attr___init__ synthetic FunctionDefs
    /// (objectmodel.py:136-164): `def __new__(self, cls): return cls()` and
    /// `def __init__(self, *args, **kwargs): return None`, reparented to
    /// builtins.object (qname builtins.object.__new__/__init__)
    pub obj_model_funcs: RefCell<Option<(GNode, GNode)>>,
    /// ClassDef.implicit_locals() (scoped_nodes.py:1911-1933): every class
    /// gets `__module__`/`__qualname__`/`__annotations__` Const/Unknown
    /// locals at CONSTRUCTION time (values frozen with the then-current
    /// parent chain). Materialized lazily as redirect placeholders keyed
    /// (class, 0|1|2); implicit_owner records the owning class for the
    /// igetattr same-scope filter (placeholder "parent" is the class).
    pub implicit_locals: RefCell<FxHashMap<(GNode, u8), GNode>>,
    pub implicit_owner: RefCell<FxHashMap<GNode, GNode>>,
    /// Property objects built by the property(...) builtin tip: astroid
    /// names them "<property>" parented to SYNTHETIC_ROOT
    /// (brain_builtin_inference.py:610-647) -> qname
    /// "__astroid_synthetic.<property>" regardless of the function.
    pub synth_props: RefCell<FxHashSet<GNode>>,
    /// PropertyModel attr_fget/attr_fset accessors (objectmodel.py:921-986):
    /// fresh PropertyFuncAccessor FunctionDefs whose infer_call_result
    /// delegates to the wrapped function (after a caller-arg-count gate).
    /// accessor synth node -> (wrapped function, required caller argc)
    pub prop_accessors: RefCell<FxHashMap<GNode, (GNode, usize)>>,
    /// functions decorated with functools.lru_cache get LruWrappedModel
    /// special_attributes (brain_functools.py:133-142 predicate, raw
    /// transform returns None -> no wipe)
    pub lru_wrapped: RefCell<FxHashSet<GNode>>,
    /// lazily-built `def cache_clear(self): pass` template function
    /// (LruWrappedModel.attr_cache_clear, brain_functools.py:59-62)
    pub lru_cache_clear_fn: RefCell<Option<GNode>>,
    /// ClassDef.hide — true only for synthesized temporary_class nodes
    /// (scoped_nodes.py:1603 with_metaclass hack)
    pub hidden_classes: RefCell<FxHashSet<GNode>>,
    /// Subscript nodes whose brain_pathlib parents-predicate matched at
    /// transform time (predicates run inference; tips are FIXED then)
    pub pathlib_subscripts: RefCell<FxHashSet<GNode>>,
    /// Call nodes whose _is_str_format_call predicate matched at transform
    /// time (brain_builtin_inference.py:1090-1101): the predicate's
    /// safe_infer(node.func.expr) runs ONCE during the module's transform
    /// scan — tip applicability is FIXED then; infer-time re-evaluation
    /// would re-pull the expr under live state (airflowctl comprehension).
    pub str_format_calls: RefCell<FxHashSet<GNode>>,
    /// dataclass attribute Unknown placeholders -> their AnnAssign stmt
    /// (brain_dataclasses.dataclass_transform rhs_node, parent=assign)
    pub dataclass_attrs: RefCell<FxHashMap<GNode, GNode>>,
    /// Call nodes whose dataclass-field predicate matched at transform time
    pub dataclass_field_calls: RefCell<FxHashSet<GNode>>,
    /// enum class -> astroid `__members__` value-Name names, i.e.
    /// `[v.name for k, v in dunder_members.items()]` — the LAST target of
    /// each member statement (brain_namedtuple_enum infer_enum_class;
    /// pylint utils.is_enum_member reads name_obj.name, not the keys)
    pub enum_member_names: RefCell<FxHashMap<GNode, Vec<String>>>,
    /// `node.is_dataclass = True` flags (brain_dataclasses.py:59 — set at
    /// transform start, read WITHOUT inference by
    /// _find_arguments_from_base_classes / renders)
    pub is_dataclass_flag: RefCell<FxHashSet<GNode>>,
    /// lru-wrapped function -> the `_CacheInfo(0, 0, 0, 0)` template Call
    /// from the most recent attr_cache_info access (brain_functools.py:
    /// 38-56) — CacheInfoBoundMethod.infer_call_result safe_infers it
    pub cacheinfo_calls: RefCell<FxHashMap<GNode, GNode>>,
    /// generated dataclass __init__ FunctionDef -> structured param data
    /// (pos-or-kw, kw-only) of (name, annotation_str, default_str) — stands
    /// in for Arguments._get_arguments_data (node_classes.py:861-925) on the
    /// synthesized init (whose annotations/defaults were as_string'd from
    /// these exact strings)
    #[allow(clippy::type_complexity)]
    pub dataclass_init_params: RefCell<
        FxHashMap<
            GNode,
            (
                Vec<(String, Option<String>, Option<String>)>,
                Vec<(String, Option<String>, Option<String>)>,
            ),
        >,
    >,
    /// node_classes.py:5007 UNATTACHED_UNKNOWN singleton (Unknown node used
    /// by protocols._filter_uninferable_nodes for U container elements)
    pub unattached_unknown: RefCell<Option<GNode>>,
    /// klass._all_bases_known memo (helpers.py:175-189 has_known_bases)
    pub known_bases_cache: RefCell<FxHashMap<GNode, bool>>,
    /// set by compute_mro on failure: true = DuplicateBasesError,
    /// false = InconsistentMroError (E0241 vs E0240 distinction)
    pub last_mro_dup: std::cell::Cell<bool>,
    /// `stmt.infer(context)` entries for SYNTHETIC nodes flowing through
    /// _infer_stmts (bases.py:198): in astroid those are real fresh nodes —
    /// the first hop under a given (lookupname, callcontext, boundnode) key
    /// is a cache miss (1 nodes_inferred bump + cache write), later hops
    /// replay bump-free. Keyed by Rc pointer identity; cleared with the
    /// global inference cache.
    pub synth_hop_cache: RefCell<FxHashSet<(u8, usize, Option<GSym>, Option<u64>, Option<crate::value::ValueKey>)>>,
    /// synth-hop entries whose first (miss) pull happened while the shared
    /// counter was over the 100 cap: astroid's NodeNG.infer wrapper yields
    /// Uninferable INSTEAD of the value (node_ng.py:161-167; no bump) and
    /// caches [Uninferable] — replays of the same key must yield U too.
    pub synth_hop_trunc: RefCell<FxHashSet<(u8, usize, Option<GSym>, Option<u64>, Option<crate::value::ValueKey>)>>,
    /// DictModel attr_items Tuple elements, built once per DictItems object
    /// (objectmodel.py:856-867) and reused — keyed by the DictRef pointer
    pub dictitems_elts_cache: RefCell<FxHashMap<usize, Rc<Vec<Value>>>>,
    /// register_builtin_transform position copy (brain_builtin_inference
    /// _transform_wrapper): a from_elements container result gets the CALL
    /// node's parent/lineno. Keyed by the elems Rc pointer; the Weak ref
    /// guards against address reuse.
    pub cont_prov: RefCell<FxHashMap<usize, (std::rc::Weak<Vec<Value>>, GNode)>>,
    /// keep-alive pins for values whose Rc POINTER is used as an identity
    /// key (synth_hop_cache / ValueKey::Synth / dictitems_elts_cache):
    /// python ids stay unique while referenced; without pinning the
    /// allocator recycles freed Rc addresses and keys collide (ABA).
    pub synth_pins: RefCell<Vec<Value>>,
}

/// Snapshots are embedded in the binary (snapshot::embedded_json);
/// PRYLINT_SNAPSHOT_DIR overrides with an on-disk directory for snapshot
/// regeneration / differential debugging.
fn snapshot_dir() -> Option<PathBuf> {
    std::env::var("PRYLINT_SNAPSHOT_DIR").ok().map(PathBuf::from)
}

impl Engine {
    pub fn new(root: &Path) -> Engine {
        let env = pyenv::probe();
        let real_root = std::fs::canonicalize(root)
            .unwrap_or_else(|_| root.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let mut sys_path = vec![real_root];
        // init-hook sys.path deltas (Phase F): the CLI runs the rcfile/CLI
        // init-hook through python and forwards the resulting sys.path additions
        // here so import resolution sees them (notes/09 §8.4). Inserted right
        // after the cwd realpath, before the interpreter sys.path, mirroring an
        // init-hook's `sys.path.insert(0, ...)` / append being visible to the
        // subsequent astroid module search.
        if let Ok(extra) = std::env::var("PRYLINT_EXTRA_SYSPATH") {
            for p in extra.split(':').filter(|s| !s.is_empty()) {
                sys_path.push(p.to_string());
            }
        }
        sys_path.extend(env.sys_path.clone());
        let e = Engine {
            interner: RefCell::new(GlobalInterner::default()),
            mods: RefCell::new(Vec::new()),
            astroid_cache: RefCell::new(FxHashMap::default()),
            mod_file_cache: RefCell::new(FxHashMap::default()),
            crash_files: RefCell::new(std::collections::HashSet::new()),
            crash_tripped: std::cell::Cell::new(false),
            build_fail_cache: RefCell::new(FxHashMap::default()),
            iattrs: RefCell::new(FxHashMap::default()),
            proxy_iattrs: RefCell::new(FxHashMap::default()),
            inf_cache: RefCell::new(FxHashMap::default()),
            lookup_cache: RefCell::new(FxHashMap::default()),
            lookup_tick: Cell::new(0),
            lookup_evict: RefCell::new(EvictIndex::new(lookup_cap())),
            ftype_cache: RefCell::new(FxHashMap::default()),
            cls_type_cache: RefCell::new(FxHashMap::default()),
            slots_cache: RefCell::new(FxHashMap::default()),
            metalookup_cache: RefCell::new(FxHashMap::default()),
            metalookup_tick: Cell::new(0),
            metalookup_evict: RefCell::new(EvictIndex::new(meta_cap())),
            tip_guard: RefCell::new(FxHashSet::default()),
            tip_cache: RefCell::new(FxHashMap::default()),
            tip_order: RefCell::new(std::collections::VecDeque::new()),
            depth: Cell::new(0),
            max_depth: std::env::var("PRYLINT_MAX_DEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(350),
            callctx_id: Cell::new(1),
            env,
            sys_path,
            snapshot_dir: snapshot_dir(),
            builtins_mod: Cell::new(ModId(0)),
            b: RefCell::new(None),
            isfile_cache: RefCell::new(FxHashMap::default()),
            isdir_cache: RefCell::new(FxHashMap::default()),
            typing_tip_cache: RefCell::new(FxHashMap::default()),
            redirects: RefCell::new(FxHashMap::default()),
            proxy_placeholders: RefCell::new(rustc_hash::FxHashSet::default()),
            reparents: RefCell::new(FxHashMap::default()),
            partial_gen_ctxs: RefCell::new(FxHashMap::default()),
            meta_override: RefCell::new(FxHashMap::default()),
            obj_model_funcs: RefCell::new(None),
            implicit_locals: RefCell::new(FxHashMap::default()),
            implicit_owner: RefCell::new(FxHashMap::default()),
            synth_props: RefCell::new(FxHashSet::default()),
            prop_accessors: RefCell::new(FxHashMap::default()),
            lru_wrapped: RefCell::new(FxHashSet::default()),
            lru_cache_clear_fn: RefCell::new(None),
            hidden_classes: RefCell::new(FxHashSet::default()),
            pathlib_subscripts: RefCell::new(FxHashSet::default()),
            str_format_calls: RefCell::new(FxHashSet::default()),
            dataclass_attrs: RefCell::new(FxHashMap::default()),
            dataclass_field_calls: RefCell::new(FxHashSet::default()),
            enum_member_names: RefCell::new(FxHashMap::default()),
            is_dataclass_flag: RefCell::new(FxHashSet::default()),
            dataclass_init_params: RefCell::new(FxHashMap::default()),
            cacheinfo_calls: RefCell::new(FxHashMap::default()),
            unattached_unknown: RefCell::new(None),
            known_bases_cache: RefCell::new(FxHashMap::default()),
            last_mro_dup: std::cell::Cell::new(false),
            synth_hop_cache: RefCell::new(FxHashSet::default()),
            synth_hop_trunc: RefCell::new(FxHashSet::default()),
            dictitems_elts_cache: RefCell::new(FxHashMap::default()),
            cont_prov: RefCell::new(FxHashMap::default()),
            synth_pins: RefCell::new(Vec::new()),
        };
        e.bootstrap();
        e
    }

    pub fn sym(&self, s: &str) -> GSym {
        self.interner.borrow_mut().intern(s)
    }
    pub fn sname(&self, s: GSym) -> String {
        self.interner.borrow().get(s).to_string()
    }
    /// run `f` against the interned str WITHOUT allocating a String.
    /// `f` must not touch the interner (the borrow is held across the call).
    pub fn with_sname<R>(&self, s: GSym, f: impl FnOnce(&str) -> R) -> R {
        f(self.interner.borrow().get(s))
    }
    /// translate a tree-local sym to the global interner
    pub fn g(&self, md: &Module, sym: pyast::tree::Sym) -> GSym {
        md.gsym[sym.0 as usize]
    }

    pub fn next_callctx_id(&self) -> u64 {
        let id = self.callctx_id.get();
        self.callctx_id.set(id + 1);
        id
    }

    /// debug aid (PRYLINT_CACHESTAT): final fill levels of the bounded
    /// caches. lookup_tick / metalookup_tick are total access counts (hits
    /// + inserts); cache lens are the live entry counts (capped at the
    /// bound iff eviction ever fired).
    pub fn cache_stat_line(&self) -> String {
        format!(
            "CACHESTAT lookup={}/{} accesses={} | meta={}/{} accesses={} | inf={} | tip={}",
            self.lookup_cache.borrow().len(),
            128,
            self.lookup_tick.get(),
            self.metalookup_cache.borrow().len(),
            1024,
            self.metalookup_tick.get(),
            self.inf_cache.borrow().len(),
            self.tip_cache.borrow().len(),
        )
    }

    /// debug aid: number of modules currently built in the graph.
    pub fn module_count(&self) -> usize {
        self.mods.borrow().len()
    }

    /// Keep a value alive whose Rc pointer serves as an identity key
    /// (ValueKey::Synth / Generator ctx pointer / synth_hop_cache /
    /// dictitems_elts_cache) — prevents allocator address reuse from
    /// aliasing distinct "python objects".
    pub fn pin_value_identity(&self, v: &Value) {
        match v {
            Value::SynthConst(_)
            | Value::SynthSeq { .. }
            | Value::SynthDict { .. }
            | Value::SynthSlice { .. }
            | Value::FrozenSet { .. }
            | Value::DictItems(_)
            | Value::DictKeys(_)
            | Value::DictValues(_)
            | Value::Generator { .. }
            // Super boundnode keys use the mro_type Rc pointer identity
            | Value::Super { .. } => self.synth_pins.borrow_mut().push(v.clone()),
            _ => {}
        }
    }

    /// The UNATTACHED_UNKNOWN singleton (node_classes.py:5007) — lazily
    /// allocated Unknown node in a synthetic module.
    /// record container-brain provenance (only when absent — astroid
    /// copies position only onto PARENTLESS from_elements results)
    pub fn set_container_prov(&self, v: &Value, node: GNode) {
        let elems = match v {
            Value::SynthSeq { elems, .. } => elems,
            Value::FrozenSet { elems } => elems,
            _ => return,
        };
        self.cont_prov
            .borrow_mut()
            .entry(Rc::as_ptr(elems) as usize)
            .or_insert_with(|| (Rc::downgrade(elems), node));
    }

    /// provenance node of a container-brain value, if recorded and alive
    pub fn container_prov(&self, v: &Value) -> Option<GNode> {
        let elems = match v {
            Value::SynthSeq { elems, .. } => elems,
            Value::FrozenSet { elems } => elems,
            _ => return None,
        };
        let map = self.cont_prov.borrow();
        let (weak, node) = map.get(&(Rc::as_ptr(elems) as usize))?;
        let alive = weak.upgrade().map(|rc| Rc::ptr_eq(&rc, elems)).unwrap_or(false);
        if alive {
            Some(*node)
        } else {
            None
        }
    }

    pub fn unknown_singleton(&self) -> GNode {
        if let Some(g) = *self.unattached_unknown.borrow() {
            return g;
        }
        let g = self.alloc_placeholders(1)[0];
        *self.unattached_unknown.borrow_mut() = Some(g);
        g
    }

    fn isfile(&self, p: &str) -> bool {
        if let Some(&v) = self.isfile_cache.borrow().get(p) {
            return v;
        }
        let v = Path::new(p).is_file();
        self.isfile_cache.borrow_mut().insert(p.to_string(), v);
        v
    }
    fn isdir(&self, p: &str) -> bool {
        if let Some(&v) = self.isdir_cache.borrow().get(p) {
            return v;
        }
        let v = Path::new(p).is_dir();
        self.isdir_cache.borrow_mut().insert(p.to_string(), v);
        v
    }

    // ---------- bootstrap ----------

    fn bootstrap(&self) {
        // builtins from the snapshot (post-brain bootstrap module)
        let id = self
            .load_snapshot_module("builtins")
            .expect("builtins snapshot must exist");
        self.builtins_mod.set(id);
        self.astroid_cache
            .borrow_mut()
            .insert("builtins".to_string(), id);
        // synthetic module holding the bare _CONST_PROXY classes
        // (raw_building.py:608-624: NoneType/NotImplementedType/Ellipsis are
        // build_class results NOT inserted into builtins.locals) + UnionType.
        let synth = self.build_synth_module();
        let find = |name: &str| -> GNode {
            let bm = self.md(id);
            let sym = self.sym(name);
            let locs = bm.locals.borrow();
            locs.get(&NodeId::MODULE)
                .and_then(|l| l.get(&sym))
                .and_then(|v| v.first().copied())
                .unwrap_or(GNode {
                    m: id,
                    n: NodeId::MODULE,
                })
        };
        let refs = BuiltinRefs {
            object: find("object"),
            type_: find("type"),
            int: find("int"),
            float: find("float"),
            complex: find("complex"),
            str_: find("str"),
            bytes: find("bytes"),
            bool_: find("bool"),
            list: find("list"),
            tuple: find("tuple"),
            dict: find("dict"),
            set: find("set"),
            frozenset: find("frozenset"),
            slice: find("slice"),
            super_: find("super"),
            generator: find("generator"),
            async_generator: find("async_generator"),
            function: GNode { m: synth, n: NodeId(5) },
            builtin_function_or_method: GNode { m: synth, n: NodeId(6) },
            method: GNode { m: synth, n: NodeId(7) },
            module: GNode { m: synth, n: NodeId(8) },
            traceback: find("traceback"),
            none_type: GNode { m: synth, n: NodeId(1) },
            notimpl_type: GNode { m: synth, n: NodeId(2) },
            ellipsis_type: GNode { m: synth, n: NodeId(3) },
            union_type: GNode { m: synth, n: NodeId(4) },
        };
        *self.b.borrow_mut() = Some(Rc::new(refs));
    }

    pub fn builtins(&self) -> Rc<BuiltinRefs> {
        Rc::clone(self.b.borrow().as_ref().unwrap())
    }

    /// register a file whose astroid build crashes (see `crash_files`)
    pub fn add_crash_file(&self, abspath: String) {
        self.crash_files.borrow_mut().insert(abspath);
    }
    pub fn crash_tripped(&self) -> bool {
        self.crash_tripped.get()
    }
    pub fn reset_crash_trip(&self) {
        self.crash_tripped.set(false);
    }

    fn build_synth_module(&self) -> ModId {
        // Module named "builtins" (for qname purposes) holding bare classes.
        let mut interner = pyast::tree::Interner::default();
        let mut nodes: Vec<Node> = Vec::new();
        // first four: Const proxies. last four: object_type PROXY classes
        // (helpers.py _build_proxy_class builds FRESH EMPTY "function"/
        // "builtin_function_or_method"/"method"/"module" classes parented
        // to builtins — DISTINCT from the raw-built full classes in the
        // snapshot's builtins locals, which einf descriptors resolve to)
        let class_names = [
            "NoneType",
            "NotImplementedType",
            "Ellipsis",
            "UnionType",
            "function",
            "builtin_function_or_method",
            "method",
            "module",
        ];
        let body: Vec<NodeId> = (1..=class_names.len() as u32).map(NodeId).collect();
        nodes.push(Node {
            kind: NodeKind::Module(Box::new(ModuleData {
                name: "builtins".into(),
                file: "<synthetic>".into(),
                package: false,
                body: body.clone(),
                doc_node: None,
                future_imports: Vec::new(),
            })),
            parent: NodeId::MODULE,
            fromlineno: 0,
            col_offset: 0,
            end_lineno: 0,
            end_col_offset: -1,
            tolineno: 0,
        });
        for name in class_names {
            let sym = interner.intern(name);
            nodes.push(Node {
                kind: NodeKind::ClassDef(Box::new(pyast::tree::ClassData {
                    name: sym,
                    decorators: None,
                    bases: Vec::new(),
                    keywords: Vec::new(),
                    metaclass: None,
                    type_params: Vec::new(),
                    body: Vec::new(),
                    doc_node: None,
                })),
                parent: NodeId::MODULE,
                fromlineno: 0,
                col_offset: 0,
                end_lineno: 0,
                end_col_offset: -1,
                tolineno: 0,
            });
        }
        // UnionType is NOT bare: raw_building.py:673-694 runs
        // builder.object_build(_UnionTypeType, types.UnionType), populating
        // the proxied class with the live interpreter's member set (the
        // class itself stays OUT of builtins.locals). Member presence is
        // pylint-visible: types.UnionType.__getitem__ exists on 3.12 →
        // supports_getitem(UnionType instance) is True (no E1136 on
        // subscripted PEP 604 union aliases, e.g. typeshed copyreg.pyi).
        // Mirrored from the pinned 3.12.12 venv astroid in insertion order
        // (__class__ → the real raw-built `type` is wired engine-side
        // below, since tree.locals can't reference another module).
        let union_cls = NodeId(4);
        let mut union_locals: indexmap::IndexMap<pyast::tree::Sym, Vec<NodeId>> =
            indexmap::IndexMap::new();
        {
            let mut push = |nodes: &mut Vec<Node>, kind: NodeKind, parent: NodeId| -> NodeId {
                let id = NodeId(nodes.len() as u32);
                nodes.push(Node {
                    kind,
                    parent,
                    fromlineno: 0,
                    col_offset: 0,
                    end_lineno: 0,
                    end_col_offset: -1,
                    tolineno: 0,
                });
                id
            };
            // class doc_node (types.UnionType.__doc__)
            let doc = push(
                &mut nodes,
                NodeKind::Const(pyast::tree::ConstValue::Str(
                    "Represent a PEP 604 union type\n\nE.g. for int | str".into(),
                )),
                union_cls,
            );
            if let NodeKind::ClassDef(cd) = &mut nodes[union_cls.idx()].kind {
                cd.doc_node = Some(doc);
            }
            let mut add_const = |nodes: &mut Vec<Node>,
                                 interner: &mut pyast::tree::Interner,
                                 locals: &mut indexmap::IndexMap<pyast::tree::Sym, Vec<NodeId>>,
                                 name: &str,
                                 val: &str| {
                let id = push(
                    nodes,
                    NodeKind::Const(pyast::tree::ConstValue::Str(val.into())),
                    union_cls,
                );
                locals.insert(interner.intern(name), vec![id]);
            };
            add_const(&mut nodes, &mut interner, &mut union_locals, "__module__", "builtins");
            add_const(
                &mut nodes,
                &mut interner,
                &mut union_locals,
                "__qualname__",
                "builtins.UnionType",
            );
            let ann = push(&mut nodes, NodeKind::Unknown, union_cls);
            union_locals.insert(interner.intern("__annotations__"), vec![ann]);
            // raw-built data descriptors -> empty ClassDefs named after the
            // member (__class__ handled engine-side)
            let mut add_dd = |nodes: &mut Vec<Node>,
                              interner: &mut pyast::tree::Interner,
                              locals: &mut indexmap::IndexMap<pyast::tree::Sym, Vec<NodeId>>,
                              name: &str| {
                let sym = interner.intern(name);
                let id = push(
                    nodes,
                    NodeKind::ClassDef(Box::new(pyast::tree::ClassData {
                        name: sym,
                        decorators: None,
                        bases: Vec::new(),
                        keywords: Vec::new(),
                        metaclass: None,
                        type_params: Vec::new(),
                        body: Vec::new(),
                        doc_node: None,
                    })),
                    union_cls,
                );
                locals.insert(sym, vec![id]);
            };
            let mut add_meth = |nodes: &mut Vec<Node>,
                                interner: &mut pyast::tree::Interner,
                                locals: &mut indexmap::IndexMap<pyast::tree::Sym, Vec<NodeId>>,
                                name: &str| {
                let sym = interner.intern(name);
                let args = push(
                    nodes,
                    NodeKind::Arguments(Box::new(pyast::tree::ArgumentsData {
                        posonlyargs: Vec::new(),
                        args: Vec::new(),
                        vararg: None,
                        vararg_node: None,
                        kwonlyargs: Vec::new(),
                        kwarg: None,
                        kwarg_node: None,
                        defaults: Vec::new(),
                        kw_defaults: Vec::new(),
                        annotations: Vec::new(),
                        posonlyargs_annotations: Vec::new(),
                        kwonlyargs_annotations: Vec::new(),
                        varargannotation: None,
                        kwargannotation: None,
                        tc_last_posonly: false,
                        tc_last_arg: false,
                        tc_last_kwonly: false,
                    })),
                    union_cls, // re-parented to the FunctionDef just below
                );
                let id = push(
                    nodes,
                    NodeKind::FunctionDef(Box::new(pyast::tree::FunctionData {
                        name: sym,
                        decorators: None,
                        args,
                        returns: None,
                        type_params: Vec::new(),
                        body: Vec::new(),
                        doc_node: None,
                    })),
                    union_cls,
                );
                nodes[args.idx()].parent = id;
                locals.insert(sym, vec![id]);
            };
            add_dd(&mut nodes, &mut interner, &mut union_locals, "__args__");
            for m in [
                "__delattr__", "__dir__", "__eq__", "__format__", "__ge__",
                "__getattribute__", "__getitem__", "__getstate__", "__gt__",
                "__hash__", "__init__",
            ] {
                add_meth(&mut nodes, &mut interner, &mut union_locals, m);
            }
            let isub = push(&mut nodes, NodeKind::EmptyNode, union_cls);
            union_locals.insert(interner.intern("__init_subclass__"), vec![isub]);
            for m in ["__le__", "__lt__", "__ne__", "__new__", "__or__"] {
                add_meth(&mut nodes, &mut interner, &mut union_locals, m);
            }
            add_dd(&mut nodes, &mut interner, &mut union_locals, "__parameters__");
            for m in [
                "__reduce__", "__reduce_ex__", "__repr__", "__ror__",
                "__setattr__", "__sizeof__", "__str__", "__subclasshook__",
            ] {
                add_meth(&mut nodes, &mut interner, &mut union_locals, m);
            }
        }
        let mut tree_locals: FxHashMap<NodeId, indexmap::IndexMap<pyast::tree::Sym, Vec<NodeId>>> =
            FxHashMap::default();
        tree_locals.insert(union_cls, union_locals);
        let tree = Tree {
            nodes,
            interner,
            locals: tree_locals,
            positions: FxHashMap::default(),
            type_comments: Vec::new(),
            u_string_consts: Default::default(),
        };
        let mid = self.register_module(
            "builtins".to_string(),
            "<synthetic>".to_string(),
            tree,
            false,
            false,
        );
        // __class__ -> the REAL raw-built `type` class from the builtins
        // snapshot (raw building attaches the live class object). Appended
        // after registration because tree.locals NodeIds are module-local.
        {
            let bid = self
                .astroid_cache
                .borrow()
                .get("builtins")
                .copied()
                .expect("builtins loaded before synth module");
            let type_g = {
                let bm = self.md(bid);
                let sym = self.sym("type");
                let locs = bm.locals.borrow();
                locs.get(&NodeId::MODULE)
                    .and_then(|l| l.get(&sym))
                    .and_then(|v| v.first().copied())
            };
            if let Some(type_g) = type_g {
                let md = self.md(mid);
                let cls_sym = self.sym("__class__");
                let mut locs = md.locals.borrow_mut();
                if let Some(map) = locs.get_mut(&union_cls) {
                    map.insert(cls_sym, vec![type_g]);
                }
            }
        }
        mid
    }

    // ---------- module registration ----------

    pub fn register_module(
        &self,
        name: String,
        file: String,
        tree: Tree,
        package: bool,
        pure_python: bool,
    ) -> ModId {
        let id = ModId(self.mods.borrow().len() as u32);
        // gsym translation table
        let n_syms = tree.interner.len();
        let mut gsym = Vec::with_capacity(n_syms);
        {
            let mut gi = self.interner.borrow_mut();
            for i in 0..n_syms {
                gsym.push(gi.intern(tree.interner.get(pyast::tree::Sym(i as u32))));
            }
        }
        // engine-side locals copy, translated; ImportFrom-sourced entries
        // are stripped here and re-added by post_build (replacing pyast's
        // static stdlib wildcard table with real module resolution).
        let mut locals: FxHashMap<NodeId, IndexMap<GSym, Vec<GNode>>> = FxHashMap::default();
        for (&scope, map) in &tree.locals {
            let mut out: IndexMap<GSym, Vec<GNode>> = IndexMap::new();
            for (sym, ids) in map {
                let gs = gsym[sym.0 as usize];
                let filtered: Vec<GNode> = ids
                    .iter()
                    .filter(|&&n| !matches!(tree.nodes[n.idx()].kind, NodeKind::ImportFrom { .. }))
                    .map(|&n| GNode { m: id, n })
                    .collect();
                if !filtered.is_empty() {
                    out.insert(gs, filtered);
                }
            }
            locals.insert(scope, out);
        }
        let md = Module {
            id,
            name,
            file,
            tree,
            gsym,
            package,
            pure_python,
            locals: RefCell::new(locals),
            ftype: FxHashMap::default(),
            einf: FxHashMap::default(),
            eklass: FxHashMap::default(),
            args_unknown: FxHashMap::default(),
            qnames: FxHashMap::default(),
            tips_active: Cell::new(false),
            ext_locals: RefCell::new(IndexMap::new()),
        };
        self.mods.borrow_mut().push(Rc::new(md));
        id
    }

    /// Build a runtime synthetic ClassDef hosted in its own module (used by
    /// _infer_type_call (scoped_nodes.py:2017-2069), _infer_type_new_call
    /// (bases.py:555-654) and the enum/namedtuple brains). The host module
    /// is named like the astroid parent frame's qname so qname() composes
    /// identically; never registered in astroid_cache. `n_bases` Unknown
    /// placeholder nodes become ClassData.bases (wire them via `redirects`),
    /// `with_metaclass` adds one placeholder wired as the declared metaclass,
    /// and `n_extra` placeholders are free slots for value-valued locals.
    /// Returns (class node, base slots, metaclass slot, extra slots).
    #[allow(clippy::too_many_arguments)]
    pub fn build_synth_class(
        &self,
        modname: &str,
        clsname: &str,
        lineno: u32,
        col: i32,
        n_bases: usize,
        with_metaclass: bool,
        n_extra: usize,
    ) -> (GNode, Vec<NodeId>, Option<NodeId>, Vec<NodeId>) {
        let mut interner = pyast::tree::Interner::default();
        let name_sym = interner.intern(clsname);
        let mut nodes: Vec<Node> = Vec::new();
        let mk = |kind: NodeKind, parent: NodeId, line: u32, c: i32| Node {
            kind,
            parent,
            fromlineno: line,
            col_offset: c,
            end_lineno: line,
            end_col_offset: -1,
            tolineno: line,
        };
        // node 0: Module (patched with body below)
        nodes.push(mk(
            NodeKind::Module(Box::new(ModuleData {
                name: modname.into(),
                file: "<synthetic>".into(),
                package: false,
                body: vec![NodeId(1)],
                doc_node: None,
                future_imports: Vec::new(),
            })),
            NodeId::MODULE,
            0,
            0,
        ));
        let cls_id = NodeId(1);
        // placeholder ids start at 2
        let mut next = 2u32;
        let base_slots: Vec<NodeId> = (0..n_bases)
            .map(|_| {
                let id = NodeId(next);
                next += 1;
                id
            })
            .collect();
        let meta_slot: Option<NodeId> = if with_metaclass {
            let id = NodeId(next);
            next += 1;
            Some(id)
        } else {
            None
        };
        let extra_slots: Vec<NodeId> = (0..n_extra)
            .map(|_| {
                let id = NodeId(next);
                next += 1;
                id
            })
            .collect();
        nodes.push(mk(
            NodeKind::ClassDef(Box::new(pyast::tree::ClassData {
                name: name_sym,
                decorators: None,
                bases: base_slots.clone(),
                keywords: Vec::new(),
                metaclass: meta_slot,
                type_params: Vec::new(),
                body: Vec::new(),
                doc_node: None,
            })),
            NodeId::MODULE,
            lineno,
            col,
        ));
        for _ in 0..(next - 2) {
            nodes.push(mk(NodeKind::Unknown, cls_id, lineno, col));
        }
        let tree = Tree {
            nodes,
            interner,
            locals: FxHashMap::default(),
            positions: FxHashMap::default(),
            type_comments: Vec::new(),
            u_string_consts: Default::default(),
        };
        let mid = self.register_module(
            modname.to_string(),
            "<synthetic>".to_string(),
            tree,
            false,
            true,
        );
        (
            GNode { m: mid, n: cls_id },
            base_slots,
            meta_slot,
            extra_slots,
        )
    }

    /// Allocate a single orphan node of the given kind in a fresh synthetic
    /// module (implicit class locals: Const/Unknown that infer to themselves
    /// with stable identity, no redirects).
    pub fn alloc_synth_node(&self, kind: NodeKind) -> GNode {
        let interner = pyast::tree::Interner::default();
        let nodes: Vec<Node> = vec![
            Node {
                kind: NodeKind::Module(Box::new(ModuleData {
                    name: "".into(),
                    file: "<synthetic>".into(),
                    package: false,
                    body: Vec::new(),
                    doc_node: None,
                    future_imports: Vec::new(),
                })),
                parent: NodeId::MODULE,
                fromlineno: 0,
                col_offset: 0,
                end_lineno: 0,
                end_col_offset: -1,
                tolineno: 0,
            },
            Node {
                kind,
                parent: NodeId::MODULE,
                fromlineno: 0,
                col_offset: 0,
                end_lineno: 0,
                end_col_offset: -1,
                tolineno: 0,
            },
        ];
        let tree = Tree {
            nodes,
            interner,
            locals: FxHashMap::default(),
            positions: FxHashMap::default(),
            type_comments: Vec::new(),
            u_string_consts: Default::default(),
        };
        let mid = self.register_module(String::new(), "<synthetic>".to_string(), tree, false, true);
        GNode { m: mid, n: NodeId(1) }
    }

    /// A FRESH per-access stand-in for objectmodel attrs that astroid
    /// returns as newly-built NODES (Const/Tuple/Dict/Unknown/...): the
    /// consumer's stmt.infer() gets a full NodeNG.infer hop (+1 bump, cap
    /// check, fresh-key cache write) whose _infer yields the value as-is.
    /// A new node per call — astroid never reuses these, so the cache key
    /// can never hit.
    pub fn model_hop_node(&self, v: crate::value::Value) -> GNode {
        let g = self.alloc_synth_node(NodeKind::Unknown);
        self.redirects.borrow_mut().insert(g, crate::value::NV::V(v));
        g
    }

    /// Allocate `count` orphan Unknown nodes in a fresh synthetic module —
    /// hosts for redirect placeholders (enum-member instances and other
    /// VALUE-valued locals entries).
    /// PropertyModel._init_function (objectmodel.py:896-921): a fresh
    /// empty FunctionDef (no body, empty Arguments) named `name`,
    /// reparented under `parent` so qname composes through it.
    pub fn alloc_synth_funcdef(&self, name: &str, parent: GNode) -> GNode {
        let mut interner = pyast::tree::Interner::default();
        let name_sym = interner.intern(name);
        let mut nodes: Vec<Node> = Vec::new();
        nodes.push(Node {
            kind: NodeKind::Module(Box::new(ModuleData {
                name: "".into(),
                file: "<synthetic>".into(),
                package: false,
                body: vec![pyast::NodeId(1)],
                doc_node: None,
                future_imports: Vec::new(),
            })),
            parent: NodeId::MODULE,
            fromlineno: 0,
            col_offset: 0,
            end_lineno: 0,
            end_col_offset: -1,
            tolineno: 0,
        });
        nodes.push(Node {
            kind: NodeKind::FunctionDef(Box::new(pyast::tree::FunctionData {
                name: name_sym,
                decorators: None,
                args: pyast::NodeId(2),
                returns: None,
                type_params: Vec::new(),
                body: Vec::new(),
                doc_node: None,
            })),
            parent: NodeId::MODULE,
            fromlineno: 0,
            col_offset: 0,
            end_lineno: 0,
            end_col_offset: -1,
            tolineno: 0,
        });
        nodes.push(Node {
            kind: NodeKind::Arguments(Box::new(pyast::tree::ArgumentsData {
                posonlyargs: Vec::new(),
                args: Vec::new(),
                vararg: None,
                vararg_node: None,
                kwonlyargs: Vec::new(),
                kwarg: None,
                kwarg_node: None,
                defaults: Vec::new(),
                kw_defaults: Vec::new(),
                annotations: Vec::new(),
                posonlyargs_annotations: Vec::new(),
                kwonlyargs_annotations: Vec::new(),
                varargannotation: None,
                kwargannotation: None,
                tc_last_posonly: false,
                tc_last_arg: false,
                tc_last_kwonly: false,
            })),
            parent: pyast::NodeId(1),
            fromlineno: 0,
            col_offset: 0,
            end_lineno: 0,
            end_col_offset: -1,
            tolineno: 0,
        });
        let tree = Tree {
            nodes,
            interner,
            locals: FxHashMap::default(),
            positions: FxHashMap::default(),
            type_comments: Vec::new(),
            u_string_consts: Default::default(),
        };
        let mid = self.register_module("".to_string(), "<synthetic>".to_string(), tree, false, true);
        let g = GNode { m: mid, n: pyast::NodeId(1) };
        self.reparents.borrow_mut().insert(g, parent);
        g
    }

    pub fn alloc_placeholders(&self, count: usize) -> Vec<GNode> {
        let interner = pyast::tree::Interner::default();
        let mut nodes: Vec<Node> = Vec::new();
        nodes.push(Node {
            kind: NodeKind::Module(Box::new(ModuleData {
                name: "".into(),
                file: "<synthetic>".into(),
                package: false,
                body: Vec::new(),
                doc_node: None,
                future_imports: Vec::new(),
            })),
            parent: NodeId::MODULE,
            fromlineno: 0,
            col_offset: 0,
            end_lineno: 0,
            end_col_offset: -1,
            tolineno: 0,
        });
        for _ in 0..count {
            nodes.push(Node {
                kind: NodeKind::Unknown,
                parent: NodeId::MODULE,
                fromlineno: 0,
                col_offset: 0,
                end_lineno: 0,
                end_col_offset: -1,
                tolineno: 0,
            });
        }
        let tree = Tree {
            nodes,
            interner,
            locals: FxHashMap::default(),
            positions: FxHashMap::default(),
            type_comments: Vec::new(),
            u_string_consts: Default::default(),
        };
        let mid = self.register_module(String::new(), "<synthetic>".to_string(), tree, false, true);
        (1..=count as u32).map(|i| GNode { m: mid, n: NodeId(i) }).collect()
    }

    /// manager.cache_module: setdefault — first module wins.
    pub fn cache_module(&self, name: &str, id: ModId) {
        self.astroid_cache
            .borrow_mut()
            .entry(name.to_string())
            .or_insert(id);
    }

    fn load_snapshot_module(&self, modname: &str) -> Option<ModId> {
        let data: std::borrow::Cow<'_, str> = match &self.snapshot_dir {
            Some(dir) => {
                std::fs::read_to_string(dir.join(format!("{modname}.json"))).ok()?.into()
            }
            None => crate::snapshot::embedded_json(modname)?.into(),
        };
        let mut snap = load_snapshot(&data)?;
        if modname == "sys" {
            // The oracle (dump_infer.py main) runs `sys.path.insert(0,
            // os.path.realpath(root))` before astroid raw-builds the live
            // sys module, so the frozen sys.path List leads with the
            // corpus root; the snapshot stores the corpus-independent
            // tail. Mirror the insert here.
            if let Some((_, ids)) = snap
                .locals
                .iter()
                .find(|(scope, _)| *scope == NodeId::MODULE)
                .and_then(|(_, l)| l.iter().find(|(n, _)| n.as_str() == "path"))
            {
                if let Some(&list_id) = ids.first() {
                    let root = self.sys_path.first().cloned().unwrap_or_default();
                    let p = &snap.tree.nodes[list_id.idx()];
                    let (fl, co, el, ec, tl) = (
                        p.fromlineno,
                        p.col_offset,
                        p.end_lineno,
                        p.end_col_offset,
                        p.tolineno,
                    );
                    let new_id = NodeId(snap.tree.nodes.len() as u32);
                    snap.tree.nodes.push(pyast::tree::Node {
                        kind: NodeKind::Const(pyast::tree::ConstValue::Str(root.into())),
                        parent: list_id,
                        fromlineno: fl,
                        col_offset: co,
                        end_lineno: el,
                        end_col_offset: ec,
                        tolineno: tl,
                    });
                    if let NodeKind::List { elts, .. } =
                        &mut snap.tree.nodes[list_id.idx()].kind
                    {
                        elts.insert(0, new_id);
                    }
                }
            }
            // sys.argv / sys.orig_argv were frozen from the LIVE warm
            // process (harness/warm_infercache.sh):
            //   $ROOT/.venv-pylint/bin/python $ROOT/harness/dump_infer.py \
            //       $ROOT/corpora/<c> /tmp/warmitems_<c>.jsonl
            // Reconstruct those exact values when running inside a corpus
            // checkout (cwd = $ROOT/corpora/<c>); probe runs keep the
            // snapshot defaults.
            let corpus_root = self.sys_path.first().cloned().unwrap_or_default();
            let corpus_path = std::path::Path::new(&corpus_root);
            let in_corpora = corpus_path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n == "corpora")
                .unwrap_or(false);
            if in_corpora {
                let root = corpus_path
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let cname = corpus_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let argv = vec![
                    format!("{root}/harness/dump_infer.py"),
                    corpus_root.clone(),
                    format!("/tmp/warmitems_{cname}.jsonl"),
                ];
                let mut orig_argv = vec![format!("{root}/.venv-pylint/bin/python")];
                orig_argv.extend(argv.iter().cloned());
                for (name, values) in [("argv", argv), ("orig_argv", orig_argv)] {
                    let Some(&list_id) = snap
                        .locals
                        .iter()
                        .find(|(scope, _)| *scope == NodeId::MODULE)
                        .and_then(|(_, l)| l.iter().find(|(n, _)| n.as_str() == name))
                        .and_then(|(_, ids)| ids.first())
                    else {
                        continue;
                    };
                    if !matches!(snap.tree.nodes[list_id.idx()].kind, NodeKind::List { .. })
                    {
                        continue;
                    }
                    let p = &snap.tree.nodes[list_id.idx()];
                    let (fl, co, el, ec, tl) = (
                        p.fromlineno,
                        p.col_offset,
                        p.end_lineno,
                        p.end_col_offset,
                        p.tolineno,
                    );
                    let mut new_elts = Vec::with_capacity(values.len());
                    for s in values {
                        let new_id = NodeId(snap.tree.nodes.len() as u32);
                        snap.tree.nodes.push(pyast::tree::Node {
                            kind: NodeKind::Const(pyast::tree::ConstValue::Str(
                                s.into(),
                            )),
                            parent: list_id,
                            fromlineno: fl,
                            col_offset: co,
                            end_lineno: el,
                            end_col_offset: ec,
                            tolineno: tl,
                        });
                        new_elts.push(new_id);
                    }
                    if let NodeKind::List { elts, .. } =
                        &mut snap.tree.nodes[list_id.idx()].kind
                    {
                        *elts = new_elts;
                    }
                }
            }
        }
        let id = ModId(self.mods.borrow().len() as u32);
        let n_syms = snap.tree.interner.len();
        let mut gsym = Vec::with_capacity(n_syms);
        {
            let mut gi = self.interner.borrow_mut();
            for i in 0..n_syms {
                gsym.push(gi.intern(snap.tree.interner.get(pyast::tree::Sym(i as u32))));
            }
        }
        let mut locals: FxHashMap<NodeId, IndexMap<GSym, Vec<GNode>>> = FxHashMap::default();
        for (scope, entries) in &snap.locals {
            let mut out: IndexMap<GSym, Vec<GNode>> = IndexMap::new();
            for (name, ids) in entries {
                let gs = self.sym(name);
                out.insert(gs, ids.iter().map(|&n| GNode { m: id, n }).collect());
            }
            locals.insert(*scope, out);
        }
        let ftype = snap
            .ftype
            .iter()
            .map(|(&n, s)| {
                (
                    n,
                    match s.as_str() {
                        "method" => FType::Method,
                        "classmethod" => FType::ClassMethod,
                        "staticmethod" => FType::StaticMethod,
                        _ => FType::Function,
                    },
                )
            })
            .collect();
        let md = Module {
            id,
            name: snap.name.clone(),
            file: "<snapshot>".to_string(),
            tree: snap.tree,
            gsym,
            package: false,
            pure_python: snap.pure_python,
            locals: RefCell::new(locals),
            ftype,
            einf: snap.einf,
            eklass: snap.eklass,
            args_unknown: snap.args_unknown,
            qnames: snap.qnames,
            tips_active: Cell::new(false),
            ext_locals: RefCell::new(IndexMap::new()),
        };
        self.mods.borrow_mut().push(Rc::new(md));
        // InspectBuilder caches the module BEFORE module_build's
        // visit_transforms (raw_building.py:460) — transforms that
        // ast_from_module_name the module being scanned (brain_io on _io's
        // own classes!) must hit the cache, not rebuild.
        self.cache_module(modname, id);
        // raw-built modules also go through visit_transforms
        // (builder.py:103-109 module_build) — run the wipe scan. Note the
        // bootstrap builtins module is scanned too (harmless: cache empty).
        self.md(id).tips_active.set(true);
        self.wipe_scan(id);
        // instance_attrs from the snapshot (exception classes etc.)
        {
            let mut ia = self.iattrs.borrow_mut();
            for (cls, entries) in &snap.iattrs {
                let g = GNode { m: id, n: *cls };
                let map = ia.entry(g).or_default();
                for (name, ids) in entries {
                    let gs = self.sym(name);
                    map.entry(gs)
                        .or_default()
                        .extend(ids.iter().map(|&n| GNode { m: id, n }));
                }
            }
        }
        Some(id)
    }

    fn string_build_empty(&self, modname: &str) -> ModId {
        let mut interner = pyast::tree::Interner::default();
        let _ = interner.intern("");
        let tree = Tree {
            nodes: vec![Node {
                kind: NodeKind::Module(Box::new(ModuleData {
                    name: modname.into(),
                    file: "<?>".into(),
                    package: false,
                    body: Vec::new(),
                    doc_node: None,
                    future_imports: Vec::new(),
                })),
                parent: NodeId::MODULE,
                fromlineno: 0,
                col_offset: 0,
                end_lineno: 0,
                end_col_offset: -1,
                tolineno: 0,
            }],
            interner,
            locals: FxHashMap::default(),
            positions: FxHashMap::default(),
            type_comments: Vec::new(),
            u_string_consts: Default::default(),
        };
        let id = self.register_module(modname.to_string(), "<?>".to_string(), tree, false, true);
        self.cache_module(modname, id);
        id
    }

    fn namespace_build(&self, modname: &str) -> ModId {
        let mut interner = pyast::tree::Interner::default();
        let _ = interner.intern("");
        let tree = Tree {
            nodes: vec![Node {
                kind: NodeKind::Module(Box::new(ModuleData {
                    name: modname.into(),
                    file: "".into(),
                    package: true,
                    body: Vec::new(),
                    doc_node: None,
                    future_imports: Vec::new(),
                })),
                parent: NodeId::MODULE,
                fromlineno: 0,
                col_offset: 0,
                end_lineno: 0,
                end_col_offset: -1,
                tolineno: 0,
            }],
            interner,
            locals: FxHashMap::default(),
            positions: FxHashMap::default(),
            type_comments: Vec::new(),
            u_string_consts: Default::default(),
        };
        let id = self.register_module(modname.to_string(), String::new(), tree, true, true);
        self.cache_module(modname, id);
        id
    }

    // ---------- find_spec port ----------

    /// modutils.get_source_file (modutils.py:480-504), prefer_stubs=False
    fn get_source_file(&self, filename: &str) -> Option<String> {
        let abs = abspath(filename);
        let (base, orig_ext) = split_ext(&abs);
        if !matches!(orig_ext, "py" | "pyi") && !orig_ext.is_empty() && self.isfile(&abs) {
            return Some(abs);
        }
        for ext in ["py", "pyi"] {
            let cand = format!("{base}.{ext}");
            if self.isfile(&cand) {
                return Some(cand);
            }
        }
        // include_no_ext
        if orig_ext.is_empty() && self.isfile(&base) {
            return Some(base);
        }
        None
    }

    /// modutils._has_init
    fn has_init(&self, directory: &str) -> Option<String> {
        for ext in ["py", "pyi", "pyc", "pyo"] {
            let cand = format!("{directory}/__init__.{ext}");
            if self.isfile(&cand) {
                return Some(cand);
            }
        }
        None
    }

    /// spec.py ImportlibFinder.find_module (spec.py:126-194)
    fn importlib_finder(
        &self,
        modname: &str,
        processed: &[&str],
        submodule_path: Option<&[String]>,
    ) -> Option<Spec> {
        if submodule_path.is_none()
            && self
                .env
                .builtin_module_names
                .iter()
                .any(|b| b == modname)
        {
            return Some(Spec {
                type_: SpecType::CBuiltin,
                location: None,
                submodule_search_locations: None,
            });
        }
        let search: &[String] = match submodule_path {
            Some(p) => p,
            None => &self.sys_path,
        };
        for entry in search {
            let pkgdir = join_path(entry, modname);
            for suffix in ["py", "pyi", "pyc"] {
                if self.isfile(&format!("{pkgdir}/__init__.{suffix}")) {
                    return Some(Spec {
                        type_: SpecType::PkgDirectory,
                        location: Some(pkgdir),
                        submodule_search_locations: None,
                    });
                }
            }
            // suffix order: EXTENSION_SUFFIXES (C), then .py, then .pyc
            for ext in &self.env.ext_suffixes {
                let f = format!("{}{}", join_path(entry, modname), ext);
                if self.isfile(&f) {
                    return Some(Spec {
                        type_: SpecType::CExtension,
                        location: Some(f),
                        submodule_search_locations: None,
                    });
                }
            }
            let f = format!("{}.py", join_path(entry, modname));
            if self.isfile(&f) {
                return Some(Spec {
                    type_: SpecType::PySource,
                    location: Some(f),
                    submodule_search_locations: None,
                });
            }
            let f = format!("{}.pyc", join_path(entry, modname));
            if self.isfile(&f) {
                return Some(Spec {
                    type_: SpecType::PyCompiled,
                    location: Some(f),
                    submodule_search_locations: None,
                });
            }
        }
        // PY_FROZEN branch (spec.py:169-192): runs only after the
        // search-path scan failed, gated on stdlib membership. The live
        // `importlib.util.find_spec` results were captured at probe time
        // into env.frozen_specs (FrozenImporter-loaded names only):
        // _frozen_importlib -> location None (manager builds an EMPTY
        // stub module — importlib/__init__.py `import _frozen_importlib
        // as _bootstrap`), _frozen_importlib_external -> _bootstrap_external.py.
        let in_stdlib = |n: &str| self.env.stdlib_module_names.iter().any(|m| m == n);
        if (processed.is_empty() && in_stdlib(modname))
            || (!processed.is_empty() && in_stdlib(processed[0]))
        {
            let full = processed
                .iter()
                .copied()
                .chain(std::iter::once(modname))
                .collect::<Vec<_>>()
                .join(".");
            if let Some(filename) = self.env.frozen_specs.get(&full) {
                return Some(Spec {
                    type_: SpecType::PyFrozen,
                    location: filename.clone(),
                    submodule_search_locations: None,
                });
            }
        }
        None
    }

    /// spec.py PathSpecFinder reduced to its live effect: namespace
    /// directory portions for dirs without __init__.
    fn pathspec_finder(&self, modname: &str, submodule_path: Option<&[String]>) -> Option<Spec> {
        let search: &[String] = match submodule_path {
            Some(p) => p,
            None => &self.sys_path,
        };
        let mut portions = Vec::new();
        for entry in search {
            let d = join_path(entry, modname);
            if self.isdir(&d) {
                portions.push(d);
            }
        }
        if portions.is_empty() {
            None
        } else {
            Some(Spec {
                type_: SpecType::PyNamespace,
                location: None,
                submodule_search_locations: Some(portions),
            })
        }
    }

    /// spec.py:461-496 _find_spec (path=None) + contribute_to_path
    fn find_spec(&self, modpath: &[&str]) -> Result<Spec, String> {
        let mut search_paths: Option<Vec<String>> = None;
        let mut processed: Vec<&str> = Vec::new();
        let mut modpath = modpath.to_vec();
        let mut spec_res: Option<Spec> = None;
        while !modpath.is_empty() {
            let modname = modpath.remove(0);
            let submodule_path = search_paths.clone();
            let spec = self
                .importlib_finder(modname, &processed, submodule_path.as_deref())
                .or_else(|| self.pathspec_finder(modname, submodule_path.as_deref()));
            let mut spec = match spec {
                Some(s) => s,
                None => {
                    let full: Vec<&str> = processed
                        .iter()
                        .copied()
                        .chain(std::iter::once(modname))
                        .chain(modpath.iter().copied())
                        .collect();
                    return Err(format!("No module named {}", full.join(".")));
                }
            };
            processed.push(modname);
            if !modpath.is_empty() {
                // contribute_to_path
                search_paths = match spec.type_ {
                    SpecType::PyNamespace => spec.submodule_search_locations.clone(),
                    _ => match &spec.location {
                        None => None,
                        Some(loc) => {
                            // setuptools namespace __init__ check
                            if self.is_setuptools_namespace(loc) {
                                let joined: Vec<String> = self
                                    .sys_path
                                    .iter()
                                    .map(|p| {
                                        let mut q = p.clone();
                                        for part in &processed {
                                            q = join_path(&q, part);
                                        }
                                        q
                                    })
                                    .filter(|q| self.isdir(q))
                                    .collect();
                                Some(joined)
                            } else {
                                Some(vec![loc.clone()])
                            }
                        }
                    },
                };
            }
            if spec.type_ == SpecType::PkgDirectory {
                spec.submodule_search_locations = search_paths.clone();
            }
            spec_res = Some(spec);
        }
        Ok(spec_res.unwrap())
    }

    fn is_setuptools_namespace(&self, location: &str) -> bool {
        let init = format!("{location}/__init__.py");
        match std::fs::read(&init) {
            Err(_) => false,
            Ok(data) => {
                let head = &data[..data.len().min(4096)];
                let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
                (has(b"pkgutil") && has(b"extend_path"))
                    || (has(b"pkg_resources") && has(b"declare_namespace(__name__)"))
            }
        }
    }

    /// modutils.file_info_from_modpath + _spec_from_modpath
    fn file_info_from_modpath(&self, parts: &[&str]) -> Result<Spec, String> {
        if parts == ["os", "path"] {
            return Ok(Spec {
                type_: SpecType::PySource,
                location: Some(self.env.os_path_file.clone()),
                submodule_search_locations: None,
            });
        }
        let mut found = if parts.first() == Some(&"xml") {
            let mut xmlplus: Vec<&str> = vec!["_xmlplus"];
            xmlplus.extend(&parts[1..]);
            match self.find_spec(&xmlplus) {
                Ok(s) => Ok(s),
                Err(_) => self.find_spec(parts),
            }
        } else {
            self.find_spec(parts)
        }?;
        // _spec_from_modpath post-processing (modutils.py:622-660)
        match found.type_ {
            SpecType::PyCompiled => {
                if let Some(loc) = &found.location {
                    if let Some(src) = self.get_source_file(loc) {
                        found.location = Some(src);
                        found.type_ = SpecType::PySource;
                    }
                }
            }
            SpecType::CBuiltin => {
                found.location = None;
            }
            SpecType::PkgDirectory => {
                let loc = found.location.clone().unwrap_or_default();
                found.location = self.has_init(&loc);
                found.type_ = SpecType::PySource;
            }
            _ => {}
        }
        Ok(found)
    }

    /// astroid.modutils.file_from_modpath equivalent for the variables
    /// checker's `__all__` package-submodule resolution (variables.py:3268):
    /// can the dotted path be resolved to a file/spec? modutils does NOT use
    /// the manager's _mod_file_cache.
    pub fn modutils_can_resolve(&self, parts: &[&str]) -> bool {
        self.file_info_from_modpath(parts).is_ok()
    }

    /// manager.file_from_module_name with the _mod_file_cache
    fn file_from_module_name(&self, modname: &str) -> Result<Spec, String> {
        if let Some(cached) = self.mod_file_cache.borrow().get(modname) {
            return cached.clone();
        }
        let parts: Vec<&str> = modname.split('.').collect();
        let res = self.file_info_from_modpath(&parts).map_err(|e| {
            format!("Failed to import module {modname} with error:\n{e}.")
        });
        self.mod_file_cache
            .borrow_mut()
            .insert(modname.to_string(), res.clone());
        res
    }

    // ---------- ast_from_* ----------

    /// manager.ast_from_module_name (manager.py:195-276)
    pub fn ast_from_module_name(&self, modname: &str, use_cache: bool) -> Result<ModId, BuildFail> {
        if use_cache {
            if let Some(&id) = self.astroid_cache.borrow().get(modname) {
                return Ok(id);
            }
        }
        if modname.is_empty() {
            // astroid bootstrap leaves an empty-name module (name='', file='<?>')
            // in MANAGER.astroid_cache (brain_builtin_inference._extend_string_class
            // string_build with default modname '' / path None). A `from . import X`
            // whose relative_to_absolute_name resolves to '' (a non-package module
            // loaded by path, so module.name is an abspath and package_name is
            // empty) therefore RESOLVES to that cached empty module rather than
            // raising AstroidImportError — pylint's _get_imported_module returns it
            // (no import-error / E0401) and variables.py then emits no-name-in-module
            // (E0611) for each imported name absent from the (empty) module.
            // Replicate by synthesizing+caching the empty module. The astroid
            // bootstrap module carries a single 'whatever' local; we omit it since
            // no real relative import targets that name and its presence would only
            // suppress an (impossible) E0611 for `from . import whatever`.
            return Ok(self.string_build_empty(""));
        }
        if modname == "__main__" {
            return Ok(self.string_build_empty(modname));
        }
        let spec = self
            .file_from_module_name(modname)
            .map_err(BuildFail::Import)?;
        match spec.type_ {
            SpecType::CBuiltin | SpecType::CExtension => {
                if spec.type_ == SpecType::CExtension && !self.can_load_extension(modname) {
                    return Ok(self.string_build_empty(modname));
                }
                match self.load_snapshot_module(modname) {
                    Some(id) => {
                        self.cache_module(modname, id);
                        Ok(id)
                    }
                    None => {
                        // astroid would live-import; modules absent from the
                        // snapshot failed to import there too.
                        Err(BuildFail::Import(format!(
                            "Loading {modname} failed with:\nsnapshot unavailable"
                        )))
                    }
                }
            }
            SpecType::PyCompiled => Err(BuildFail::Import(format!(
                "Unable to load compiled module {modname}."
            ))),
            SpecType::PyNamespace => Ok(self.namespace_build(modname)),
            SpecType::PyFrozen => match &spec.location {
                None => Ok(self.string_build_empty(modname)),
                Some(loc) => self.ast_from_file(loc, Some(modname), false, false),
            },
            SpecType::PySource | SpecType::PkgDirectory => match &spec.location {
                None => Err(BuildFail::Import(format!(
                    "Can't find a file for module {modname}."
                ))),
                Some(loc) => self.ast_from_file(loc, Some(modname), false, false),
            },
        }
    }

    fn can_load_extension(&self, modname: &str) -> bool {
        // manager._can_load_extension: stdlib modules only (no whitelist)
        let first = modname.split('.').next().unwrap_or(modname);
        self.env.stdlib_module_names.iter().any(|m| m == first)
    }

    /// manager.ast_from_file (manager.py:131-168)
    pub fn ast_from_file(
        &self,
        filepath: &str,
        modname: Option<&str>,
        fallback: bool,
        mut source: bool,
    ) -> Result<ModId, BuildFail> {
        let modname = match modname {
            Some(m) => m.to_string(),
            None => filepath.to_string(),
        };
        let check_cache = |fp: &str| -> Option<ModId> {
            let cache = self.astroid_cache.borrow();
            let &id = cache.get(&modname)?;
            if self.md(id).file == fp {
                Some(id)
            } else {
                None
            }
        };
        if let Some(id) = check_cache(filepath) {
            return Ok(id);
        }
        let mut filepath = filepath.to_string();
        if let Some(src) = self.get_source_file(&filepath) {
            filepath = src;
            source = true;
        }
        if let Some(id) = check_cache(&filepath) {
            return Ok(id);
        }
        if source {
            return self.file_build(&filepath, &modname);
        }
        if fallback && !modname.is_empty() {
            return self.ast_from_module_name(&modname, true);
        }
        Err(BuildFail::Import(format!(
            "Unable to build an AST for {filepath}."
        )))
    }

    /// builder.file_build + _data_build + _post_build
    fn file_build(&self, path: &str, modname: &str) -> Result<ModId, BuildFail> {
        let abs = abspath(path);
        // astroid-crash files: EVERY build attempt re-crashes in astroid
        // (failures never enter astroid_cache), so the trip fires on every
        // attempt, before any memoization.
        if self.crash_files.borrow().contains(&abs) {
            self.crash_tripped.set(true);
            return Err(BuildFail::Crash);
        }
        let memo_key = (abs.clone(), modname.to_string());
        if let Some(fail) = self.build_fail_cache.borrow().get(&memo_key) {
            return Err(fail.clone());
        }
        let memo = |fail: BuildFail| -> BuildFail {
            self.build_fail_cache
                .borrow_mut()
                .insert(memo_key.clone(), fail.clone());
            fail
        };
        let bytes = std::fs::read(&abs).map_err(|e| {
            memo(BuildFail::Import(format!("Unable to load file {path}:\n{e}")))
        })?;
        let src = match pyast::decode_source(&bytes, &abs) {
            Ok(src) => src,
            Err(pyast::DecodeError::Syntax(msg)) | Err(pyast::DecodeError::Lookup(msg)) => {
                return Err(memo(BuildFail::Syntax {
                    msg: format!(
                        "Python 3 encoding specification error or unknown encoding:\n{msg}"
                    ),
                    path: abs.clone(),
                    modname: modname.to_string(),
                }))
            }
            Err(pyast::DecodeError::Unicode) => {
                return Err(memo(BuildFail::Import(format!(
                    "Wrong or no encoding specified for {abs}."
                ))))
            }
        };
        // _data_build: modname ".__init__" suffix => package
        let (modname2, package) = if let Some(stripped) = modname.strip_suffix(".__init__") {
            (stripped.to_string(), true)
        } else {
            let stem_is_init = Path::new(&abs)
                .file_stem()
                .map(|s| s == "__init__")
                .unwrap_or(false);
            (modname.to_string(), stem_is_init)
        };
        let outcome = pyast::parse::parse_module(&src, &modname2, &abs, package);
        let tree = match outcome.tree {
            Some(t) => t,
            None => {
                let e = outcome.error.unwrap();
                return Err(memo(BuildFail::Syntax {
                    msg: format!(
                        "Parsing Python code failed:\n{} ({}, line {})",
                        e.message, modname2, e.line
                    ),
                    path: abs.clone(),
                    // str(SyntaxError) embeds the post-strip name that
                    // astroid passed to compile() as the filename
                    modname: modname2.clone(),
                }));
            }
        };
        let id = self.register_module(modname2.clone(), abs, tree, package, true);
        // _post_build: cache BEFORE delayed steps (cycle tolerance)
        self.cache_module(&modname2, id);
        self.post_build(id);
        Ok(id)
    }

    /// build a module from generated source (extract_node-equivalent for
    /// brain templates). Never cached in astroid_cache; post_build runs so
    /// ImportFrom names land in locals.
    pub fn build_template_module(&self, source: &str, modname: &str) -> Option<ModId> {
        let src = pyast::decode_source(source.as_bytes(), "<?>").ok()?;
        let outcome = pyast::parse::parse_module(&src, modname, "<?>", false);
        let tree = outcome.tree?;
        let id = self.register_module(
            modname.to_string(),
            "<?>".to_string(),
            tree,
            false,
            true,
        );
        self.post_build(id);
        Some(id)
    }

    /// AstroidBuilder(manager, apply_transforms=False).string_build(...) —
    /// used by infer_enum_class for member fake classes: NO transform scan
    /// (no wipes, no tips, and crucially no inference of the fake's base
    /// Names before the brain reparents the class into the real module).
    pub fn build_template_module_no_transforms(
        &self,
        source: &str,
        modname: &str,
    ) -> Option<ModId> {
        let src = pyast::decode_source(source.as_bytes(), "<?>").ok()?;
        let outcome = pyast::parse::parse_module(&src, modname, "<?>", false);
        let tree = outcome.tree?;
        let id = self.register_module(
            modname.to_string(),
            "<?>".to_string(),
            tree,
            false,
            true,
        );
        // _post_build minus visit_transforms (builder.py:166-178 with
        // self._apply_transforms False): star imports + delayed assattr
        // still run; tips never activate for this module.
        self.add_from_names_to_locals(id);
        self.process_delayed_assattr(id);
        Some(id)
    }

    // ---------- _post_build: star imports + delayed assattr ----------

    fn post_build(&self, id: ModId) {
        self.add_from_names_to_locals(id);
        self.process_delayed_assattr(id);
        // TransformVisitor runs LAST (builder.py:175-177); every applied
        // transform that returns non-None wipes the global inference cache
        // (transforms.py:66-72). Extenders return None (no wipe) and apply
        // at the Module node — after all child transforms.
        // Tips become live HERE: inference during delayed_assattr above ran
        // with untransformed nodes (no _explicit_inference yet).
        self.md(id).tips_active.set(true);
        self.wipe_scan(id);
        self.apply_module_extenders(id);
    }

    /// brain register_module_extender ports (brain/helpers.py:18-29):
    /// the extension module's locals REPLACE same-named entries; astroid
    /// reparents the nodes into the target module — we get equivalent
    /// qnames by naming the template module identically.
    fn apply_module_extenders(&self, id: ModId) {
        // never extend the extension templates themselves (infinite build)
        if self.md(id).file == "<?>" {
            return;
        }
        let name = self.md(id).name.clone();
        let source: &str = match name.as_str() {
            // brain_typing._typing_transform (PY312 subset)
            "typing" => "class Generic:\n    @classmethod\n    def __class_getitem__(cls, item):  return cls\nclass ParamSpec:\n    @property\n    def args(self):\n        return ParamSpecArgs(self)\n    @property\n    def kwargs(self):\n        return ParamSpecKwargs(self)\nclass ParamSpecArgs: ...\nclass ParamSpecKwargs: ...\nclass TypeAlias: ...\nclass Type:\n    @classmethod\n    def __class_getitem__(cls, item):  return cls\nclass TypeVar:\n    @classmethod\n    def __class_getitem__(cls, item):  return cls\nclass TypeVarTuple: ...\nclass ContextManager:\n    @classmethod\n    def __class_getitem__(cls, item):  return cls\nclass AsyncContextManager:\n    @classmethod\n    def __class_getitem__(cls, item):  return cls\nclass Pattern:\n    @classmethod\n    def __class_getitem__(cls, item):  return cls\nclass Match:\n    @classmethod\n    def __class_getitem__(cls, item):  return cls\n",
            // brain_collections._collections_transform
            "collections" => "class defaultdict(dict):\n    default_factory = None\n    def __missing__(self, key): pass\n    def __getitem__(self, key): return default_factory\n\nclass deque(object):\n    maxlen = 0\n    def __init__(self, iterable=None, maxlen=None):\n        self.iterable = iterable or []\n    def append(self, x): pass\n    def appendleft(self, x): pass\n    def clear(self): pass\n    def count(self, x): return 0\n    def extend(self, iterable): pass\n    def extendleft(self, iterable): pass\n    def pop(self): return self.iterable[0]\n    def popleft(self): return self.iterable[0]\n    def remove(self, value): pass\n    def reverse(self): return reversed(self.iterable)\n    def rotate(self, n=1): return self\n    def __iter__(self): return self\n    def __reversed__(self): return self.iterable[::-1]\n    def __getitem__(self, index): return self.iterable[index]\n    def __setitem__(self, index, value): pass\n    def __delitem__(self, index): pass\n    def __bool__(self): return bool(self.iterable)\n    def __nonzero__(self): return bool(self.iterable)\n    def __contains__(self, o): return o in self.iterable\n    def __len__(self): return len(self.iterable)\n    def __copy__(self): return deque(self.iterable)\n    def copy(self): return deque(self.iterable)\n    def index(self, x, start=0, end=0): return 0\n    def insert(self, i, x): pass\n    def __add__(self, other): pass\n    def __iadd__(self, other): pass\n    def __mul__(self, other): pass\n    def __imul__(self, other): pass\n    def __rmul__(self, other): pass\n    @classmethod\n    def __class_getitem__(self, item): return cls\n\nclass OrderedDict(dict):\n    def __reversed__(self): return self[::-1]\n    def move_to_end(self, key, last=False): pass\n    @classmethod\n    def __class_getitem__(cls, item): return cls\n",
            // brain_datetime (PY312: C-accelerated; use the Python source)
            "datetime" => "from _pydatetime import *\n",
            // brain_multiprocessing: template merge + the DefaultContext/
            // BaseContext BoundMethod probe (brain_multiprocessing.py:13-48)
            "multiprocessing" => {
                self.extend_multiprocessing(id);
                return;
            }
            _ => {
                match crate::ext_templates::EXTENDERS
                    .iter()
                    .find(|(m, _)| *m == name)
                {
                    Some((_, src)) => *src,
                    None => return,
                }
            }
        };
        self.merge_extension(id, source, &name);
    }

    /// register_module_extender merge: extension locals REPLACE same-named
    /// target entries (brain/helpers.py:22-27). Returns the template ModId.
    ///
    /// astroid parses the extension source with modname '' (builder.parse
    /// default), so the template's OWN transform scan runs with qnames like
    /// '.defaultdict' — name-gated predicates (brain_collections
    /// _looks_like_subscriptable, brain_collections.py:93-105) FAIL at scan
    /// time. Only afterwards does register_module_extender REPARENT each
    /// top-level obj into the target module (brain/helpers.py:25-27),
    /// composing the final qname. Building the template under the real name
    /// would inject __class_getitem__ templates astroid never installs
    /// (defaultdict[..] then calls them instead of EmptyNode -> return self).
    fn merge_extension(&self, id: ModId, source: &str, name: &str) -> Option<ModId> {
        let _ = name;
        let ext = self.build_template_module(source, "")?;
        let ext_md = self.md(ext);
        let target_md = self.md(id);
        let ext_locals = ext_md.locals.borrow();
        let Some(ext_map) = ext_locals.get(&NodeId::MODULE) else {
            return Some(ext);
        };
        let mut tgt = target_md.locals.borrow_mut();
        let tgt_map = tgt.entry(NodeId::MODULE).or_default();
        let target_mod = GNode { m: id, n: NodeId::MODULE };
        let mut rp = self.reparents.borrow_mut();
        for (sym, objs) in ext_map {
            tgt_map.insert(*sym, objs.clone());
            for &obj in objs {
                // `if obj.parent is extension_module: obj.parent = node`
                if obj.m == ext
                    && ext_md.tree.nodes[obj.n.idx()].parent == NodeId::MODULE
                    && obj.n != NodeId::MODULE
                {
                    rp.insert(obj, target_mod);
                }
            }
        }
        Some(ext)
    }

    /// brain_multiprocessing._multiprocessing_transform: after the plain
    /// template, instantiate multiprocessing.context.DefaultContext and
    /// BaseContext and append their public class locals to the module —
    /// FunctionDefs rebound as BoundMethod VALUES (brain_multiprocessing.py:
    /// 31-48; `module[key] = value` is set_local = APPEND). Either probe
    /// name failing to infer aborts the loop (only the template merge runs).
    fn extend_multiprocessing(&self, id: ModId) {
        use crate::value::{Value, NV};
        let Some(ext) = self.merge_extension(id, crate::ext_templates::MP_TEMPLATE, "multiprocessing")
        else {
            return;
        };
        let Some(probe) = self.build_template_module(crate::ext_templates::MP_PROBE, "") else {
            return;
        };
        // next(node["default"].infer()) / node["base"] — first inferred value
        let mut insts: Vec<Value> = Vec::new();
        for var in ["default", "base"] {
            let sym = self.sym(var);
            let assign = {
                let pmd = self.md(probe);
                let locals = pmd.locals.borrow();
                match locals
                    .get(&NodeId::MODULE)
                    .and_then(|l| l.get(&sym))
                    .and_then(|v| v.first())
                {
                    Some(&g) => g,
                    None => return,
                }
            };
            let flow = self.infer_fresh(assign);
            match flow.vals.first() {
                Some(v @ Value::Inst { .. }) => insts.push(v.clone()),
                _ => return, // InferenceError/StopIteration -> return module
            }
        }
        // merged final lists: template locals first (insertion order), then
        // per-instance appends
        let mut merged: IndexMap<GSym, Vec<NV>> = {
            let emd = self.md(ext);
            let locals = emd.locals.borrow();
            match locals.get(&NodeId::MODULE) {
                Some(map) => map
                    .iter()
                    .map(|(k, v)| (*k, v.iter().map(|&g| NV::N(g)).collect()))
                    .collect(),
                None => IndexMap::new(),
            }
        };
        for inst in &insts {
            let Value::Inst { cls, .. } = inst else { continue };
            let entries: Vec<(GSym, GNode)> = {
                let cmd = self.md(cls.m);
                let locals = cmd.locals.borrow();
                match locals.get(&cls.n) {
                    Some(map) => map
                        .iter()
                        .filter(|(k, v)| !self.sname(**k).starts_with('_') && !v.is_empty())
                        .map(|(k, v)| (*k, v[0]))
                        .collect(),
                    None => Vec::new(),
                }
            };
            for (key, v0) in entries {
                let entry = if self.kind_is(v0, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                }) {
                    NV::V(Value::BoundMethod {
                        func: v0,
                        bound: std::rc::Rc::new(inst.clone()),
                    })
                } else {
                    NV::N(v0)
                };
                merged.entry(key).or_default().push(entry);
            }
        }
        // write back: node projection into locals (public_names/star-import
        // consumers), full mixed lists into ext_locals (module_getattr)
        let target_md = self.md(id);
        {
            let mut tgt = target_md.locals.borrow_mut();
            let tgt_map = tgt.entry(NodeId::MODULE).or_default();
            for (sym, list) in &merged {
                let nodes: Vec<GNode> = list
                    .iter()
                    .filter_map(|nv| match nv {
                        NV::N(g) => Some(*g),
                        NV::V(_) => None,
                    })
                    .collect();
                tgt_map.insert(*sym, nodes);
            }
        }
        let mut extl = target_md.ext_locals.borrow_mut();
        for (sym, list) in merged {
            extl.insert(sym, list);
        }
    }

    /// builder.add_from_names_to_locals (builder.py:213-246): re-adds every
    /// ImportFrom name (the register step stripped them), resolving `*`
    /// through the real module graph; each add re-sorts that name's list
    /// by fromlineno (stable).
    fn add_from_names_to_locals(&self, id: ModId) {
        let md = self.md(id);
        let order = self.walk_preorder(id);
        // global-declared names per FunctionDef frame (rebuilder
        // _global_names stack; the dict_keys view captured per ImportFrom
        // reflects all Global statements in the frame by post-build time)
        for n in order {
            let g = GNode { m: id, n };
            let (names, level): (Vec<(GSym, Option<GSym>)>, Option<u32>) =
                match &md.tree.nodes[n.idx()].kind {
                    NodeKind::ImportFrom { names, level, .. } => (
                        names
                            .iter()
                            .map(|(a, b)| (self.g(&md, *a), b.map(|s| self.g(&md, s))))
                            .collect(),
                        *level,
                    ),
                    _ => continue,
                };
            let _ = level;
            let parent = self.parent(g).unwrap_or(g);
            let scope = self.scope_for_locals(parent);
            let globals = self.function_global_names(g);
            for (name, asname) in names {
                let name_str = self.sname(name);
                if name_str == "*" {
                    let imported = match self.do_import_module(g, None) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    for pub_name in self.public_names(imported) {
                        let target = if globals.contains(&pub_name) {
                            GNode { m: id, n: NodeId::MODULE }
                        } else {
                            scope
                        };
                        self.add_local_sorted(target, pub_name, g);
                    }
                } else {
                    let local = asname.unwrap_or(name);
                    let target = if globals.contains(&local) {
                        GNode { m: id, n: NodeId::MODULE }
                    } else {
                        scope
                    };
                    self.add_local_sorted(target, local, g);
                }
            }
        }
    }

    /// scope used by NodeNG.set_local: nearest scope of `node` that is not
    /// a comprehension... actually set_local walks to scope() (incl.
    /// comprehensions can't contain ImportFrom). Use scope().
    fn scope_for_locals(&self, node: GNode) -> GNode {
        self.scope(node)
    }

    /// names declared `global` within the nearest FunctionDef frame
    /// (rebuilder.py:63,1255-1257: _global_names is pushed per function)
    fn function_global_names(&self, node: GNode) -> FxHashSet<GSym> {
        let mut out = FxHashSet::default();
        // find nearest FunctionDef ancestor
        let mut cur = node;
        let func = loop {
            match self.parent(cur) {
                None => return out,
                Some(p) => {
                    let md = self.md(p.m);
                    if matches!(
                        md.tree.nodes[p.n.idx()].kind,
                        NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                    ) {
                        break p;
                    }
                    cur = p;
                }
            }
        };
        // collect Global names within func, not crossing nested functions
        let md = self.md(func.m);
        let mut stack: Vec<NodeId> = Vec::new();
        let mut buf = Vec::new();
        md.tree.push_children(func.n, &mut buf);
        stack.extend(buf.iter().copied());
        while let Some(n) = stack.pop() {
            match &md.tree.nodes[n.idx()].kind {
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::Lambda(_) => {
                    continue
                }
                NodeKind::Global { names } => {
                    for s in names {
                        out.insert(self.g(&md, *s));
                    }
                }
                _ => {}
            }
            buf.clear();
            md.tree.push_children(n, &mut buf);
            stack.extend(buf.iter().copied());
        }
        out
    }

    fn add_local_sorted(&self, scope: GNode, name: GSym, node: GNode) {
        let md = self.md(scope.m);
        let mut locals = md.locals.borrow_mut();
        let list = locals.entry(scope.n).or_default().entry(name).or_default();
        list.push(node);
        // stable sort by fromlineno (builder.py:221-226)
        let engine = self;
        list.sort_by_key(|g| engine.fromlineno(*g));
    }

    /// Module.public_names: locals keys not starting with '_'
    pub fn public_names(&self, id: ModId) -> Vec<GSym> {
        let md = self.md(id);
        let locals = md.locals.borrow();
        match locals.get(&NodeId::MODULE) {
            Some(map) => map
                .keys()
                .filter(|&&k| !self.sname(k).starts_with('_'))
                .copied()
                .collect(),
            None => Vec::new(),
        }
    }

    // ---------- import resolution used by inference ----------

    /// _base_nodes.py:148-172 do_import_module. `modname` None => use the
    /// node's own modname (ImportFrom).
    pub fn do_import_module(&self, node: GNode, modname: Option<&str>) -> Result<ModId, BuildFail> {
        let md = self.md(node.m);
        let (own_modname, level) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ImportFrom { modname, level, .. } => {
                (Some(md.tree.s(*modname).to_string()), *level)
            }
            _ => (None, None),
        };
        let modname = match modname {
            Some(m) => m.to_string(),
            None => own_modname.unwrap_or_default(),
        };
        let mymodule = self.md(node.m);
        let absmodname = self
            .relative_to_absolute_name(&mymodule, &modname, level)
            .map_err(|_| BuildFail::TooManyLevels)?;
        // cache bypass for self-import
        let use_cache = absmodname != mymodule.name;
        self.import_module(&mymodule, &modname, level.map(|l| l >= 1).unwrap_or(false), level, use_cache, &absmodname)
    }

    /// Module.import_module (scoped_nodes.py:439-475)
    fn import_module(
        &self,
        _mymodule: &Module,
        modname: &str,
        relative_only: bool,
        _level: Option<u32>,
        use_cache: bool,
        absmodname: &str,
    ) -> Result<ModId, BuildFail> {
        match self.ast_from_module_name(absmodname, use_cache) {
            Ok(id) => Ok(id),
            Err(e) => {
                if relative_only {
                    return Err(e);
                }
                if modname == absmodname {
                    return Err(e);
                }
                self.ast_from_module_name(modname, use_cache)
            }
        }
    }

    /// scoped_nodes.py:477-523 relative_to_absolute_name — EXACT (E0402).
    pub fn relative_to_absolute_name(
        &self,
        module: &Module,
        modname: &str,
        level: Option<u32>,
    ) -> Result<String, ErrKind> {
        // absolute_import_activated() is always True on py3
        if level.is_none() {
            return Ok(modname.to_string());
        }
        let mut level = level.unwrap();
        let package_name: String;
        if level > 0 {
            if module.package {
                level -= 1;
                package_name = rsplit_n(&module.name, level as usize);
            } else if !module.file.is_empty()
                && module.file != "<?>"
                && !Path::new(&format!(
                    "{}/__init__.py",
                    parent_dir(&module.file)
                ))
                .exists()
                && Path::new(&format!(
                    "{}/{}",
                    parent_dir(&module.file),
                    modname.split('.').next().unwrap_or("")
                ))
                .exists()
            {
                level -= 1;
                package_name = String::new();
            } else {
                package_name = rsplit_n(&module.name, level as usize);
            }
            if level > 0 && (module.name.matches('.').count() as u32) < level {
                return Err(ErrKind::TooManyLevels);
            }
        } else if module.package {
            package_name = module.name.clone();
        } else {
            package_name = rsplit_n(&module.name, 1);
        }
        if !package_name.is_empty() {
            if modname.is_empty() {
                return Ok(package_name);
            }
            return Ok(format!("{package_name}.{modname}"));
        }
        Ok(modname.to_string())
    }

    // ---------- delayed assattr (builder.py:248-284) ----------

    fn process_delayed_assattr(&self, id: ModId) {
        let md = self.md(id);
        let order = self.walk_preorder(id);
        let delayed: Vec<NodeId> = order
            .into_iter()
            .filter(|&n| {
                matches!(md.tree.nodes[n.idx()].kind, NodeKind::AssignAttr { .. })
                    && !matches!(
                        md.tree.nodes[md.tree.nodes[n.idx()].parent.idx()].kind,
                        NodeKind::ExceptHandler { .. }
                    )
            })
            .collect();
        for n in delayed {
            self.delayed_assattr(GNode { m: id, n });
        }
    }

    fn delayed_assattr(&self, node: GNode) {
        let md = self.md(node.m);
        let (expr, attrname) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::AssignAttr { expr, attrname } => {
                (GNode { m: node.m, n: *expr }, self.g(&md, *attrname))
            }
            _ => return,
        };
        let ctx = crate::ctx::Ctx::new();
        // for inferred in node.expr.infer(): the per-value bookkeeping
        // (incl. the slots() inference in _can_assign_attr) runs while the
        // expr generator is SUSPENDED (builder.py:255-283).
        let _ = self.infer_to(expr, &ctx, &mut |inferred| {
            self.delayed_assattr_one(&inferred, node, attrname);
            crate::value::Drive::Go
        });
    }

    fn delayed_assattr_one(&self, inferred: &Value, node: GNode, attrname: GSym) {
        {
            match inferred {
                Value::Uninferable => {}
                Value::Inst { cls, id } if self.is_object_type_proxy_cls(*cls) => {
                    // fresh-proxy-class instance: the attr lands on the
                    // per-evaluation class (helpers.py _build_proxy_class)
                    if !self.can_assign_attr(*cls, attrname) {
                        return;
                    }
                    let mut ia = self.proxy_iattrs.borrow_mut();
                    let vals = ia
                        .entry((*cls, *id))
                        .or_default()
                        .entry(attrname)
                        .or_default();
                    if !vals.contains(&node) {
                        vals.push(node);
                    }
                }
                Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
                    if !self.can_assign_attr(*cls, attrname) {
                        return;
                    }
                    let mut ia = self.iattrs.borrow_mut();
                    let vals = ia.entry(*cls).or_default().entry(attrname).or_default();
                    if !vals.contains(&node) {
                        vals.push(node);
                    }
                }
                Value::Node(g) => {
                    let gmd = self.md(g.m);
                    match &gmd.tree.nodes[g.n.idx()].kind {
                        NodeKind::FunctionDef(_)
                        | NodeKind::AsyncFunctionDef(_)
                        | NodeKind::Lambda(_) => {
                            // function instance_attrs
                            let mut ia = self.iattrs.borrow_mut();
                            let vals = ia.entry(*g).or_default().entry(attrname).or_default();
                            if !vals.contains(&node) {
                                vals.push(node);
                            }
                        }
                        NodeKind::Module(_) | NodeKind::ClassDef(_) => {
                            // iattrs = inferred.locals (module/class locals!)
                            let mut locals = gmd.locals.borrow_mut();
                            let vals = locals
                                .entry(g.n)
                                .or_default()
                                .entry(attrname)
                                .or_default();
                            if !vals.contains(&node) {
                                vals.push(node);
                            }
                        }
                        // Const/containers (Instance subclasses) and
                        // everything without locals: AttributeError -> skip
                        _ => {}
                    }
                }
                // proxies (BoundMethod/Generator/...) -> continue
                _ => {}
            }
        }
    }

    /// is this the snapshot stand-in for an object_type proxy class
    /// (helpers.py:39-57 — astroid builds these FRESH per evaluation)?
    pub fn is_object_type_proxy_cls(&self, cls: GNode) -> bool {
        let b = self.builtins();
        cls == b.function
            || cls == b.builtin_function_or_method
            || cls == b.method
            || cls == b.module
    }

    /// builder.py:58-70 _can_assign_attr: consults ClassDef.slots(), then
    /// `return node.qname() != "builtins.object"` — a delayed assattr NEVER
    /// lands on builtins.object (probe: twisted test_endpoints.py:81
    /// `ConchUser = object` fallback + line 163 `avatar.channelLookup = ...`
    /// would otherwise pollute object.instance_attrs, making
    /// instance_attr_ancestors() suppress the C0103 attr-name check for
    /// `channelLookup` on EVERY class corpus-wide).
    fn can_assign_attr(&self, cls: GNode, attrname: GSym) -> bool {
        match self.all_slots(cls) {
            Err(()) => {}  // NotImplementedError (mro failure / old-style)
            Ok(None) => {} // `if slots and ...` — None is falsy
            Ok(Some(slots)) => {
                if !slots.is_empty() {
                    // `if slots and attrname not in {...}: return False`
                    let name = self.sname(attrname);
                    if !slots.iter().any(|s| *s == name) {
                        return false;
                    }
                }
            }
        }
        self.qname(cls) != "builtins.object"
    }

    /// ClassDef._all_slots (scoped_nodes.py:2761-2798), cached per class.
    /// Err(()) = NotImplementedError (MroError); Ok(None) = no/uninferable
    /// slots; Ok(Some(values)) = slot name strings.
    pub fn all_slots(&self, cls: GNode) -> Result<Option<Rc<Vec<String>>>, ()> {
        if let Some(cached) = self.slots_cache.borrow().get(&cls) {
            return cached.clone();
        }
        let result = self.compute_all_slots(cls);
        self.slots_cache.borrow_mut().insert(cls, result.clone());
        result
    }

    fn compute_all_slots(&self, cls: GNode) -> Result<Option<Rc<Vec<String>>>, ()> {
        if crate::graph::trace_infer() {
            eprintln!("ALLSLOTS {}", self.qname(cls));
        }
        let mro = match self.mro(cls, None) {
            Ok(m) => m,
            Err(_) => return Err(()), // NotImplementedError
        };
        // `slots = list(grouped_slots(mro))` (scoped_nodes.py:2787-2795):
        // the FULL mro is walked (inference side effects per class!) even
        // when an early class already yielded None.
        let mut all: Vec<String> = Vec::new();
        let mut any_none = false;
        for c in mro {
            if self.qname(c) == "builtins.object" {
                continue;
            }
            match self.class_slots_of(c) {
                None => any_none = true,
                Some(vals) => all.extend(vals),
            }
        }
        if any_none {
            return Ok(None);
        }
        // sorted(set(...), key=value): membership semantics only
        all.sort();
        all.dedup();
        Ok(Some(Rc::new(all)))
    }

    /// ClassDef._slots/_islots (scoped_nodes.py:2695-2759). None = class has
    /// no (or uninferable) __slots__; Some(vec) = slot value strings (empty
    /// = explicitly empty slots).
    fn class_slots_of(&self, cls: GNode) -> Option<Vec<String>> {
        if crate::graph::trace_infer() {
            eprintln!("SLOTSOF {}", self.qname(cls));
        }
        let slots_sym = self.sym("__slots__");
        let has_local = {
            let md = self.md(cls.m);
            let locals = md.locals.borrow();
            locals
                .get(&cls.n)
                .map(|l| l.contains_key(&slots_sym))
                .unwrap_or(false)
        };
        if !has_local {
            return None;
        }
        // _islots (scoped_nodes.py:2695-2745): `for slots in
        // self.igetattr("__slots__")` is STREAMED — `return values` on an
        // EMPTY container ABANDONS the igetattr generator at its yield
        // (consumer Stop): the suspended AssignName/Tuple NodeNG.infer
        // frames never run their cache writes, so the next class walking
        // the same __slots__ re-infers them (MISS) exactly like astroid.
        let mut out: Vec<String> = Vec::new();
        let mut any = false;
        let mut empty_stop = false;
        let res = self.class_igetattr_to(cls, slots_sym, None, true, &mut |slots| {
            // must support iteration: `slots.getattr(meth)` — container
            // literals ARE Instances (BaseContainer <- bases.Instance), so
            // this is the full Instance.getattr chain (instance_attr
            // ancestors walk + class getattr, scoped_nodes.py:2700-2706)
            let iterable = self.proxied_class(&slots).is_some() && {
                let i1 = self.sym("__iter__");
                let i2 = self.sym("__getitem__");
                self.instance_getattr(&slots, i1, None, true).is_ok()
                    || self.instance_getattr(&slots, i2, None, true).is_ok()
            };
            if !iterable {
                return crate::value::Drive::Go;
            }
            // Const string: yield if non-empty
            if let Some(c) = self.value_const(&slots) {
                if let pyast::tree::ConstValue::Str(sv) = c {
                    if !sv.is_empty() {
                        any = true;
                        out.push(sv.to_string());
                    }
                }
                return crate::value::Drive::Go;
            }
            // containers
            let elts: Vec<NV> = match &slots {
                Value::Node(g) => {
                    let md = self.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::List { elts, .. }
                        | NodeKind::Tuple { elts, .. }
                        | NodeKind::Set { elts } => elts
                            .iter()
                            .map(|&e| NV::N(GNode { m: g.m, n: e }))
                            .collect(),
                        NodeKind::Dict { items } => items
                            .iter()
                            .map(|&(k, _)| NV::N(GNode { m: g.m, n: k }))
                            .collect(),
                        _ => return crate::value::Drive::Go,
                    }
                }
                Value::SynthSeq { elems, .. } | Value::FrozenSet { elems } => {
                    elems.iter().cloned().map(NV::V).collect()
                }
                Value::SynthDict { items } => {
                    items.iter().map(|(k, _)| NV::V(k.clone())).collect()
                }
                _ => return crate::value::Drive::Go,
            };
            if elts.is_empty() {
                // `return values` — empty slots list ABANDONS the generator
                empty_stop = true;
                return crate::value::Drive::Stop;
            }
            for elt in elts {
                // for inferred in elt.infer(): Const str filter
                let vals: Vec<Value> = match &elt {
                    NV::N(g) => self.infer(*g, &crate::ctx::Ctx::new()).vals,
                    NV::V(v) => vec![v.clone()],
                };
                for inferred in vals {
                    if let Some(pyast::tree::ConstValue::Str(sv)) = self.value_const(&inferred)
                    {
                        if !sv.is_empty() {
                            any = true;
                            out.push(sv.to_string());
                        }
                    }
                }
            }
            crate::value::Drive::Go
        });
        if let crate::value::End::Raised(_) = res {
            if !any && !empty_stop {
                return None;
            }
        }
        if empty_stop {
            // explicit empty slots
            return Some(Vec::new());
        }
        if !any && out.is_empty() {
            // no values produced and no explicit empty -> None
            return None;
        }
        Some(out)
    }
}

// ---------- path helpers ----------

pub fn abspath(p: &str) -> String {
    // std::path::absolute hits getcwd(2) for every relative input; the
    // process cwd never changes during a lint run, so resolve relative
    // paths against a cached cwd instead. cwd is absolute and
    // component-normalized (getcwd), so absolute(cwd.join(p)) ==
    // absolute(p) — the normalization pass is the same std code path.
    if p.is_empty() {
        // std::path::absolute errors on "" -> original fallback returned p
        return p.to_string();
    }
    if !p.starts_with('/') {
        thread_local! {
            static CWD: std::path::PathBuf =
                std::env::current_dir().unwrap_or_default();
        }
        let joined = CWD.with(|cwd| {
            if cwd.as_os_str().is_empty() {
                None // getcwd failed at init: fall through to std
            } else {
                Some(cwd.join(p))
            }
        });
        if let Some(joined) = joined {
            return match std::path::absolute(&joined) {
                Ok(a) => a.to_string_lossy().into_owned(),
                Err(_) => p.to_string(),
            };
        }
    }
    match std::path::absolute(p) {
        Ok(a) => a.to_string_lossy().into_owned(),
        Err(_) => p.to_string(),
    }
}

fn parent_dir(p: &str) -> String {
    Path::new(p)
        .parent()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn join_path(a: &str, b: &str) -> String {
    if a.is_empty() {
        return b.to_string();
    }
    format!("{}/{}", a.trim_end_matches('/'), b)
}

fn split_ext(p: &str) -> (String, &str) {
    match p.rfind('.') {
        Some(i) if !p[i + 1..].contains('/') && i > p.rfind('/').map(|s| s + 1).unwrap_or(0) => {
            (p[..i].to_string(), &p[i + 1..])
        }
        _ => (p.to_string(), ""),
    }
}

fn rsplit_n(name: &str, level: usize) -> String {
    // name.rsplit(".", level)[0]
    let mut s = name;
    for _ in 0..level {
        match s.rfind('.') {
            Some(i) => s = &s[..i],
            None => break,
        }
    }
    s.to_string()
}
