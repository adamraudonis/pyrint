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

impl Engine {
    /// transforms.py _invalidate_cache: clears ONLY the global inference
    /// cache (lookup lru / tip caches survive).
    fn wipe(&self) {
        self.inf_cache.borrow_mut().clear();
    }

    /// Bottom-up transform application over a freshly built module.
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

    fn is_decorated_with_dataclass(&self, cls: GNode) -> bool {
        self.decorator_nodes(cls)
            .into_iter()
            .any(|d| self.looks_like_dataclass_decorator(d))
    }

    /// brain_numpy_utils._is_a_numpy_module: lookup-based Import check.
    fn is_a_numpy_module(&self, name_node: GNode) -> bool {
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
                    self.wipe();
                }
            }
        }
        // brain_dataclasses field() tip
        if self.looks_like_dataclass_field_call(g) {
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
        {
            let stat_match = match (&func_name, &func_attr) {
                (_, Some((expr, attr)))
                    if attr == "quantiles"
                        && self.name_of(*expr).as_deref() == Some("statistics") =>
                {
                    true
                }
                (Some(n), _) if n == "quantiles" => {
                    // from statistics import quantiles in the frame body
                    let frame = self.frame(g);
                    let md = self.md(frame.m);
                    let sym = self.sym("quantiles");
                    let has_local = md
                        .locals
                        .borrow()
                        .get(&frame.n)
                        .map(|l| l.contains_key(&sym))
                        .unwrap_or(false);
                    has_local && {
                        let body: Vec<NodeId> = match &md.tree.nodes[frame.n.idx()].kind {
                            NodeKind::Module(d) => d.body.clone(),
                            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                                d.body.clone()
                            }
                            NodeKind::ClassDef(d) => d.body.clone(),
                            _ => Vec::new(),
                        };
                        body.iter().any(|&b| {
                            match &md.tree.nodes[b.idx()].kind {
                                NodeKind::ImportFrom { modname, names, .. } => {
                                    md.tree.s(*modname) == "statistics"
                                        && names
                                            .iter()
                                            .any(|(n, _)| md.tree.s(*n) == "quantiles")
                                }
                                _ => false,
                            }
                        })
                    }
                }
                _ => false,
            };
            if stat_match {
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

    fn scan_classdef(&self, g: GNode) {
        // brain_boto3
        if self.qname(g) == "boto3.resources.factory.ResourceFactory" {
            self.wipe();
        }
        // brain_builtin_inference @object.__new__ decorator tip
        for dec in self.decorator_nodes(g) {
            if self.dotted_string(dec).as_deref() == Some("object.__new__") {
                self.wipe();
                break;
            }
        }
        // brain_dataclasses dataclass_transform (raw, returns node)
        if self.is_decorated_with_dataclass(g) {
            self.wipe();
        }
        // brain_namedtuple_enum infer_enum_class (raw, returns node);
        // predicate _is_enum_subclass = is_subtype_of("enum.Enum") —
        // inference happens even when False
        if self.is_subtype_of(g, "enum.Enum", None) {
            self.wipe();
        }
        // brain_namedtuple_enum typing.NamedTuple base tip
        {
            let md = self.md(g.m);
            let bases: Vec<NodeId> = match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::ClassDef(d) => d.bases.clone(),
                _ => Vec::new(),
            };
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
        // brain_six add_metaclass / with_metaclass (raw, return node)
        for dec in self.decorator_nodes(g) {
            if let Some(f) = self.call_func(dec) {
                if self.dotted_string(f).as_deref() == Some("six.add_metaclass") {
                    self.wipe();
                    break;
                }
            }
        }
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
            // brain_typing pep695 generic class tip
            if let NodeKind::ClassDef(d) = &md.tree.nodes[g.n.idx()].kind {
                if !d.type_params.is_empty() {
                    self.wipe();
                }
            }
        }
    }

    fn scan_functiondef(&self, g: GNode) {
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
        // brain_numpy_core_function_base + multiarray Attribute tips
        if (NUMPY_FUNCTION_BASE.contains(&attr.as_str())
            || NUMPY_MULTIARRAY.contains(&attr.as_str()))
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
                if let Ok(Value::Inst { cls }) = self.infer_first(value, None) {
                    if self.qname(cls) == "pathlib._PathParents" {
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
