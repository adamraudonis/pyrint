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
    /// Enum("X", "a b") functional calls (brain_namedtuple_enum.infer_enum)
    EnumCall,
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
    /// brain_type: Name "type" directly under a Subscript
    TypeSubscript,
    /// brain_pathlib: <path>.parents[const] (predicate matched at scan time)
    PathlibParents,
    /// brain_dataclasses infer_dataclass_attribute (Unknown placeholders)
    DataclassAttr,
    /// brain_dataclasses infer_dataclass_field_call
    DataclassFieldCall,
    /// brain_re infer_pattern_match: `Pattern = type(...)` in stdlib re
    RePatternMatch,
    /// brain_argparse infer_namespace: argparse.Namespace(...) calls
    ArgparseNamespace,
    /// brain_typing infer_typing_generic_class_pep695 (type_params classes)
    Pep695Generic,
    /// brain_typing infer_typing_cast (cast(typ, val) -> val)
    TypingCast,
    /// brain_statistics infer_statistics_quantiles: yields Uninferable
    StatisticsQuantiles,
    /// brain_random infer_random_sample
    RandomSample,
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
        Tip::EnumCall => (6, 4),
        Tip::TypingNamedTupleCall => (6, 1),
        Tip::TypingNamedTupleClass => (6, 2),
        Tip::TypingNamedTupleFunc => (6, 3),
        Tip::NumpyMember(i) => (7, i),
        Tip::NumpyNdarray => (7, 31),
        Tip::TypeSubscript => (7, 30),
        Tip::PathlibParents => (7, 29),
        Tip::DataclassAttr => (7, 28),
        Tip::DataclassFieldCall => (7, 27),
        Tip::RePatternMatch => (7, 26),
        Tip::ArgparseNamespace => (7, 25),
        Tip::Pep695Generic => (7, 24),
        Tip::TypingCast => (5, 4),
        Tip::StatisticsQuantiles => (7, 23),
        Tip::RandomSample => (7, 22),
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

/// brain_namedtuple_enum.py:325-358 — the infer_enum EnumMeta template
/// (post-dedent _extract_single_node source; rebuilt FRESH per invocation).
const ENUM_META_SRC: &str = "
class EnumMeta(object):
    'docstring'
    def __call__(self, node):
        class EnumAttribute(object):
            name = ''
            value = 0
        return EnumAttribute()
    def __iter__(self):
        class EnumAttribute(object):
            name = ''
            value = 0
        return [EnumAttribute()]
    def __reversed__(self):
        class EnumAttribute(object):
            name = ''
            value = 0
        return (EnumAttribute, )
    def __next__(self):
        return next(iter(self))
    def __getitem__(self, attr):
        class Value(object):
            @property
            def name(self):
                return ''
            @property
            def value(self):
                return attr

        return Value()
    __members__ = ['']
";

/// Container classes appearing in the klass/iterables parameters of the
/// _infer_builtin_container partials (brain_builtin_inference.py:319-360).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContClass {
    List,
    Tuple,
    Set,
    Frozen,
    DictItems,
    DictKeys,
    DictValues,
}

/// the `iterables` whitelist per builtin (brain_builtin_inference.py:319-360)
fn cont_iterables(klass: ContClass) -> &'static [ContClass] {
    match klass {
        // tuple: List, Set, FrozenSet, DictItems, DictKeys, DictValues
        ContClass::Tuple => &[
            ContClass::List,
            ContClass::Set,
            ContClass::Frozen,
            ContClass::DictItems,
            ContClass::DictKeys,
            ContClass::DictValues,
        ],
        // list: Tuple, Set, FrozenSet, DictItems, DictKeys, DictValues
        ContClass::List => &[
            ContClass::Tuple,
            ContClass::Set,
            ContClass::Frozen,
            ContClass::DictItems,
            ContClass::DictKeys,
            ContClass::DictValues,
        ],
        // set: List, Tuple, FrozenSet, DictKeys
        ContClass::Set => &[
            ContClass::List,
            ContClass::Tuple,
            ContClass::Frozen,
            ContClass::DictKeys,
        ],
        // frozenset: List, Tuple, Set, FrozenSet, DictKeys
        ContClass::Frozen => &[
            ContClass::List,
            ContClass::Tuple,
            ContClass::Set,
            ContClass::Frozen,
            ContClass::DictKeys,
        ],
        _ => &[],
    }
}

/// klass.from_elements over already-inferred element VALUES
fn cont_build(klass: ContClass, elems: Vec<Value>) -> Value {
    let elems = Rc::new(elems);
    match klass {
        ContClass::List => Value::SynthSeq { kind: SeqKind::List, elems },
        ContClass::Tuple => Value::SynthSeq { kind: SeqKind::Tuple, elems },
        ContClass::Set => Value::SynthSeq { kind: SeqKind::Set, elems },
        ContClass::Frozen => Value::FrozenSet { elems },
        _ => unreachable!(),
    }
}

/// dedupe key emulating CPython value equality for build_elts=set/frozenset
/// (True == 1 == 1.0; bytes/str by content)
fn const_py_key(c: &ConstValue) -> String {
    match c {
        ConstValue::None => "N".into(),
        ConstValue::NotImplemented => "NI".into(),
        ConstValue::Ellipsis => "E".into(),
        ConstValue::Bool(b) => format!("i{}", *b as i64),
        ConstValue::Int(IntValue::Small(i)) => format!("i{i}"),
        ConstValue::Int(IntValue::Big(d)) => format!("i{d}"),
        ConstValue::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 9e15 {
                format!("i{}", *f as i64)
            } else {
                format!("f{}", f.to_bits())
            }
        }
        ConstValue::Complex { real, imag } => {
            if *imag == 0.0 && real.fract() == 0.0 && real.abs() < 9e15 {
                format!("i{}", *real as i64)
            } else {
                format!("c{}:{}", real.to_bits(), imag.to_bits())
            }
        }
        ConstValue::Str(s) => format!("s{s}"),
        ConstValue::StrSurrogate(cp) => format!("u{cp:?}"),
        ConstValue::Bytes(b) => format!("b{b:?}"),
    }
}

/// klass.from_elements(build_elts(values)) for the all-Const branch:
/// set/frozenset deduplicate by python value equality (first occurrence
/// kept), list/tuple keep order
fn cont_build_consts(klass: ContClass, values: Vec<ConstValue>) -> Value {
    let dedupe = matches!(klass, ContClass::Set | ContClass::Frozen);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for v in values {
        if dedupe && !seen.insert(const_py_key(&v)) {
            continue;
        }
        out.push(Value::SynthConst(Rc::new(v)));
    }
    cont_build(klass, out)
}

impl Engine {
    /// NodeNG._explicit_inference equivalent. None => no tip applies or
    /// UseInferenceDefault.
    pub fn explicit_inference(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        // _explicit_inference is registered on nodes by the TransformVisitor
        // at the END of the module's build (builder.py:175-177): inference
        // running during delayed_assattr of a module-in-build sees NO tips
        // on that module's nodes (default path; results land in the global
        // cache and are only erased if a later transform wipes it).
        if !self.md(node.m).tips_active.get()
            && !self.dataclass_attrs.borrow().contains_key(&node)
        {
            // dataclass Unknown placeholders live in synthetic modules but
            // get their tip applied at creation (dataclass_transform calls
            // visit_transforms(rhs_node) directly)
            return None;
        }
        // brain_typing.py:189/332/394 REPLACE node._explicit_inference with
        // `lambda node, context: iter([class_def])` — subsequent infer()
        // calls invoke the lambda DIRECTLY: no recursion guard, no FIFO
        // lookup and (critically) no FIFO insert/eviction.
        if let Some(vals) = self.typing_tip_cache.borrow().get(&node) {
            return Some(Flow::ok(vals.clone()));
        }
        let tip = self.find_tip(node)?;
        let (a, b) = tip_id(tip);
        let guard_key = (a * 32 + b, node);
        // inference_tip.py:45-50: a re-entry on (func, node) REMOVES the
        // in-flight guard entry (the outer's finally-remove then no-ops:
        // "Recursion may beat us to the punch") and raises
        // UseInferenceDefault.
        if self.tip_guard.borrow_mut().remove(&guard_key) {
            return None;
        }
        // inference_tip.py:50-52: `if context is not None and
        // context.is_empty(): context = None` — cache key None; otherwise
        // the key is the context OBJECT IDENTITY (contexts are unhashable
        // by value), so non-empty-context invocations basically always
        // miss and recompute.
        let empty = ctx.is_empty();
        let ckey = (
            a * 32 + b,
            node,
            if empty { 0 } else { Rc::as_ptr(ctx) as usize },
        );
        if let Some((hit, _)) = self.tip_cache.borrow().get(&ckey) {
            return Some(Flow::ok(hit.to_vec()));
        }
        self.tip_guard.borrow_mut().insert(guard_key);
        // empty ctx => the tip runs with context=None: EVERY internal
        // node.infer(None) materializes its own fresh context (no shared
        // counter, no shared path) — modeled by the synthetic_none marker.
        let run_ctx = if empty { Ctx::new_none() } else { Rc::clone(ctx) };
        let res = self.run_tip(tip, node, &run_ctx);
        // finally-remove (may have been removed by an inner recursion trip)
        self.tip_guard.borrow_mut().remove(&guard_key);
        if let Some(flow) = &res {
            if flow.err.is_none() {
                // EVERY successful miss inserts (even ctx-identity keys);
                // evict the OLDEST insertion when len exceeds 64
                // (inference_tip.py:64-66, 78-79).
                let mut cache = self.tip_cache.borrow_mut();
                let mut order = self.tip_order.borrow_mut();
                let pin = if empty { None } else { Some(Rc::clone(ctx)) };
                if cache
                    .insert(ckey, (Rc::new(flow.vals.clone()), pin))
                    .is_none()
                {
                    order.push_back(ckey);
                }
                while cache.len() > 64 {
                    match order.pop_front() {
                        Some(oldest) => {
                            cache.remove(&oldest);
                        }
                        None => break,
                    }
                }
            } else {
                // exception during the eager `list(func(...))`
                // materialization (inference_tip.py:64-66): partial yields
                // are DISCARDED, the error alone propagates; nothing is
                // cached.
                return Some(Flow::err(flow.err.clone().unwrap()));
            }
        }
        res
    }

    /// transform predicates, evaluated lazily (astroid runs them once per
    /// build; they are pure syntactic checks so this is equivalent).
    fn find_tip(&self, node: GNode) -> Option<Tip> {
        let md = self.md(node.m);
        // brain_dataclasses Unknown attribute placeholders
        if matches!(md.tree.nodes[node.n.idx()].kind, NodeKind::Unknown) {
            if self.dataclass_attrs.borrow().contains_key(&node) {
                return Some(Tip::DataclassAttr);
            }
            return None;
        }
        // Subscript tips: brain_pathlib parents (decided at scan time),
        // then typing.X[...] (brain_typing _looks_like_typing_subscript)
        if let NodeKind::Subscript { value, .. } = &md.tree.nodes[node.n.idx()].kind {
            if self.pathlib_subscripts.borrow().contains(&node) {
                return Some(Tip::PathlibParents);
            }
            if self.looks_like_typing_subscript(GNode { m: node.m, n: *value }) {
                return Some(Tip::TypingSubscript);
            }
            return None;
        }
        // ClassDef tip: NamedTuple bases (brain_namedtuple_enum
        // _has_namedtuple_base; registered before the typing tips)
        if let NodeKind::ClassDef(d) = &md.tree.nodes[node.n.idx()].kind {
            // brain_typing PEP695 generic classes (registered after the
            // namedtuple tips; disjoint predicates)
            if !d.type_params.is_empty() {
                return Some(Tip::Pep695Generic);
            }
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
            // brain_type._looks_like_type_subscript: parent is Subscript
            if n == "type" {
                let parent = md.tree.nodes[node.n.idx()].parent;
                if matches!(md.tree.nodes[parent.idx()].kind, NodeKind::Subscript { .. }) {
                    return Some(Tip::TypeSubscript);
                }
            }
            return None;
        }
        let NodeKind::Call { func, .. } = &md.tree.nodes[node.n.idx()].kind else {
            return None;
        };
        // brain_dataclasses field() tip (decided at transform time)
        if self.dataclass_field_calls.borrow().contains(&node) {
            return Some(Tip::DataclassFieldCall);
        }
        // brain_statistics (registered LAST among Call tips its predicate
        // can co-match; the syntactic predicate is disjoint in practice)
        if self.looks_like_statistics_quantiles(node) {
            return Some(Tip::StatisticsQuantiles);
        }
        // brain_random._looks_like_random_sample: ANY `<x>.sample(...)`
        // attribute call or bare `sample(...)` (brain_random.py:84-90)
        if self.looks_like_random_sample(node) {
            return Some(Tip::RandomSample);
        }
        match &md.tree.nodes[func.idx()].kind {
            NodeKind::Name { name } => {
                let n = md.tree.s(*name);
                if n == "namedtuple" {
                    return Some(Tip::NamedTupleCall);
                }
                // _looks_like_enum (brain_namedtuple_enum.py:187)
                if n == "Enum" {
                    return Some(Tip::EnumCall);
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
                // brain_typing _looks_like_typing_cast (registered after
                // typevar/newtype; purely syntactic)
                if n == "cast" {
                    return Some(Tip::TypingCast);
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
                // re module Pattern/Match carve-out: the builtin type() tip
                // does NOT apply (brain_builtin_inference.py:171-180) but
                // brain_re.infer_pattern_match DOES (brain_re.py:56-92)
                if n == "type" && md.name == "re" {
                    let parent = md.tree.nodes[node.n.idx()].parent;
                    if let NodeKind::Assign { targets, .. } = &md.tree.nodes[parent.idx()].kind {
                        if targets.len() == 1 {
                            if let NodeKind::AssignName { name: tn } =
                                &md.tree.nodes[targets[0].idx()].kind
                            {
                                let t = md.tree.s(*tn);
                                if t == "Pattern" || t == "Match" {
                                    return Some(Tip::RePatternMatch);
                                }
                            }
                        }
                    }
                }
                Some(Tip::Builtin(idx as u8))
            }
            NodeKind::Attribute { expr, attrname, .. } => {
                let attr = md.tree.s(*attrname);
                // brain_argparse._looks_like_namespace (registered FIRST in
                // register_all_brains)
                if attr == "Namespace" {
                    if let NodeKind::Name { name } = &md.tree.nodes[expr.idx()].kind {
                        if md.tree.s(*name) == "argparse" {
                            return Some(Tip::ArgparseNamespace);
                        }
                    }
                }
                if attr == "namedtuple" {
                    return Some(Tip::NamedTupleCall);
                }
                // _looks_like_enum (brain_namedtuple_enum.py:187)
                if attr == "Enum" {
                    return Some(Tip::EnumCall);
                }
                if attr == "NamedTuple" {
                    return Some(Tip::TypingNamedTupleCall);
                }
                if attr == "TypeVar" || attr == "NewType" {
                    return Some(Tip::TypingTypeVar);
                }
                if attr == "cast" {
                    return Some(Tip::TypingCast);
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
                    // _is_str_format_call ran at TRANSFORM-SCAN time (its
                    // safe_infer side effect happened then); applicability
                    // was recorded in str_format_calls — never re-evaluated
                    // at infer time (astroid stores _explicit_inference on
                    // the node during the scan).
                    if self.str_format_calls.borrow().contains(&node) {
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
            Tip::Builtin(i) => {
                let res = self.run_builtin_tip(BUILTIN_NAMES[i as usize], node, ctx);
                // _transform_wrapper (brain_builtin_inference.py:201-218):
                // `if result and not result.parent: result.parent = node` —
                // a parentless NODE result (a Module, e.g. the default arg
                // of getattr) is PERMANENTLY reparented under the Call,
                // changing qname() of its whole subtree for the rest of
                // the run.
                if let Some(flow) = &res {
                    if flow.err.is_none() && flow.vals.len() == 1 {
                        if let Value::Node(g) = &flow.vals[0] {
                            if self.parent(*g).is_none() {
                                self.reparents.borrow_mut().insert(*g, node);
                            }
                        }
                    }
                }
                res
            }
            Tip::DictFromkeys => self.tip_dict_fromkeys(node, ctx),
            Tip::CopyMethod => self.tip_copy_method(node, ctx),
            Tip::StrFormat => self.tip_str_format(node, ctx),
            Tip::Partial => self.tip_partial(node, ctx),
            Tip::TypingTypeVar => self.tip_typing_typevar(node, ctx),
            Tip::TypingAlias => self.tip_typing_alias(node, ctx),
            Tip::TypingSubscript => self.tip_typing_subscript(node, ctx),
            Tip::TypedDictFunc => self.tip_typeddict_func(node),
            Tip::NamedTupleCall => self.tip_named_tuple(node, ctx),
            Tip::EnumCall => self.tip_enum_call(node, ctx),
            Tip::TypingNamedTupleCall => self.tip_typing_namedtuple_call(node, ctx),
            Tip::TypingNamedTupleClass => self.tip_typing_namedtuple_class(node, ctx),
            Tip::TypingNamedTupleFunc => self.tip_typing_namedtuple_func(node, ctx),
            Tip::NumpyMember(i) => {
                self.tip_numpy_extract(NUMPY_MEMBER_SRC[i as usize].1, ctx)
            }
            Tip::NumpyNdarray => {
                self.tip_numpy_extract(crate::numpy_templates::NUMPY_NDARRAY_SRC, ctx)
            }
            Tip::TypeSubscript => self.tip_type_subscript(node, ctx),
            Tip::PathlibParents => self.tip_pathlib_parents(node, ctx),
            Tip::DataclassAttr => self.tip_dataclass_attr(node, ctx),
            Tip::DataclassFieldCall => self.tip_dataclass_field_call(node, ctx),
            Tip::RePatternMatch => self.tip_re_pattern_match(node),
            Tip::ArgparseNamespace => self.tip_argparse_namespace(node, ctx),
            Tip::Pep695Generic => self.tip_pep695_generic(node),
            Tip::TypingCast => self.tip_typing_cast(node, ctx),
            // brain_statistics.infer_statistics_quantiles: yields U
            // unconditionally (brain_statistics.py:52-65)
            Tip::StatisticsQuantiles => Some(Flow::one(Value::Uninferable)),
            Tip::RandomSample => self.tip_random_sample(node, ctx),
        }
    }

    /// brain_random._looks_like_random_sample (brain_random.py:84-90)
    fn looks_like_random_sample(&self, node: GNode) -> bool {
        let md = self.md(node.m);
        let func = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, .. } => *func,
            _ => return false,
        };
        match &md.tree.nodes[func.idx()].kind {
            NodeKind::Attribute { attrname, .. } => md.tree.s(*attrname) == "sample",
            NodeKind::Name { name } => md.tree.s(*name) == "sample",
            _ => false,
        }
    }

    /// brain_random.infer_random_sample (brain_random.py:41-80): safe_infer
    /// both args; the sequence must be a real List/Set/Tuple container and
    /// k <= len; the result is a fresh List of k CLONED elements chosen by
    /// `random.sample` AT INFERENCE TIME — the warm oracle's unseeded
    /// Mersenne state makes the SELECTION irreducible; we pick a stable
    /// pseudo-random subset (deterministic LCG) so the List length (the
    /// dump-visible part) matches and reruns are stable.
    fn tip_random_sample(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(node.m);
        let args: Vec<pyast::NodeId> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { args, .. } => args.clone(),
            _ => return None,
        };
        if args.len() != 2 {
            return None; // UseInferenceDefault
        }
        let k = match self.safe_infer(GNode { m: node.m, n: args[1] }, ctx) {
            Some(v) => match self.value_const(&v) {
                Some(ConstValue::Int(pyast::tree::IntValue::Small(i))) => i,
                Some(ConstValue::Bool(b)) => b as i64,
                _ => return None,
            },
            None => return None,
        };
        let seq = self.safe_infer(GNode { m: node.m, n: args[0] }, ctx)?;
        let elts: Vec<Value> = match &seq {
            Value::Node(g) => {
                let smd = self.md(g.m);
                match &smd.tree.nodes[g.n.idx()].kind {
                    NodeKind::List { elts, .. }
                    | NodeKind::Tuple { elts, .. }
                    | NodeKind::Set { elts } => elts
                        .iter()
                        .map(|&e| Value::Node(GNode { m: g.m, n: e }))
                        .collect(),
                    _ => return None,
                }
            }
            Value::SynthSeq { elems, .. } => elems.to_vec(),
            _ => return None,
        };
        if k < 0 || k as usize > elts.len() {
            return None; // ValueError -> UseInferenceDefault
        }
        // deterministic Fisher-Yates with an LCG keyed off the node
        let mut pool: Vec<Value> = elts;
        let mut state: u64 = 0x9E3779B97F4A7C15u64 ^ ((node.n.idx() as u64) << 16);
        let mut chosen: Vec<Value> = Vec::with_capacity(k as usize);
        for _ in 0..k {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let idx = (state >> 33) as usize % pool.len();
            chosen.push(pool.remove(idx));
        }
        Some(Flow::one(Value::SynthSeq {
            kind: crate::value::SeqKind::List,
            elems: Rc::new(chosen),
        }))
    }

    /// brain_typing.infer_typing_cast (brain_typing.py:404-422):
    /// func = next(node.func.infer(context=ctx)) — single pull, LIVE ctx;
    /// must be FunctionDef qname "typing.cast" with exactly 2 positional
    /// args, else UseInferenceDefault; result = node.args[1].infer(ctx).
    fn tip_typing_cast(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(node.m);
        let (func, args) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, args, .. } => (GNode { m: node.m, n: *func }, args.clone()),
            _ => return None,
        };
        let first = self.infer_first(func, Some(ctx)).ok()?;
        let q = self.value_qname(&first)?;
        let is_funcdef = matches!(&first, Value::Node(g) if self.kind_is(*g, |k| {
            matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
        }));
        if !is_funcdef || q != "typing.cast" || args.len() != 2 {
            return None;
        }
        let val = GNode { m: node.m, n: args[1] };
        Some(self.infer(val, ctx))
    }

    /// infer_typing_generic_class_pep695 (brain_typing.py:201-207): inject
    /// __class_getitem__ into the class locals and yield the class.
    fn tip_pep695_generic(&self, node: GNode) -> Option<Flow> {
        let cgi = self.sym("__class_getitem__");
        let tmpl = self.build_template_module(
            "@classmethod\ndef __class_getitem__(cls, item):\n    return cls\n",
            "",
        )?;
        let func = {
            let tmd = self.md(tmpl);
            let locals = tmd.locals.borrow();
            locals
                .get(&pyast::NodeId::MODULE)
                .and_then(|m| m.get(&cgi))
                .and_then(|v| v.first().copied())
        }?;
        {
            let md = self.md(node.m);
            let mut locals = md.locals.borrow_mut();
            locals.entry(node.n).or_default().insert(cgi, vec![func]);
        }
        Some(Flow::one(crate::value::Value::Node(node)))
    }

    /// brain_argparse.infer_namespace: keyword-only CallSite -> fresh
    /// `Namespace` ClassDef parented to SYNTHETIC_ROOT with EmptyNode
    /// instance_attrs per keyword; yields instantiate_class().
    fn tip_argparse_namespace(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let call_site = self.call_site_of_call(node, ctx);
        let kw: Vec<GSym> = call_site
            .keyword_arguments()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        if kw.is_empty() {
            return None; // UseInferenceDefault
        }
        let fake_mid = self.build_template_module(
            "class Namespace:
    pass
",
            "__astroid_synthetic",
        )?;
        let cls = {
            let fmd = self.md(fake_mid);
            let locals = fmd.locals.borrow();
            locals
                .get(&pyast::NodeId::MODULE)
                .and_then(|l| l.get(&self.sym("Namespace")))
                .and_then(|v| v.first())
                .copied()?
        };
        {
            let mut ia = self.iattrs.borrow_mut();
            let map = ia.entry(cls).or_default();
            for k in kw {
                if map.contains_key(&k) {
                    continue; // set() semantics: one entry per name
                }
                let ph = self.alloc_synth_node(NodeKind::EmptyNode);
                map.insert(k, vec![ph]);
            }
        }
        Some(Flow::one(self.instantiate_class(cls)))
    }

    /// brain_re.infer_pattern_match (brain_re.py:79-92): a FRESH ClassDef
    /// named after the assign target with only __class_getitem__ in locals;
    /// parent = node.parent so qname composes to re.Pattern / re.Match.
    fn tip_re_pattern_match(&self, node: GNode) -> Option<Flow> {
        let md = self.md(node.m);
        let parent = md.tree.nodes[node.n.idx()].parent;
        let tname = match &md.tree.nodes[parent.idx()].kind {
            NodeKind::Assign { targets, .. } if targets.len() == 1 => {
                match &md.tree.nodes[targets[0].idx()].kind {
                    NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                    _ => return None,
                }
            }
            _ => return None,
        };
        let src = format!(
            "class {tname}:\n    @classmethod\n    def __class_getitem__(cls, item):\n        return cls\n"
        );
        let fake_mid = self.build_template_module(&src, "re")?;
        let sym = self.sym(&tname);
        let fmd = self.md(fake_mid);
        let locals = fmd.locals.borrow();
        let cls = locals
            .get(&pyast::NodeId::MODULE)
            .and_then(|l| l.get(&sym))
            .and_then(|v| v.first())
            .copied()?;
        Some(Flow::ok(vec![crate::value::Value::Node(cls)]))
    }

    /// brain_dataclasses.infer_dataclass_field_call: default -> the value's
    /// inference; default_factory -> the factory called with no arguments
    /// (astroid re-parses `<factory>()` — the synthetic call gets the
    /// builtin container tips)
    fn tip_dataclass_field_call(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(node.m);
        let parent = md.tree.nodes[node.n.idx()].parent;
        if !matches!(
            md.tree.nodes[parent.idx()].kind,
            NodeKind::AnnAssign { .. } | NodeKind::Assign { .. }
        ) {
            return None; // UseInferenceDefault
        }
        let keywords: Vec<pyast::NodeId> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { keywords, .. } => keywords.clone(),
            _ => return None,
        };
        let mut default: Option<pyast::NodeId> = None;
        let mut default_factory: Option<pyast::NodeId> = None;
        for kw in keywords {
            if let NodeKind::Keyword { arg: Some(a), value } = &md.tree.nodes[kw.idx()].kind {
                match md.tree.s(*a) {
                    "default" => default = Some(*value),
                    "default_factory" => default_factory = Some(*value),
                    _ => {}
                }
            }
        }
        drop(md);
        match (default, default_factory) {
            (Some(d), None) => Some(self.infer(GNode { m: node.m, n: d }, ctx)),
            (None, Some(f)) => {
                let fg = GNode { m: node.m, n: f };
                // brain_dataclasses.py:430-432: `new_call =
                // parse(default.as_string()).body[0].value` — a REAL
                // template module build whose transform scan applies the
                // builtin Call tips (each application WIPES the global
                // inference cache mid-dump, transforms.py:72), then
                // `yield from new_call.infer(context=ctx)` under the live
                // ctx. Name/Attribute factories take this path; other
                // shapes keep the value-equivalent emulation below.
                let src_opt = if let Some(dotted) = self.dotted_of(fg) {
                    Some(format!("{dotted}()\n"))
                } else if self.kind_is(fg, |k| matches!(k, NodeKind::Lambda(_))) {
                    // Call.as_string puts precedence parens around a lambda
                    // func (as_string.py _precedence_parens):
                    // `(lambda: defaultdict(dict))()` — the re-parse builds
                    // a FRESH Lambda (template line 1) whose body call gets
                    // its own NodeNG.infer hops (esphome entry_data
                    // field(default_factory=lambda: defaultdict(dict)))
                    self.expr_source(fg).map(|s| format!("({s})()\n"))
                } else {
                    None
                };
                if let Some(src) = src_opt {
                    if let Some(tmpl_call) = self.template_extract_node(&src) {
                        // `new_call.parent = node.parent`
                        // (brain_dataclasses.py:431): the re-parsed call is
                        // REPARENTED to the field call's parent, so its
                        // factory Name resolves in the REAL module's scope
                        // (airflow `field(default_factory=ParamsDict)`)
                        let parent = {
                            let md = self.md(node.m);
                            GNode { m: node.m, n: md.tree.nodes[node.n.idx()].parent }
                        };
                        self.reparents.borrow_mut().insert(tmpl_call, parent);
                        return Some(self.infer(tmpl_call, ctx));
                    }
                }
                // emulate `parse(factory.as_string() + "()").infer(ctx)`
                let flow = self.infer(fg, ctx);
                let mut out: Vec<Value> = Vec::new();
                for callee in flow.vals {
                    if callee.is_uninferable() {
                        out.push(Value::Uninferable);
                        continue;
                    }
                    // builtin container classes get the builtin Call tip in
                    // astroid's re-parsed module: empty containers
                    if let Value::Node(g) = &callee {
                        let b = self.builtins();
                        let kind = if *g == b.list {
                            Some(crate::value::SeqKind::List)
                        } else if *g == b.tuple {
                            Some(crate::value::SeqKind::Tuple)
                        } else if *g == b.set {
                            Some(crate::value::SeqKind::Set)
                        } else {
                            None
                        };
                        if let Some(kind) = kind {
                            out.push(Value::SynthSeq { kind, elems: Rc::new(Vec::new()) });
                            continue;
                        }
                        if *g == b.dict {
                            out.push(Value::SynthDict { items: Rc::new(Vec::new()) });
                            continue;
                        }
                        if *g == b.frozenset {
                            out.push(Value::FrozenSet { elems: Rc::new(Vec::new()) });
                            continue;
                        }
                    }
                    let ctx2 = copy_context(Some(ctx));
                    *ctx2.boundnode.borrow_mut() = None;
                    *ctx2.callcontext.borrow_mut() = Some(Rc::new(crate::ctx::CallCtx {
                        id: self.next_callctx_id(),
                        args: std::cell::RefCell::new(Vec::new()),
                        keywords: std::cell::RefCell::new(Vec::new()),
                        callee: std::cell::RefCell::new(Some(callee.clone())),
                    }));
                    let res = self.infer_call_result(&callee, None, Some(&ctx2));
                    out.extend(res.vals);
                }
                Some(Flow::ok(out))
            }
            _ => Some(Flow::one(Value::Uninferable)),
        }
    }

    /// brain_dataclasses.infer_dataclass_attribute: default value infers
    /// first, then an instance from the annotation
    fn tip_dataclass_attr(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let assign = *self.dataclass_attrs.borrow().get(&node)?;
        let (annotation, value) = {
            let md = self.md(assign.m);
            match &md.tree.nodes[assign.n.idx()].kind {
                NodeKind::AnnAssign { annotation, value, .. } => (*annotation, *value),
                _ => return Some(Flow::one(Value::Uninferable)),
            }
        };
        let mut vals: Vec<Value> = Vec::new();
        if let Some(v) = value {
            let f = self.infer(GNode { m: assign.m, n: v }, ctx);
            if let Some(e) = f.err {
                if vals.is_empty() && f.vals.is_empty() {
                    return Some(Flow::err(e));
                }
            }
            vals.extend(f.vals);
        }
        // _infer_instance_from_annotation
        let ann = GNode { m: assign.m, n: annotation };
        let klass = self.first_value(ann, ctx).ok().flatten();
        let mut from_ann: Vec<Value> = Vec::new();
        if klass.is_none() {
            from_ann.push(Value::Uninferable);
        }
        match &klass {
            Some(Value::Node(g))
                if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) =>
            {
                // klass.root().name (brain_dataclasses.py:614) — REPARENT-
                // AWARE: extender-template classes (collections.defaultdict)
                // live in a ''-named template module but are reparented
                // into the real module (brain/helpers.py:25-27); the raw
                // module name matched the '' branch and wrongly yielded U
                // (esphome entry_data defaultdict annotation).
                let root = {
                    let mut top = *g;
                    while let Some(p) = self.parent(top) {
                        top = p;
                    }
                    self.md(top.m).name.clone()
                };
                if matches!(root.as_str(), "typing" | "_collections_abc" | "") {
                    let n = self.node_name(*g).unwrap_or_default();
                    if matches!(n.as_str(), "Dict" | "FrozenSet" | "List" | "Set" | "Tuple") {
                        from_ann.push(self.instantiate_class(*g));
                    } else {
                        from_ann.push(Value::Uninferable);
                    }
                } else {
                    from_ann.push(self.instantiate_class(*g));
                }
            }
            _ => {
                // not a ClassDef (incl. the None-after-error case — astroid
                // falls through the isinstance check and yields U again)
                from_ann.push(Value::Uninferable);
            }
        }
        vals.extend(from_ann);
        Some(Flow::ok(vals))
    }

    /// brain_pathlib.infer_parents_subscript: Const slice -> Inst of the
    /// REAL pathlib.Path. brain_pathlib.py:44
    /// `next(_extract_single_node(PATH_TEMPLATE).infer())` — NO context
    /// (fresh InferenceContext, no live-counter bumps) and a SINGLE pull.
    fn tip_pathlib_parents(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(node.m);
        let slice_is_const = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Subscript { slice, .. } => {
                matches!(md.tree.nodes[slice.idx()].kind, NodeKind::Const(_))
            }
            _ => false,
        };
        if !slice_is_const {
            return None; // UseInferenceDefault
        }
        let g = self.template_extract_node("from pathlib import Path\nPath\n")?;
        let cls = match self.infer_first(g, None).ok()? {
            Value::Node(g) if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) => g,
            _ => return None,
        };
        let _ = ctx;
        Some(Flow::ok(vec![self.instantiate_class(cls)]))
    }

    /// brain_type.infer_type_sub: `type[...]` only when "type" resolves to
    /// the builtins module; yields a synthetic `class type` (qname ".type")
    fn tip_type_subscript(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let sym = self.sym("type");
        let scope = self.scope(node);
        // `node.scope().lookup("type")` (brain_type.py:55) — LookupMixIn
        // .lookup uses SELF as the filter node: the SCOPE node, not the
        // subscripted Name! A class-level `type: type[X] = ...` binding IS
        // visible from the class node's own perspective, so node_scope is
        // the ClassDef -> UseInferenceDefault (pandas CategoricalDtype).
        let (found_scope, _) = self.scope_lookup(scope, scope, sym, 0);
        if !self.kind_is(found_scope, |k| matches!(k, NodeKind::Module(_)))
            || self.md(found_scope.m).name != "builtins"
        {
            return None; // UseInferenceDefault
        }
        self.tip_numpy_extract(
            "class type:\n    def __class_getitem__(cls, key):\n        return cls\n",
            ctx,
        )
    }

    /// brain_numpy_utils.infer_numpy_attribute / infer_numpy_name:
    /// `extract_node(sources[name]).infer(context=context)` — a FRESH
    /// template module per tip run (module name '' -> qname ".array" etc.),
    /// inferred with the LIVE context (counter bumps included).
    fn tip_numpy_extract(&self, source: &str, ctx: &Rc<Ctx>) -> Option<Flow> {
        let g = self.template_extract_node(source)?;
        Some(self.infer(g, ctx))
    }

    /// extract_node(source) on a fresh template module: the last body
    /// statement, unwrapped from Expr
    fn template_extract_node(&self, source: &str) -> Option<GNode> {
        let mid = self.build_template_module(source, "")?;
        let md = self.md(mid);
        let mut last = match &md.tree.nodes[pyast::NodeId::MODULE.idx()].kind {
            NodeKind::Module(d) => *d.body.last()?,
            _ => return None,
        };
        // extract_node unwraps Expr statements to their value
        if let NodeKind::Expr { value } = &md.tree.nodes[last.idx()].kind {
            last = *value;
        }
        Some(GNode { m: mid, n: last })
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
        let md = self.md(node.m);
        let (func, args) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, args, .. } => (GNode { m: node.m, n: *func }, args.clone()),
            _ => return None,
        };
        // next(node.func.infer(context=context)) — single pull, same ctx
        let q = self.value_qname(&self.infer_first(func, Some(ctx)).ok()?)?;
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
        // brain_typing.py:131-133: fresh template per run, then
        // `return node.infer(context=context_itton)` — live-ctx infer hop
        Some(self.infer(cls, ctx))
    }

    /// brain_typing.infer_typing_alias + infer_special_alias. The final
    /// `node._explicit_inference = lambda ...` replacement is modeled by the
    /// typing_tip_cache insert below — explicit_inference consults that map
    /// FIRST (bypassing guard + FIFO) on later invocations.
    fn tip_typing_alias(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
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
            let bname = self.node_name(b)?;
            // REPARENT-AWARE root module name: extension-template classes
            // (collections.defaultdict etc.) live in a ''-named template
            // module but are reparented into the real module — walk parents
            // to the top like astroid's b.root() (astroid puts the inferred
            // ClassDef node DIRECTLY in bases; our textual import must
            // resolve to the same class through the merged module).
            let broot_name = {
                let mut top = b;
                while let Some(p) = self.parent(top) {
                    top = p;
                }
                self.md(top.m).name.clone()
            };
            // base importable only when top-level in its module
            let top_level = self
                .parent(b)
                .map(|p| self.frame(p))
                .map(|f| {
                    self.kind_is(f, |k| matches!(k, NodeKind::Module(_)))
                })
                .unwrap_or(false);
            if top_level && broot_name != modname && !broot_name.is_empty() {
                src.push_str(&format!(
                    "from {} import {} as _alias_base
",
                    broot_name, bname
                ));
                base_clause = "(_alias_base)".to_string();
            } else if broot_name == "builtins" {
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

    /// brain_typing.infer_typing_attr (Subscript). Only the Generic/
    /// Annotated branch pins node._explicit_inference (typing_tip_cache);
    /// the TYPING_TYPE_TEMPLATE branch re-builds a fresh synthetic class on
    /// every FIFO miss (brain_typing.py:192-193).
    fn tip_typing_subscript(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(node.m);
        let value = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Subscript { value, .. } => GNode { m: node.m, n: *value },
            _ => return None,
        };
        // brain_typing.py:151 `value = next(node.value.infer())` — fresh
        // context, SINGLE pull (the suspended chain is abandoned: no cache
        // writes for the value Name)
        let first = self.infer_first_fresh(value).ok().flatten()?;
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
        // brain_typing.py:192-193: a FRESH template per tip run, then
        // `return node.infer(context=ctx)` — a full NodeNG.infer hop on
        // the template ClassDef under the LIVE context (+1 bump when the
        // consumer drains; cache write under the live key). Empty-context
        // runs are cached by the generic tip cache (explicit_inference).
        Some(self.infer(cls, ctx))
    }

    /// brain_typing.infer_typedDict (brain_typing.py:217-233): builds a
    /// FRESH ClassDef on every invocation — no node._explicit_inference
    /// replacement, so re-runs whenever the 64-FIFO misses.
    fn tip_typeddict_func(&self, node: GNode) -> Option<Flow> {
        let modname = self.md(node.m).name.clone();
        // trailing bare `dict` Expr supplies the Name node injected as
        // class_def.locals["__call__"] = [func_to_add]
        // (brain_typing.py:228-231 -- instances of the synthetic class are
        // callable; calling them instantiates builtins.dict)
        // NO transform scan: astroid constructs the ClassDef MANUALLY
        // (brain_typing.py:221-231) — no ClassDef transform ever sees it, so
        // our template build must not infer the `dict` base eagerly
        // (counter parity: the esphome try_parse_enum cap truncation).
        let mid = self.build_template_module_no_transforms(
            "class TypedDict(dict):
    pass
dict
",
            &modname,
        )?;
        let sym = self.sym("TypedDict");
        let call_sym = self.sym("__call__");
        let tmd = self.md(mid);
        let cls = {
            let locals = tmd.locals.borrow();
            locals
                .get(&NodeId::MODULE)
                .and_then(|l| l.get(&sym))
                .and_then(|v| v.first().copied())?
        };
        // the module's last statement is Expr(Name dict)
        let dict_name: Option<GNode> = match &tmd.tree.nodes[NodeId::MODULE.idx()].kind {
            NodeKind::Module(m) => m.body.last().and_then(|&stmt| {
                match &tmd.tree.nodes[stmt.idx()].kind {
                    NodeKind::Expr { value } => Some(GNode { m: mid, n: *value }),
                    _ => None,
                }
            }),
            _ => None,
        };
        if let Some(dn) = dict_name {
            tmd.locals
                .borrow_mut()
                .entry(cls.n)
                .or_default()
                .insert(call_sym, vec![dn]);
        }
        Some(Flow::ok(vec![Value::Node(cls)]))
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
                // next(argument.infer(context=context)) — single pull
                let first = match self.infer_first(args[0], Some(ctx)) {
                    Ok(v) => v,
                    Err(_) => return Some(Flow::uninferable()),
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
                // next(argument.infer(context=context)) — single pull
                let first = match self.infer_first(args[0], Some(ctx)) {
                    Ok(v) => v,
                    Err(_) => return Some(Flow::uninferable()),
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
                // next(getter.infer(context=context)) — single pull
                let f = self.infer_first(args[0], Some(ctx)).ok();
                match f.as_ref() {
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
                        // objects.Property(name="<property>",
                        // parent=SYNTHETIC_ROOT) — qname is the synthetic
                        // root's, not the function's
                        self.synth_props.borrow_mut().insert(*g);
                        Some(Flow::one(Value::Property { func: *g, synth: true }))
                    }
                    _ => None,
                }
            }
            "getattr" => {
                let (obj, attr) = self.tip_getattr_args(&args, ctx)?;
                // infer_getattr: `or not hasattr(obj, "igetattr")` ->
                // Uninferable (no default fallback!). Objects WITHOUT an
                // igetattr attribute: Lambda (scoped_nodes.py:883 — only
                // FunctionDef defines igetattr), Unknown, EvaluatedObject
                // (plain NodeNG subclasses). Containers/Const mix in
                // bases.Instance, Slice defines its own — they all have it.
                let no_igetattr = match &obj {
                    Value::Node(g) => self.kind_is(*g, |k| {
                        matches!(k, NodeKind::Lambda(_) | NodeKind::Unknown)
                    }),
                    Value::EvaluatedObject { .. } => true,
                    _ => false,
                };
                if no_igetattr {
                    return Some(Flow::uninferable());
                }
                match (obj, attr) {
                    (Value::Uninferable, _) | (_, None) => Some(Flow::uninferable()),
                    (obj, Some(attr)) => {
                        let sym = self.sym(&attr);
                        // next(obj.igetattr(attr, context=context)) — SINGLE
                        // pull (brain_builtin_inference.py infer_getattr):
                        // the suspended igetattr chain is abandoned (no
                        // cache writes / post-yield bumps)
                        match self.igetattr_first(&obj, sym, Some(ctx)) {
                            Ok(Some(v)) => Some(Flow::one(v)),
                            _ => {
                                if args.len() == 3 {
                                    // next(node.args[2].infer(context)) —
                                    // single pull, same ctx
                                    self.infer_first(args[2], Some(ctx))
                                        .ok()
                                        .map(Flow::one)
                                } else {
                                    None
                                }
                            }
                        }
                    }
                }
            }
            "hasattr" => {
                // infer_hasattr (brain_builtin_inference.py:570-585):
                // UseInferenceDefault from _infer_getattr_args is CAUGHT
                // and returns Uninferable — hasattr never falls back to
                // default Call inference (unlike getattr).
                let (obj, attr) = match self.tip_getattr_args(&args, ctx) {
                    Some(x) => x,
                    None => return Some(Flow::uninferable()),
                };
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
            "tuple" => self.tip_container(node, &args, ContClass::Tuple, ctx),
            "set" => self.tip_container(node, &args, ContClass::Set, ctx),
            "list" => self.tip_container(node, &args, ContClass::List, ctx),
            "frozenset" => self.tip_container(node, &args, ContClass::Frozen, ctx),
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
                // `args = [infer_func(arg) for arg in args]`
                // (brain_builtin_inference.py:687-688): EAGER safe_infer of
                // EVERY arg under the SHARED context (partial(safe_infer,
                // context=context)) BEFORE any validation — the count
                // bumps land even when a later check bails to default
                // (pandas _partial_date_slice slice(left, right) caps the
                // chain at the callee in astroid).
                let inferred: Vec<Option<Value>> =
                    args.iter().map(|&a| self.safe_infer(a, ctx)).collect();
                let mut bounds: [Option<ConstValue>; 3] = [None, None, None];
                for (i, v) in inferred.iter().enumerate() {
                    let Some(v) = v else { return None };
                    if v.is_uninferable() {
                        return None;
                    }
                    let c = self.value_const(v)?;
                    match c {
                        ConstValue::None | ConstValue::Int(_) | ConstValue::Bool(_) => {
                            bounds[i] = Some(c)
                        }
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
                    // next(call.positional_arguments[0].infer(context)) —
                    // single pull, same ctx
                    let first_value = match first {
                        NV::N(g) => self.infer_first(*g, Some(ctx)).ok()?,
                        NV::V(v) => v.clone(),
                    };
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
        // next(node.args[i].infer(context=context)) — single pulls
        let obj = self.infer_first(args[0], Some(ctx)).ok()?;
        let attr = self.infer_first(args[1], Some(ctx)).ok()?;
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
            // next(node.args[i].infer(context=context)) — single pulls
            let p = self.infer_first(args[0], Some(ctx)).ok()?;
            let t = self.infer_first(args[1], Some(ctx)).ok()?;
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
        // ARG PULL ORDER differs per tip: infer_issubclass pulls the OBJ
        // first (brain_builtin_inference.py:751-757 — a non-ClassDef obj
        // raises UseInferenceDefault BEFORE the 2nd arg is ever inferred);
        // infer_isinstance builds the class container first (:783-787).
        let issubclass_obj: Option<GNode> = if name == "issubclass" {
            let obj = match &pos[0] {
                NV::N(g) => self.first_value(*g, ctx).ok().flatten()?,
                NV::V(v) => v.clone(),
            };
            match obj {
                Value::Node(g) if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) => {
                    Some(g)
                }
                _ => return None,
            }
        } else {
            None
        };
        // _class_or_tuple_to_container (brain_builtin_inference.py): a
        // SINGLE pull of the second arg; Tuple literal -> single pull per
        // element; any InferenceError -> UseInferenceDefault
        let cls_first = match &pos[1] {
            NV::N(g) => self.first_value(*g, ctx).ok().flatten()?,
            NV::V(v) => v.clone(),
        };
        let classes: Vec<Value> = match &cls_first {
            Value::Node(g)
                if self.kind_is(*g, |k| matches!(k, NodeKind::Tuple { .. })) =>
            {
                let md = self.md(g.m);
                let elts: Vec<pyast::NodeId> = match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Tuple { elts, .. } => elts.clone(),
                    _ => Vec::new(),
                };
                let mut out = Vec::new();
                for e in elts {
                    out.push(self.first_value(GNode { m: g.m, n: e }, ctx).ok().flatten()?);
                }
                out
            }
            Value::SynthSeq { kind: SeqKind::Tuple, elems } => {
                // synthetic Tuples (binop concat) are nodes.Tuple in
                // astroid: each element gets a single infer pull
                let mut out = Vec::new();
                for e in elems.iter() {
                    match e {
                        Value::Node(g) => {
                            out.push(self.first_value(*g, ctx).ok().flatten()?)
                        }
                        v => out.push(v.clone()),
                    }
                }
                out
            }
            other => vec![other.clone()],
        };
        let obj_type: GNode = if name == "isinstance" {
            // helpers.object_isinstance -> object_type(obj_node): full
            // inference of the ARG NODE with set-of-types semantics
            let t = match &pos[0] {
                NV::N(g) => self.object_type_of_node(*g, ctx),
                NV::V(v) => match self.object_type(v, ctx) {
                    Some(t) => Value::Node(t),
                    None => Value::Uninferable,
                },
            };
            match t {
                // Uninferable obj type -> infer_isinstance raises
                // UseInferenceDefault (brain_builtin_inference.py)
                Value::Node(g) => g,
                _ => return None,
            }
        } else {
            // infer_issubclass: obj was already pulled (and ClassDef-
            // checked) BEFORE the class container above.
            issubclass_obj?
        };
        for klass in &classes {
            // class_seq sanitisation (helpers.object_isinstance): any
            // Instance (incl. Const/containers) -> AstroidTypeError ->
            // UseInferenceDefault
            let instance_like = match klass {
                Value::Uninferable => true,
                // bases.Instance subclasses only: UnionType/Generator are
                // BaseInstance but NOT Instance (bases.py class hierarchy)
                Value::Inst { .. }
                | Value::ExcInst { .. }
                | Value::SynthConst(_)
                | Value::SynthSeq { .. }
                | Value::SynthDict { .. }
                | Value::FrozenSet { .. } => true,
                Value::Node(g) => self.kind_is(*g, |k| {
                    matches!(
                        k,
                        NodeKind::Const(_)
                            | NodeKind::List { .. }
                            | NodeKind::Tuple { .. }
                            | NodeKind::Set { .. }
                            | NodeKind::Dict { .. }
                    )
                }),
                _ => false,
            };
            if instance_like {
                return None;
            }
            // for obj_subclass in obj_type.mro(): MroError ->
            // UseInferenceDefault
            let mro = self.mro(obj_type, None).ok()?;
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
            match c {
                ConstValue::Str(s) => return Some(s.chars().count() as i64),
                ConstValue::Bytes(b) => return Some(b.len() as i64),
                // helpers.py:276-277 `isinstance(inferred_node, Const) and
                // isinstance(value, (bytes, str))` — OTHER consts (None,
                // int, ...) fall THROUGH to object_type + __len__ lookup,
                // burning those pulls before AstroidTypeError
                _ => {}
            }
        }
        // helpers.py:278-281: ONLY List/Set/Tuple/FrozenSet (node classes)
        // and Dict take the elts/items shortcut — DictKeys/Values/Items
        // proxies fall through to the __len__ lookup (which fails -> the
        // len() tip raises UseInferenceDefault -> default Call inference)
        let is_seq = matches!(
            inferred,
            Value::SynthSeq { .. } | Value::FrozenSet { .. }
        ) || matches!(&inferred, Value::Node(g) if self.kind_is(*g, |k| matches!(
            k,
            NodeKind::List { .. } | NodeKind::Tuple { .. } | NodeKind::Set { .. }
        )));
        if is_seq {
            if let Some(elts) = self.value_elts(&inferred) {
                return Some(elts.len() as i64);
            }
        }
        if let Some(items) = self.value_dict_items(&inferred) {
            return Some(items.len() as i64);
        }
        // __len__ through the type
        let t = self.object_type(&inferred, ctx)?;
        let sym = self.sym("__len__");
        // helpers.py:288 `next(node_type.igetattr("__len__", context))` —
        // a SINGLE pull, the rest of the igetattr stream ABANDONED (no
        // post-yield bump for the FunctionDef, no truncated-wrapper cache
        // writes); StopIteration/AttributeInferenceError -> AstroidTypeError
        let mut len_call: Option<Value> = None;
        let _ = {
            let len_call = &mut len_call;
            self.class_igetattr_to(t, sym, Some(ctx), true, &mut |v| {
                *len_call = Some(v);
                crate::value::Drive::Stop
            })
        };
        let len_call = len_call?;
        // helpers.py:296-313: `result_of_len = next(inferred, None)` —
        // single abandoned pull; NO result -> `return 0`; Const int ->
        // value; int-subclass Instance -> 0; anything else (incl. U) ->
        // AstroidTypeError; an InferenceError during the pull propagates
        // (infer_len catches both -> UseInferenceDefault)
        let mut result: Option<Value> = None;
        let mut raised = false;
        let end = {
            let result = &mut result;
            self.infer_call_result_to(&len_call, None, Some(&copy_context(Some(ctx))), &mut |v| {
                *result = Some(v);
                crate::value::Drive::Stop
            })
        };
        if let crate::value::End::Raised(_) = end {
            if result.is_none() {
                raised = true;
            }
        }
        if raised {
            return None;
        }
        match result {
            Some(v) => match self.value_const(&v) {
                // pytype() == "builtins.int" excludes bool Consts
                Some(ConstValue::Int(IntValue::Small(i))) => Some(i),
                Some(_) => None,
                None => {
                    if matches!(v, Value::Inst { .. })
                        && self
                            .proxied_class(&v)
                            .map(|c| self.is_subtype_of(c, "builtins.int", None))
                            .unwrap_or(false)
                    {
                        // unknown instance-call arguments -> fake 0
                        Some(0)
                    } else {
                        None
                    }
                }
            },
            // next(inferred, None) exhausted -> result None -> return 0
            None => Some(0),
        }
    }

    /// _container_generic_inference (brain_builtin_inference.py:227-257),
    /// exact port. `klass` is the container class of the builtin being
    /// called (List/Tuple/Set/Frozen).
    fn tip_container(
        &self,
        _node: GNode,
        args: &[GNode],
        klass: ContClass,
        ctx: &Rc<Ctx>,
    ) -> Option<Flow> {
        if args.is_empty() {
            // node_type(lineno=..., parent=node.parent) — fresh empty container
            return Some(Flow::one(cont_build(klass, Vec::new())));
        }
        if args.len() > 1 {
            return None; // UseInferenceDefault
        }
        let arg = args[0];
        // transform(arg) on the RAW node first
        let transformed = match self.cont_transform(&Value::Node(arg), klass, ctx) {
            Err(()) => return None, // _use_default() raised inside the transform
            Ok(t) => t,
        };
        if let Some(t) = transformed {
            return Some(Flow::one(t));
        }
        // not transformed: `next(arg.infer(context=context))` — single pull
        // with the tip's context (brain_builtin_inference.py:248-253);
        // InferenceError/StopIteration -> UseInferenceDefault
        let inferred = self.infer_first(arg, Some(ctx)).ok()?;
        if inferred.is_uninferable() {
            return None;
        }
        let t = self.cont_transform(&inferred, klass, ctx).ok()??;
        if t.is_uninferable() {
            return None;
        }
        Some(Flow::one(t))
    }

    /// the ContClass a value would isinstance-match in
    /// _container_generic_transform's klass/iterables checks
    fn cont_class_of(&self, v: &Value) -> Option<ContClass> {
        match v {
            Value::SynthSeq { kind, .. } => Some(match kind {
                SeqKind::List => ContClass::List,
                SeqKind::Tuple => ContClass::Tuple,
                SeqKind::Set => ContClass::Set,
            }),
            Value::FrozenSet { .. } => Some(ContClass::Frozen),
            Value::DictItems(_) => Some(ContClass::DictItems),
            Value::DictKeys(_) => Some(ContClass::DictKeys),
            Value::DictValues(_) => Some(ContClass::DictValues),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::List { .. } => Some(ContClass::List),
                    NodeKind::Tuple { .. } => Some(ContClass::Tuple),
                    NodeKind::Set { .. } => Some(ContClass::Set),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// elements (`arg.elts`) of a container-ish value for the transform.
    /// DictKeys/Values/Items proxy a synthesized List whose elts are the
    /// dict's key/value nodes (objectmodel.py:856-890).
    fn cont_elts(&self, v: &Value) -> Vec<NV> {
        match v {
            Value::SynthSeq { elems, .. } | Value::FrozenSet { elems } => {
                elems.iter().cloned().map(NV::V).collect()
            }
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::List { elts, .. }
                    | NodeKind::Tuple { elts, .. }
                    | NodeKind::Set { elts } => elts
                        .iter()
                        .map(|&e| NV::N(GNode { m: g.m, n: e }))
                        .collect(),
                    _ => Vec::new(),
                }
            }
            Value::DictKeys(r) | Value::DictValues(r) => {
                let want_key = matches!(v, Value::DictKeys(_));
                match &**r {
                    crate::value::DictRef::Node(g) => {
                        let md = self.md(g.m);
                        match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::Dict { items } => items
                                .iter()
                                .map(|&(k, val)| {
                                    NV::N(GNode {
                                        m: g.m,
                                        n: if want_key { k } else { val },
                                    })
                                })
                                .collect(),
                            _ => Vec::new(),
                        }
                    }
                    crate::value::DictRef::Synth(items) => items
                        .iter()
                        .map(|(k, val)| NV::V(if want_key { k.clone() } else { val.clone() }))
                        .collect(),
                }
            }
            Value::DictItems(r) => {
                // elts are synthetic Tuple nodes pairing key/value, built
                // ONCE per DictItems object (objectmodel.py:856-867 builds
                // the Tuples at attr_items time; the same nodes are seen by
                // every later consumer) — keyed by the DictRef pointer.
                let key = Rc::as_ptr(r) as usize;
                if let Some(cached) = self.dictitems_elts_cache.borrow().get(&key) {
                    return cached.iter().cloned().map(NV::V).collect();
                }
                // pin the DictItems identity (keyed by pointer)
                self.pin_value_identity(v);
                let pairs: Vec<(Value, Value)> = match &**r {
                    crate::value::DictRef::Node(g) => {
                        let md = self.md(g.m);
                        match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::Dict { items } => items
                                .iter()
                                .map(|&(k, val)| {
                                    (
                                        Value::Node(GNode { m: g.m, n: k }),
                                        Value::Node(GNode { m: g.m, n: val }),
                                    )
                                })
                                .collect(),
                            _ => Vec::new(),
                        }
                    }
                    crate::value::DictRef::Synth(items) => items.to_vec(),
                };
                let built: Vec<Value> = pairs
                    .into_iter()
                    .map(|(k, val)| Value::SynthSeq {
                        kind: SeqKind::Tuple,
                        elems: Rc::new(vec![k, val]),
                    })
                    .collect();
                self.dictitems_elts_cache
                    .borrow_mut()
                    .insert(key, Rc::new(built.clone()));
                built.into_iter().map(NV::V).collect()
            }
            _ => Vec::new(),
        }
    }

    /// _container_generic_transform (brain_builtin_inference.py:260-297).
    /// Err(()) = UseInferenceDefault raised mid-transform (_use_default for
    /// non-Const dict keys); Ok(None) = arg not transformable.
    fn cont_transform(
        &self,
        arg: &Value,
        klass: ContClass,
        ctx: &Rc<Ctx>,
    ) -> Result<Option<Value>, ()> {
        let cls = self.cont_class_of(arg);
        // if isinstance(arg, klass): return arg
        if cls == Some(klass) {
            return Ok(Some(arg.clone()));
        }
        // if isinstance(arg, iterables)
        if let Some(c) = cls {
            if cont_iterables(klass).contains(&c) {
                let elts = self.cont_elts(arg);
                let consts: Option<Vec<ConstValue>> = elts
                    .iter()
                    .map(|e| match e {
                        NV::N(g) => {
                            let md = self.md(g.m);
                            match &md.tree.nodes[g.n.idx()].kind {
                                NodeKind::Const(c) => Some(c.clone()),
                                _ => None,
                            }
                        }
                        NV::V(Value::SynthConst(c)) => Some((**c).clone()),
                        // SynthSeq elements carrying REAL Const nodes (binop
                        // list concat keeps the original elt nodes — astroid's
                        // fresh List node passes all(isinstance(elt, Const)))
                        NV::V(Value::Node(g)) => {
                            let md = self.md(g.m);
                            match &md.tree.nodes[g.n.idx()].kind {
                                NodeKind::Const(c) => Some(c.clone()),
                                _ => None,
                            }
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(values) = consts {
                    // build_elts over raw python values (set()/frozenset()
                    // deduplicate by value equality)
                    return Ok(Some(cont_build_consts(klass, values)));
                }
                // EvaluatedObject branch: `if not element: continue`
                // (only Uninferable is falsy), safe_infer with the SAME
                // context; failures skipped. No dedup (astroid TODO).
                let mut out = Vec::new();
                for e in elts {
                    match e {
                        NV::V(Value::Uninferable) => continue,
                        // raw AST nodes — whether held directly (parsed
                        // containers) or inside a fresh Tuple's elts (the
                        // materialized *args tuple holds the call site's
                        // RAW argument nodes, arguments.py infer_argument)
                        // — go through safe_infer; failures skipped
                        NV::N(g) | NV::V(Value::Node(g)) => {
                            // `if inferred:` (brain_builtin_inference.py:281)
                            // — bool(Uninferable) is False: a safe_infer
                            // that resolves to Uninferable is SKIPPED.
                            // Successes are wrapped in an EvaluatedObject
                            // (brain_builtin_inference.py:283-285) — the
                            // element carries the value but exposes NO
                            // getitem (loop-unpack getitem on it raises
                            // AttributeError -> continue).
                            if let Some(v) = self.safe_infer(g, ctx) {
                                if !v.is_uninferable() {
                                    out.push(Value::EvaluatedObject {
                                        value: Rc::new(v),
                                    });
                                }
                            }
                        }
                        NV::V(v) => {
                            // safe_infer of a synthetic NODE (DictModel's
                            // fresh Tuples etc.) completes a real
                            // NodeNG.infer: bump-once per ctx key
                            self.synth_value_pull(&v, ctx);
                            out.push(Value::EvaluatedObject { value: Rc::new(v) });
                        }
                    }
                }
                return Ok(Some(cont_build(klass, out)));
            }
        }
        // elif isinstance(arg, nodes.Dict): keys must already be Const
        if matches!(arg, Value::SynthDict { .. })
            || matches!(arg, Value::Node(g) if matches!(&self.md(g.m).tree.nodes[g.n.idx()].kind, NodeKind::Dict { .. }))
        {
            let items = self.value_dict_items(arg).unwrap_or_default();
            let mut keys = Vec::new();
            for (k, _) in items {
                match self.value_const(&k) {
                    Some(c) => keys.push(c),
                    None => return Err(()), // _use_default()
                }
            }
            return Ok(Some(cont_build_consts(klass, keys)));
        }
        // elif Const str/bytes: elts = arg.value (iterate chars / byte ints)
        if let Some(c) = self.value_const(arg) {
            match c {
                ConstValue::Str(s) => {
                    let vals = s
                        .chars()
                        .map(|ch| ConstValue::Str(ch.to_string().into()))
                        .collect();
                    return Ok(Some(cont_build_consts(klass, vals)));
                }
                ConstValue::Bytes(b) => {
                    let vals = b
                        .iter()
                        .map(|&x| ConstValue::Int(IntValue::Small(x as i64)))
                        .collect();
                    return Ok(Some(cont_build_consts(klass, vals)));
                }
                _ => {}
            }
        }
        Ok(None)
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
        // _get_elts (brain_builtin_inference.py:362-388):
        // `inferred = next(arg.infer(context))` — SINGLE pull (abandoned),
        // NOT safe_infer: dict(parse_qsl(q)) takes the first return value
        let inferred = match arg {
            NV::N(g) => self.infer_first(*g, Some(ctx)).ok()?,
            NV::V(v) => v.clone(),
        };
        if let Some(items) = self.value_dict_items(&inferred) {
            return Some(items);
        }
        // is_iterable: nodes.List/Tuple/Set EXACTLY (no FrozenSet/Const str)
        let is_iter_kind = |v: &Value| -> Option<Vec<NV>> {
            match v {
                Value::Node(g) => {
                    let md = self.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::List { elts, .. }
                        | NodeKind::Tuple { elts, .. }
                        | NodeKind::Set { elts } => Some(
                            elts.iter().map(|&e| NV::N(GNode { m: g.m, n: e })).collect(),
                        ),
                        _ => None,
                    }
                }
                Value::SynthSeq { kind, elems }
                    if matches!(kind, SeqKind::List | SeqKind::Tuple | SeqKind::Set) =>
                {
                    Some(elems.iter().cloned().map(NV::V).collect())
                }
                _ => None,
            }
        };
        let elts = is_iter_kind(&inferred)?;
        let mut out = Vec::new();
        for e in elts {
            let ev = match &e {
                NV::N(g) => Value::Node(*g),
                NV::V(v) => v.clone(),
            };
            // each item must itself be a List/Tuple/Set pair of 2
            let pair = is_iter_kind(&ev)?;
            if pair.len() != 2 {
                return None;
            }
            // elts[0] must be Tuple/Const/Name (hashable-ish filter)
            let key_ok = match &pair[0] {
                NV::N(g) => {
                    let md = self.md(g.m);
                    matches!(
                        &md.tree.nodes[g.n.idx()].kind,
                        NodeKind::Tuple { .. } | NodeKind::Const(_) | NodeKind::Name { .. }
                    )
                }
                NV::V(Value::SynthConst(_)) => true,
                NV::V(Value::SynthSeq { kind: SeqKind::Tuple, .. }) => true,
                _ => false,
            };
            if !key_ok {
                return None;
            }
            let to_v = |nv: &NV| match nv {
                NV::N(g) => Value::Node(*g),
                NV::V(v) => v.clone(),
            };
            out.push((to_v(&pair[0]), to_v(&pair[1])));
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
            // next(values.infer(context=context)) — single pull
            NV::N(g) => match self.infer_first(*g, Some(ctx)) {
                Ok(v) => v,
                Err(_) => return empty(),
            },
            NV::V(v) => v.clone(),
        };
        if inferred.is_uninferable() {
            return empty();
        }
        // container of Consts / str / dict keys.
        // brain_builtin_inference.py:1019-1027: ONLY List/Set/Tuple node
        // classes take the elts branch — DictKeys/Values/Items proxies fall
        // to the else -> empty dict
        let is_seq = matches!(inferred, Value::SynthSeq { .. })
            || matches!(&inferred, Value::Node(g) if self.kind_is(*g, |k| matches!(
                k,
                NodeKind::List { .. } | NodeKind::Tuple { .. } | NodeKind::Set { .. }
            )));
        let keys: Vec<Value> = if let Some(elts) =
            if is_seq { self.value_elts(&inferred) } else { None }
        {
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
        // brain_builtin_inference._infer_copy_method:
        // `node.func.expr.infer(context=context)` — the LIVE context: the
        // receiver's path pushes PERSIST into the caller's path (a later
        // re-pull of the same Name in the default Attribute._infer is then
        // path-blocked -> InferenceError -> the call site yields U).
        // `all(...)` short-circuits: the generator is abandoned right after
        // the first non-container value (its post-yield bump never fires).
        let mut vals: Vec<Value> = Vec::new();
        let mut bad = false;
        let _ = {
            let vals = &mut vals;
            let bad = &mut bad;
            self.infer_to(expr, ctx, &mut |v| {
                let is_container = matches!(
                    v,
                    Value::SynthDict { .. } | Value::SynthSeq { .. } | Value::FrozenSet { .. }
                ) || matches!(&v, Value::Node(g)
                    if self.kind_is(*g, |k| matches!(k,
                        NodeKind::Dict { .. } | NodeKind::List { .. } | NodeKind::Set { .. })));
                if !is_container {
                    *bad = true;
                    return crate::value::Drive::Stop;
                }
                vals.push(v);
                crate::value::Drive::Go
            })
        };
        if bad || vals.is_empty() {
            return None; // UseInferenceDefault
        }
        Some(Flow::ok(vals))
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
        // brain_builtin_inference._infer_str_format_call: any non-Const
        // (incl. ambiguous safe_infer) argument -> Uninferable
        let mut pos_values: Vec<ConstValue> = Vec::new();
        for p in site.positional_arguments() {
            let Some(v) = self.safe_infer_nv(&p, ctx) else {
                return Some(Flow::uninferable());
            };
            match self.value_const(&v) {
                Some(c) => pos_values.push(c),
                None => return Some(Flow::uninferable()),
            }
        }
        let mut kw_values: Vec<(String, ConstValue)> = Vec::new();
        for (k, v) in site.keyword_arguments() {
            let Some(v) = self.safe_infer_nv(&v, ctx) else {
                return Some(Flow::uninferable());
            };
            match self.value_const(&v) {
                Some(c) => kw_values.push((self.sname(k), c)),
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
            // next(partial_function.infer(context=context)) — single pull
            NV::N(g) => self.infer_first(*g, Some(ctx)).ok(),
            NV::V(v) => Some(v.clone()),
        }?;
        // `isinstance(inferred_wrapped_function, nodes.FunctionDef)` —
        // objects.PartialFunction IS a FunctionDef subclass: nested
        // partial(partial(f, 1), 2) passes the check; its own parameter
        // set is empty (no postinit), so any keywords -> UseInferenceDefault
        let (func, wrapped_partial) = match &wrapped {
            Value::Node(g)
                if self.kind_is(*g, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                }) =>
            {
                (*g, false)
            }
            Value::Partial { func, .. } => (*func, true),
            _ => return None,
        };
        // keyword names must be parameters of the wrapped function
        if wrapped_partial {
            if !kwargs.is_empty() {
                return None;
            }
        } else if let Some(spec) = &self.arg_spec(func) {
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
        // PartialFunction.__init__ (objects.py:292-296):
        // `next(wrapped_function.infer())` — a SECOND single pull, NO
        // context (fresh; abandoned generator: no cache write). Only used
        // to merge filled args of a nested PartialFunction.
        let second = match &pos[0] {
            NV::N(g) => self.infer_first_fresh(*g).ok().flatten(),
            NV::V(v) => Some(v.clone()),
        };
        let (filled_args, filled_keywords) = if let Some(Value::Partial {
            filled_args: pa,
            filled_keywords: pk,
            ..
        }) = &second
        {
            let mut args: Vec<GNode> = pa.to_vec();
            args.extend(filled_args);
            // dict-merge: earlier keys keep position, later values win
            let mut kws: Vec<(GSym, GNode)> = pk.to_vec();
            for (k, v) in filled_keywords {
                if let Some(slot) = kws.iter_mut().find(|(ek, _)| *ek == k) {
                    slot.1 = v;
                } else {
                    kws.push((k, v));
                }
            }
            (args, kws)
        } else {
            (filled_args, filled_keywords)
        };
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

/// python repr() of a Const value (the {x!r} conversion)
fn const_py_repr(c: &ConstValue) -> Option<String> {
    Some(match c {
        ConstValue::Str(s) => pyast::pyrepr::repr_str(s),
        ConstValue::Bytes(_) | ConstValue::Complex { .. }
        | ConstValue::StrSurrogate(_) => return None,
        other => const_format_value(other)?,
    })
}

/// str.format with auto/explicit numbering and named fields (no format
/// specs beyond plain {}; specs bail to None -> Uninferable)
fn simple_str_format(
    template: &str,
    pos: &[ConstValue],
    kw: &[(String, ConstValue)],
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
                // `name[!conv][:spec]` — the real str.format applies the
                // full mini-language (astroid folds via the real
                // `.format(...)`); attribute access / nested-brace specs
                // are out of scope -> Uninferable
                let (name_part, spec) = match field.split_once(':') {
                    Some((n, sp)) => (n, Some(sp)),
                    None => (field.as_str(), None),
                };
                // conversion: {x!r} / {x!s} / {x!a} (invalid -> ValueError
                // -> Uninferable)
                let (name_part, conv) = match name_part.split_once('!') {
                    Some((n, c)) if matches!(c, "r" | "s" | "a") => (n, Some(c)),
                    Some(_) => return None,
                    None => (name_part, None),
                };
                if name_part.contains('.') || name_part.contains('[')
                    || spec.is_some_and(|sp| sp.contains('{'))
                {
                    return None;
                }
                let c: &ConstValue = if name_part.is_empty() {
                    let v = pos.get(auto)?;
                    auto += 1;
                    v
                } else if name_part.chars().all(|ch| ch.is_ascii_digit()) {
                    let i: usize = name_part.parse().ok()?;
                    pos.get(i)?
                } else {
                    &kw.iter().find(|(k, _)| *k == *name_part)?.1
                };
                // apply the conversion first; the spec then formats the
                // resulting STRING (CPython Formatter.convert_field)
                let converted: Option<ConstValue> = match conv {
                    None => None,
                    Some("s") => Some(ConstValue::Str(const_format_value(c)?.into())),
                    Some("r") => Some(ConstValue::Str(const_py_repr(c)?.into())),
                    Some("a") => {
                        let r = const_py_repr(c)?;
                        let mut esc = String::new();
                        for ch in r.chars() {
                            let cp = ch as u32;
                            if cp < 0x80 {
                                esc.push(ch);
                            } else if cp <= 0xff {
                                esc.push_str(&format!("\\x{cp:02x}"));
                            } else if cp <= 0xffff {
                                esc.push_str(&format!("\\u{cp:04x}"));
                            } else {
                                esc.push_str(&format!("\\U{cp:08x}"));
                            }
                        }
                        Some(ConstValue::Str(esc.into()))
                    }
                    _ => return None,
                };
                let c = converted.as_ref().unwrap_or(c);
                match spec {
                    None | Some("") => out.push_str(&const_format_value(c)?),
                    Some(sp) => out.push_str(&crate::infer::python_format(c, sp)?),
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

    /// brain_namedtuple_enum.infer_enum (brain_namedtuple_enum.py:309-370):
    /// a synthetic class (parent __astroid_synthetic) whose single base is a
    /// FRESH EnumMeta template ClassDef; yields class_node.instantiate_class().
    fn tip_enum_call(&self, call: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        use crate::value::{Drive, End};
        let func = {
            let md = self.md(call.m);
            match &md.tree.nodes[call.n.idx()].kind {
                NodeKind::Call { func, .. } => GNode { m: call.m, n: *func },
                _ => return None,
            }
        };
        // `any(... for item in node.func.infer(context))` — LAZY pulls under
        // the ctx AS-IS, stopping at the FIRST enum.Enum ClassDef (the rest
        // of the generator is abandoned). Only generator CREATION sits in
        // the try (brain_namedtuple_enum.py:314-317) — an InferenceError
        // raised during the any() iteration propagates out of the tip's
        // eager list() materialization.
        let mut is_enum = false;
        let end = self.infer_to(func, ctx, &mut |v| {
            if let Value::Node(g) = &v {
                if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_)))
                    && self.qname(*g) == "enum.Enum"
                {
                    is_enum = true;
                    return Drive::Stop;
                }
            }
            Drive::Go
        });
        if !is_enum {
            return match end {
                End::Raised(e) => Some(Flow::err(e)),
                _ => None, // UseInferenceDefault
            };
        }
        // enum_meta = _extract_single_node(...) — a fresh template build
        // per invocation (brain_namedtuple_enum.py:325-358)
        let meta_mid = self.build_template_module(ENUM_META_SRC, "")?;
        let meta_cls = {
            let mmd = self.md(meta_mid);
            let locals = mmd.locals.borrow();
            let sym = self.sym("EnumMeta");
            locals
                .get(&NodeId::MODULE)
                .and_then(|l| l.get(&sym))
                .and_then(|v| v.first().copied())?
        };
        // infer_func_form(node, enum_meta, parent=SYNTHETIC_ROOT, enum=True)
        let (args, kws) = self.call_parts(call);
        let name_v = self.func_form_arg(&args, &kws, 0, "typename", ctx)?;
        let names_v = self.func_form_arg(&args, &kws, 1, "field_names", ctx)?;
        // `name.value` — AttributeError on non-Const -> UseInferenceDefault
        let name_cv = self.value_const(&name_v)?.clone();
        // attributes (brain_namedtuple_enum.py:91-131, enum=True branch)
        let attributes: Vec<String> = match self.value_const(&names_v) {
            Some(ConstValue::Str(s)) => s
                .replace(',', " ")
                .split_whitespace()
                .map(String::from)
                .collect(),
            _ => {
                let attrs = self.enum_container_attrs(&names_v, ctx)?;
                if attrs.is_empty() {
                    // `if not attributes: raise AttributeError` (py:130-131)
                    return None;
                }
                attrs
            }
        };
        // namedtuple's str() mapping is SKIPPED for enum=True; the
        // space-filter still applies (brain_namedtuple_enum.py:140)
        let attributes: Vec<String> =
            attributes.into_iter().filter(|a| !a.contains(' ')).collect();
        // `name = name or "Uninferable"` (falsy Const values included)
        let name: String = match &name_cv {
            ConstValue::Str(s) if !s.is_empty() => s.to_string(),
            ConstValue::Str(_) => "Uninferable".to_string(),
            _ => return None, // non-str class names don't occur in practice
        };
        let (lineno, col) = {
            let md = self.md(call.m);
            let n = &md.tree.nodes[call.n.idx()];
            (n.fromlineno, n.col_offset)
        };
        let (cls, base_slots, _, _) =
            self.build_synth_class("__astroid_synthetic", &name, lineno, col, 1, false, 0);
        // bases=[enum_meta] — the template ClassDef NODE itself
        // (brain_namedtuple_enum.py:361-363 FIXME notes the broken
        // parent invariant; inferring the base is ONE ClassDef.infer hop)
        self.redirects
            .borrow_mut()
            .insert(GNode { m: cls.m, n: base_slots[0] }, NV::N(meta_cls));
        {
            let phs = self.alloc_placeholders(attributes.len());
            let mut ia = self.iattrs.borrow_mut();
            let entry = ia.entry(cls).or_default();
            for (attr, ph) in attributes.iter().zip(phs) {
                entry.insert(self.sym(attr), vec![ph]);
            }
        }
        Some(Flow::one(self.instantiate_class(cls)))
    }

    /// infer_func_form enum container handling (brain_namedtuple_enum.py:
    /// 104-131): dict keys / list of pairs / list of strs, each via
    /// _infer_first under the live ctx.
    fn enum_container_attrs(&self, names_v: &Value, ctx: &Rc<Ctx>) -> Option<Vec<String>> {
        let infer_first_str = |g: GNode| -> Option<String> {
            match self.infer_first(g, Some(ctx)) {
                Ok(v) if !v.is_uninferable() => match self.value_const(&v) {
                    Some(ConstValue::Str(s)) => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            }
        };
        match names_v {
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    // `hasattr(names, "items")` — Dict node: Const keys only
                    // (_infer_first(const[0]) — a Const infers to itself)
                    NodeKind::Dict { items } => {
                        let mut out = Vec::new();
                        for (k, _) in items.iter() {
                            if let NodeKind::Const(ConstValue::Str(s)) =
                                &md.tree.nodes[k.idx()].kind
                            {
                                out.push(s.to_string());
                            }
                        }
                        Some(out)
                    }
                    NodeKind::List { elts, .. }
                    | NodeKind::Tuple { elts, .. }
                    | NodeKind::Set { elts } => {
                        let all_tuples = elts.iter().all(|&e| {
                            matches!(md.tree.nodes[e.idx()].kind, NodeKind::Tuple { .. })
                        });
                        let mut out = Vec::new();
                        if all_tuples {
                            for &e in elts.iter() {
                                if let NodeKind::Tuple { elts: pair, .. } =
                                    &md.tree.nodes[e.idx()].kind
                                {
                                    let first = *pair.first()?;
                                    out.push(infer_first_str(GNode { m: g.m, n: first })?);
                                }
                            }
                        } else {
                            for &e in elts.iter() {
                                out.push(infer_first_str(GNode { m: g.m, n: e })?);
                            }
                        }
                        Some(out)
                    }
                    _ => None, // raise AttributeError -> UseInferenceDefault
                }
            }
            _ => None,
        }
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
                // _get_namedtuple_fields (brain_namedtuple_enum.py:611-650):
                // `container = next(node.args[1].infer())` — a SECOND,
                // FRESH single pull (abandoned generator: no cache writes),
                // falling back to the field_names keyword when absent/falsy;
                // non-BaseContainer results -> UseInferenceDefault. The
                // as_string round-trip through extract_node nets out to
                // Const values stringified.
                let container: Option<Value> = if args.len() > 1 {
                    match self.infer_first_fresh(args[1]) {
                        Ok(Some(v)) => Some(v),
                        // StopIteration / InferenceError -> UseInferenceDefault
                        _ => return None,
                    }
                } else {
                    None // IndexError -> pass
                };
                // `if not container:` — Uninferable is falsy
                let mut container = match container {
                    Some(Value::Uninferable) | None => None,
                    c => c,
                };
                if container.is_none() {
                    let fn_sym = self.sym("field_names");
                    if let Some((_, vnode)) = kws.iter().find(|(k, _)| *k == Some(fn_sym)) {
                        container = match self.infer_first_fresh(*vnode) {
                            Ok(Some(v)) => Some(v),
                            _ => return None,
                        };
                    }
                }
                let container = container?;
                let elts = match &container {
                    Value::Node(g) => {
                        let md = self.md(g.m);
                        match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::List { elts, .. }
                            | NodeKind::Tuple { elts, .. }
                            | NodeKind::Set { elts } => Some(
                                elts.iter()
                                    .map(|&e| Value::Node(GNode { m: g.m, n: e }))
                                    .collect::<Vec<Value>>(),
                            ),
                            _ => None,
                        }
                    }
                    // `isinstance(container, nodes.BaseContainer)` is an
                    // EXACT node-class check (brain_namedtuple_enum.py:635)
                    // — dict-view proxies (DictKeys from `d.keys()`) are
                    // NOT BaseContainer -> UseInferenceDefault (airflow
                    // test_container_instances namedtuple-from-keys -> U)
                    Value::DictItems(_) | Value::DictKeys(_) | Value::DictValues(_) => {
                        None
                    }
                    _ => self.value_elts(&container),
                }?;
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
        // bases=[_extract_single_node("tuple")] (brain_namedtuple_enum.py:
        // infer_named_tuple) — the base is a real Name node in a fresh
        // throwaway module; inferring it enters Name.infer THEN the builtins
        // tuple ClassDef.infer (two NodeNG.infer hops, two bumps when the
        // consumer drains). NV::N makes the slot transparent.
        let base_redirect = self
            .template_extract_node("tuple\n")
            .map(crate::value::NV::N)
            .unwrap_or(crate::value::NV::V(Value::Node(self.builtins().tuple)));
        self.redirects
            .borrow_mut()
            .insert(GNode { m: cls.m, n: base_slots[0] }, base_redirect);
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
        // func = util.safe_infer(_extract_single_node("import collections;
        // collections.namedtuple")) — brain_namedtuple_enum.py:201-203 runs
        // this PER TIP INVOCATION: a fresh throwaway module is built and the
        // Attribute is inferred under a context=None fresh ctx (the
        // Attribute -> Name collections -> Import -> FunctionDef module-
        // igetattr chain burns real pulls every run; the namedtuple+Enum
        // mixin member access Role.OP is count-exact only with them).
        let func = match self
            .template_extract_node("import collections; collections.namedtuple\n")
        {
            Some(attr_node) => {
                let fresh = Ctx::new();
                match self.safe_infer(attr_node, &fresh) {
                    Some(Value::Node(g)) => g,
                    Some(Value::BoundMethod { func, .. })
                    | Some(Value::UnboundMethod { func }) => func,
                    _ => return false,
                }
            }
            None => return false,
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
