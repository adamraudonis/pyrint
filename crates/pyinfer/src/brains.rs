//! Inference tips (astroid brain ports): brain_builtin_inference call
//! transforms, functools.partial, str.format folding, .copy().
//! Tip caching/guard mirrors astroid/inference_tip.py:37-86.

use std::cell::RefCell;
use std::rc::Rc;

use pyast::tree::{ConstValue, IntValue, NodeKind};
use pyast::NodeId;

use crate::ctx::{copy_context, Ctx};
use crate::graph::Engine;
use crate::value::{ErrKind, Flow, GNode, GSym, SeqKind, Value, NV};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tip {
    Builtin(u8), // index into BUILTIN_NAMES
    DictFromkeys,
    CopyMethod,
    StrFormat,
    Partial,
    /// typing.TypeVar(...) / typing.NewType(...) calls
    TypingTypeVar,
    /// `X = _alias(...)` / special alias calls inside typing.py
    TypingAlias,
    /// typing.X[...] subscripts (non-alias members)
    TypingSubscript,
    /// typing.TypedDict FunctionDef -> ClassDef
    TypedDictFunc,
    /// namedtuple(...) calls (brain_namedtuple_enum.infer_named_tuple)
    NamedTupleCall,
    /// typing.NamedTuple(...) calls
    TypingNamedTupleCall,
    /// class X(NamedTuple): ... (infer_typing_namedtuple_class)
    TypingNamedTupleClass,
    /// the typing.NamedTuple FunctionDef itself -> _NamedTuple class
    TypingNamedTupleFunc,
    /// brain_numpy member tips: index into NUMPY_MEMBER_SRC (Attribute
    /// nodes via attribute_name_looks_like_numpy_member, Name nodes via
    /// member_name_looks_like_numpy_member — multiarray only)
    NumpyMember(u8),
    /// brain_numpy_ndarray: ANY Attribute with attrname == "ndarray"
    NumpyNdarray,
}

/// registration-ordered numpy member templates: function_base (3),
/// multiarray (20), numeric (1); name sets are disjoint.
const NUMPY_MEMBER_SRC: [(&str, &str); 24] = {
    let fb = crate::numpy_templates::NUMPY_FUNCTION_BASE_SRC;
    let ma = crate::numpy_templates::NUMPY_MULTIARRAY_SRC;
    let nu = crate::numpy_templates::NUMPY_NUMERIC_SRC;
    [
        fb[0], fb[1], fb[2],
        ma[0], ma[1], ma[2], ma[3], ma[4], ma[5], ma[6], ma[7], ma[8], ma[9],
        ma[10], ma[11], ma[12], ma[13], ma[14], ma[15], ma[16], ma[17], ma[18], ma[19],
        nu[0],
    ]
};


const BUILTIN_NAMES: [&str; 18] = [
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

fn tip_id(t: Tip) -> (u8, u8) {
    match t {
        Tip::Builtin(i) => (0, i),
        Tip::DictFromkeys => (1, 0),
        Tip::CopyMethod => (2, 0),
        Tip::StrFormat => (3, 0),
        Tip::Partial => (4, 0),
        Tip::TypingTypeVar => (5, 0),
        Tip::TypingAlias => (5, 1),
        Tip::TypingSubscript => (5, 2),
        Tip::TypedDictFunc => (5, 3),
        Tip::NamedTupleCall => (6, 0),
        Tip::TypingNamedTupleCall => (6, 1),
        Tip::TypingNamedTupleClass => (6, 2),
        Tip::TypingNamedTupleFunc => (6, 3),
        Tip::NumpyMember(i) => (7, i),
        Tip::NumpyNdarray => (7, 31),
    }
}

/// typing.__all__ on CPython 3.12 (brain_typing TYPING_MEMBERS)
const TYPING_MEMBERS: [&str; 99] = [
    "Self", "TYPE_CHECKING", "Text", "TypeAlias", "TypeAliasType",
    "TypeGuard", "Unpack",
    "Annotated", "Any", "Callable", "ClassVar", "Concatenate", "Final",
    "ForwardRef", "Generic", "Literal", "Optional", "ParamSpec", "Protocol",
    "Tuple", "Type", "TypeVar", "TypeVarTuple", "Union", "AbstractSet",
    "ByteString", "Container", "ContextManager", "Hashable", "ItemsView",
    "Iterable", "Iterator", "KeysView", "Mapping", "MappingView",
    "MutableMapping", "MutableSequence", "MutableSet", "Sequence", "Sized",
    "ValuesView", "Awaitable", "AsyncIterator", "AsyncIterable", "Coroutine",
    "Collection", "AsyncGenerator", "AsyncContextManager", "Reversible",
    "SupportsAbs", "SupportsBytes", "SupportsComplex", "SupportsFloat",
    "SupportsIndex", "SupportsInt", "SupportsRound", "ChainMap", "Counter",
    "Deque", "Dict", "DefaultDict", "List", "OrderedDict", "Set", "FrozenSet",
    "NamedTuple", "TypedDict", "Generator", "BinaryIO", "IO", "Match",
    "Pattern", "TextIO", "AnyStr", "assert_type", "assert_never", "cast",
    "clear_overloads", "dataclass_transform", "final", "get_args",
    "get_origin", "get_overloads", "get_type_hints", "is_typeddict",
    "LiteralString", "Never", "NewType", "no_type_check",
    "no_type_check_decorator", "NoReturn", "NotRequired", "overload",
    "override", "ParamSpecArgs", "ParamSpecKwargs", "Required", "reveal_type",
    "runtime_checkable",
];

const TYPING_ALIAS_QNAMES: [&str; 41] = [
    "typing.Hashable", "typing.Awaitable", "typing.Coroutine",
    "typing.AsyncIterable", "typing.AsyncIterator", "typing.Iterable",
    "typing.Iterator", "typing.Reversible", "typing.Sized",
    "typing.Container", "typing.Collection", "typing.Callable",
    "typing.AbstractSet", "typing.MutableSet", "typing.Mapping",
    "typing.MutableMapping", "typing.Sequence", "typing.MutableSequence",
    "typing.ByteString", "typing.Tuple", "typing.List", "typing.Deque",
    "typing.Set", "typing.FrozenSet", "typing.MappingView",
    "typing.KeysView", "typing.ItemsView", "typing.ValuesView",
    "typing.ContextManager", "typing.AsyncContextManager", "typing.Dict",
    "typing.DefaultDict", "typing.OrderedDict", "typing.Counter",
    "typing.ChainMap", "typing.Generator", "typing.AsyncGenerator",
    "typing.Type", "typing.Pattern", "typing.Match", "typing.NoReturn",
];

const TYPING_TYPE_TEMPLATE: &str = "
class Meta(type):
    def __getitem__(self, item):
        return self

    @property
    def __args__(self):
        return ()

class {0}(metaclass=Meta):
    pass
";

impl Engine {
    /// NodeNG._explicit_inference equivalent. None => no tip applies or
    /// UseInferenceDefault.
    pub fn explicit_inference(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        // _explicit_inference is registered on nodes by the TransformVisitor
        // at the END of the module's build (builder.py:175-177): inference
        // running during delayed_assattr of a module-in-build sees NO tips
        // on that module's nodes (default path; results land in the global
        // cache and are only erased if a later transform wipes it).
        if !self.md(node.m).tips_active.get() {
            return None;
        }
        let tip = self.find_tip(node)?;
        let (a, b) = tip_id(tip);
        let key = (a * 32 + b, node);
        if self.tip_guard.borrow().contains(&key) {
            return None; // recursion -> UseInferenceDefault
        }
        let cacheable = ctx.is_empty();
        if cacheable {
            if let Some(hit) = self.tip_cache.borrow().get(&key) {
                return Some(Flow::ok(hit.to_vec()));
            }
        }
        self.tip_guard.borrow_mut().insert(key);
        let res = self.run_tip(tip, node, ctx);
        self.tip_guard.borrow_mut().remove(&key);
        if let Some(flow) = &res {
            if cacheable && flow.err.is_none() {
                let mut cache = self.tip_cache.borrow_mut();
                let mut order = self.tip_order.borrow_mut();
                if cache.len() >= 64 {
                    if let Some(oldest) = order.pop_front() {
                        cache.remove(&oldest);
                    }
                }
                cache.insert(key, Rc::new(flow.vals.clone()));
                order.push_back(key);
            }
        }
        res
    }

    /// transform predicates, evaluated lazily (astroid runs them once per
    /// build; they are pure syntactic checks so this is equivalent).
    fn find_tip(&self, node: GNode) -> Option<Tip> {
        let md = self.md(node.m);
        // Subscript tip: typing.X[...] (brain_typing _looks_like_typing_subscript)
        if let NodeKind::Subscript { value, .. } = &md.tree.nodes[node.n.idx()].kind {
            if self.looks_like_typing_subscript(GNode { m: node.m, n: *value }) {
                return Some(Tip::TypingSubscript);
            }
            return None;
        }
        // ClassDef tip: NamedTuple bases (brain_namedtuple_enum
        // _has_namedtuple_base; registered before the typing tips)
        if let NodeKind::ClassDef(d) = &md.tree.nodes[node.n.idx()].kind {
            let has_nt = d.bases.iter().any(|&b| {
                let g = GNode { m: node.m, n: b };
                self.dotted_of(g)
                    .map(|s| {
                        s == "NamedTuple"
                            || s == "typing.NamedTuple"
                            || s == "typing_extensions.NamedTuple"
                    })
                    .unwrap_or(false)
            });
            if has_nt {
                return Some(Tip::TypingNamedTupleClass);
            }
            return None;
        }
        // FunctionDef tips: typing.NamedTuple function
        // (brain_namedtuple_enum) and typing.TypedDict (brain_typing)
        if matches!(md.tree.nodes[node.n.idx()].kind, NodeKind::FunctionDef(_)) {
            let q = self.qname(node);
            if q == "typing.NamedTuple" && md.name == "typing" {
                return Some(Tip::TypingNamedTupleFunc);
            }
            if q == "typing.TypedDict" || q == "typing_extensions.TypedDict" {
                return Some(Tip::TypedDictFunc);
            }
            return None;
        }
        // brain_numpy Attribute tips (registration order: function_base,
        // multiarray, numeric member tips, then ndarray)
        if let NodeKind::Attribute { expr, attrname, .. } = &md.tree.nodes[node.n.idx()].kind {
            let attr = md.tree.s(*attrname).to_string();
            let expr = GNode { m: node.m, n: *expr };
            if let Some(idx) = NUMPY_MEMBER_SRC.iter().position(|(n, _)| *n == attr) {
                // attribute_name_looks_like_numpy_member: expr is a Name
                // representing a numpy import (lookup-based, works without
                // numpy being importable)
                if self.kind_is(expr, |k| matches!(k, NodeKind::Name { .. }))
                    && self.is_a_numpy_module(expr)
                {
                    return Some(Tip::NumpyMember(idx as u8));
                }
            }
            // brain_numpy_ndarray._looks_like_numpy_ndarray: attrname only
            if attr == "ndarray" {
                return Some(Tip::NumpyNdarray);
            }
            return None;
        }
        // brain_numpy_core_multiarray Name tip
        // (member_name_looks_like_numpy_member: only inside numpy modules)
        if let NodeKind::Name { name } = &md.tree.nodes[node.n.idx()].kind {
            let n = md.tree.s(*name);
            if md.name.starts_with("numpy") {
                if let Some(idx) = NUMPY_MEMBER_SRC[3..23].iter().position(|(m, _)| *m == n) {
                    return Some(Tip::NumpyMember((idx + 3) as u8));
                }
            }
            return None;
        }
        let NodeKind::Call { func, .. } = &md.tree.nodes[node.n.idx()].kind else {
            return None;
        };
        match &md.tree.nodes[func.idx()].kind {
            NodeKind::Name { name } => {
                let n = md.tree.s(*name);
                if n == "namedtuple" {
                    return Some(Tip::NamedTupleCall);
                }
                if n == "NamedTuple" {
                    return Some(Tip::TypingNamedTupleCall);
                }
                if n == "partial" {
                    return Some(Tip::Partial);
                }
                if n == "TypeVar" || n == "NewType" {
                    return Some(Tip::TypingTypeVar);
                }
                // _looks_like_typing_alias / _looks_like_special_alias
                if let NodeKind::Call { args, .. } = &md.tree.nodes[node.n.idx()].kind {
                    if (n == "_alias" || n == "_DeprecatedGenericAlias")
                        && args.len() == 2
                        && matches!(
                            md.tree.nodes[args[0].idx()].kind,
                            NodeKind::Attribute { .. } | NodeKind::Name { .. }
                        )
                    {
                        return Some(Tip::TypingAlias);
                    }
                    if n == "_TupleType" || n == "_CallableType" {
                        let first_ok = args.first().map(|&a| match &md.tree.nodes[a.idx()].kind {
                            NodeKind::Name { name } => {
                                n == "_TupleType" && md.tree.s(*name) == "tuple"
                            }
                            NodeKind::Attribute { .. } => n == "_CallableType",
                            _ => false,
                        }) == Some(true);
                        if first_ok {
                            return Some(Tip::TypingAlias);
                        }
                    }
                }
                let idx = BUILTIN_NAMES.iter().position(|&b| b == n)?;
                // re module Pattern/Match carve-out
                if n == "type" && md.name == "re" {
                    let parent = md.tree.nodes[node.n.idx()].parent;
                    if let NodeKind::Assign { targets, .. } = &md.tree.nodes[parent.idx()].kind {
                        if targets.len() == 1 {
                            if let NodeKind::AssignName { name: tn } =
                                &md.tree.nodes[targets[0].idx()].kind
                            {
                                let t = md.tree.s(*tn);
                                if t == "Pattern" || t == "Match" {
                                    return None;
                                }
                            }
                        }
                    }
                }
                Some(Tip::Builtin(idx as u8))
            }
            NodeKind::Attribute { expr, attrname, .. } => {
                let attr = md.tree.s(*attrname);
                if attr == "namedtuple" {
                    return Some(Tip::NamedTupleCall);
                }
                if attr == "NamedTuple" {
                    return Some(Tip::TypingNamedTupleCall);
                }
                if attr == "TypeVar" || attr == "NewType" {
                    return Some(Tip::TypingTypeVar);
                }
                if attr == "fromkeys" {
                    if let NodeKind::Name { name } = &md.tree.nodes[expr.idx()].kind {
                        if md.tree.s(*name) == "dict" {
                            return Some(Tip::DictFromkeys);
                        }
                    }
                    return None;
                }
                if attr == "partial" {
                    if let NodeKind::Name { name } = &md.tree.nodes[expr.idx()].kind {
                        if md.tree.s(*name) == "functools" {
                            return Some(Tip::Partial);
                        }
                    }
                    return None;
                }
                if attr == "copy" {
                    return Some(Tip::CopyMethod);
                }
                if attr == "format" {
                    // _is_str_format_call
                    let expr_g = GNode { m: node.m, n: *expr };
                    let value_is_str = match &md.tree.nodes[expr.idx()].kind {
                        NodeKind::Const(ConstValue::Str(_)) => true,
                        NodeKind::Name { .. } => matches!(
                            self.safe_infer(expr_g, &Ctx::new())
                                .and_then(|v| self.value_const(&v)),
                            Some(ConstValue::Str(_))
                        ),
                        _ => false,
                    };
                    if value_is_str {
                        return Some(Tip::StrFormat);
                    }
                    return None;
                }
                None
            }
            _ => None,
        }
    }

    fn call_parts(&self, node: GNode) -> (Vec<GNode>, Vec<(Option<GSym>, GNode)>) {
        let md = self.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { args, keywords, .. } => {
                let a = args.iter().map(|&x| GNode { m: node.m, n: x }).collect();
                let kw = keywords
                    .iter()
                    .filter_map(|&k| match &md.tree.nodes[k.idx()].kind {
                        NodeKind::Keyword { arg, value } => Some((
                            arg.map(|s| self.g(&md, s)),
                            GNode { m: node.m, n: *value },
                        )),
                        _ => None,
                    })
                    .collect();
                (a, kw)
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// CallSite.from_call equivalent
    fn call_site_of_call(&self, node: GNode, ctx: &Rc<Ctx>) -> crate::calls::CallSite {
        let (args, kws) = self.call_parts(node);
        let cc = crate::ctx::CallCtx {
            id: self.next_callctx_id(),
            args: RefCell::new(args.into_iter().map(NV::N).collect()),
            keywords: RefCell::new(kws),
            callee: RefCell::new(None),
        };
        self.call_site_from(&cc, ctx)
    }

    fn run_tip(&self, tip: Tip, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        match tip {
            Tip::Builtin(i) => self.run_builtin_tip(BUILTIN_NAMES[i as usize], node, ctx),
            Tip::DictFromkeys => self.tip_dict_fromkeys(node, ctx),
            Tip::CopyMethod => self.tip_copy_method(node, ctx),
            Tip::StrFormat => self.tip_str_format(node, ctx),
            Tip::Partial => self.tip_partial(node, ctx),
            Tip::TypingTypeVar => self.tip_typing_typevar(node, ctx),
            Tip::TypingAlias => self.tip_typing_alias(node, ctx),
            Tip::TypingSubscript => self.tip_typing_subscript(node, ctx),
            Tip::TypedDictFunc => self.tip_typeddict_func(node),
            Tip::NamedTupleCall => self.tip_named_tuple(node, ctx),
            Tip::TypingNamedTupleCall => self.tip_typing_namedtuple_call(node, ctx),
            Tip::TypingNamedTupleClass => self.tip_typing_namedtuple_class(node, ctx),
            Tip::TypingNamedTupleFunc => self.tip_typing_namedtuple_func(node, ctx),
            Tip::NumpyMember(i) => {
                self.tip_numpy_extract(NUMPY_MEMBER_SRC[i as usize].1, ctx)
            }
            Tip::NumpyNdarray => {
                self.tip_numpy_extract(crate::numpy_templates::NUMPY_NDARRAY_SRC, ctx)
            }
        }
    }

    /// brain_numpy_utils.infer_numpy_attribute / infer_numpy_name:
    /// `extract_node(sources[name]).infer(context=context)` — a FRESH
    /// template module per tip run (module name '' -> qname ".array" etc.),
    /// inferred with the LIVE context (counter bumps included).
    fn tip_numpy_extract(&self, source: &str, ctx: &Rc<Ctx>) -> Option<Flow> {
        let mid = self.build_template_module(source, "")?;
        let md = self.md(mid);
        let last = match &md.tree.nodes[pyast::NodeId::MODULE.idx()].kind {
            NodeKind::Module(d) => *d.body.last()?,
            _ => return None,
        };
        let g = GNode { m: mid, n: last };
        Some(self.infer(g, ctx))
    }

    fn looks_like_typing_subscript(&self, value: GNode) -> bool {
        let md = self.md(value.m);
        match &md.tree.nodes[value.n.idx()].kind {
            NodeKind::Name { name } => TYPING_MEMBERS.contains(&md.tree.s(*name)),
            NodeKind::Attribute { attrname, .. } => {
                TYPING_MEMBERS.contains(&md.tree.s(*attrname))
            }
            NodeKind::Subscript { value: v, .. } => {
                self.looks_like_typing_subscript(GNode { m: value.m, n: *v })
            }
            _ => false,
        }
    }

    /// brain_typing.infer_typing_typevar_or_newtype
    fn tip_typing_typevar(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        if let Some(cached) = self.typing_tip_cache.borrow().get(&node) {
            return Some(Flow::ok(cached.clone()));
        }
        let md = self.md(node.m);
        let (func, args) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, args, .. } => (GNode { m: node.m, n: *func }, args.clone()),
            _ => return None,
        };
        let f = self.infer(func, &copy_context(Some(ctx)));
        let q = self.value_qname(f.vals.first()?)?;
        if !matches!(
            q.as_str(),
            "typing.TypeVar" | "typing.NewType" | "typing_extensions.TypeVar"
        ) {
            return None;
        }
        let first = args.first()?;
        let typename = match &md.tree.nodes[first.idx()].kind {
            NodeKind::Const(ConstValue::Str(s)) => s.to_string(),
            NodeKind::JoinedStr { .. } => return None,
            _ => return None, // as_string of non-Const rarely a valid identifier
        };
        if !typename
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || typename.is_empty()
        {
            return None;
        }
        let src = TYPING_TYPE_TEMPLATE.replace("{0}", &typename);
        let mid = self.build_template_module(&src, "")?;
        let sym = self.sym(&typename);
        let tmd = self.md(mid);
        let cls = {
            let locals = tmd.locals.borrow();
            locals
                .get(&NodeId::MODULE)
                .and_then(|l| l.get(&sym))
                .and_then(|v| v.first().copied())?
        };
        let vals = vec![Value::Node(cls)];
        self.typing_tip_cache.borrow_mut().insert(node, vals.clone());
        Some(Flow::ok(vals))
    }

    /// brain_typing.infer_typing_alias + infer_special_alias
    fn tip_typing_alias(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        if let Some(cached) = self.typing_tip_cache.borrow().get(&node) {
            return Some(Flow::ok(cached.clone()));
        }
        let md = self.md(node.m);
        // parent must be single-target Assign to an AssignName
        let parent = self.parent(node)?;
        let target_name = match &md.tree.nodes[parent.n.idx()].kind {
            NodeKind::Assign { targets, .. } if targets.len() == 1 => {
                match &md.tree.nodes[targets[0].idx()].kind {
                    NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                    _ => return None,
                }
            }
            _ => return None,
        };
        let (args, is_special) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, args, .. } => {
                let special = matches!(
                    &md.tree.nodes[func.idx()].kind,
                    NodeKind::Name { name } if {
                        let n = md.tree.s(*name);
                        n == "_TupleType" || n == "_CallableType"
                    }
                );
                (args.clone(), special)
            }
            _ => return None,
        };
        let res = self
            .infer(GNode { m: node.m, n: args[0] }, &copy_context(Some(ctx)))
            .vals
            .first()
            .cloned();
        let base: Option<GNode> = match res {
            Some(Value::Node(g))
                if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) =>
            {
                Some(g)
            }
            _ => None,
        };
        // subscriptable: special aliases always; _alias when args[1] Const > 0
        let subscriptable = is_special
            || args.get(1).map(|&a| match &md.tree.nodes[a.idx()].kind {
                NodeKind::Const(ConstValue::Int(pyast::tree::IntValue::Small(i))) => *i > 0,
                NodeKind::Const(ConstValue::Bool(b)) => *b,
                _ => false,
            }) == Some(true);
        // build via source template; module named like the origin module so
        // qname matches (typing.Set etc.)
        let modname = md.name.clone();
        let mut src = String::new();
        let mut base_clause = String::new();
        if let Some(b) = base {
            let bmd = self.md(b.m);
            let bname = self.node_name(b)?;
            // base importable only when top-level in its module
            let top_level = self
                .parent(b)
                .map(|p| self.frame(p))
                .map(|f| f.n == NodeId::MODULE)
                .unwrap_or(false);
            if top_level && bmd.name != modname {
                src.push_str(&format!(
                    "from {} import {} as _alias_base
",
                    bmd.name, bname
                ));
                base_clause = "(_alias_base)".to_string();
            } else if bmd.name == "builtins" {
                base_clause = format!("({bname})");
            }
        }
        src.push_str(&format!("class {target_name}{base_clause}:
"));
        if subscriptable {
            src.push_str("    @classmethod
    def __class_getitem__(cls, item):
        return cls
");
        } else {
            src.push_str("    pass
");
        }
        let mid = self.build_template_module(&src, &modname)?;
        let sym = self.sym(&target_name);
        let tmd = self.md(mid);
        let cls = {
            let locals = tmd.locals.borrow();
            locals
                .get(&NodeId::MODULE)
                .and_then(|l| l.get(&sym))
                .and_then(|v| v.last().copied())?
        };
        let vals = vec![Value::Node(cls)];
        self.typing_tip_cache.borrow_mut().insert(node, vals.clone());
        Some(Flow::ok(vals))
    }

    /// brain_typing.infer_typing_attr (Subscript)
    fn tip_typing_subscript(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        if let Some(cached) = self.typing_tip_cache.borrow().get(&node) {
            return Some(Flow::ok(cached.clone()));
        }
        let md = self.md(node.m);
        let value = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Subscript { value, .. } => GNode { m: node.m, n: *value },
            _ => return None,
        };
        let first = self
            .infer(value, &Ctx::new())
            .vals
            .first()
            .cloned()?;
        let q = self.value_qname(&first)?;
        if !q.starts_with("typing.") || TYPING_ALIAS_QNAMES.contains(&q.as_str()) {
            return None;
        }
        if let Value::Node(g) = &first {
            if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_)))
                && matches!(
                    q.as_str(),
                    "typing.Generic" | "typing.Annotated" | "typing_extensions.Annotated"
                )
            {
                // subscriptable via injected __class_getitem__
                let cg = self.sym("__class_getitem__");
                let already = !self.class_locals_get(*g, cg).is_empty();
                if !already {
                    if let Some(tmid) = self.build_template_module(
                        "class _CG:
    @classmethod
    def __class_getitem__(cls, item):
        return cls
",
                        "",
                    ) {
                        let tmd = self.md(tmid);
                        let csym = self.sym("_CG");
                        let cls_g = {
                            let locals = tmd.locals.borrow();
                            locals
                                .get(&NodeId::MODULE)
                                .and_then(|l| l.get(&csym))
                                .and_then(|v| v.first().copied())
                        };
                        if let Some(cls_g) = cls_g {
                            let func = self.class_locals_get(cls_g, cg);
                            if let Some(&f) = func.first() {
                                let gmd = self.md(g.m);
                                gmd.locals
                                    .borrow_mut()
                                    .entry(g.n)
                                    .or_default()
                                    .insert(cg, vec![f]);
                            }
                        }
                    }
                }
                let vals = vec![first.clone()];
                self.typing_tip_cache.borrow_mut().insert(node, vals.clone());
                return Some(Flow::ok(vals));
            }
        }
        let last_seg = q.rsplit('.').next().unwrap_or("X").to_string();
        if !last_seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || last_seg.is_empty()
        {
            return None;
        }
        let src = TYPING_TYPE_TEMPLATE.replace("{0}", &last_seg);
        let mid = self.build_template_module(&src, "")?;
        let sym = self.sym(&last_seg);
        let tmd = self.md(mid);
        let cls = {
            let locals = tmd.locals.borrow();
            locals
                .get(&NodeId::MODULE)
                .and_then(|l| l.get(&sym))
                .and_then(|v| v.first().copied())?
        };
        let vals = vec![Value::Node(cls)];
        self.typing_tip_cache.borrow_mut().insert(node, vals.clone());
        Some(Flow::ok(vals))
    }

    /// brain_typing.infer_typedDict
    fn tip_typeddict_func(&self, node: GNode) -> Option<Flow> {
        if let Some(cached) = self.typing_tip_cache.borrow().get(&node) {
            return Some(Flow::ok(cached.clone()));
        }
        let modname = self.md(node.m).name.clone();
        let mid = self.build_template_module("class TypedDict(dict):
    pass
", &modname)?;
        let sym = self.sym("TypedDict");
        let tmd = self.md(mid);
        let cls = {
            let locals = tmd.locals.borrow();
            locals
                .get(&NodeId::MODULE)
                .and_then(|l| l.get(&sym))
                .and_then(|v| v.first().copied())?
        };
        let vals = vec![Value::Node(cls)];
        self.typing_tip_cache.borrow_mut().insert(node, vals.clone());
        Some(Flow::ok(vals))
    }

    fn run_builtin_tip(&self, name: &str, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let (args, kws) = self.call_parts(node);
        match name {
            "bool" => {
                if args.len() > 1 {
                    return None;
                }
                if args.is_empty() {
                    return Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(
                        false,
                    )))));
                }
                let f = self.infer(args[0], &copy_context(Some(ctx)));
                let first = match f.vals.first() {
                    Some(v) => v.clone(),
                    None => return Some(Flow::uninferable()),
                };
                if first.is_uninferable() {
                    return Some(Flow::uninferable());
                }
                match self.bool_value(&first, ctx) {
                    Some(b) => Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(b))))),
                    None => Some(Flow::uninferable()),
                }
            }
            "super" => self.tip_super(node, ctx, &args),
            "callable" => {
                if args.len() != 1 {
                    return None;
                }
                let f = self.infer(args[0], &copy_context(Some(ctx)));
                let first = match f.vals.first() {
                    Some(v) => v.clone(),
                    None => return Some(Flow::uninferable()),
                };
                if first.is_uninferable() {
                    return Some(Flow::uninferable());
                }
                Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(
                    self.value_callable(&first, ctx),
                )))))
            }
            "property" => {
                if args.is_empty() {
                    return None;
                }
                let f = self.infer(args[0], &copy_context(Some(ctx)));
                match f.vals.first() {
                    Some(Value::Node(g))
                        if self.kind_is(*g, |k| {
                            matches!(
                                k,
                                NodeKind::FunctionDef(_)
                                    | NodeKind::AsyncFunctionDef(_)
                                    | NodeKind::Lambda(_)
                            )
                        }) =>
                    {
                        Some(Flow::one(Value::Property { func: *g }))
                    }
                    _ => None,
                }
            }
            "getattr" => {
                let (obj, attr) = self.tip_getattr_args(&args, ctx)?;
                match (obj, attr) {
                    (Value::Uninferable, _) | (_, None) => Some(Flow::uninferable()),
                    (obj, Some(attr)) => {
                        let sym = self.sym(&attr);
                        match self.igetattr_value(&obj, sym, Some(ctx)) {
                            Ok(f) if !f.vals.is_empty() => {
                                Some(Flow::one(f.vals[0].clone()))
                            }
                            _ => {
                                if args.len() == 3 {
                                    let f = self.infer(args[2], &copy_context(Some(ctx)));
                                    f.vals.first().map(|v| Flow::one(v.clone()))
                                } else {
                                    None
                                }
                            }
                        }
                    }
                }
            }
            "hasattr" => {
                let (obj, attr) = self.tip_getattr_args(&args, ctx)?;
                match (obj, attr) {
                    (Value::Uninferable, _) | (_, None) => Some(Flow::uninferable()),
                    (obj, Some(attr)) => {
                        let sym = self.sym(&attr);
                        match self.value_getattr(&obj, sym, ctx) {
                            Ok(_) => Some(Flow::one(Value::SynthConst(Rc::new(
                                ConstValue::Bool(true),
                            )))),
                            Err(ErrKind::Attribute) => Some(Flow::one(Value::SynthConst(
                                Rc::new(ConstValue::Bool(false)),
                            ))),
                            Err(_) => Some(Flow::uninferable()),
                        }
                    }
                }
            }
            "tuple" => self.tip_container(node, &args, &kws, SeqKind::Tuple, ctx),
            "set" => self.tip_container(node, &args, &kws, SeqKind::Set, ctx),
            "list" => self.tip_container(node, &args, &kws, SeqKind::List, ctx),
            "frozenset" => {
                let f = self.tip_container(node, &args, &kws, SeqKind::Set, ctx)?;
                let mapped: Vec<Value> = f
                    .vals
                    .into_iter()
                    .map(|v| match v {
                        Value::SynthSeq { elems, .. } => Value::FrozenSet { elems },
                        other => other,
                    })
                    .collect();
                Some(Flow::ok(mapped))
            }
            "dict" => self.tip_dict(node, ctx),
            "type" => {
                if args.len() != 1 {
                    return None;
                }
                // helpers.object_type: set(_object_type(node, ctx)) over ALL
                // inferred values; len != 1 -> Uninferable; InferenceError ->
                // Uninferable. Function/method/module types are FRESH proxy
                // classes per occurrence (never equal); class metaclasses /
                // _proxied dedupe by node identity; Uninferable is a
                // singleton (helpers.py _object_type/object_type).
                Some(Flow::one(self.object_type_of_node(args[0], ctx)))
            }
            "slice" => {
                if args.is_empty() || args.len() > 3 {
                    return None;
                }
                let mut bounds: [Option<ConstValue>; 3] = [None, None, None];
                for (i, &a) in args.iter().enumerate() {
                    let v = self.safe_infer(a, &copy_context(Some(ctx)))?;
                    let c = self.value_const(&v)?;
                    match c {
                        ConstValue::None | ConstValue::Int(_) => bounds[i] = Some(c),
                        _ => return None,
                    }
                }
                Some(Flow::one(Value::SynthSlice {
                    bounds: Rc::new(bounds),
                }))
            }
            "isinstance" | "issubclass" => self.tip_isinstance(name, node, ctx),
            "len" => {
                let site = self.call_site_of_call(node, ctx);
                if !site.keyword_arguments().is_empty() {
                    return None;
                }
                let pos = site.positional_arguments();
                if pos.len() != 1 {
                    return None;
                }
                let len = self.object_len(&pos[0], ctx)?;
                Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Int(
                    IntValue::Small(len),
                )))))
            }
            "str" => {
                let site = self.call_site_of_call(node, ctx);
                if !site.keyword_arguments().is_empty() {
                    return None;
                }
                Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Str(
                    "".into(),
                )))))
            }
            "int" => {
                let site = self.call_site_of_call(node, ctx);
                if !site.keyword_arguments().is_empty() {
                    return None;
                }
                let pos = site.positional_arguments();
                if let Some(first) = pos.first() {
                    let f = self.infer_nv(first, &copy_context(Some(ctx)));
                    let first_value = f.vals.first()?.clone();
                    if first_value.is_uninferable() {
                        return None;
                    }
                    if let Some(c) = self.value_const(&first_value) {
                        match c {
                            ConstValue::Int(_) | ConstValue::Bool(_) => {
                                let i = match c {
                                    ConstValue::Int(IntValue::Small(i)) => i,
                                    ConstValue::Bool(b) => b as i64,
                                    _ => 0,
                                };
                                return Some(Flow::one(Value::SynthConst(Rc::new(
                                    ConstValue::Int(IntValue::Small(i)),
                                ))));
                            }
                            ConstValue::Str(s) => {
                                let v = s.trim().parse::<i64>().unwrap_or(0);
                                return Some(Flow::one(Value::SynthConst(Rc::new(
                                    ConstValue::Int(IntValue::Small(v)),
                                ))));
                            }
                            _ => {}
                        }
                    }
                }
                Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Int(
                    IntValue::Small(0),
                )))))
            }
            _ => None,
        }
    }

    fn tip_getattr_args(
        &self,
        args: &[GNode],
        ctx: &Rc<Ctx>,
    ) -> Option<(Value, Option<String>)> {
        if args.len() != 2 && args.len() != 3 {
            return None;
        }
        let obj = self.infer(args[0], &copy_context(Some(ctx))).vals.first()?.clone();
        let attr = self.infer(args[1], &copy_context(Some(ctx))).vals.first()?.clone();
        if obj.is_uninferable() || attr.is_uninferable() {
            return Some((Value::Uninferable, None));
        }
        match self.value_const(&attr) {
            Some(ConstValue::Str(s)) => Some((obj, Some(s.to_string()))),
            _ => None,
        }
    }

    /// non-inferring getattr over a value (for hasattr)
    pub fn value_getattr(&self, owner: &Value, name: GSym, ctx: &Rc<Ctx>) -> Result<Vec<NV>, ErrKind> {
        match owner {
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Module(_) => self.module_getattr(g.m, name, false),
                    NodeKind::ClassDef(_) => self.class_getattr(*g, name, Some(ctx), true),
                    NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::Lambda(_) => {
                        // function getattr: instance attrs + model
                        if self
                            .iattrs
                            .borrow()
                            .get(g)
                            .map(|m| m.contains_key(&name))
                            .unwrap_or(false)
                        {
                            return Ok(vec![]);
                        }
                        let names = [
                            "__name__",
                            "__doc__",
                            "__qualname__",
                            "__defaults__",
                            "__annotations__",
                            "__dict__",
                            "__kwdefaults__",
                            "__module__",
                            "__get__",
                        ];
                        if names.contains(&self.sname(name).as_str()) {
                            Ok(vec![])
                        } else {
                            Err(ErrKind::Attribute)
                        }
                    }
                    _ => self.instance_getattr(owner, name, Some(ctx), true),
                }
            }
            Value::Uninferable => Ok(vec![]),
            _ => self.instance_getattr(owner, name, Some(ctx), true),
        }
    }

    fn tip_super(&self, node: GNode, ctx: &Rc<Ctx>, args: &[GNode]) -> Option<Flow> {
        if args.len() == 1 {
            return None;
        }
        let scope = self.scope(node);
        if !self.kind_is(scope, |k| {
            matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
        }) {
            return None;
        }
        let ftype = self.func_type(scope);
        if !matches!(ftype, crate::graph::FType::Method | crate::graph::FType::ClassMethod) {
            return None;
        }
        // get_wrapping_class: nearest ClassDef frame above
        let mut cls = None;
        let mut cur = scope;
        while let Some(p) = self.parent(cur) {
            let f = self.frame(p);
            if self.kind_is(f, |k| matches!(k, NodeKind::ClassDef(_))) {
                cls = Some(f);
                break;
            }
            cur = f;
        }
        let cls = cls?;
        let (mro_pointer, mro_type): (Value, Value) = if args.is_empty() {
            let t = if ftype == crate::graph::FType::ClassMethod {
                Value::Node(cls)
            } else {
                self.instantiate_class(cls)
            };
            (Value::Node(cls), t)
        } else {
            let p = self.infer(args[0], &copy_context(Some(ctx))).vals.first()?.clone();
            let t = self.infer(args[1], &copy_context(Some(ctx))).vals.first()?.clone();
            (p, t)
        };
        if mro_pointer.is_uninferable() || mro_type.is_uninferable() {
            return None;
        }
        let pointer = match mro_pointer {
            Value::Node(g) if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) => g,
            _ => return None,
        };
        Some(Flow::one(Value::Super {
            mro_pointer: pointer,
            mro_type: Rc::new(mro_type),
            self_class: cls,
            scope,
        }))
    }

    fn tip_isinstance(&self, name: &str, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let site = self.call_site_of_call(node, ctx);
        if !site.keyword_arguments().is_empty() {
            return None;
        }
        let pos = site.positional_arguments();
        if pos.len() != 2 {
            return None;
        }
        let obj = self.infer_nv(&pos[0], &copy_context(Some(ctx))).vals.first()?.clone();
        let obj_type: GNode = if name == "isinstance" {
            self.object_type(&obj, ctx)?
        } else {
            match obj {
                Value::Node(g) if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) => g,
                _ => return None,
            }
        };
        // second arg: class or tuple of classes
        let cls_v = self.safe_infer_nv(&pos[1], ctx)?;
        let classes: Vec<Value> = match self.value_elts(&cls_v) {
            Some(elts) => elts
                .iter()
                .map(|e| match e {
                    Value::Node(g) => self
                        .safe_infer(*g, &copy_context(Some(ctx)))
                        .unwrap_or(Value::Uninferable),
                    o => o.clone(),
                })
                .collect(),
            None => vec![cls_v],
        };
        let mro = self.mro(obj_type, None).ok()?;
        for klass in &classes {
            if klass.is_uninferable() {
                return None; // AstroidTypeError -> UseInferenceDefault
            }
            if let Value::Node(kg) = klass {
                if mro.contains(kg) {
                    return Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(
                        true,
                    )))));
                }
            }
        }
        Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(
            false,
        )))))
    }

    fn safe_infer_nv(&self, nv: &NV, ctx: &Rc<Ctx>) -> Option<Value> {
        match nv {
            NV::N(g) => self.safe_infer(*g, &copy_context(Some(ctx))),
            NV::V(v) => Some(v.clone()),
        }
    }

    /// helpers.object_len subset
    fn object_len(&self, nv: &NV, ctx: &Rc<Ctx>) -> Option<i64> {
        let inferred = self.safe_infer_nv(nv, ctx)?;
        if inferred.is_uninferable() {
            return None;
        }
        if let Some(c) = self.value_const(&inferred) {
            return match c {
                ConstValue::Str(s) => Some(s.chars().count() as i64),
                ConstValue::Bytes(b) => Some(b.len() as i64),
                _ => None,
            };
        }
        if let Some(elts) = self.value_elts(&inferred) {
            return Some(elts.len() as i64);
        }
        if let Some(items) = self.value_dict_items(&inferred) {
            return Some(items.len() as i64);
        }
        // __len__ through the type
        let t = self.object_type(&inferred, ctx)?;
        let sym = self.sym("__len__");
        let f = self.class_igetattr(t, sym, Some(ctx), true).ok()?;
        let len_call = f.vals.first()?.clone();
        let res = self.infer_call_result(&len_call, None, Some(&copy_context(Some(ctx))));
        match res.vals.first() {
            Some(v) => match self.value_const(v) {
                Some(ConstValue::Int(IntValue::Small(i))) => Some(i),
                _ => match v {
                    Value::Inst { cls } if self.is_subtype_of(*cls, "builtins.int", None) => {
                        Some(0)
                    }
                    _ => None,
                },
            },
            None => Some(0),
        }
    }

    fn tip_container(
        &self,
        _node: GNode,
        args: &[GNode],
        _kws: &[(Option<GSym>, GNode)],
        kind: SeqKind,
        ctx: &Rc<Ctx>,
    ) -> Option<Flow> {
        if args.is_empty() {
            return Some(Flow::one(Value::SynthSeq {
                kind,
                elems: Rc::new(Vec::new()),
            }));
        }
        if args.len() > 1 {
            return None;
        }
        let arg = args[0];
        // transform on the raw node first
        let md = self.md(arg.m);
        let node_matches = match (&md.tree.nodes[arg.n.idx()].kind, kind) {
            (NodeKind::List { .. }, SeqKind::List)
            | (NodeKind::Tuple { .. }, SeqKind::Tuple)
            | (NodeKind::Set { .. }, SeqKind::Set) => true,
            _ => false,
        };
        if node_matches {
            return Some(Flow::one(Value::Node(arg)));
        }
        let transformed = self.container_transform(&Value::Node(arg), kind, ctx);
        if let Some(t) = transformed {
            return Some(Flow::one(t));
        }
        let inferred = self.infer(arg, &copy_context(Some(ctx))).vals.first()?.clone();
        if inferred.is_uninferable() {
            return None;
        }
        let transformed = self.container_transform(&inferred, kind, ctx)?;
        Some(Flow::one(transformed))
    }

    /// _container_generic_transform
    fn container_transform(&self, arg: &Value, kind: SeqKind, ctx: &Rc<Ctx>) -> Option<Value> {
        // same class -> as-is
        match (arg, kind) {
            (Value::SynthSeq { kind: k, .. }, _) if *k == kind => return Some(arg.clone()),
            (Value::Node(g), _) => {
                let md = self.md(g.m);
                let matches = match (&md.tree.nodes[g.n.idx()].kind, kind) {
                    (NodeKind::List { .. }, SeqKind::List)
                    | (NodeKind::Tuple { .. }, SeqKind::Tuple)
                    | (NodeKind::Set { .. }, SeqKind::Set) => true,
                    _ => false,
                };
                if matches {
                    return Some(arg.clone());
                }
            }
            _ => {}
        }
        // iterables
        if let Some(elts) = self.value_elts(arg) {
            let mut out = Vec::new();
            for e in elts {
                let v = match &e {
                    Value::Node(g) => self.safe_infer(*g, &copy_context(Some(ctx))),
                    other => Some(other.clone()),
                };
                if let Some(v) = v {
                    out.push(v);
                }
            }
            return Some(Value::SynthSeq {
                kind,
                elems: Rc::new(out),
            });
        }
        // dict -> keys (must be Const)
        if let Some(items) = self.value_dict_items(arg) {
            let mut out = Vec::new();
            for (k, _) in items {
                let kc = match &k {
                    Value::Node(g) => {
                        let md = self.md(g.m);
                        match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::Const(c) => Some(c.clone()),
                            _ => None,
                        }
                    }
                    Value::SynthConst(c) => Some((**c).clone()),
                    _ => None,
                }?;
                out.push(Value::SynthConst(Rc::new(kc)));
            }
            return Some(Value::SynthSeq {
                kind,
                elems: Rc::new(out),
            });
        }
        // Const str/bytes
        if let Some(c) = self.value_const(arg) {
            match c {
                ConstValue::Str(s) => {
                    return Some(Value::SynthSeq {
                        kind,
                        elems: Rc::new(
                            s.chars()
                                .map(|ch| {
                                    Value::SynthConst(Rc::new(ConstValue::Str(
                                        ch.to_string().into(),
                                    )))
                                })
                                .collect(),
                        ),
                    })
                }
                ConstValue::Bytes(b) => {
                    return Some(Value::SynthSeq {
                        kind,
                        elems: Rc::new(
                            b.iter()
                                .map(|&x| {
                                    Value::SynthConst(Rc::new(ConstValue::Int(IntValue::Small(
                                        x as i64,
                                    ))))
                                })
                                .collect(),
                        ),
                    })
                }
                _ => return None,
            }
        }
        None
    }

    fn tip_dict(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let site = self.call_site_of_call(node, ctx);
        if site.has_invalid_arguments() || site.has_invalid_keywords() {
            return None;
        }
        let args = site.positional_arguments();
        let kwargs = site.keyword_arguments();
        let mut items: Vec<(Value, Value)> = Vec::new();
        if args.is_empty() && kwargs.is_empty() {
            return Some(Flow::one(Value::SynthDict {
                items: Rc::new(items),
            }));
        }
        let kw_items = |kwargs: &[(GSym, NV)]| -> Vec<(Value, Value)> {
            kwargs
                .iter()
                .map(|(k, v)| {
                    (
                        Value::SynthConst(Rc::new(ConstValue::Str(self.sname(*k).into()))),
                        match v {
                            NV::N(g) => Value::Node(*g),
                            NV::V(val) => val.clone(),
                        },
                    )
                })
                .collect()
        };
        if !kwargs.is_empty() && args.is_empty() {
            items = kw_items(&kwargs);
        } else if args.len() == 1 {
            let elts = self.dict_arg_elts(&args[0], ctx)?;
            items = elts;
            if !kwargs.is_empty() {
                items.extend(kw_items(&kwargs));
            }
        } else {
            return None;
        }
        Some(Flow::one(Value::SynthDict {
            items: Rc::new(items),
        }))
    }

    fn dict_arg_elts(&self, arg: &NV, ctx: &Rc<Ctx>) -> Option<Vec<(Value, Value)>> {
        let inferred = self.safe_infer_nv(arg, ctx)?;
        if let Some(items) = self.value_dict_items(&inferred) {
            // each key must be Const-ish per _get_elts
            return Some(items);
        }
        let elts = self.value_elts(&inferred)?;
        let mut out = Vec::new();
        for e in elts {
            let pair = self.value_elts(&e)?;
            if pair.len() != 2 {
                return None;
            }
            out.push((pair[0].clone(), pair[1].clone()));
        }
        Some(out)
    }

    fn tip_dict_fromkeys(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let site = self.call_site_of_call(node, ctx);
        if !site.keyword_arguments().is_empty() {
            return None;
        }
        let pos = site.positional_arguments();
        if pos.is_empty() || pos.len() > 2 {
            return None;
        }
        let default = Value::SynthConst(Rc::new(ConstValue::None));
        let empty = || {
            Some(Flow::one(Value::SynthDict {
                items: Rc::new(Vec::new()),
            }))
        };
        let inferred = match &pos[0] {
            NV::N(g) => match self.infer(*g, &copy_context(Some(ctx))).vals.first() {
                Some(v) => v.clone(),
                None => return empty(),
            },
            NV::V(v) => v.clone(),
        };
        if inferred.is_uninferable() {
            return empty();
        }
        // container of Consts / str / dict keys
        let keys: Vec<Value> = if let Some(elts) = self.value_elts(&inferred) {
            for e in &elts {
                let is_const = matches!(e, Value::SynthConst(_))
                    || matches!(e, Value::Node(g)
                        if self.kind_is(*g, |k| matches!(k, NodeKind::Const(_))));
                if !is_const {
                    return empty();
                }
            }
            elts
        } else if let Some(items) = self.value_dict_items(&inferred) {
            let keys: Vec<Value> = items.iter().map(|(k, _)| k.clone()).collect();
            for e in &keys {
                let is_const = matches!(e, Value::SynthConst(_))
                    || matches!(e, Value::Node(g)
                        if self.kind_is(*g, |k| matches!(k, NodeKind::Const(_))));
                if !is_const {
                    return empty();
                }
            }
            keys
        } else if let Some(c) = self.value_const(&inferred) {
            match c {
                ConstValue::Str(s) => s
                    .chars()
                    .map(|ch| Value::SynthConst(Rc::new(ConstValue::Str(ch.to_string().into()))))
                    .collect(),
                ConstValue::Bytes(b) => b
                    .iter()
                    .map(|&x| {
                        Value::SynthConst(Rc::new(ConstValue::Int(IntValue::Small(x as i64))))
                    })
                    .collect(),
                _ => return empty(),
            }
        } else {
            return empty();
        };
        let items: Vec<(Value, Value)> = keys.into_iter().map(|k| (k, default.clone())).collect();
        Some(Flow::one(Value::SynthDict {
            items: Rc::new(items),
        }))
    }

    fn tip_copy_method(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(node.m);
        let expr = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, .. } => match &md.tree.nodes[func.idx()].kind {
                NodeKind::Attribute { expr, .. } => GNode { m: node.m, n: *expr },
                _ => return None,
            },
            _ => return None,
        };
        let f = self.infer(expr, &copy_context(Some(ctx)));
        if f.vals.is_empty() {
            return None;
        }
        let all_containers = f.vals.iter().all(|v| {
            matches!(
                v,
                Value::SynthDict { .. } | Value::SynthSeq { .. } | Value::FrozenSet { .. }
            ) || matches!(v, Value::Node(g)
                if self.kind_is(*g, |k| matches!(k,
                    NodeKind::Dict { .. } | NodeKind::List { .. } | NodeKind::Set { .. })))
        });
        if !all_containers {
            return None;
        }
        Some(Flow::ok(f.vals))
    }

    fn tip_str_format(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(node.m);
        let expr = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, .. } => match &md.tree.nodes[func.idx()].kind {
                NodeKind::Attribute { expr, .. } => GNode { m: node.m, n: *expr },
                _ => return None,
            },
            _ => return None,
        };
        let template = match &md.tree.nodes[expr.n.idx()].kind {
            NodeKind::Const(ConstValue::Str(s)) => s.to_string(),
            NodeKind::Name { .. } => match self
                .safe_infer(expr, &Ctx::new())
                .and_then(|v| self.value_const(&v))
            {
                Some(ConstValue::Str(s)) => s.to_string(),
                _ => return Some(Flow::uninferable()),
            },
            _ => return Some(Flow::uninferable()),
        };
        let site = self.call_site_of_call(node, ctx);
        let mut pos_values: Vec<String> = Vec::new();
        for p in site.positional_arguments() {
            let v = self.safe_infer_nv(&p, ctx)?;
            match self.value_const(&v) {
                Some(c) => pos_values.push(const_format_value(&c)?),
                None => return Some(Flow::uninferable()),
            }
        }
        let mut kw_values: Vec<(String, String)> = Vec::new();
        for (k, v) in site.keyword_arguments() {
            let v = self.safe_infer_nv(&v, ctx)?;
            match self.value_const(&v) {
                Some(c) => kw_values.push((self.sname(k), const_format_value(&c)?)),
                None => return Some(Flow::uninferable()),
            }
        }
        match simple_str_format(&template, &pos_values, &kw_values) {
            Some(s) => Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Str(
                s.into(),
            ))))),
            None => Some(Flow::uninferable()),
        }
    }

    fn tip_partial(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let site = self.call_site_of_call(node, ctx);
        let pos = site.positional_arguments();
        if pos.is_empty() {
            return None;
        }
        let kwargs = site.keyword_arguments();
        if pos.len() == 1 && kwargs.is_empty() {
            return None;
        }
        let wrapped = match &pos[0] {
            NV::N(g) => self.infer(*g, &copy_context(Some(ctx))).vals.first().cloned(),
            NV::V(v) => Some(v.clone()),
        }?;
        let func = match wrapped {
            Value::Node(g)
                if self.kind_is(g, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                }) =>
            {
                g
            }
            _ => return None,
        };
        // keyword names must be parameters of the wrapped function
        let spec = self.arg_spec(func);
        if let Some(spec) = &spec {
            let mut param_names: Vec<GSym> = Vec::new();
            for a in spec
                .args
                .iter()
                .chain(spec.posonlyargs.iter())
                .chain(spec.kwonlyargs.iter())
            {
                if let Some(n) = self.assign_name_of(*a) {
                    param_names.push(n);
                }
            }
            for (k, _) in &kwargs {
                if !param_names.contains(k) {
                    return None;
                }
            }
        }
        let filled_args: Vec<GNode> = pos[1..]
            .iter()
            .filter_map(|nv| match nv {
                NV::N(g) => Some(*g),
                NV::V(_) => None,
            })
            .collect();
        let filled_keywords: Vec<(GSym, GNode)> = kwargs
            .iter()
            .filter_map(|(k, v)| match v {
                NV::N(g) => Some((*k, *g)),
                NV::V(_) => None,
            })
            .collect();
        Some(Flow::one(Value::Partial {
            func,
            filled_args: Rc::new(filled_args),
            filled_keywords: Rc::new(filled_keywords),
        }))
    }
}

fn const_format_value(c: &ConstValue) -> Option<String> {
    Some(match c {
        ConstValue::Str(s) => s.to_string(),
        ConstValue::Int(IntValue::Small(i)) => i.to_string(),
        ConstValue::Int(IntValue::Big(s)) => s.to_string(),
        ConstValue::Float(f) => pyast::pyrepr::repr_float(*f),
        ConstValue::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        ConstValue::None => "None".to_string(),
        _ => return None,
    })
}

/// str.format with auto/explicit numbering and named fields (no format
/// specs beyond plain {}; specs bail to None -> Uninferable)
fn simple_str_format(
    template: &str,
    pos: &[String],
    kw: &[(String, String)],
) -> Option<String> {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    let mut auto = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let mut field = String::new();
                let mut depth = 1;
                for c2 in chars.by_ref() {
                    if c2 == '{' {
                        depth += 1;
                    } else if c2 == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    field.push(c2);
                }
                if depth != 0 {
                    return None;
                }
                // no conversions / format specs / attribute access support
                if field.contains(':') || field.contains('!') || field.contains('.')
                    || field.contains('[')
                {
                    return None;
                }
                if field.is_empty() {
                    let v = pos.get(auto)?;
                    auto += 1;
                    out.push_str(v);
                } else if field.chars().all(|ch| ch.is_ascii_digit()) {
                    let i: usize = field.parse().ok()?;
                    out.push_str(pos.get(i)?);
                } else {
                    let v = kw.iter().find(|(k, _)| *k == field)?;
                    out.push_str(&v.1);
                }
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    out.push('}');
                } else {
                    return None;
                }
            }
            c => out.push(c),
        }
    }
    Some(out)
}

// ============== brain_namedtuple_enum: namedtuple / typing.NamedTuple ==============

const PY_KEYWORDS: [&str; 35] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break",
    "class", "continue", "def", "del", "elif", "else", "except", "finally",
    "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal",
    "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
];

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

impl Engine {
    fn dotted_of(&self, g: GNode) -> Option<String> {
        let md = self.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
            NodeKind::Attribute { expr, attrname, .. } => {
                let base = self.dotted_of(GNode { m: g.m, n: *expr })?;
                Some(format!("{}.{}", base, md.tree.s(*attrname)))
            }
            _ => None,
        }
    }

    /// brain_namedtuple_enum.infer_named_tuple — Call tip. None =>
    /// UseInferenceDefault.
    fn tip_named_tuple(&self, call: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let cls = self.infer_named_tuple_core(call, ctx)?;
        Some(Flow::one(Value::Node(cls)))
    }

    /// _find_func_form_arguments member: positional or keyword,
    /// `_infer_first` (single pull; Uninferable -> UseInferenceDefault).
    fn func_form_arg(
        &self,
        args: &[GNode],
        kws: &[(Option<GSym>, GNode)],
        position: usize,
        key_name: &str,
        ctx: &Rc<Ctx>,
    ) -> Option<Value> {
        let node = if args.len() > position {
            args[position]
        } else {
            let sym = self.sym(key_name);
            kws.iter().find(|(k, _)| *k == Some(sym))?.1
        };
        match self.infer_first(node, Some(ctx)) {
            Ok(v) if !v.is_uninferable() => Some(v),
            _ => None,
        }
    }

    fn infer_named_tuple_core(&self, call: GNode, ctx: &Rc<Ctx>) -> Option<GNode> {
        let (args, kws) = self.call_parts(call);
        let name_v = self.func_form_arg(&args, &kws, 0, "typename", ctx)?;
        let names_v = self.func_form_arg(&args, &kws, 1, "field_names", ctx)?;
        let name = match self.value_const(&name_v) {
            Some(ConstValue::Str(s)) => s.to_string(),
            _ => return None, // non-str typename fails the checks anyway
        };
        // attributes: str field spec or container of Consts / 2-tuples
        let attributes: Vec<String> = match self.value_const(&names_v) {
            Some(ConstValue::Str(s)) => s
                .replace(',', " ")
                .split_whitespace()
                .map(String::from)
                .collect(),
            _ => {
                // _get_namedtuple_fields (as_string round-trip through
                // extract_node; net effect: Const values stringified)
                let elts = self.value_elts(&names_v).or_else(|| match &names_v {
                    Value::Node(g) => {
                        let md = self.md(g.m);
                        match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::List { elts, .. }
                            | NodeKind::Tuple { elts, .. }
                            | NodeKind::Set { elts } => Some(
                                elts.iter()
                                    .map(|&e| Value::Node(GNode { m: g.m, n: e }))
                                    .collect(),
                            ),
                            _ => None,
                        }
                    }
                    _ => None,
                })?;
                let mut out = Vec::new();
                for elt in elts {
                    // pairs: (name, type)
                    let first = match &elt {
                        Value::Node(g) => {
                            let md = self.md(g.m);
                            match &md.tree.nodes[g.n.idx()].kind {
                                NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } => {
                                    if elts.len() != 2 {
                                        return None;
                                    }
                                    Value::Node(GNode { m: g.m, n: elts[0] })
                                }
                                NodeKind::Const(_) => elt.clone(),
                                _ => return None,
                            }
                        }
                        Value::SynthSeq { elems, .. } => {
                            if elems.len() != 2 {
                                return None;
                            }
                            elems[0].clone()
                        }
                        Value::SynthConst(_) => elt.clone(),
                        _ => return None,
                    };
                    // str(value): Const str -> the string; other Consts
                    // stringify (and then fail the identifier checks)
                    let v = match &first {
                        Value::Node(g) => {
                            let md = self.md(g.m);
                            match &md.tree.nodes[g.n.idx()].kind {
                                NodeKind::Const(c) => c.clone(),
                                _ => return None,
                            }
                        }
                        Value::SynthConst(c) => (**c).clone(),
                        _ => return None,
                    };
                    out.push(match v {
                        ConstValue::Str(s) => s.to_string(),
                        other => crate::dump::render(self, &Value::SynthConst(Rc::new(other)))
                            .split(':')
                            .nth(2)
                            .unwrap_or("")
                            .to_string(),
                    });
                }
                out
            }
        };
        let attributes: Vec<String> = attributes
            .into_iter()
            .filter(|a| !a.contains(' '))
            .collect();
        // rename: CallSite.infer_argument against the REAL
        // collections.namedtuple (InferenceError/StopIteration -> False)
        let rename = self.namedtuple_rename(call, ctx);
        let attributes = check_namedtuple_attributes(&name, attributes, rename)?;
        if !is_identifier(&name) || PY_KEYWORDS.contains(&name.as_str()) {
            return None;
        }
        // class_node: ClassDef(name, parent=SYNTHETIC_ROOT, lineno=call)
        let (lineno, col) = {
            let md = self.md(call.m);
            let n = &md.tree.nodes[call.n.idx()];
            (n.fromlineno, n.col_offset)
        };
        let (cls, base_slots, _, _) =
            self.build_synth_class("__astroid_synthetic", &name, lineno, col, 1, false, 0);
        self.redirects.borrow_mut().insert(
            GNode { m: cls.m, n: base_slots[0] },
            crate::value::NV::V(Value::Node(self.builtins().tuple)),
        );
        // instance_attrs: EmptyNode-ish placeholders per attribute
        {
            let phs = self.alloc_placeholders(attributes.len());
            let mut ia = self.iattrs.borrow_mut();
            let entry = ia.entry(cls).or_default();
            for (attr, ph) in attributes.iter().zip(phs) {
                entry.insert(self.sym(attr), vec![ph]);
            }
        }
        // fake module (string_build, module name "") with the helpers
        let replace_args = attributes
            .iter()
            .map(|a| format!("{a}=None"))
            .collect::<Vec<_>>()
            .join(", ");
        let field_defs = attributes
            .iter()
            .enumerate()
            .map(|(i, a)| {
                format!(
                    "    {a} = property(lambda self: self[{i}], doc='Alias for field number {i}')"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        // _check_namedtuple_attributes returns a TUPLE: {attributes!r}
        let fields_repr = if attributes.len() == 1 {
            format!("({},)", pyast::pyrepr::repr_str(&attributes[0]))
        } else {
            format!(
                "({})",
                attributes
                    .iter()
                    .map(|a| pyast::pyrepr::repr_str(a))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let src = format!(
            "\nclass {name}(tuple):\n    __slots__ = ()\n    _fields = {fields_repr}\n    def _asdict(self):\n        return self.__dict__\n    @classmethod\n    def _make(cls, iterable, new=tuple.__new__, len=len):\n        return new(cls, iterable)\n    def _replace(self, {replace_args}):\n        return self\n    def __getnewargs__(self):\n        return tuple(self)\n{field_defs}\n    "
        );
        let fake_mid = self.build_template_module(&src, "")?;
        let name_sym = self.sym(&name);
        let fake_cls = {
            let fmd = self.md(fake_mid);
            let locals = fmd.locals.borrow();
            locals
                .get(&NodeId::MODULE)
                .and_then(|l| l.get(&name_sym))
                .and_then(|v| v.first())
                .copied()?
        };
        // copy locals (insertion order: _asdict, _make, _replace, _fields,
        // then the field properties)
        {
            let fmd = self.md(fake_mid);
            let flocals = fmd.locals.borrow();
            let fmap = flocals.get(&fake_cls.n)?;
            let cmd = self.md(cls.m);
            let mut clocals = cmd.locals.borrow_mut();
            let centry = clocals.entry(cls.n).or_default();
            for key in ["_asdict", "_make", "_replace", "_fields"] {
                let sym = self.sym(key);
                if let Some(v) = fmap.get(&sym) {
                    centry.insert(sym, v.clone());
                }
            }
            for attr in &attributes {
                let sym = self.sym(attr);
                if let Some(v) = fmap.get(&sym) {
                    centry.insert(sym, v.clone());
                }
            }
        }
        Some(cls)
    }

    fn namedtuple_rename(&self, call: GNode, ctx: &Rc<Ctx>) -> bool {
        // func = safe_infer(extract_node("import collections;
        // collections.namedtuple")) — the real FunctionDef
        let Ok(collections_mid) = self.ast_from_module_name("collections", true) else {
            return false;
        };
        let nt_sym = self.sym("namedtuple");
        let func = {
            let md = self.md(collections_mid);
            let locals = md.locals.borrow();
            match locals
                .get(&NodeId::MODULE)
                .and_then(|l| l.get(&nt_sym))
                .and_then(|v| v.first())
            {
                Some(&g) => g,
                None => return false,
            }
        };
        if !self.kind_is(func, |k| {
            matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
        }) {
            return false;
        }
        let site = self.call_site_of_call(call, ctx);
        let rename_sym = self.sym("rename");
        let mut first: Option<Value> = None;
        let _ = {
            let first = &mut first;
            self.infer_argument_to(&site, func, rename_sym, ctx, &mut |v| {
                *first = Some(v);
                crate::value::Drive::Stop
            })
        };
        match first {
            Some(v) => self.bool_value(&v, &Ctx::new()) == Some(true),
            None => false,
        }
    }

    /// brain_namedtuple_enum.infer_typing_namedtuple — Call tip.
    fn tip_typing_namedtuple_call(&self, call: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        // func must first-infer to typing.NamedTuple
        let func = {
            let md = self.md(call.m);
            match &md.tree.nodes[call.n.idx()].kind {
                NodeKind::Call { func, .. } => GNode { m: call.m, n: *func },
                _ => return None,
            }
        };
        let f = self.infer_first(func, None).ok()?;
        let q = self.value_qname(&f)?;
        if q != "typing.NamedTuple" && q != "typing_extensions.NamedTuple" {
            return None;
        }
        let (args, _) = self.call_parts(call);
        if args.len() != 2 {
            return None;
        }
        if !self.kind_is(args[1], |k| {
            matches!(k, NodeKind::List { .. } | NodeKind::Tuple { .. })
        }) {
            return None;
        }
        self.tip_named_tuple(call, ctx)
    }

    /// brain_namedtuple_enum.infer_typing_namedtuple_class — ClassDef tip.
    fn tip_typing_namedtuple_class(&self, cls_node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(cls_node.m);
        let (name, body): (String, Vec<NodeId>) = match &md.tree.nodes[cls_node.n.idx()].kind {
            NodeKind::ClassDef(d) => (md.tree.s(d.name).to_string(), d.body.clone()),
            _ => return None,
        };
        let fields: Vec<String> = body
            .iter()
            .filter_map(|&b| match &md.tree.nodes[b.idx()].kind {
                NodeKind::AnnAssign { target, .. } => {
                    match &md.tree.nodes[target.idx()].kind {
                        NodeKind::AssignName { name } => Some(md.tree.s(*name).to_string()),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        // extract_node(f"from collections import namedtuple\n
        //               namedtuple({typename!r}, {fields!r})")
        let src = format!(
            "\nfrom collections import namedtuple\nnamedtuple({}, {})\n",
            pyast::pyrepr::repr_str(&name),
            pyast::pyrepr::repr_str(&fields.join(","))
        );
        let tmpl = self.build_template_module(&src, "")?;
        let call = {
            let tmd = self.md(tmpl);
            // module body: ImportFrom, Expr(Call)
            let mb = match &tmd.tree.nodes[NodeId::MODULE.idx()].kind {
                NodeKind::Module(d) => d.body.clone(),
                _ => return None,
            };
            let expr = *mb.get(1)?;
            match &tmd.tree.nodes[expr.idx()].kind {
                NodeKind::Expr { value } => GNode { m: tmpl, n: *value },
                _ => return None,
            }
        };
        // InferenceError from infer_named_tuple -> InferenceError (the tip
        // raises); UseInferenceDefault -> default
        let generated = self.infer_named_tuple_core(call, ctx)?;
        // copy methods + Assign/ClassDef body entries into generated locals
        {
            let entries: Vec<(GSym, Vec<GNode>)> = {
                let locals = md.locals.borrow();
                match locals.get(&cls_node.n) {
                    Some(map) => map.iter().map(|(k, v)| (*k, v.clone())).collect(),
                    None => Vec::new(),
                }
            };
            let gmd = self.md(generated.m);
            let mut glocals = gmd.locals.borrow_mut();
            let gentry = glocals.entry(generated.n).or_default();
            // mymethods: first local per key that is a FunctionDef
            for (k, v) in &entries {
                if let Some(&first) = v.first() {
                    if self.kind_is(first, |kd| {
                        matches!(kd, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                    }) {
                        gentry.insert(*k, vec![first]);
                    }
                }
            }
            // body Assign targets / nested ClassDefs
            for &b in &body {
                match &md.tree.nodes[b.idx()].kind {
                    NodeKind::Assign { targets, .. } => {
                        for &t in targets {
                            if let NodeKind::AssignName { name } = &md.tree.nodes[t.idx()].kind {
                                let sym = self.g(&md, *name);
                                let from_cls = {
                                    let locals = md.locals.borrow();
                                    locals
                                        .get(&cls_node.n)
                                        .and_then(|l| l.get(&sym))
                                        .cloned()
                                };
                                if let Some(v) = from_cls {
                                    gentry.insert(sym, v);
                                }
                            }
                        }
                    }
                    NodeKind::ClassDef(d) => {
                        let sym = self.g(&md, d.name);
                        gentry.insert(sym, vec![GNode { m: cls_node.m, n: b }]);
                    }
                    _ => {}
                }
            }
        }
        Some(Flow::one(Value::Node(generated)))
    }
}

/// _check_namedtuple_attributes + _get_renamed_namedtuple_attributes.
/// None => UseInferenceDefault (Astroid{Type,Value}Error).
fn check_namedtuple_attributes(
    typename: &str,
    attributes: Vec<String>,
    rename: bool,
) -> Option<Vec<String>> {
    let attributes = if rename {
        let mut names = attributes.clone();
        let mut seen: std::collections::HashSet<String> = Default::default();
        for (i, name) in attributes.iter().enumerate() {
            let invalid = !name.chars().all(|c| c.is_alphanumeric() || c == '_')
                || PY_KEYWORDS.contains(&name.as_str())
                || name.is_empty()
                || name.chars().next().map(|c| c.is_ascii_digit()) == Some(true)
                || name.starts_with('_')
                || seen.contains(name);
            if invalid {
                names[i] = format!("_{i}");
            }
            seen.insert(name.clone());
        }
        names
    } else {
        attributes
    };
    for name in std::iter::once(typename).chain(attributes.iter().map(|s| s.as_str())) {
        if !is_identifier(name) {
            return None;
        }
        if PY_KEYWORDS.contains(&name) {
            return None;
        }
    }
    let mut seen: std::collections::HashSet<&str> = Default::default();
    for name in &attributes {
        if name.starts_with('_') && !rename {
            return None;
        }
        if seen.contains(name.as_str()) {
            return None;
        }
        seen.insert(name);
    }
    Some(attributes)
}


impl Engine {
    /// brain_namedtuple_enum.infer_typing_namedtuple_function: the typing
    /// NamedTuple FunctionDef infers as `_NamedTuple` (extract_node
    /// "from typing import _NamedTuple\n_NamedTuple" -> klass.infer(ctx)).
    fn tip_typing_namedtuple_func(&self, _node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let tmpl = self.build_template_module(
            "from typing import _NamedTuple\n_NamedTuple\n",
            "",
        )?;
        let name_node = {
            let tmd = self.md(tmpl);
            let mb = match &tmd.tree.nodes[NodeId::MODULE.idx()].kind {
                NodeKind::Module(d) => d.body.clone(),
                _ => return None,
            };
            let expr = *mb.get(1)?;
            match &tmd.tree.nodes[expr.idx()].kind {
                NodeKind::Expr { value } => GNode { m: tmpl, n: *value },
                _ => return None,
            }
        };
        let f = self.infer(name_node, &copy_context(Some(ctx)));
        Some(f)
    }
}
