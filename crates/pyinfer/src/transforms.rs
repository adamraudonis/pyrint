//! TransformVisitor wipe-scan port (astroid/transforms.py:60-77).
//!
//! Every transform whose function returns non-None calls
//! `_invalidate_cache()` — clearing the ENTIRE global inference cache
//! (context.py:26-27). All `inference_tip(...)` transforms return the node,
//! as do a handful of raw transforms (infer_enum_class, six metaclass
//! rewrites, gi require_version, hypothesis composite, boto3). Because
//! modules are built lazily mid-inference, these wipes punctuate the cache
//! lifetime and are LOAD-BEARING for nodes_inferred counter parity (cached
//! replays don't bump the counter).
//!
//! This scan runs at the end of module build (builder.py:175-177 — after
//! delayed_assattr) in TransformVisitor's bottom-up order, evaluating the
//! registered predicates (register_all_brains order, brain/helpers.py) and
//! clearing our inf_cache per application. Predicates that perform
//! inference in astroid (dataclass decorator checks, _is_enum_subclass,
//! _is_str_format_call's safe_infer, pathlib parents) do the same here so
//! their cache side effects land at the right time.
//!
//! NOT ported (predicates that cannot match in the pinned corpora or whose
//! transforms return None — no cache invalidation): attrs, collections
//! __class_getitem__, crypt/ctypes/curses/datetime/dateutil/hashlib/http/
//! mechanize/multiprocessing/numpy-extender/pytest/qt/responses/scipy/
//! signal/sqlalchemy/ssl/subprocess/threading/unittest module extenders
//! (register_module_extender's transform returns None — transforms.py:66-72
//! only invalidates on non-None), functools lru_cache, io buffered, uuid,
//! argparse Namespace IS ported, boto3 qname check ported.

use pyast::tree::{Ctx as ExprCtx, NodeKind};
use pyast::NodeId;

use crate::ctx::Ctx;
use crate::graph::Engine;
use crate::value::{GNode, ModId, Value};

const BUILTIN_TIP_NAMES: [&str; 18] = [
    "bool",
    "super",
    "callable",
    "property",
    "getattr",
    "hasattr",
    "tuple",
    "set",
    "list",
    "dict",
    "frozenset",
    "type",
    "slice",
    "isinstance",
    "issubclass",
    "len",
    "str",
    "int",
];

const TYPING_MEMBERS: [&str; 99] = [
    "AbstractSet",
    "Annotated",
    "Any",
    "AnyStr",
    "AsyncContextManager",
    "AsyncGenerator",
    "AsyncIterable",
    "AsyncIterator",
    "Awaitable",
    "BinaryIO",
    "ByteString",
    "Callable",
    "ChainMap",
    "ClassVar",
    "Collection",
    "Concatenate",
    "Container",
    "ContextManager",
    "Coroutine",
    "Counter",
    "DefaultDict",
    "Deque",
    "Dict",
    "Final",
    "ForwardRef",
    "FrozenSet",
    "Generator",
    "Generic",
    "Hashable",
    "IO",
    "ItemsView",
    "Iterable",
    "Iterator",
    "KeysView",
    "List",
    "Literal",
    "LiteralString",
    "Mapping",
    "MappingView",
    "Match",
    "MutableMapping",
    "MutableSequence",
    "MutableSet",
    "NamedTuple",
    "Never",
    "NewType",
    "NoReturn",
    "NotRequired",
    "Optional",
    "OrderedDict",
    "ParamSpec",
    "ParamSpecArgs",
    "ParamSpecKwargs",
    "Pattern",
    "Protocol",
    "Required",
    "Reversible",
    "Self",
    "Sequence",
    "Set",
    "Sized",
    "SupportsAbs",
    "SupportsBytes",
    "SupportsComplex",
    "SupportsFloat",
    "SupportsIndex",
    "SupportsInt",
    "SupportsRound",
    "TYPE_CHECKING",
    "Text",
    "TextIO",
    "Tuple",
    "Type",
    "TypeAlias",
    "TypeAliasType",
    "TypeGuard",
    "TypeVar",
    "TypeVarTuple",
    "TypedDict",
    "Union",
    "Unpack",
    "ValuesView",
    "assert_never",
    "assert_type",
    "cast",
    "clear_overloads",
    "dataclass_transform",
    "final",
    "get_args",
    "get_origin",
    "get_overloads",
    "get_type_hints",
    "is_typeddict",
    "no_type_check",
    "no_type_check_decorator",
    "overload",
    "override",
    "reveal_type",
    "runtime_checkable",
];

const NUMPY_FUNCTION_BASE: [&str; 3] = ["linspace", "logspace", "geomspace"];
const NUMPY_MULTIARRAY: [&str; 20] = [
    "array",
    "dot",
    "empty_like",
    "concatenate",
    "where",
    "empty",
    "bincount",
    "busday_count",
    "busday_offset",
    "can_cast",
    "copyto",
    "datetime_as_string",
    "is_busday",
    "lexsort",
    "may_share_memory",
    "packbits",
    "shares_memory",
    "unpackbits",
    "unravel_index",
    "zeros",
];
const DATACLASS_MODULES: [&str; 3] = ["dataclasses", "marshmallow_dataclass", "pydantic.dataclasses"];
const COMPOSITE_NAMES: [&str; 4] = [
    "composite",
    "st.composite",
    "strategies.composite",
    "hypothesis.strategies.composite",
];

thread_local! {
    /// (module, line, kind) of the node currently being transform-scanned —
    /// debug context for WIPE traces
    static CUR_SCAN: std::cell::RefCell<(ModId, u32, u8)> =
        const { std::cell::RefCell::new((ModId(0), 0, 0)) };
}

fn m_name(e: &Engine, m: ModId) -> String {
    e.md(m).name.clone()
}

impl Engine {
    /// transforms.py _invalidate_cache: clears ONLY the global inference
    /// cache (lookup lru / tip caches survive).
    fn wipe(&self) {
        crate::graph::bump_wipe_count();
        if crate::graph::trace_infer() {
            CUR_SCAN.with(|c| {
                let (m, l, k) = *c.borrow();
                eprintln!("WIPE {} line={} kind={}", m_name(self, m), l, k);
            });
        }
        self.inf_cache.borrow_mut().clear();
        self.synth_hop_cache.borrow_mut().clear();
        self.synth_hop_trunc.borrow_mut().clear();
    }

    /// Bottom-up transform application over a freshly built module.
    pub fn wipe_scan_set_cur(&self, mid: ModId, line: u32, kind: u8) {
        CUR_SCAN.with(|c| *c.borrow_mut() = (mid, line, kind));
    }

    pub fn wipe_scan(&self, mid: ModId) {
        // bootstrap (builtins snapshot) loads before BuiltinRefs exist; the
        // cache is empty then and astroid's bootstrap is special-cased anyway
        if self.b.borrow().is_none() {
            return;
        }
        let order = self.postorder(mid);
        for n in order {
            let g = GNode { m: mid, n };
            let kind_tag = {
                let md = self.md(mid);
                match &md.tree.nodes[n.idx()].kind {
                    NodeKind::Call { .. } => 1,
                    NodeKind::ClassDef(_) => 2,
                    NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => 3,
                    NodeKind::Name { .. } => 4,
                    NodeKind::Attribute { .. } => 5,
                    NodeKind::Subscript { .. } => 6,
                    NodeKind::Unknown => 7,
                    _ => 0,
                }
            };
            if crate::graph::trace_infer() {
                let line = self.md(mid).tree.nodes[n.idx()].fromlineno as u32;
                self.wipe_scan_set_cur(mid, line, kind_tag);
                if kind_tag == 2 || kind_tag == 3 {
                    let nm = self.node_name(g).unwrap_or_default();
                    eprintln!("SCAN {} {} @{}:{}", if kind_tag == 2 { "ClassDef" } else { "FunctionDef" }, nm, self.md(mid).name, line);
                }
            }
            match kind_tag {
                1 => self.scan_call(g),
                2 => self.scan_classdef(g),
                3 => self.scan_functiondef(g),
                4 => self.scan_name(g),
                5 => self.scan_attribute(g),
                6 => self.scan_subscript(g),
                7 => self.scan_unknown(g),
                _ => {}
            }
        }
    }

    fn postorder(&self, mid: ModId) -> Vec<NodeId> {
        let md = self.md(mid);
        let mut out = Vec::with_capacity(md.tree.nodes.len());
        // iterative postorder preserving get_children order
        let mut stack: Vec<(NodeId, bool)> = vec![(NodeId::MODULE, false)];
        let mut buf = Vec::new();
        while let Some((n, expanded)) = stack.pop() {
            if expanded {
                out.push(n);
                continue;
            }
            stack.push((n, true));
            buf.clear();
            md.tree.push_children(n, &mut buf);
            for &c in buf.iter().rev() {
                stack.push((c, false));
            }
        }
        out
    }

    // ---------- helpers ----------

    fn call_func(&self, call: GNode) -> Option<GNode> {
        let md = self.md(call.m);
        match &md.tree.nodes[call.n.idx()].kind {
            NodeKind::Call { func, .. } => Some(GNode { m: call.m, n: *func }),
            _ => None,
        }
    }

    fn call_args(&self, call: GNode) -> Vec<GNode> {
        let md = self.md(call.m);
        match &md.tree.nodes[call.n.idx()].kind {
            NodeKind::Call { args, .. } => {
                args.iter().map(|&a| GNode { m: call.m, n: a }).collect()
            }
            _ => Vec::new(),
        }
    }

    fn name_of(&self, g: GNode) -> Option<String> {
        let md = self.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
            _ => None,
        }
    }

    fn attr_of(&self, g: GNode) -> Option<(GNode, String)> {
        let md = self.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Attribute { expr, attrname, .. } => Some((
                GNode { m: g.m, n: *expr },
                md.tree.s(*attrname).to_string(),
            )),
            _ => None,
        }
    }

    /// `func.as_string()` for the dotted forms predicates compare against
    /// (Name / Attribute chains only).
    fn dotted_string(&self, g: GNode) -> Option<String> {
        let md = self.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
            NodeKind::Attribute { expr, attrname, .. } => {
                let base = self.dotted_string(GNode { m: g.m, n: *expr })?;
                Some(format!("{}.{}", base, md.tree.s(*attrname)))
            }
            _ => None,
        }
    }

    /// `node.func` name for "looks like" predicates: Name name or Attribute
    /// attrname (brain_namedtuple_enum._looks_like and friends).
    fn func_simple_name(&self, call: GNode) -> Option<String> {
        let f = self.call_func(call)?;
        let md = self.md(f.m);
        match &md.tree.nodes[f.n.idx()].kind {
            NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
            NodeKind::Attribute { attrname, .. } => Some(md.tree.s(*attrname).to_string()),
            _ => None,
        }
    }

    fn decorator_nodes(&self, g: GNode) -> Vec<GNode> {
        let md = self.md(g.m);
        let dec = match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::ClassDef(d) => d.decorators,
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.decorators,
            _ => None,
        };
        let Some(dec) = dec else { return Vec::new() };
        match &md.tree.nodes[dec.idx()].kind {
            NodeKind::Decorators { nodes } => {
                nodes.iter().map(|&n| GNode { m: g.m, n }).collect()
            }
            _ => Vec::new(),
        }
    }

    /// brain_dataclasses._looks_like_dataclass_decorator — INFERENCE FIRST
    /// (next(node.infer())), name fallback only when Uninferable.
    fn looks_like_dataclass_decorator(&self, dec: GNode) -> bool {
        let target = match self.call_func(dec) {
            Some(f) => f, // decorator with arguments
            None => dec,
        };
        let inferred = self.infer_first(target, None).ok();
        match inferred {
            Some(v) if !v.is_uninferable() => {
                if let Value::Node(g) = &v {
                    let md = self.md(g.m);
                    if let NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) =
                        &md.tree.nodes[g.n.idx()].kind
                    {
                        let name = md.tree.s(d.name).to_string();
                        return name == "dataclass"
                            && DATACLASS_MODULES.contains(&md.name.as_str());
                    }
                }
                false
            }
            _ => {
                // Uninferable / InferenceError -> name match
                let md = self.md(target.m);
                match &md.tree.nodes[target.n.idx()].kind {
                    NodeKind::Name { name } => md.tree.s(*name) == "dataclass",
                    NodeKind::Attribute { attrname, .. } => md.tree.s(*attrname) == "dataclass",
                    _ => false,
                }
            }
        }
    }

    pub(crate) fn is_decorated_with_dataclass(&self, cls: GNode) -> bool {
        self.decorator_nodes(cls)
            .into_iter()
            .any(|d| self.looks_like_dataclass_decorator(d))
    }

    /// brain_dataclasses.dataclass_transform (instance_attrs part): every
    /// dataclass AnnAssign target gets instance_attrs[name] = [Unknown]
    /// (infers to Uninferable). The attribute filters run inference at
    /// transform time (_is_class_var: next(annotation.infer());
    /// _is_keyword_only_sentinel: safe_infer).
    fn apply_dataclass_transform(&self, cls: GNode) {
        let body: Vec<NodeId> = {
            let md = self.md(cls.m);
            match &md.tree.nodes[cls.n.idx()].kind {
                NodeKind::ClassDef(d) => d.body.clone(),
                _ => return,
            }
        };
        for stmt in body {
            let (target, annotation) = {
                let md = self.md(cls.m);
                match &md.tree.nodes[stmt.idx()].kind {
                    NodeKind::AnnAssign { target, annotation, .. } => {
                        match &md.tree.nodes[target.idx()].kind {
                            NodeKind::AssignName { name } => {
                                (md.tree.s(*name).to_string(), *annotation)
                            }
                            _ => continue,
                        }
                    }
                    _ => continue,
                }
            };
            let ann = GNode { m: cls.m, n: annotation };
            // _is_class_var: next(node.infer()) — single pull, name check
            let is_class_var = match self.infer_first(ann, None) {
                // getattr(inferred, "name", "") == "ClassVar" — the name
                // attribute PROXIES through Instances (Instance.__getattr__
                // -> _proxied.name), so an Instance of ClassVar matches too
                Ok(v) => self.getattr_name_of(&v).as_deref() == Some("ClassVar"),
                _ => false,
            };
            if is_class_var {
                continue;
            }
            // _is_keyword_only_sentinel: safe_infer + qname check
            let kw_only = matches!(
                self.safe_infer(ann, &Ctx::new()),
                Some(Value::Inst { cls: c, .. }) if self.qname(c) == "dataclasses._KW_ONLY_TYPE"
            );
            if kw_only {
                continue;
            }
            // _is_init_var (brain_dataclasses.py:565-572): next(node.infer())
            // — pull C, a CACHE HIT after the safe_infer above wrote the
            // annotation's value cache. InitVar fields are SKIPPED in the
            // init=False pass (_get_dataclass_attributes:130-132).
            if self.is_init_var(ann) {
                continue;
            }
            let unknown = self.alloc_placeholders(1)[0];
            // brain_dataclasses.py:64-68: Unknown(parent=assign_node) — the
            // AnnAssign field statement. The reparent makes unknown.root()
            // resolve to the REAL module so the R0902 filter
            // (v[0].root() is root, design_analysis.py:474) keeps these
            // synthetic attributes.
            self.reparents
                .borrow_mut()
                .insert(unknown, GNode { m: cls.m, n: stmt });
            self.dataclass_attrs
                .borrow_mut()
                .insert(unknown, GNode { m: cls.m, n: stmt });
            let sym = self.sym(&target);
            {
                let mut ia = self.iattrs.borrow_mut();
                ia.entry(cls).or_default().insert(sym, vec![unknown]);
            }
            // rhs_node = AstroidManager().visit_transforms(rhs_node)
            // (brain_dataclasses.py:69): the Unknown-node inference_tip
            // predicate _looks_like_dataclass_attribute re-checks
            // is_decorated_with_dataclass(scope) — one decorator pull —
            // and the applied tip transform returns the node -> WIPE
            // (transforms.py:66-72), once per field.
            let _ = self.is_decorated_with_dataclass(cls);
            self.wipe();
        }
    }

    /// brain_dataclasses._is_init_var: single next(node.infer()) pull;
    /// matches by inferred .name == "InitVar".
    pub(crate) fn is_init_var(&self, ann: GNode) -> bool {
        match self.infer_first(ann, None) {
            // getattr(inferred, "name", "") == "InitVar" (name proxies
            // through Instances: InitVar[str] infers to an InitVar Instance)
            Ok(v) => self.getattr_name_of(&v).as_deref() == Some("InitVar"),
            _ => false,
        }
    }

    /// `getattr(inferred, "name", "")` over inference results: direct
    /// attribute for named nodes (ClassDef/FunctionDef/Module), proxied
    /// class name for Instance-likes (bases.Proxy.__getattr__).
    pub(crate) fn getattr_name_of(&self, v: &Value) -> Option<String> {
        match v {
            Value::Node(g) => self.node_name(*g),
            Value::Uninferable => None,
            _ => self.proxied_class(v).and_then(|c| self.node_name(c)),
        }
    }

    /// brain_numpy_utils._is_a_numpy_module: lookup-based Import check.
    pub(crate) fn is_a_numpy_module(&self, name_node: GNode) -> bool {
        let Some(nick) = self.name_of(name_node) else {
            return false;
        };
        let sym = self.sym(&nick);
        let looked = self.lookup(name_node, sym);
        for nv in &looked.1 {
            if let crate::value::NV::N(g) = nv {
                let md = self.md(g.m);
                if let NodeKind::Import { names } = &md.tree.nodes[g.n.idx()].kind {
                    for (n, asn) in names {
                        let n_str = md.tree.s(*n);
                        let asn_str = asn.map(|a| md.tree.s(a).to_string());
                        if n_str == "numpy" && (asn_str.as_deref() == Some(nick.as_str()) || asn_str.is_none())
                        {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    // ---------- per-kind scans (registration order) ----------

    fn scan_call(&self, g: GNode) {
        let func = self.call_func(g);
        let func_name = func.and_then(|f| self.name_of(f));
        let func_attr = func.and_then(|f| self.attr_of(f));
        let simple = self.func_simple_name(g);
        // brain_argparse: argparse.Namespace(...)
        if let Some((expr, attr)) = &func_attr {
            if attr == "Namespace" && self.name_of(*expr).as_deref() == Some("argparse") {
                self.wipe();
            }
        }
        // brain_builtin_inference tips (Name == builtin, incl. the stdlib
        // `re` Pattern/Match carve-out for type())
        if let Some(fname) = &func_name {
            if BUILTIN_TIP_NAMES.contains(&fname.as_str()) {
                let re_carveout = fname == "type" && self.md(g.m).name == "re" && {
                    let parent = self.parent(g);
                    parent
                        .map(|p| {
                            let md = self.md(p.m);
                            match &md.tree.nodes[p.n.idx()].kind {
                                NodeKind::Assign { targets, .. } if targets.len() == 1 => {
                                    match &md.tree.nodes[targets[0].idx()].kind {
                                        NodeKind::AssignName { name } => {
                                            let n = md.tree.s(*name);
                                            n == "Pattern" || n == "Match"
                                        }
                                        _ => false,
                                    }
                                }
                                _ => false,
                            }
                        })
                        .unwrap_or(false)
                };
                if !re_carveout {
                    self.wipe();
                } else {
                    // brain_re.infer_pattern_match applies instead
                    // (inference_tip transform returns the node -> wipe)
                    self.wipe();
                }
            }
        }
        // dict.fromkeys
        if let Some((expr, attr)) = &func_attr {
            if attr == "fromkeys" && self.name_of(*expr).as_deref() == Some("dict") {
                self.wipe();
            }
        }
        // _infer_copy_method: <expr>.copy()
        if let Some((_, attr)) = &func_attr {
            if attr == "copy" {
                self.wipe();
            }
        }
        // _is_str_format_call: \"...\".format(...) / name.format(...)
        if let Some((expr, attr)) = &func_attr {
            if attr == "format" {
                let is_const_str = {
                    let md = self.md(expr.m);
                    match &md.tree.nodes[expr.n.idx()].kind {
                        NodeKind::Const(pyast::tree::ConstValue::Str(_)) => true,
                        NodeKind::Name { .. } => {
                            // safe_infer(node.func.expr) — inference side
                            // effect happens even when the result is not str
                            matches!(
                                self.safe_infer(*expr, &Ctx::new()),
                                Some(v) if matches!(
                                    self.value_const(&v),
                                    Some(pyast::tree::ConstValue::Str(_))
                                )
                            )
                        }
                        _ => false,
                    }
                };
                if is_const_str {
                    // tip applicability is decided HERE (transform time);
                    // infer-time tip_for consults the side table only
                    self.str_format_calls.borrow_mut().insert(g);
                    self.wipe();
                }
            }
        }
        // brain_dataclasses field() tip — applicability decided HERE
        if self.looks_like_dataclass_field_call(g) {
            self.dataclass_field_calls.borrow_mut().insert(g);
            self.wipe();
        }
        // brain_functools partial tip
        let partial_like = func_name.as_deref() == Some("partial")
            || matches!(&func_attr, Some((expr, attr))
                if attr == "partial" && self.name_of(*expr).as_deref() == Some("functools"));
        if partial_like {
            self.wipe();
        }
        // brain_gi require_version
        {
            let args = self.call_args(g);
            if args.len() == 2
                && args.iter().all(|&a| {
                    self.kind_is(a, |k| matches!(k, NodeKind::Const(_)))
                })
            {
                let matches_gi = match (&func_name, &func_attr) {
                    (Some(n), _) if n == "require_version" => true,
                    (_, Some((expr, attr)))
                        if attr == "require_version"
                            && self.name_of(*expr).as_deref() == Some("gi") =>
                    {
                        true
                    }
                    _ => false,
                };
                if matches_gi {
                    self.wipe();
                }
            }
        }
        // brain_namedtuple_enum Call tips
        if let Some(s) = &simple {
            if s == "namedtuple" || s == "Enum" || s == "NamedTuple" {
                self.wipe();
            }
            // brain_random sample tip
            if s == "sample" {
                self.wipe();
            }
        }
        // brain_re / brain_regex: type() in module re/regex assigned to
        // Pattern/Match (the carve-out above skipped the builtin tip; these
        // register their own tip that DOES apply)
        {
            let mod_name = self.md(g.m).name.clone();
            if (mod_name == "re" || mod_name == "regex")
                && func_name.as_deref() == Some("type")
            {
                if let Some(p) = self.parent(g) {
                    let md = self.md(p.m);
                    if let NodeKind::Assign { targets, .. } = &md.tree.nodes[p.n.idx()].kind {
                        if targets.len() == 1 {
                            if let NodeKind::AssignName { name } =
                                &md.tree.nodes[targets[0].idx()].kind
                            {
                                let n = md.tree.s(*name);
                                if n == "Pattern" || n == "Match" {
                                    self.wipe();
                                }
                            }
                        }
                    }
                }
            }
        }
        // brain_statistics quantiles
        if self.looks_like_statistics_quantiles(g) {
            self.wipe();
        }
        // brain_random sample (predicate is syntactic: ANY .sample() call)
        {
            let md = self.md(g.m);
            let is_sample = match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Call { func, .. } => match &md.tree.nodes[func.idx()].kind {
                    NodeKind::Attribute { attrname, .. } => md.tree.s(*attrname) == "sample",
                    NodeKind::Name { name } => md.tree.s(*name) == "sample",
                    _ => false,
                },
                _ => false,
            };
            if is_sample {
                self.wipe();
            }
        }
        // brain_typing tips
        if let Some(s) = &simple {
            if s == "TypeVar" || s == "NewType" || s == "cast" {
                self.wipe();
            }
        }
        if let Some(fname) = &func_name {
            let args = self.call_args(g);
            // _alias(...)
            if (fname == "_alias" || fname == "_DeprecatedGenericAlias")
                && args.len() == 2
                && self.kind_is(args[0], |k| {
                    matches!(k, NodeKind::Attribute { .. } | NodeKind::Name { .. })
                })
            {
                self.wipe();
            }
            // _TupleType(tuple, ...) / _CallableType(collections.abc.Callable, ...)
            if !args.is_empty()
                && ((fname == "_TupleType"
                    && self.name_of(args[0]).as_deref() == Some("tuple"))
                    || (fname == "_CallableType"
                        && self.kind_is(args[0], |k| matches!(k, NodeKind::Attribute { .. }))))
            {
                self.wipe();
            }
        }
    }

    /// brain_statistics._looks_like_statistics_quantiles (purely syntactic
    /// except the frame-locals/body ImportFrom check for the bare-Name form)
    pub(crate) fn looks_like_statistics_quantiles(&self, g: GNode) -> bool {
        let Some(func) = self.call_func(g) else {
            return false;
        };
        let md = self.md(g.m);
        match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::Attribute { expr, attrname, .. } => {
                md.tree.s(*attrname) == "quantiles"
                    && matches!(&md.tree.nodes[expr.idx()].kind,
                        NodeKind::Name { name } if md.tree.s(*name) == "statistics")
            }
            NodeKind::Name { name } if md.tree.s(*name) == "quantiles" => {
                // from statistics import quantiles in the frame body
                let frame = self.frame(g);
                let fmd = self.md(frame.m);
                let sym = self.sym("quantiles");
                let has_local = fmd
                    .locals
                    .borrow()
                    .get(&frame.n)
                    .map(|l| l.contains_key(&sym))
                    .unwrap_or(false);
                has_local && {
                    let body: Vec<NodeId> = match &fmd.tree.nodes[frame.n.idx()].kind {
                        NodeKind::Module(d) => d.body.clone(),
                        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                            d.body.clone()
                        }
                        NodeKind::ClassDef(d) => d.body.clone(),
                        _ => Vec::new(),
                    };
                    body.iter().any(|&b| match &fmd.tree.nodes[b.idx()].kind {
                        NodeKind::ImportFrom { modname, names, .. } => {
                            fmd.tree.s(*modname) == "statistics"
                                && names.iter().any(|(n, _)| fmd.tree.s(*n) == "quantiles")
                        }
                        _ => false,
                    })
                }
            }
            _ => false,
        }
    }

    /// brain_dataclasses._looks_like_dataclass_field_call (check_scope=True)
    fn looks_like_dataclass_field_call(&self, call: GNode) -> bool {
        let Some(stmt) = self.statement(call) else {
            return false;
        };
        let scope = self.scope(stmt);
        let stmt_ok = {
            let md = self.md(stmt.m);
            matches!(&md.tree.nodes[stmt.n.idx()].kind,
                NodeKind::AnnAssign { value: Some(_), .. })
        };
        if !stmt_ok
            || !self.kind_is(scope, |k| matches!(k, NodeKind::ClassDef(_)))
            || !self.is_decorated_with_dataclass(scope)
        {
            return false;
        }
        let Some(func) = self.call_func(call) else {
            return false;
        };
        // next(node.func.infer()) — inference side effect
        match self.infer_first(func, None) {
            Ok(Value::Node(g))
                if self.kind_is(g, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                }) =>
            {
                let md = self.md(g.m);
                self.node_name(g).as_deref() == Some("field")
                    && DATACLASS_MODULES.contains(&md.name.as_str())
            }
            _ => false,
        }
    }

    const ATTRS_NAMES: [&'static str; 13] = [
        "attr.s",
        "attrs",
        "attr.attrs",
        "attr.attributes",
        "attr.define",
        "attr.mutable",
        "attr.frozen",
        "attrs.define",
        "attrs.mutable",
        "attrs.frozen",
        "attr.dataclass",
        "attrs.attrs",
        "attrs.s",
    ];

    const NEW_ATTRS_NAMES: [&'static str; 3] = ["attrs.define", "attrs.mutable", "attrs.frozen"];

    const ATTRIB_NAMES: [&'static str; 7] = [
        "attr.Factory",
        "attr.ib",
        "attrib",
        "attr.attrib",
        "attr.field",
        "attrs.field",
        "field",
    ];

    /// brain_attrs.attr_attributes_transform (brain_attrs.py:65-107):
    /// rewrite attrs class attributes as instance attributes — fresh
    /// Unknown placeholders (parent = the body statement, via reparents)
    /// REPLACE both locals[name] and instance_attrs[name].
    fn apply_attrs_transform(&self, node: GNode) {
        let md = self.md(node.m);
        // node.locals["__attrs_attrs__"] = [Unknown(parent=node)]
        {
            let ph = self.alloc_placeholders(1)[0];
            self.reparents.borrow_mut().insert(ph, node);
            let sym = self.sym("__attrs_attrs__");
            md.locals
                .borrow_mut()
                .entry(node.n)
                .or_default()
                .insert(sym, vec![ph]);
        }
        // use_bare_annotations = is_decorated_with_attrs(node, NEW_ATTRS_NAMES)
        // (re-runs the decorator scan incl. safe_infer side effects)
        let use_bare_annotations = {
            let mut m = false;
            for dec in self.decorator_nodes(node) {
                let target = match self.call_func(dec) {
                    Some(f) => f,
                    None => dec,
                };
                if let Some(ds) = self.dotted_string(target) {
                    if Self::NEW_ATTRS_NAMES.contains(&ds.as_str()) {
                        m = true;
                        break;
                    }
                }
                let v = self.safe_infer(target, &Ctx::new());
                if let Some(Value::Node(ng)) = &v {
                    if self.kind_is(*ng, |k| {
                        matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                    }) && self.md(ng.m).name == "attr._next_gen"
                    {
                        m = true;
                        break;
                    }
                }
            }
            m
        };
        let body: Vec<NodeId> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ClassDef(d) => d.body.clone(),
            _ => return,
        };
        for stmt in body {
            let (targets, value, annotation): (Vec<NodeId>, Option<NodeId>, Option<NodeId>) =
                match &md.tree.nodes[stmt.idx()].kind {
                    NodeKind::Assign { targets, value, .. } => {
                        (targets.clone(), Some(*value), None)
                    }
                    NodeKind::AnnAssign {
                        target,
                        value,
                        annotation,
                        ..
                    } => (vec![*target], *value, Some(*annotation)),
                    _ => continue,
                };
            // value Call -> func name must be an attrib constructor
            let value_is_call = value
                .map(|v| matches!(md.tree.nodes[v.idx()].kind, NodeKind::Call { .. }))
                .unwrap_or(false);
            if value_is_call {
                let func = match &md.tree.nodes[value.unwrap().idx()].kind {
                    NodeKind::Call { func, .. } => GNode { m: node.m, n: *func },
                    _ => continue,
                };
                match self.dotted_string(func) {
                    Some(fs) if Self::ATTRIB_NAMES.contains(&fs.as_str()) => {}
                    _ => continue,
                }
            } else if !use_bare_annotations {
                continue;
            }
            // skip ClassVar-annotated attributes (brain/helpers.is_class_var:
            // next(node.infer()) — fresh single pull; name == "ClassVar")
            if let Some(ann) = annotation {
                let ag = GNode { m: node.m, n: ann };
                let is_cv = match self.infer_first_fresh(ag) {
                    Ok(Some(v)) => {
                        let nm = match &v {
                            Value::Node(g) => self.node_name(*g),
                            other => self
                                .proxied_class(other)
                                .and_then(|c| self.node_name(c)),
                        };
                        nm.as_deref() == Some("ClassVar")
                    }
                    _ => false,
                };
                if is_cv {
                    continue;
                }
            }
            for t in targets {
                let tg = GNode { m: node.m, n: t };
                let tname = match &md.tree.nodes[t.idx()].kind {
                    NodeKind::AssignName { name } => *name,
                    _ => continue,
                };
                let _ = tg;
                let ph = self.alloc_placeholders(1)[0];
                // Unknown(parent=cdef_body_node)
                self.reparents
                    .borrow_mut()
                    .insert(ph, GNode { m: node.m, n: stmt });
                let gsym = self.sym(md.tree.s(tname));
                md.locals
                    .borrow_mut()
                    .entry(node.n)
                    .or_default()
                    .insert(gsym, vec![ph]);
                self.iattrs
                    .borrow_mut()
                    .entry(node)
                    .or_default()
                    .insert(gsym, vec![ph]);
            }
        }
    }

    /// ClassDef transform chain in EXACT astroid registration order with
    /// the transforms.py:60-78 break rule: an APPLIED transform whose
    /// return value's class differs from ClassDef (incl. the common
    /// `return None`) STOPS the remaining transforms for this node — and
    /// only non-None returns invalidate the inference cache.
    fn scan_classdef(&self, g: GNode) {
        // 1. brain_attrs is_decorated_with_attrs (brain_attrs.py:48-61):
        // registered FIRST; per decorator: Call -> func; as_string() name
        // match short-circuits, otherwise safe_infer (2 pulls) runs for
        // EVERY decorated class and checks root module "attr._next_gen".
        // The transform mutates locals and returns None -> BREAK, no wipe.
        {
            let mut matched = false;
            for dec in self.decorator_nodes(g) {
                let target = match self.call_func(dec) {
                    Some(f) => f,
                    None => dec,
                };
                if let Some(ds) = self.dotted_string(target) {
                    if Self::ATTRS_NAMES.contains(&ds.as_str()) {
                        matched = true;
                        break;
                    }
                }
                let v = self.safe_infer(target, &Ctx::new());
                if let Some(Value::Node(ng)) = &v {
                    if self.kind_is(*ng, |k| {
                        matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                    }) && self.md(ng.m).name == "attr._next_gen"
                    {
                        matched = true;
                        break;
                    }
                }
            }
            if matched {
                // attr_attributes_transform (brain_attrs.py:65-107):
                // returns None -> BREAK, no wipe
                self.apply_attrs_transform(g);
                return;
            }
        }
        // 2. brain_boto3 (qname == boto3.resources.base.ServiceResource);
        // service_request_transform returns node -> wipe + continue
        if self.qname(g) == "boto3.resources.base.ServiceResource" {
            self.wipe();
        }
        // 3. brain_builtin_inference @object.__new__ decorator tip
        // (inference_tip -> returns node -> wipe + continue)
        for dec in self.decorator_nodes(g) {
            if self.dotted_string(dec).as_deref() == Some("object.__new__") {
                self.wipe();
                break;
            }
        }
        // 4. brain_collections easy_class_getitem_inference
        // (brain_collections.py:92-133): the predicate runs a full
        // class-context getattr('__class_getitem__') — inference side
        // effects (metaclass chain) run for EVERY collections/_collections*
        // class; on success locals['__class_getitem__'] is REPLACED with a
        // fresh extract_node template. Transform returns None -> BREAK,
        // no wipe.
        {
            let q = self.qname(g);
            if q.starts_with("_collections") || q.starts_with("collections") {
                let cgi = self.sym("__class_getitem__");
                if self.class_getattr(g, cgi, None, true).is_ok() {
                    if let Some(tmpl) = self.build_template_module(
                        "@classmethod\ndef __class_getitem__(cls, item):\n    return cls\n",
                        "",
                    ) {
                        let func = {
                            let tmd = self.md(tmpl);
                            let locals = tmd.locals.borrow();
                            locals
                                .get(&NodeId::MODULE)
                                .and_then(|m| m.get(&cgi))
                                .and_then(|v| v.first().copied())
                        };
                        if let Some(func) = func {
                            let md = self.md(g.m);
                            let mut locals = md.locals.borrow_mut();
                            locals.entry(g.n).or_default().insert(cgi, vec![func]);
                        }
                    }
                    return; // transform applied, returned None
                }
            }
        }
        // 5. brain_dataclasses dataclass_transform: returns node (-> wipe +
        // continue) only when an __init__ is generated
        // (_check_generate_dataclass_init); otherwise None -> BREAK no wipe
        if self.is_decorated_with_dataclass(g) {
            // node.is_dataclass = True (brain_dataclasses.py:59)
            self.is_dataclass_flag.borrow_mut().insert(g);
            self.apply_dataclass_transform(g);
            if self.check_generate_dataclass_init(g) {
                // kw_only_decorated (brain_dataclasses.py:76-85): breaks
                // False on the first non-Call decorator; otherwise the LAST
                // kw_only keyword across all Call decorators wins.
                let mut kw_only_decorated = false;
                for dec in self.decorator_nodes(g) {
                    let md = self.md(dec.m);
                    let keywords: Vec<NodeId> = match &md.tree.nodes[dec.n.idx()].kind {
                        NodeKind::Call { keywords, .. } => keywords.clone(),
                        _ => {
                            kw_only_decorated = false;
                            break;
                        }
                    };
                    for kw in keywords {
                        if let NodeKind::Keyword { arg: Some(a), value } =
                            &md.tree.nodes[kw.idx()].kind
                        {
                            if md.tree.s(*a) == "kw_only" {
                                // keyword.value.bool_value() is True
                                kw_only_decorated = matches!(
                                    &md.tree.nodes[value.idx()].kind,
                                    NodeKind::Const(pyast::tree::ConstValue::Bool(true))
                                );
                            }
                        }
                    }
                }
                self.generate_dataclass_init(g, kw_only_decorated);
                self.wipe();
            } else {
                return;
            }
        }
        // 6/7. brain_io: BufferedReader/BufferedWriter get locals["raw"] =
        // [FileIO instance]; TextIOWrapper gets locals["buffer"] =
        // [BufferedWriter instance] (any module!). Returns None -> BREAK.
        {
            let name = self.node_name(g).unwrap_or_default();
            if name == "BufferedReader" || name == "BufferedWriter" {
                self.apply_io_transform(g, "raw", "FileIO");
                return;
            }
            if name == "TextIOWrapper" {
                self.apply_io_transform(g, "buffer", "BufferedWriter");
                return;
            }
        }
        // 8. brain_namedtuple_enum infer_enum_class (raw, returns node ->
        // wipe + continue); predicate _is_enum_subclass =
        // is_subtype_of("enum.Enum") — inference happens even when False
        if self.is_subtype_of(g, "enum.Enum", None) {
            self.apply_enum_transform(g);
            self.wipe();
        }
        // 9. brain_namedtuple_enum typing.NamedTuple base tip (wipe+continue)
        {
            let md = self.md(g.m);
            let bases: Vec<NodeId> = match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::ClassDef(d) => d.bases.clone(),
                _ => Vec::new(),
            };
            drop(md);
            let has_nt_base = bases.iter().any(|&b| {
                self.dotted_string(GNode { m: g.m, n: b })
                    .map(|s| {
                        s == "NamedTuple"
                            || s == "typing.NamedTuple"
                            || s == "typing_extensions.NamedTuple"
                    })
                    .unwrap_or(false)
            });
            if has_nt_base {
                self.wipe();
            }
        }
        // 10. brain_qt transform_pyside_signal (qname gated to PySide) — not
        // ported (no PySide in the pinned corpora).
        // 11. brain_six add_metaclass: predicate is the SYNTACTIC
        // as_string() == "six.add_metaclass" check; the transform then
        // single-pulls each Call decorator's func — match with args returns
        // node (wipe + continue), otherwise None -> BREAK no wipe
        // (brain_six.py:169-192).
        {
            let mut pred = false;
            for dec in self.decorator_nodes(g) {
                if let Some(f) = self.call_func(dec) {
                    if self.dotted_string(f).as_deref() == Some("six.add_metaclass") {
                        pred = true;
                        break;
                    }
                }
            }
            if pred {
                let mut applied = false;
                for dec in self.decorator_nodes(g) {
                    let Some(f) = self.call_func(dec) else { continue };
                    let inferred = self.first_value(f, &Ctx::new()).ok().flatten();
                    let Some(Value::Node(fg)) = inferred else { continue };
                    let is_func_or_class = self.kind_is(fg, |k| {
                        matches!(
                            k,
                            NodeKind::FunctionDef(_)
                                | NodeKind::AsyncFunctionDef(_)
                                | NodeKind::ClassDef(_)
                        )
                    });
                    let has_args = {
                        let md = self.md(dec.m);
                        matches!(&md.tree.nodes[dec.n.idx()].kind,
                            NodeKind::Call { args, .. } if !args.is_empty())
                    };
                    if is_func_or_class
                        && self.qname(fg) == "six.add_metaclass"
                        && has_args
                    {
                        // node._metaclass = decorator.args[0] side table
                        self.apply_six_add_metaclass(g, dec);
                        applied = true;
                        break;
                    }
                }
                if applied {
                    self.wipe();
                } else {
                    return; // transform ran and returned None
                }
            }
        }
        // 12. brain_six with_metaclass (returns node -> wipe + continue)
        {
            let md = self.md(g.m);
            let bases: Vec<NodeId> = match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::ClassDef(d) => d.bases.clone(),
                _ => Vec::new(),
            };
            if bases.len() == 1 {
                let b = GNode { m: g.m, n: bases[0] };
                if let Some(f) = self.call_func(b) {
                    let dotted = self.dotted_string(f);
                    let matched = match dotted.as_deref() {
                        Some("six.with_metaclass") => true,
                        Some(name) if !name.contains('.') && name == "with_metaclass" => {
                            // local import: module locals["with_metaclass"][0]
                            // must be an ImportFrom from six
                            let sym = self.sym("with_metaclass");
                            let locals = md.locals.borrow();
                            locals
                                .get(&NodeId::MODULE)
                                .and_then(|l| l.get(&sym))
                                .and_then(|v| v.first())
                                .map(|&imp| {
                                    let imd = self.md(imp.m);
                                    matches!(&imd.tree.nodes[imp.n.idx()].kind,
                                        NodeKind::ImportFrom { modname, .. }
                                            if imd.tree.s(*modname) == "six")
                                })
                                .unwrap_or(false)
                        }
                        _ => false,
                    };
                    if matched {
                        self.wipe();
                    }
                }
            }
        }
        // 13. brain_typing pep695 generic class tip (wipe + continue)
        {
            let md = self.md(g.m);
            if let NodeKind::ClassDef(d) = &md.tree.nodes[g.n.idx()].kind {
                if !d.type_params.is_empty() {
                    self.wipe();
                }
            }
        }
        // 14. brain_uuid _patch_uuid_class: locals["int"] = [Const(0,
        // parent=node)] (returns None -> break; last in the chain)
        if self.qname(g) == "uuid.UUID" {
            let ph = self.alloc_synth_node(NodeKind::Const(
                pyast::tree::ConstValue::Int(pyast::tree::IntValue::Small(0)),
            ));
            self.implicit_owner.borrow_mut().insert(ph, g);
            let md = self.md(g.m);
            let mut locals = md.locals.borrow_mut();
            locals
                .entry(g.n)
                .or_default()
                .insert(self.sym("int"), vec![ph]);
        }
    }

    /// brain_io._generic_io_transform: locals[name] = [Instance of _io.<cls>]
    fn apply_io_transform(&self, g: GNode, name: &str, cls_name: &str) {
        // AstroidManager().ast_from_module_name("_io")
        let Ok(io_mod) = self.ast_from_module_name("_io", true) else { return };
        let cls = {
            let md = self.md(io_mod);
            let locals = md.locals.borrow();
            locals
                .get(&NodeId::MODULE)
                .and_then(|m| m.get(&self.sym(cls_name)))
                .and_then(|v| v.first().copied())
        };
        let Some(cls) = cls else { return };
        let inst = self.instantiate_class(cls);
        let ph = self.alloc_placeholders(1)[0];
        self.redirects.borrow_mut().insert(ph, crate::value::NV::V(inst));
        let md = self.md(g.m);
        let mut locals = md.locals.borrow_mut();
        locals
            .entry(g.n)
            .or_default()
            .insert(self.sym(name), vec![ph]);
    }

    /// six.add_metaclass: node._metaclass = decorator.args[0]
    fn apply_six_add_metaclass(&self, g: GNode, dec: GNode) {
        let md = self.md(dec.m);
        if let NodeKind::Call { args, .. } = &md.tree.nodes[dec.n.idx()].kind {
            if let Some(&meta) = args.first() {
                self.meta_override
                    .borrow_mut()
                    .insert(g, GNode { m: dec.m, n: meta });
            }
        }
    }

    /// brain_dataclasses._check_generate_dataclass_init
    /// (brain_dataclasses.py): True when the class has no own __init__ and
    /// the dataclass decorator call has no init=False keyword.
    fn check_generate_dataclass_init(&self, g: GNode) -> bool {
        let init_sym = self.sym("__init__");
        {
            let md = self.md(g.m);
            let locals = md.locals.borrow();
            if locals
                .get(&g.n)
                .map(|l| l.contains_key(&init_sym))
                .unwrap_or(false)
            {
                return false;
            }
        }
        // find the LAST dataclass-looking Call decorator
        let mut found: Option<GNode> = None;
        for dec in self.decorator_nodes(g) {
            let md = self.md(dec.m);
            let is_call = matches!(md.tree.nodes[dec.n.idx()].kind, NodeKind::Call { .. });
            drop(md);
            if !is_call {
                continue;
            }
            if self.looks_like_dataclass_decorator(dec) {
                found = Some(dec);
            }
        }
        let Some(found) = found else { return true };
        // not any(kw.arg == "init" and kw.value.bool_value() is False)
        let md = self.md(found.m);
        if let NodeKind::Call { keywords, .. } = &md.tree.nodes[found.n.idx()].kind {
            for &kw in keywords {
                if let NodeKind::Keyword { arg: Some(a), value } = &md.tree.nodes[kw.idx()].kind
                {
                    if md.tree.s(*a) == "init" {
                        if let NodeKind::Const(pyast::tree::ConstValue::Bool(false)) =
                            &md.tree.nodes[value.idx()].kind
                        {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// brain_dataclasses._generate_dataclass_init + the locals["__init__"]
    /// install (brain_dataclasses.py:86-106, 244-390). Pull-for-pull port:
    /// the assigns listing re-runs per-field is_class_var (next) +
    /// _is_keyword_only_sentinel (safe_infer); the body re-checks property
    /// overrides (decoratornames inference), field calls (func next-pull)
    /// and _is_init_var (cache hit); base mro walks happen via mro().
    fn generate_dataclass_init(&self, node: GNode, kw_only_decorated: bool) {
        const DEFAULT_FACTORY: &str = "_HAS_DEFAULT_FACTORY";
        // assigns = list(_get_dataclass_attributes(node, init=True))
        let body: Vec<NodeId> = {
            let md = self.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::ClassDef(d) => d.body.clone(),
                _ => return,
            }
        };
        struct FieldAssign {
            name: String,
            ann: GNode,
            value: Option<GNode>,
        }
        let mut assigns: Vec<FieldAssign> = Vec::new();
        for stmt in &body {
            let md = self.md(node.m);
            let (target, annotation, value) = match &md.tree.nodes[stmt.idx()].kind {
                NodeKind::AnnAssign { target, annotation, value, .. } => {
                    match &md.tree.nodes[target.idx()].kind {
                        NodeKind::AssignName { name } => {
                            (md.tree.s(*name).to_string(), *annotation, *value)
                        }
                        _ => continue,
                    }
                }
                _ => continue,
            };
            drop(md);
            let ann = GNode { m: node.m, n: annotation };
            // is_class_var: next(node.infer())
            let is_class_var = match self.infer_first(ann, None) {
                Ok(Value::Node(vg)) => self.node_name(vg).as_deref() == Some("ClassVar"),
                _ => false,
            };
            if is_class_var {
                continue;
            }
            // _is_keyword_only_sentinel: safe_infer
            let kw_only = matches!(
                self.safe_infer(ann, &Ctx::new()),
                Some(Value::Inst { cls: c, .. }) if self.qname(c) == "dataclasses._KW_ONLY_TYPE"
            );
            if kw_only {
                continue;
            }
            // init=True: InitVar fields are KEPT (no _is_init_var pull here)
            assigns.push(FieldAssign {
                name: target,
                ann,
                value: value.map(|v| GNode { m: node.m, n: v }),
            });
        }
        // _find_arguments_from_base_classes(node)
        type ParamData = (Option<String>, Option<String>);
        let mut prev_pos: indexmap::IndexMap<String, ParamData> = Default::default();
        let mut prev_kw: indexmap::IndexMap<String, ParamData> = Default::default();
        let mro = self.mro(node, None).unwrap_or_default();
        let init_sym = self.sym("__init__");
        for base in mro.iter().rev() {
            if !self.is_dataclass_flag.borrow().contains(base) {
                continue;
            }
            let init_fn = {
                let md = self.md(base.m);
                let locals = md.locals.borrow();
                match locals
                    .get(&base.n)
                    .and_then(|l| l.get(&init_sym))
                    .and_then(|v| v.first())
                {
                    Some(&f) => f,
                    None => continue,
                }
            };
            if let Some((pos, kw)) = self.dataclass_init_params.borrow().get(&init_fn) {
                for (n, a, d) in pos {
                    prev_pos.insert(n.clone(), (a.clone(), d.clone()));
                }
                for (n, a, d) in kw {
                    prev_kw.insert(n.clone(), (a.clone(), d.clone()));
                }
            }
            // dataclass bases with a HANDWRITTEN __init__ (init=False) fall
            // through _get_arguments_data on real Arguments — out of corpus
            // scope (no side data): skipped.
        }
        // main loop
        let mut params: Vec<(String, Option<String>, Option<String>)> = Vec::new();
        let mut kw_only_params: Vec<(String, Option<String>, Option<String>)> = Vec::new();
        let mut assignments: Vec<String> = Vec::new();
        let prop_qname = "builtins.property".to_string();
        for fa in &assigns {
            let name = &fa.name;
            // property override check: decoratornames() inference
            let mut property_node: Option<GNode> = None;
            {
                let locs: Vec<GNode> = {
                    let md = self.md(node.m);
                    let locals = md.locals.borrow();
                    let sym = self.sym(name);
                    locals
                        .get(&node.n)
                        .and_then(|l| l.get(&sym))
                        .cloned()
                        .unwrap_or_default()
                };
                for additional in locs {
                    if !self.kind_is(additional, |k| {
                        matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                    }) {
                        continue;
                    }
                    if self.decorator_nodes(additional).is_empty() {
                        continue;
                    }
                    if self
                        .decoratornames(additional, None)
                        .into_iter()
                        .flatten()
                        .any(|q| q == prop_qname)
                    {
                        property_node = Some(additional);
                        break;
                    }
                }
            }
            let is_field = match fa.value {
                Some(v)
                    if self.kind_is(v, |k| matches!(k, NodeKind::Call { .. })) =>
                {
                    self.looks_like_dataclass_field_call_noscope(v)
                }
                _ => false,
            };
            if is_field {
                // skip fields with init=False
                let v = fa.value.unwrap();
                let md = self.md(v.m);
                let mut skip = false;
                if let NodeKind::Call { keywords, .. } = &md.tree.nodes[v.n.idx()].kind {
                    for &kw in keywords {
                        if let NodeKind::Keyword { arg: Some(a), value } =
                            &md.tree.nodes[kw.idx()].kind
                        {
                            if md.tree.s(*a) == "init"
                                && matches!(
                                    &md.tree.nodes[value.idx()].kind,
                                    NodeKind::Const(pyast::tree::ConstValue::Bool(false))
                                )
                            {
                                skip = true;
                            }
                        }
                    }
                }
                drop(md);
                if skip {
                    prev_pos.shift_remove(name);
                    prev_kw.shift_remove(name);
                    continue;
                }
            }
            // _is_init_var(annotation): next pull (cache hit after the
            // safe_infer in the listing pass)
            let init_var = self.is_init_var(fa.ann);
            let (ann_node, mut assignment): (Option<GNode>, String) = if init_var {
                let md = self.md(fa.ann.m);
                let slice = match &md.tree.nodes[fa.ann.n.idx()].kind {
                    NodeKind::Subscript { slice, .. } => {
                        Some(GNode { m: fa.ann.m, n: *slice })
                    }
                    _ => None,
                };
                (slice, String::new())
            } else {
                (Some(fa.ann), format!("self.{name} = {name}"))
            };
            // annotation.as_string() / default.as_string() — the REAL
            // as_string port (asstr.rs): astroid renders NamedExpr bare,
            // so walrus defaults make the generated init UNPARSEABLE
            // (brain_dataclasses.py:91-94 `except AstroidSyntaxError: pass`)
            let ann_str: Option<String> =
                ann_node.map(|a| crate::asstr::as_string(self, a));
            let mut default_str: Option<String> = None;
            if let Some(v) = fa.value {
                if is_field {
                    match self.dataclass_field_default(v) {
                        Some((false, dn)) => {
                            default_str = Some(crate::asstr::as_string(self, dn))
                        }
                        Some((true, dn)) => {
                            default_str = Some(DEFAULT_FACTORY.to_string());
                            let f = crate::asstr::as_string(self, dn);
                            // new_call.as_string() == f"{factory}()"
                            assignment = format!(
                                "self.{name} = {f}() if {name} is {DEFAULT_FACTORY} else {name}"
                            );
                        }
                        None => {}
                    }
                } else {
                    default_str = Some(crate::asstr::as_string(self, v));
                }
            } else if let Some(pn) = property_node {
                // str(next(property_node.infer_call_result(None)).as_string())
                let cx = Ctx::new();
                match self.infer_call_result_first(&Value::Node(pn), None, Some(&cx)) {
                    Ok(Some(v)) => {
                        default_str = Some(match &v {
                            Value::Uninferable => "Uninferable".to_string(),
                            Value::Node(g) => crate::asstr::as_string(self, *g),
                            Value::SynthConst(c) => crate::asstr::const_repr(c),
                            _ => "None".to_string(),
                        });
                    }
                    _ => {}
                }
            } else {
                // _get_previous_field_default(node, name) — astroid calls
                // node.mro() FRESH here (inference side effects repeat)
                let mro2 = self.mro(node, None).unwrap_or_default();
                'outer: for base in mro2.iter().rev() {
                    if !self.is_dataclass_flag.borrow().contains(base) {
                        continue;
                    }
                    let locs: Vec<GNode> = {
                        let md = self.md(base.m);
                        let locals = md.locals.borrow();
                        let sym = self.sym(name);
                        locals
                            .get(&base.n)
                            .and_then(|l| l.get(&sym))
                            .cloned()
                            .unwrap_or_default()
                    };
                    for assign in locs {
                        let Some(parent) = self.parent(assign) else { continue };
                        let md = self.md(parent.m);
                        let pv = match &md.tree.nodes[parent.n.idx()].kind {
                            NodeKind::AnnAssign { value: Some(v), .. } => {
                                GNode { m: parent.m, n: *v }
                            }
                            _ => continue,
                        };
                        let is_call = matches!(
                            md.tree.nodes[pv.n.idx()].kind,
                            NodeKind::Call { .. }
                        );
                        drop(md);
                        if !is_call || !self.looks_like_dataclass_field_call(pv) {
                            continue;
                        }
                        if let Some((is_factory, dn)) = self.dataclass_field_default(pv) {
                            default_str = if is_factory {
                                Some(format!("{}()", crate::asstr::as_string(self, dn)))
                            } else {
                                Some(crate::asstr::as_string(self, dn))
                            };
                            break 'outer;
                        }
                    }
                }
            }
            // param tuple
            let param = (name.clone(), ann_str.clone(), default_str.clone());
            // field-level kw_only keyword overrides the decorator
            if is_field {
                let v = fa.value.unwrap();
                let md = self.md(v.m);
                let mut kw_only_kw: Option<bool> = None;
                if let NodeKind::Call { keywords, .. } = &md.tree.nodes[v.n.idx()].kind {
                    for &kw in keywords {
                        if let NodeKind::Keyword { arg: Some(a), value } =
                            &md.tree.nodes[kw.idx()].kind
                        {
                            if md.tree.s(*a) == "kw_only" && kw_only_kw.is_none() {
                                kw_only_kw = Some(matches!(
                                    &md.tree.nodes[value.idx()].kind,
                                    NodeKind::Const(pyast::tree::ConstValue::Bool(true))
                                ));
                            }
                        }
                    }
                }
                drop(md);
                if let Some(is_kw) = kw_only_kw {
                    if is_kw {
                        kw_only_params.push(param);
                    } else {
                        params.push(param);
                    }
                    continue;
                }
            }
            if kw_only_decorated {
                if prev_kw.contains_key(name) {
                    prev_kw.insert(name.clone(), (ann_str, default_str));
                } else {
                    kw_only_params.push(param);
                }
            } else if prev_pos.contains_key(name) {
                prev_pos.insert(name.clone(), (ann_str, default_str));
            } else if prev_kw.contains_key(name) {
                // params = [name, *params] — bare name promoted to FRONT
                params.insert(0, (name.clone(), None, None));
                prev_kw.shift_remove(name);
            } else {
                params.push(param);
            }
            if !init_var {
                assignments.push(assignment);
            }
        }
        // _parse_arguments_into_strings
        let render = |store: &indexmap::IndexMap<String, ParamData>| -> String {
            let mut s = String::new();
            for (n, (a, d)) in store {
                s.push_str(n);
                if let Some(a) = a {
                    s.push_str(": ");
                    s.push_str(a);
                }
                if let Some(d) = d {
                    s.push_str(" = ");
                    s.push_str(d);
                }
                s.push_str(", ");
            }
            s
        };
        let render_param = |(n, a, d): &(String, Option<String>, Option<String>)| {
            let mut s = n.clone();
            if let Some(a) = a {
                s.push_str(&format!(": {a}"));
            }
            if let Some(d) = d {
                s.push_str(&format!(" = {d}"));
            }
            s
        };
        let prev_pos_only = render(&prev_pos);
        let prev_kw_only = render(&prev_kw);
        // `"self" in prev_pos_only` — SUBSTRING check on the rendered string
        // (brain_dataclasses.py:377), bug-for-bug
        let mut params_string = if prev_pos_only.contains("self") {
            String::new()
        } else {
            "self, ".to_string()
        };
        params_string.push_str(&prev_pos_only);
        params_string.push_str(
            &params.iter().map(render_param).collect::<Vec<_>>().join(", "),
        );
        if !params_string.ends_with(", ") {
            params_string.push_str(", ");
        }
        if !prev_kw_only.is_empty() || !kw_only_params.is_empty() {
            params_string.push_str("*, ");
        }
        params_string.push_str(&prev_kw_only);
        params_string.push_str(
            &kw_only_params
                .iter()
                .map(render_param)
                .collect::<Vec<_>>()
                .join(", "),
        );
        let assignments_string = if assignments.is_empty() {
            "pass".to_string()
        } else {
            assignments.join("\n    ")
        };
        let init_str =
            format!("def __init__({params_string}) -> None:\n    {assignments_string}");
        // parse(init_str) — astroid's parse builds with modname '' and
        // applies transforms; AstroidSyntaxError -> no init installed
        let Some(mid) = self.build_template_module(&init_str, "") else {
            return;
        };
        let init_fn = {
            let md = self.md(mid);
            let locals = md.locals.borrow();
            match locals
                .get(&NodeId::MODULE)
                .and_then(|l| l.get(&init_sym))
                .and_then(|v| v.first())
            {
                Some(&f) => f,
                None => return,
            }
        };
        // init_node.parent = node; node.locals["__init__"] = [init_node]
        self.reparents.borrow_mut().insert(init_fn, node);
        {
            let md = self.md(node.m);
            let mut locals = md.locals.borrow_mut();
            locals
                .entry(node.n)
                .or_default()
                .insert(init_sym, vec![init_fn]);
        }
        // side data for subclass _find_arguments_from_base_classes: pos =
        // self + merged prev + params; kw = merged prev + kw_only_params
        {
            let mut pos_data: Vec<(String, Option<String>, Option<String>)> = Vec::new();
            if !prev_pos_only.contains("self") {
                pos_data.push(("self".to_string(), None, None));
            }
            for (n, (a, d)) in &prev_pos {
                pos_data.push((n.clone(), a.clone(), d.clone()));
            }
            pos_data.extend(params.iter().cloned());
            let mut kw_data: Vec<(String, Option<String>, Option<String>)> = Vec::new();
            for (n, (a, d)) in &prev_kw {
                kw_data.push((n.clone(), a.clone(), d.clone()));
            }
            kw_data.extend(kw_only_params.iter().cloned());
            self.dataclass_init_params
                .borrow_mut()
                .insert(init_fn, (pos_data, kw_data));
        }
        // DEFAULT_FACTORY sentinel in the class's root module locals
        {
            let fac_sym = self.sym(DEFAULT_FACTORY);
            let has = {
                let md = self.md(node.m);
                let locals = md.locals.borrow();
                locals
                    .get(&NodeId::MODULE)
                    .map(|l| l.contains_key(&fac_sym))
                    .unwrap_or(false)
            };
            if !has {
                if let Some(fmid) =
                    self.build_template_module(&format!("{DEFAULT_FACTORY} = object()"), "")
                {
                    let assign_name = {
                        let fmd = self.md(fmid);
                        let locals = fmd.locals.borrow();
                        locals
                            .get(&NodeId::MODULE)
                            .and_then(|l| l.get(&fac_sym))
                            .and_then(|v| v.first())
                            .copied()
                    };
                    if let Some(an) = assign_name {
                        // new_assign.parent = root
                        let stmt = self.statement(an).unwrap_or(an);
                        self.reparents.borrow_mut().insert(stmt, GNode {
                            m: node.m,
                            n: NodeId::MODULE,
                        });
                        let md = self.md(node.m);
                        let mut locals = md.locals.borrow_mut();
                        locals
                            .entry(NodeId::MODULE)
                            .or_default()
                            .insert(fac_sym, vec![an]);
                    }
                }
            }
        }
    }

    /// _looks_like_dataclass_field_call(value, check_scope=False)
    fn looks_like_dataclass_field_call_noscope(&self, call: GNode) -> bool {
        let Some(func) = self.call_func(call) else {
            return false;
        };
        match self.infer_first(func, None) {
            Ok(Value::Node(g))
                if self.kind_is(g, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                }) =>
            {
                let md = self.md(g.m);
                self.node_name(g).as_deref() == Some("field")
                    && DATACLASS_MODULES.contains(&md.name.as_str())
            }
            _ => false,
        }
    }

    /// brain_dataclasses._get_field_default -> (is_factory, value node)
    fn dataclass_field_default(&self, field_call: GNode) -> Option<(bool, GNode)> {
        let md = self.md(field_call.m);
        let keywords: Vec<NodeId> = match &md.tree.nodes[field_call.n.idx()].kind {
            NodeKind::Call { keywords, .. } => keywords.clone(),
            _ => return None,
        };
        let mut default: Option<NodeId> = None;
        let mut default_factory: Option<NodeId> = None;
        for kw in keywords {
            if let NodeKind::Keyword { arg: Some(a), value } = &md.tree.nodes[kw.idx()].kind {
                match md.tree.s(*a) {
                    "default" => default = Some(*value),
                    "default_factory" => default_factory = Some(*value),
                    _ => {}
                }
            }
        }
        match (default, default_factory) {
            (Some(d), None) => Some((false, GNode { m: field_call.m, n: d })),
            (None, Some(f)) => Some((true, GNode { m: field_call.m, n: f })),
            _ => None,
        }
    }

    fn scan_functiondef(&self, g: GNode) {
        // brain_functools _looks_like_lru_cache (syntactic; transform
        // returns None -> no wipe)
        {
            for dec in self.decorator_nodes(g) {
                let md = self.md(dec.m);
                let hit = match &md.tree.nodes[dec.n.idx()].kind {
                    NodeKind::Attribute { attrname, .. } => {
                        md.tree.s(*attrname) == "lru_cache"
                    }
                    NodeKind::Call { func, .. } => match &md.tree.nodes[func.idx()].kind {
                        NodeKind::Name { name } => md.tree.s(*name) == "lru_cache",
                        NodeKind::Attribute { expr, attrname, .. } => {
                            md.tree.s(*attrname) == "lru_cache"
                                && matches!(&md.tree.nodes[expr.idx()].kind,
                                    NodeKind::Name { name } if md.tree.s(*name) == "functools")
                        }
                        _ => false,
                    },
                    _ => false,
                };
                if hit {
                    self.lru_wrapped.borrow_mut().insert(g);
                    // _transform_lru_cache returns None -> the remaining
                    // FunctionDef transforms are SKIPPED (transforms.py:74-77)
                    return;
                }
            }
        }
        // brain_hypothesis composite (raw, returns node)
        {
            let md = self.md(g.m);
            let first_arg_draw = match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    match &md.tree.nodes[d.args.idx()].kind {
                        NodeKind::Arguments(a) => a.args.first().map(|&arg| {
                            matches!(&md.tree.nodes[arg.idx()].kind,
                                NodeKind::AssignName { name } if md.tree.s(*name) == "draw")
                        }) == Some(true),
                        _ => false,
                    }
                }
                _ => false,
            };
            if first_arg_draw {
                for dec in self.decorator_nodes(g) {
                    if let Some(s) = self.dotted_string(dec) {
                        if COMPOSITE_NAMES.contains(&s.as_str()) {
                            self.wipe();
                            break;
                        }
                    }
                }
            }
        }
        // brain_namedtuple_enum typing.NamedTuple function tip
        if self.node_name(g).as_deref() == Some("NamedTuple") && self.md(g.m).name == "typing" {
            self.wipe();
        }
        // brain_typing TypedDict function tip
        let qn = self.qname(g);
        if qn == "typing.TypedDict" || qn == "typing_extensions.TypedDict" {
            self.wipe();
        }
    }

    fn scan_name(&self, g: GNode) {
        let Some(name) = self.name_of(g) else { return };
        // brain_numpy_core_multiarray Name tip
        if NUMPY_MULTIARRAY.contains(&name.as_str()) && self.md(g.m).name.starts_with("numpy") {
            self.wipe();
        }
        // brain_type: Name "type" inside a Subscript
        if name == "type" {
            if let Some(p) = self.parent(g) {
                if self.kind_is(p, |k| matches!(k, NodeKind::Subscript { .. })) {
                    self.wipe();
                }
            }
        }
    }

    fn scan_attribute(&self, g: GNode) {
        let Some((expr, attr)) = self.attr_of(g) else {
            return;
        };
        // brain_numpy_core_function_base + multiarray + numeric Attribute tips
        if (NUMPY_FUNCTION_BASE.contains(&attr.as_str())
            || NUMPY_MULTIARRAY.contains(&attr.as_str())
            || attr == "ones")
            && self.kind_is(expr, |k| matches!(k, NodeKind::Name { .. }))
            && self.is_a_numpy_module(expr)
        {
            self.wipe();
        }
        // brain_numpy_ndarray: ANY x.ndarray attribute
        if attr == "ndarray" {
            self.wipe();
        }
    }

    fn scan_subscript(&self, g: GNode) {
        let md = self.md(g.m);
        let value = match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Subscript { value, .. } => GNode { m: g.m, n: *value },
            _ => return,
        };
        drop(md);
        // brain_pathlib parents tip: value Attribute attrname == "parents";
        // predicate infers node.value (side effect) and matches
        // pathlib._PathParents instances
        if let Some((_, attr)) = self.attr_of(value) {
            if attr == "parents" {
                if let Ok(Value::Inst { cls, .. }) = self.infer_first(value, None) {
                    if self.qname(cls) == "pathlib._PathParents" {
                        // tip applicability is decided HERE (transform time)
                        self.pathlib_subscripts.borrow_mut().insert(g);
                        self.wipe();
                    }
                }
            }
        }
        // brain_typing subscript tip (recursive value check)
        fn typing_member(e: &Engine, v: GNode) -> bool {
            let md = e.md(v.m);
            match &md.tree.nodes[v.n.idx()].kind {
                NodeKind::Name { name } => TYPING_MEMBERS.contains(&md.tree.s(*name)),
                NodeKind::Attribute { attrname, .. } => {
                    TYPING_MEMBERS.contains(&md.tree.s(*attrname))
                }
                NodeKind::Subscript { value, .. } => {
                    typing_member(e, GNode { m: v.m, n: *value })
                }
                _ => false,
            }
        }
        if typing_member(self, value) {
            self.wipe();
        }
    }

    fn scan_unknown(&self, g: GNode) {
        // brain_dataclasses Unknown tip (dataclass attribute placeholders)
        let Some(parent) = self.parent(g) else { return };
        let is_annassign = self.kind_is(parent, |k| matches!(k, NodeKind::AnnAssign { .. }));
        if !is_annassign {
            return;
        }
        let scope = self.scope(parent);
        if self.kind_is(scope, |k| matches!(k, NodeKind::ClassDef(_)))
            && self.is_decorated_with_dataclass(scope)
        {
            self.wipe();
        }
    }
}


// ===================== brain_namedtuple_enum.infer_enum_class =====================

impl Engine {
    /// infer_enum_class (brain_namedtuple_enum.py:391-538): replace each
    /// all-AssignName local of an enum subclass with an Instance of a fake
    /// per-member class (name/value/_name_/_value_ properties), add
    /// `_value2member_map_` / `__members__`, and a guessed `name` property
    /// when no member is called "name". The whole body runs ONCE (for the
    /// first basename in the mro chain) — `break` at the end — and is
    /// skipped entirely for classes defined in the `enum` module itself.
    fn apply_enum_transform(&self, node: GNode) {
        // for basename in (b for cls in node.mro() for b in cls.basenames):
        let first_basename = {
            let mro = match self.mro(node, None) {
                Ok(m) => m,
                Err(_) => return, // MroError propagates out in astroid; be tolerant
            };
            let mut found: Option<String> = None;
            'outer: for cls in mro {
                let md = self.md(cls.m);
                let bases: Vec<NodeId> = match &md.tree.nodes[cls.n.idx()].kind {
                    NodeKind::ClassDef(d) => d.bases.clone(),
                    _ => continue,
                };
                for b in bases {
                    if let Some(s) = self.dotted_string(GNode { m: cls.m, n: b }) {
                        found = Some(s);
                        break 'outer;
                    }
                }
            }
            match found {
                Some(b) => b,
                None => return, // no basenames at all -> loop body never runs
            }
        };
        if self.md(node.m).name == "enum" {
            return;
        }
        let node_md = self.md(node.m);
        // basenames text of THIS class (types in the fake source).
        // ClassDef.basenames = [b.as_string() for b in bases] — the FULL
        // expression text, NOT just dotted names: `class Role(
        // namedtuple("Role", "name order"), Enum)` keeps the namedtuple
        // CALL as a textual base of every member fake class; the fake
        // module has no tips (apply_transforms=False), so each member's
        // ancestors walk infers that Call through the REAL
        // collections.namedtuple infer_call_result (+13 bumps per member —
        // Role.OP count parity). expr_source = raw source slice (~
        // as_string); dotted fallback for template-built enums.
        let basenames: Vec<String> = {
            match &node_md.tree.nodes[node.n.idx()].kind {
                NodeKind::ClassDef(d) => d
                    .bases
                    .iter()
                    .filter_map(|&b| {
                        let g = GNode { m: node.m, n: b };
                        self.expr_source(g).or_else(|| self.dotted_string(g))
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let types = basenames.join(", ");
        let enum_qname = self.qname(node);
        // snapshot of the locals entries (astroid iterates .items() while
        // REPLACING values of existing keys)
        let entries: Vec<(crate::value::GSym, Vec<GNode>)> = {
            let locals = node_md.locals.borrow();
            match locals.get(&node.n) {
                Some(map) => map.iter().map(|(k, v)| (*k, v.clone())).collect(),
                None => Vec::new(),
            }
        };
        // (local, fake name, member proxy ph) — DICT semantics keyed by
        // local (astroid `dunder_members[local] = fake`: last target wins,
        // first-insertion position kept)
        let mut dunder_members: Vec<(String, String, GNode)> = Vec::new();
        let mut target_names: rustc_hash::FxHashSet<String> = Default::default();
        // mymethods: FIRST local of each key that is a FunctionDef
        let mymethods: Vec<(crate::value::GSym, GNode)> = entries
            .iter()
            .filter_map(|(k, v)| {
                v.first().and_then(|&g| {
                    if self.kind_is(g, |kd| {
                        matches!(kd, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                    }) {
                        Some((*k, g))
                    } else {
                        None
                    }
                })
            })
            .collect();
        for (local, values) in &entries {
            let local_str = self.sname(*local);
            if local_str == "_ignore_"
                || values.iter().any(|&v| {
                    !self.kind_is(v, |k| matches!(k, NodeKind::AssignName { .. }))
                })
                || values.is_empty()
            {
                continue;
            }
            let Some(stmt) = self.statement(values[0]) else { continue };
            let smd = self.md(stmt.m);
            // targets per statement kind
            let (targets, stmt_value): (Vec<GNode>, Option<GNode>) =
                match &smd.tree.nodes[stmt.n.idx()].kind {
                    NodeKind::Assign { targets, value, .. } => {
                        let t0 = targets[0];
                        let ts = match &smd.tree.nodes[t0.idx()].kind {
                            NodeKind::Tuple { elts, .. } => {
                                elts.iter().map(|&e| GNode { m: stmt.m, n: e }).collect()
                            }
                            _ => targets.iter().map(|&t| GNode { m: stmt.m, n: t }).collect(),
                        };
                        (ts, Some(GNode { m: stmt.m, n: *value }))
                    }
                    NodeKind::AnnAssign { target, value, .. } => (
                        vec![GNode { m: stmt.m, n: *target }],
                        value.map(|v| GNode { m: stmt.m, n: v }),
                    ),
                    _ => continue,
                };
            // inferred_return_value
            let return_value: String = match stmt_value {
                None => "None".to_string(), // format(None) -> "None"
                Some(v) => {
                    let vmd = self.md(v.m);
                    match &vmd.tree.nodes[v.n.idx()].kind {
                        NodeKind::Const(pyast::tree::ConstValue::Str(sv)) => {
                            pyast::pyrepr::repr_str(sv)
                        }
                        NodeKind::Const(c) => const_format(c),
                        _ => self
                            .expr_source(v)
                            .unwrap_or_else(|| "____astroid_unparse_fallback".to_string()),
                    }
                }
            };
            let mut new_targets: Vec<GNode> = Vec::new();
            let mut placeholders =
                self.alloc_placeholders(targets.len()).into_iter();
            let mut made_any = false;
            for target in &targets {
                let tmd = self.md(target.m);
                let tname = match &tmd.tree.nodes[target.n.idx()].kind {
                    NodeKind::AssignName { name } => tmd.tree.s(*name).to_string(),
                    NodeKind::Starred { .. } => continue,
                    _ => continue,
                };
                target_names.insert(tname.clone());
                let mut classdef = format!(
                    "\nclass {name}({types}):\n    @property\n    def value(self):\n        return {rv}\n    @property\n    def _value_(self):\n        return {rv}\n    @property\n    def name(self):\n        return \"{name}\"\n    @property\n    def _name_(self):\n        return \"{name}\"\n",
                    name = tname,
                    types = types,
                    rv = return_value,
                );
                if first_basename.contains("IntFlag") {
                    classdef.push_str(&format!(
                        "    def __or__(self, other):\n        return {n}(self.value | other.value)\n    def __and__(self, other):\n        return {n}(self.value & other.value)\n    def __xor__(self, other):\n        return {n}(self.value ^ other.value)\n    def __add__(self, other):\n        return {n}(self.value + other.value)\n    def __div__(self, other):\n        return {n}(self.value / other.value)\n    def __invert__(self):\n        return {n}(~self.value)\n    def __mul__(self, other):\n        return {n}(self.value * other.value)\n",
                        n = tname
                    ));
                }
                // fake module named like the ENUM CLASS qname so the fake
                // member class qname composes to <enum>.<member>.
                // apply_transforms=False (brain_namedtuple_enum.py:126-128):
                // no transform scan — nothing infers the fake's base Names
                // before the reparent below, no wipes, no tips on the fake.
                let Some(fake_mid) =
                    self.build_template_module_no_transforms(&classdef, &enum_qname)
                else {
                    continue;
                };
                let sym = self.sym(&tname);
                let fake_cls = {
                    let fmd = self.md(fake_mid);
                    let locals = fmd.locals.borrow();
                    match locals
                        .get(&NodeId::MODULE)
                        .and_then(|l| l.get(&sym))
                        .and_then(|v| v.first())
                    {
                        Some(&g) => g,
                        None => continue,
                    }
                };
                // fake.parent = target.parent (brain_namedtuple_enum.py:
                // infer_enum_class) — the fake class is REPARENTED into the
                // enum's module: its base Name (e.g. `Enum`) resolves
                // through the enum class's scope chain (class scope skipped
                // -> module locals -> the real ImportFrom), and igetattr's
                // descriptor checks re-infer those per-member base Names.
                if let Some(tp) = self.parent(*target) {
                    if crate::graph::trace_infer() {
                        eprintln!("REPARENT fake {} -> {:?}", tname, tp);
                    }
                    self.reparents.borrow_mut().insert(fake_cls, tp);
                }
                // fake.locals[method.name] = [method] for mymethods
                {
                    let fmd = self.md(fake_mid);
                    let mut locals = fmd.locals.borrow_mut();
                    let entry = locals.entry(fake_cls.n).or_default();
                    for (mname, mnode) in &mymethods {
                        entry.insert(*mname, vec![*mnode]);
                    }
                }
                // new_targets.append(fake.instantiate_class())
                let inst = self.instantiate_class(fake_cls);
                let ph = placeholders.next().expect("placeholder");
                self.redirects.borrow_mut().insert(ph, crate::value::NV::V(inst));
                // the locals entry holds the Instance PROXY itself —
                // stmt.infer is a bare `yield self` (no NodeNG machinery)
                self.proxy_placeholders.borrow_mut().insert(ph);
                new_targets.push(ph);
                made_any = true;
                if stmt_value.is_none() {
                    continue;
                }
                // dunder_members[local] = fake (replace, keep position)
                if let Some(e) = dunder_members.iter_mut().find(|(l, ..)| *l == local_str) {
                    e.1 = tname.clone();
                    e.2 = ph;
                } else {
                    dunder_members.push((local_str.clone(), tname.clone(), ph));
                }
            }
            let _ = made_any;
            // node.locals[local] = new_targets (REPLACE)
            let mut locals = node_md.locals.borrow_mut();
            let map = locals.entry(node.n).or_default();
            map.insert(*local, new_targets);
        }
        // _value2member_map_ = [Dict()] and __members__ = [Dict(...)]
        let extra = self.alloc_placeholders(2);
        self.redirects.borrow_mut().insert(
            extra[0],
            crate::value::NV::V(Value::SynthDict {
                items: std::rc::Rc::new(Vec::new()),
            }),
        );
        // astroid's __members__ Dict values are LAZY Name nodes referencing
        // the member locals (brain_namedtuple_enum.py:489-507) — they
        // resolve through the class locals to the SAME Instance proxy as
        // new_targets (identity preserved; no second instantiate_class, so
        // no second per-member mro/INFBASES walk at transform time).
        let member_items: Vec<(Value, Value)> = dunder_members
            .iter()
            .map(|(k, _, ph)| {
                (
                    Value::SynthConst(std::rc::Rc::new(pyast::tree::ConstValue::Str(
                        k.clone().into(),
                    ))),
                    Value::Node(*ph),
                )
            })
            .collect();
        // pylint is_enum_member reads `[name_obj.name for _, name_obj in
        // members.items]` — the value-Name names, NOT the keys
        self.enum_member_names.borrow_mut().insert(
            node,
            dunder_members.iter().map(|(_, n, _)| n.clone()).collect(),
        );
        self.redirects.borrow_mut().insert(
            extra[1],
            crate::value::NV::V(Value::SynthDict {
                items: std::rc::Rc::new(member_items),
            }),
        );
        {
            let mut locals = node_md.locals.borrow_mut();
            let map = locals.entry(node.n).or_default();
            map.insert(self.sym("_value2member_map_"), vec![extra[0]]);
            map.insert(self.sym("__members__"), vec![extra[1]]);
        }
        // guessed `name` property when no member named "name"
        if !target_names.contains("name") {
            let src = "@property\ndef name(self):\n    return ''\n";
            if let Some(mid) = self.build_template_module(src, "") {
                let sym = self.sym("name");
                let func = {
                    let fmd = self.md(mid);
                    let locals = fmd.locals.borrow();
                    locals
                        .get(&NodeId::MODULE)
                        .and_then(|l| l.get(&sym))
                        .and_then(|v| v.first())
                        .copied()
                };
                if let Some(func) = func {
                    let mut locals = node_md.locals.borrow_mut();
                    let map = locals.entry(node.n).or_default();
                    map.insert(sym, vec![func]);
                }
            }
        }
    }

    /// source slice for an expression (as_string() approximation: enum
    /// member values are formatted back into the fake class source).
    pub(crate) fn expr_source(&self, g: GNode) -> Option<String> {
        let md = self.md(g.m);
        if md.file.starts_with('<') {
            return None;
        }
        let n = &md.tree.nodes[g.n.idx()];
        let bytes = std::fs::read(&md.file).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let (l1, c1, l2, c2) = (
            n.fromlineno as usize,
            n.col_offset.max(0) as usize,
            n.end_lineno as usize,
            n.end_col_offset.max(0) as usize,
        );
        if l1 == 0 || l2 == 0 || l1 > lines.len() || l2 > lines.len() {
            return None;
        }
        let slice = if l1 == l2 {
            lines[l1 - 1].as_bytes().get(c1..c2).map(|b| String::from_utf8_lossy(b).into_owned())?
        } else {
            let mut out = String::from_utf8_lossy(lines[l1 - 1].as_bytes().get(c1..)?).into_owned();
            for l in &lines[l1..l2 - 1] {
                out.push_str(l);
            }
            out.push_str(&String::from_utf8_lossy(lines[l2 - 1].as_bytes().get(..c2)?));
            out
        };
        let s = slice.trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// "{}".format(python_value) for non-str Consts in the enum fake source.
fn const_format(c: &pyast::tree::ConstValue) -> String {
    use pyast::tree::{ConstValue, IntValue};
    match c {
        ConstValue::None => "None".to_string(),
        ConstValue::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        ConstValue::Int(IntValue::Small(i)) => i.to_string(),
        ConstValue::Int(IntValue::Big(s)) => s.to_string(),
        ConstValue::Float(f) => pyast::pyrepr::repr_float(*f),
        ConstValue::Complex { .. } => "0j".to_string(),
        ConstValue::Bytes(_) => "b''".to_string(),
        ConstValue::Ellipsis => "Ellipsis".to_string(),
        ConstValue::NotImplemented => "NotImplemented".to_string(),
        ConstValue::Str(s) => s.to_string(),
        ConstValue::StrSurrogate(_) => String::new(),
    }
}
