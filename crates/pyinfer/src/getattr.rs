//! Attribute access: Module/ClassDef/Instance getattr & igetattr, MRO,
//! metaclass resolution, FunctionDef.type, property detection, object
//! models (subset). Ports of astroid scoped_nodes.py + bases.py +
//! interpreter/objectmodel.py — notes/07 §12-13.

use std::rc::Rc;

use indexmap::IndexMap;
use pyast::tree::{ConstValue, IntValue, NodeKind};
use pyast::NodeId;

use crate::ctx::{copy_context, CallCtx, Ctx};
use crate::graph::{Engine, FType};
use crate::infer::Sink;
use crate::value::{DictRef, Drive, End, ErrKind, Flow, GNode, GSym, SeqKind, Value, NV};
use crate::yield_v;

const PROPERTIES: [&str; 4] = [
    "builtins.property",
    "abc.abstractproperty",
    "functools.cached_property",
    "enum.property",
];
const POSSIBLE_PROPERTIES: [&str; 12] = [
    "cached_property",
    "cachedproperty",
    "lazyproperty",
    "lazy_property",
    "reify",
    "lazyattribute",
    "lazy_attribute",
    "LazyProperty",
    "lazy",
    "cache_readonly",
    "DynamicClassAttribute",
    "AwaitableProperty", // not in astroid 4.0.4; removed if diffs say so
];

impl Engine {
    /// TEMP debug: high-level walk markers under PRYLINT_TRACE_INFER
    pub fn twalk(&self, tag: &str, cls: GNode) {
        if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
            eprintln!("WALK {} {}", tag, self.qname(cls));
        }
    }

    // ---------- dispatch igetattr over a value ----------

    /// eager shim: Err only when the generator raised before any yield.
    pub fn igetattr_value(
        &self,
        owner: &Value,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        let mut vals = Vec::new();
        let end = self.igetattr_value_to(owner, name, ctx, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        match end {
            End::Raised(e) if vals.is_empty() => Err(e),
            e => Ok(Flow {
                vals,
                err: e.err_opt(),
            }),
        }
    }

    pub fn igetattr_value_to(
        &self,
        owner: &Value,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        // Result<Flow>-based sub-lookups stream their eager Flow
        let stream_result = |r: Result<Flow, ErrKind>, sink: &mut Sink| -> End {
            match r {
                Ok(flow) => {
                    for v in flow.vals {
                        yield_v!(sink, v);
                    }
                    match flow.err {
                        Some(e) => End::Raised(e),
                        None => End::Done,
                    }
                }
                Err(e) => End::Raised(e),
            }
        };
        match owner {
            Value::Uninferable => {
                yield_v!(sink, Value::Uninferable);
                End::Done
            }
            Value::Node(g) => {
                let tag = {
                    let md = self.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::Module(_) => 1,
                        NodeKind::ClassDef(_) => 2,
                        NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => 3,
                        // Lambda defines getattr but NO igetattr
                        // (scoped_nodes.py:1047-1060 vs FunctionDef's
                        // 1313+): `owner.igetattr(...)` in _infer_attribute
                        // raises AttributeError -> owner skipped
                        // (`lambda x: x` then `.__name__` -> ERR, pandas
                        // test_aggregation)
                        NodeKind::Lambda(_) => 0,
                        NodeKind::Slice { .. } => 4,
                        NodeKind::Const(_)
                        | NodeKind::List { .. }
                        | NodeKind::Tuple { .. }
                        | NodeKind::Set { .. }
                        | NodeKind::Dict { .. } => 5,
                        _ => 0,
                    }
                };
                match tag {
                    1 => {
                        // Module.igetattr (scoped_nodes.py:381-397)
                        let stmts = match self.module_getattr(g.m, name, false) {
                            Ok(s) => s,
                            Err(_) => return End::Raised(ErrKind::Inference),
                        };
                        let ctx2 = copy_context(ctx);
                        ctx2.lookupname.set(Some(name));
                        self.infer_stmts_to(&stmts, Some(&ctx2), Some(*g), sink)
                    }
                    2 => self.class_igetattr_to(*g, name, ctx, true, sink),
                    3 => self.function_igetattr_to(*g, name, ctx, sink),
                    4 => stream_result(self.slice_igetattr(*g, name, ctx), sink),
                    5 => self.instance_igetattr_to(owner, name, ctx, sink),
                    _ => End::Raised(ErrKind::Attribute),
                }
            }
            Value::Inst { .. }
            | Value::ExcInst { .. }
            | Value::SynthConst(_)
            | Value::SynthSeq { .. }
            | Value::SynthDict { .. }
            | Value::FrozenSet { .. }
            | Value::Generator { .. }
            | Value::UnionType => self.instance_igetattr_to(owner, name, ctx, sink),
            Value::SynthSlice { .. } => {
                stream_result(self.synth_slice_igetattr(owner, name, ctx), sink)
            }
            // EvaluatedObject has no igetattr -> AttributeError, owner
            // skipped by _infer_attribute
            Value::EvaluatedObject { .. } => End::Raised(ErrKind::Attribute),
            Value::BoundMethod { func, bound } => {
                stream_result(self.method_igetattr(*func, Some(bound), name, ctx), sink)
            }
            // DescriptorBoundMethod getattr: BoundMethod semantics on the
            // wrapped function
            Value::DescBM { func, inner } => {
                let inner = Rc::clone(inner);
                stream_result(self.method_igetattr(*func, Some(&inner), name, ctx), sink)
            }
            Value::UnboundMethod { func } => {
                stream_result(self.method_igetattr(*func, None, name, ctx), sink)
            }
            Value::Property { func, .. } => {
                stream_result(self.property_igetattr(owner, *func, name, ctx), sink)
            }
            // PartialFunction is a plain FunctionDef subclass: FunctionModel
            Value::Partial { func, .. } => {
                stream_result(self.function_igetattr(*func, name, ctx), sink)
            }
            Value::Super { .. } => self.super_igetattr_to(owner, name, ctx, sink),
            // bare bases.Proxy (objects.py:262-274): __getattr__ delegates
            // igetattr to the _proxied synthesized List NODE, which acts as
            // a builtins.list instance (BaseInstance.igetattr) — e.g.
            // d.keys().sort -> BM:builtins.list.sort
            Value::DictItems(dr) | Value::DictKeys(dr) | Value::DictValues(dr) => {
                let pairs = self.dictref_pairs(dr);
                let elems: Vec<Value> = match owner {
                    Value::DictKeys(_) => pairs.into_iter().map(|(k, _)| k).collect(),
                    Value::DictValues(_) => pairs.into_iter().map(|(_, v)| v).collect(),
                    _ => pairs
                        .into_iter()
                        .map(|(k, v)| Value::SynthSeq {
                            kind: crate::value::SeqKind::Tuple,
                            elems: Rc::new(vec![k, v]),
                        })
                        .collect(),
                };
                let as_list = Value::SynthSeq {
                    kind: crate::value::SeqKind::List,
                    elems: Rc::new(elems),
                };
                self.instance_igetattr_to(&as_list, name, ctx, sink)
            }
        }
    }

    // ---------- Module getattr (§12.6) ----------

    pub fn module_getattr(
        &self,
        m: crate::value::ModId,
        name: GSym,
        ignore_locals: bool,
    ) -> Result<Vec<NV>, ErrKind> {
        let md = self.md(m);
        let name_str = self.sname(name);
        if name_str.is_empty() {
            return Err(ErrKind::Attribute);
        }
        let mut result: Vec<NV> = Vec::new();
        // module-extender VALUE locals (brain_multiprocessing BoundMethods)
        // override the plain node lists — see graph.rs ext_locals.
        let ext_hit: Option<Vec<NV>> = md.ext_locals.borrow().get(&name).cloned();
        let name_in_locals = ext_hit.is_some() || {
            let locals = md.locals.borrow();
            locals
                .get(&NodeId::MODULE)
                .map(|l| l.contains_key(&name))
                .unwrap_or(false)
        };
        if MODULE_MODEL_ATTRS.contains(&name_str.as_str()) && !ignore_locals && !name_in_locals {
            result = vec![NV::V(self.module_model_attr(&md, &name_str))];
            if name_str == "__name__" {
                result.push(NV::V(Value::SynthConst(Rc::new(ConstValue::Str(
                    "__main__".into(),
                )))));
            }
        } else if !ignore_locals && name_in_locals {
            result = match ext_hit {
                Some(list) => list,
                None => {
                    let locals = md.locals.borrow();
                    locals
                        .get(&NodeId::MODULE)
                        .and_then(|l| l.get(&name))
                        .map(|v| v.iter().map(|&g| NV::N(g)).collect())
                        .unwrap_or_default()
                }
            };
        } else if md.package {
            // submodule import fallback (relative_only=True)
            let submod = format!("{}.{}", md.name, name_str);
            match self.ast_from_module_name(&submod, true) {
                Ok(mid) => {
                    result = vec![NV::N(GNode {
                        m: mid,
                        n: NodeId::MODULE,
                    })]
                }
                Err(_) => return Err(ErrKind::Attribute),
            }
        }
        // filter DelName
        result.retain(|nv| match nv {
            NV::N(g) => !self.kind_is(*g, |k| matches!(k, NodeKind::DelName { .. })),
            NV::V(_) => true,
        });
        if result.is_empty() {
            return Err(ErrKind::Attribute);
        }
        Ok(result)
    }

    fn module_model_attr(&self, md: &crate::graph::Module, name: &str) -> Value {
        match name {
            "__name__" => Value::SynthConst(Rc::new(ConstValue::Str(md.name.clone().into()))),
            "__file__" => Value::SynthConst(Rc::new(ConstValue::Str(md.file.clone().into()))),
            "__doc__" => {
                let doc = self.module_doc(md);
                Value::SynthConst(Rc::new(doc))
            }
            "__package__" => {
                let v = if md.package { md.name.clone() } else { String::new() };
                Value::SynthConst(Rc::new(ConstValue::Str(v.into())))
            }
            "__path__" => {
                if !md.package {
                    return Value::Uninferable; // AttributeInferenceError-ish
                }
                let p = std::path::Path::new(&md.file)
                    .parent()
                    .map(|x| x.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Value::SynthSeq {
                    kind: SeqKind::List,
                    elems: Rc::new(vec![Value::SynthConst(Rc::new(ConstValue::Str(p.into())))]),
                }
            }
            "__dict__" | "builtins" => self.dunder_dict_of_locals(md),
            // ObjectModel base attrs (objectmodel.py:136-164): ModuleModel
            // inherits attr___new__/attr___init__ — BoundMethods of the
            // synthetic builtins.object functions bound to the MODULE (the
            // raw builder's `from builtins import __new__` member shims
            // resolve through this, e.g. _ctypes.Structure.__new__)
            "__new__" | "__init__" => {
                let Some((new_fn, init_fn)) = self.obj_model_func_nodes() else {
                    return Value::Uninferable;
                };
                Value::BoundMethod {
                    func: if name == "__new__" { new_fn } else { init_fn },
                    bound: Rc::new(Value::Node(GNode {
                        m: md.id,
                        n: NodeId::MODULE,
                    })),
                }
            }
            // __spec__/__loader__/__cached__ are Unknown -> infer Uninferable
            _ => Value::Uninferable,
        }
    }

    fn module_doc(&self, md: &crate::graph::Module) -> ConstValue {
        match &md.tree.nodes[NodeId::MODULE.idx()].kind {
            NodeKind::Module(d) => match d.doc_node {
                Some(doc) => match &md.tree.nodes[doc.idx()].kind {
                    NodeKind::Const(c) => c.clone(),
                    _ => ConstValue::None,
                },
                None => ConstValue::None,
            },
            _ => ConstValue::None,
        }
    }

    fn dunder_dict_of_locals(&self, md: &crate::graph::Module) -> Value {
        let locals = md.locals.borrow();
        let items: Vec<(Value, Value)> = locals
            .get(&NodeId::MODULE)
            .map(|l| {
                l.iter()
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(&k, v)| {
                        (
                            Value::SynthConst(Rc::new(ConstValue::Str(
                                self.sname(k).into(),
                            ))),
                            Value::Node(v[0]),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Value::SynthDict {
            items: Rc::new(items),
        }
    }

    // ---------- ClassDef getattr / igetattr (§12.4-12.5) ----------

    pub fn class_locals_get(&self, cls: GNode, name: GSym) -> Vec<GNode> {
        let md = self.md(cls.m);
        let locals = md.locals.borrow();
        locals
            .get(&cls.n)
            .and_then(|l| l.get(&name))
            .cloned()
            .unwrap_or_default()
    }

    pub fn class_getattr(
        &self,
        cls: GNode,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
        class_context: bool,
    ) -> Result<Vec<NV>, ErrKind> {
        let name_str = self.sname(name);
        if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
            eprintln!("GETATTR {} .{} cc={}", self.qname(cls), name_str, class_context);
        }
        if name_str.is_empty() {
            return Err(ErrKind::Attribute);
        }
        // ClassDef.implicit_locals(): __module__/__qualname__/__annotations__
        // Consts/Unknown added at class construction -> FIRST in locals
        let which: Option<u8> = match name_str.as_str() {
            "__module__" => Some(0),
            "__qualname__" => Some(1),
            "__annotations__" => Some(2),
            _ => None,
        };
        let mut values: Vec<NV> = Vec::new();
        // snapshot classes already carry the implicit consts as real
        // serialized locals (raw-built ClassDef.__init__ ran in astroid)
        let needs_implicit =
            |e: &Self, c: GNode| -> bool { e.md(c.m).file != "<snapshot>" };
        if let Some(w) = which {
            if needs_implicit(self, cls) {
                values.push(NV::N(self.implicit_class_local(cls, w)));
            }
        }
        values.extend(self.class_locals_get(cls, name).into_iter().map(NV::N));
        for anc in self.ancestors(cls, true, ctx) {
            if let Some(w) = which {
                if needs_implicit(self, anc) {
                    values.push(NV::N(self.implicit_class_local(anc, w)));
                }
            }
            values.extend(self.class_locals_get(anc, name).into_iter().map(NV::N));
        }
        if CLASS_MODEL_ATTRS.contains(&name_str.as_str()) && class_context && values.is_empty() {
            let v = self.class_model_attr(cls, &name_str, ctx);
            // objectmodel.py ClassModel: most attrs are FRESH NODES (Const /
            // Tuple / Dict / Unknown / ClassDef) built per access — they get
            // a real NodeNG.infer hop in _infer_stmts (+1 bump, cap check,
            // fresh-key cache write). Proxy results (attr___call__ Instance,
            // attr_mro / attr___subclasses__ BoundMethods) infer via
            // Proxy.infer `yield self` — NO hop (bases.py:139).
            let hop = match name_str.as_str() {
                "__call__" | "mro" | "__subclasses__" | "__new__" | "__init__" => false,
                _ => true,
            };
            if hop {
                // attr___class__ = helpers.object_type(...) — the REAL
                // ClassDef node: the hop (and its cache write) lands on it
                if let Value::Node(g) = &v {
                    return Ok(vec![NV::N(*g)]);
                }
                return Ok(vec![NV::N(self.model_hop_node(v))]);
            }
            return Ok(vec![NV::V(v)]);
        }
        if class_context {
            values.extend(self.metaclass_lookup_attribute(cls, name, ctx));
        }
        // filter bare AnnAssign declarations
        let mut result: Vec<NV> = Vec::new();
        for v in values {
            if let NV::N(g) = &v {
                if self.kind_is(*g, |k| matches!(k, NodeKind::AssignName { .. })) {
                    if let Some(stmt) = self.statement(*g) {
                        let md = self.md(stmt.m);
                        if let NodeKind::AnnAssign { value, .. } =
                            &md.tree.nodes[stmt.n.idx()].kind
                        {
                            if value.is_none() {
                                continue;
                            }
                        }
                    }
                }
            }
            result.push(v);
        }
        if result.is_empty() {
            return Err(ErrKind::Attribute);
        }
        Ok(result)
    }

    /// _metaclass_lookup_attribute (scoped_nodes.py:2375-2415).
    /// astroid collects into a set (id-ordered); we use insertion order:
    /// implicit metaclass first, declared metaclass second (notes/07 §21.4).
    /// @lru_cache(maxsize=1024) keyed (self, name, context-IDENTITY) —
    /// hits replay the set with NO re-inference (no counter bumps);
    /// hits refresh recency, inserts beyond 1024 evict the LRU entry.
    fn metaclass_lookup_attribute(&self, cls: GNode, name: GSym, ctx: Option<&Rc<Ctx>>) -> Vec<NV> {
        let key = (cls, name, ctx.map(|c| Rc::as_ptr(c) as usize));
        let tick = self.metalookup_tick.get() + 1;
        self.metalookup_tick.set(tick);
        if let Some(entry) = self.metalookup_cache.borrow_mut().get_mut(&key) {
            entry.1 = tick; // refresh recency (lru_cache hit)
            return entry.0.as_ref().clone();
        }
        if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
            eprintln!("MLA {} .{}", self.qname(cls), self.sname(name));
        }
        let out = self.metaclass_lookup_attribute_uncached(cls, name, ctx);
        let mut cache = self.metalookup_cache.borrow_mut();
        if cache.len() >= 1024 {
            if let Some((&oldest, _)) = cache.iter().min_by_key(|(_, e)| e.1) {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (Rc::new(out.clone()), tick, ctx.map(Rc::clone)));
        out
    }

    fn metaclass_lookup_attribute_uncached(
        &self,
        cls: GNode,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Vec<NV> {
        let mut out = Vec::new();
        // scoped_nodes.py:2378 — `attrs = set()` + attrs.update(...): python
        // SET semantics dedup by OBJECT IDENTITY (NodeNG/Proxy define no
        // __eq__). Both metaclass walks (implicit type + declared) yielding
        // the SAME node object (e.g. `__class__` resolving to the one
        // builtins.type ClassDef) collapse to ONE entry — the consumer's
        // _infer_stmts then pulls it once, not twice (the StrEnum
        // metacls.__new__ +4-bump cascade). Fresh objects (BoundMethods
        // constructed below, per-materialization instances) never dedup.
        let mut seen: std::collections::HashSet<crate::infer::DedupKey> =
            std::collections::HashSet::new();
        let mut push = |out: &mut Vec<NV>, v: NV| {
            let key = match &v {
                NV::V(val) => match val {
                    Value::Node(g) => Some(crate::infer::DedupKey::Node(*g)),
                    Value::Uninferable => Some(crate::infer::DedupKey::Uninferable),
                    // python id(): InstId is fresh per materialization and
                    // preserved through clones/cache replays
                    Value::Inst { id, .. } => Some(crate::infer::DedupKey::ExcId(*id)),
                    Value::ExcInst { id, .. } => Some(crate::infer::DedupKey::ExcId(*id)),
                    Value::BoundMethod { func, bound } => Some(crate::infer::DedupKey::BMId(
                        *func,
                        Rc::as_ptr(bound) as *const () as usize,
                    )),
                    Value::Generator { call_ctx, .. } => Some(crate::infer::DedupKey::Ptr(
                        Rc::as_ptr(call_ctx) as *const () as usize,
                    )),
                    _ => None, // treated as always-unique (fresh objects)
                },
                NV::N(g) => Some(crate::infer::DedupKey::Node(*g)),
            };
            if let Some(k) = key {
                if !seen.insert(k) {
                    return;
                }
            }
            out.push(v);
        };
        let implicit = self.builtins().type_;
        // scoped_nodes.py:2380 — `context = copy_context(context)` BEFORE
        // metaclass(): lookupname is reset for the whole metaclass chain
        let ctx = copy_context(ctx);
        let ctx = Some(&ctx);
        let metaclass = self.metaclass(cls, ctx);
        // scoped_nodes.py:2375-2386: `if cls and cls != self` — the
        // implicit metaclass of `type` is `type` itself; without this guard
        // the lookup recurses forever.
        let mut metaclasses: Vec<GNode> = Vec::new();
        if implicit != cls {
            metaclasses.push(implicit);
        }
        if let Some(Value::Node(g)) = metaclass {
            if g != cls && !metaclasses.contains(&g) {
                metaclasses.push(g);
            }
        }
        for meta in metaclasses {
            if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
                eprintln!(
                    "GAFM self={} ({:?},{:?}) meta={} ({:?},{:?}) implicit=({:?},{:?}) .{}",
                    self.qname(cls), cls.m, cls.n,
                    self.qname(meta), meta.m, meta.n,
                    implicit.m, implicit.n,
                    self.sname(name)
                );
            }
            if let Ok(attrs) = self.class_getattr(meta, name, ctx, true) {
                // _get_attribute_from_metaclass (scoped_nodes.py:2388-2415):
                // `for attr in bases._infer_stmts(attrs, context, frame=cls)`
                // — node attrs get a FULL infer hop (bump + cache) and the
                // INFERRED values are then wrapped (properties already
                // resolved to Property objects by FunctionDef._infer)
                let flow = self.infer_stmts(&attrs, ctx, Some(meta));
                for attr in flow.vals {
                    match &attr {
                        Value::Node(g) if self.kind_is(*g, |k| {
                            matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                        }) =>
                        {
                            match self.func_type(*g) {
                                FType::ClassMethod => {
                                    // BoundMethod(attr, get_wrapping_class(attr) or self)
                                    let frame = self.wrapping_class_of(*g).unwrap_or(cls);
                                    push(&mut out, NV::V(Value::BoundMethod {
                                        func: *g,
                                        bound: Rc::new(Value::Node(frame)),
                                    }));
                                }
                                FType::StaticMethod => push(&mut out, NV::V(attr.clone())),
                                _ => push(&mut out, NV::V(Value::BoundMethod {
                                    func: *g,
                                    bound: Rc::new(Value::Node(cls)),
                                })),
                            }
                        }
                        _ => push(&mut out, NV::V(attr.clone())),
                    }
                }
            }
        }
        out
    }

    /// scoped_nodes.get_wrapping_class: nearest ClassDef frame at or above
    fn wrapping_class_of(&self, node: GNode) -> Option<GNode> {
        let mut k = self.frame(node);
        loop {
            if self.kind_is(k, |kind| matches!(kind, NodeKind::ClassDef(_))) {
                return Some(k);
            }
            let parent = self.parent(k)?;
            k = self.frame(parent);
        }
    }

    /// eager shim
    pub fn class_igetattr(
        &self,
        cls: GNode,
        name: GSym,
        ctx_in: Option<&Rc<Ctx>>,
        class_context: bool,
    ) -> Result<Flow, ErrKind> {
        let mut vals = Vec::new();
        let end = self.class_igetattr_to(cls, name, ctx_in, class_context, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        match end {
            End::Raised(e) if vals.is_empty() => Err(e),
            e => Ok(Flow {
                vals,
                err: e.err_opt(),
            }),
        }
    }

    /// next(cls.igetattr(name, ctx)) — single pull, abandoning the
    /// generator (no cache writes for partially-evaluated attributes).
    pub fn class_igetattr_first(
        &self,
        cls: GNode,
        name: GSym,
        ctx_in: Option<&Rc<Ctx>>,
        class_context: bool,
    ) -> Result<Option<Value>, ErrKind> {
        let mut first: Option<Value> = None;
        let end = {
            let first = &mut first;
            self.class_igetattr_to(cls, name, ctx_in, class_context, &mut |v| {
                *first = Some(v);
                Drive::Stop
            })
        };
        match (first, end) {
            (Some(v), _) => Ok(Some(v)),
            (None, End::Raised(e)) => Err(e),
            (None, _) => Ok(None),
        }
    }

    pub fn class_igetattr_to(
        &self,
        cls: GNode,
        name: GSym,
        ctx_in: Option<&Rc<Ctx>>,
        class_context: bool,
        sink: &mut Sink,
    ) -> End {
        let ctx = copy_context(ctx_in);
        ctx.lookupname.set(Some(name));
        let metaclass = self.metaclass(cls, Some(&ctx));
        let mut attributes = match self.class_getattr(cls, name, Some(&ctx), class_context) {
            Ok(a) => a,
            Err(e) => return self.class_igetattr_fallback_to(cls, name, &ctx, e, sink),
        };
        // same-scope filtering for multiple attributes
        // (scoped_nodes.py:2426-2433); proxies resolve .parent via
        // their wrapped function (Proxy.__getattr__)
        if attributes.len() > 1 {
            let scope_of = |nv: &NV| -> Option<GNode> {
                match nv {
                    NV::N(g) => {
                        // implicit class locals: parent IS the owning class
                        // (add_local_node), which is itself a scope
                        if let Some(owner) = self.implicit_owner.borrow().get(g) {
                            return Some(*owner);
                        }
                        self.parent(*g).map(|p| self.scope(p))
                    }
                    NV::V(Value::Node(g)) => {
                        if let Some(owner) = self.implicit_owner.borrow().get(g) {
                            return Some(*owner);
                        }
                        self.parent(*g).map(|p| self.scope(p))
                    }
                    NV::V(Value::BoundMethod { func, .. })
                    | NV::V(Value::UnboundMethod { func })
                    | NV::V(Value::Property { func, .. })
                    | NV::V(Value::Partial { func, .. }) => {
                        self.parent(*func).map(|p| self.scope(p))
                    }
                    NV::V(_) => None,
                }
            };
            let first_scope = scope_of(&attributes[0]);
            if first_scope.is_some() {
                let mut filtered = vec![attributes[0].clone()];
                for attr in &attributes[1..] {
                    let s = scope_of(attr);
                    if s == first_scope || s.is_none() {
                        if s.is_some() {
                            filtered.push(attr.clone());
                        } else if matches!(attr, NV::V(_)) {
                            // model values without parents: kept to
                            // avoid astroid's AttributeError path
                            filtered.push(attr.clone());
                        }
                    }
                }
                attributes = filtered;
            }
        }
        if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
            let descr: Vec<String> = attributes
                .iter()
                .map(|a| match a {
                    NV::N(g) => format!("N:{}", self.qname(*g)),
                    NV::V(v) => format!("V:{}", crate::dump::render(self, v)),
                })
                .collect();
            eprintln!(
                "IGA-ATTRS {} .{} = [{}]",
                self.qname(cls),
                self.sname(name),
                descr.join(", ")
            );
        }
        let functions: Vec<GNode> = attributes
            .iter()
            .filter_map(|a| match a {
                NV::N(g)
                    if self.kind_is(*g, |k| {
                        matches!(
                            k,
                            NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                        )
                    }) =>
                {
                    Some(*g)
                }
                _ => None,
            })
            .collect();
        // setter scan
        let mut setter: Option<GNode> = None;
        'outer: for function in &functions {
            for dec_name in self.decoratornames(*function, Some(&ctx)).into_iter().flatten() {
                if dec_name.rsplit('.').next() == Some("setter") {
                    setter = Some(*function);
                    break 'outer;
                }
            }
        }
        if !functions.is_empty() {
            let last_function = *functions.last().unwrap();
            attributes.retain(|a| match a {
                NV::N(g) => {
                    // bases._is_property(a) — context None (scoped_nodes.py:2467)
                    !functions.contains(g)
                        || *g == last_function
                        || self.is_property(*g, None)
                }
                NV::V(_) => true,
            });
        }
        // stream _infer_stmts, transforming per value
        // (scoped_nodes.py:2452-2483)
        let mut stopped = false;
        let end = {
            let stopped = &mut stopped;
            let ctx = &ctx;
            let metaclass = &metaclass;
            let setter = &setter;
            self.infer_stmts_to(&attributes, Some(ctx), Some(cls), &mut |inferred| {
                let is_const = self.value_const(&inferred).is_some();
                let is_instance = matches!(
                    inferred,
                    Value::Inst { .. }
                        | Value::ExcInst { .. }
                        | Value::SynthSeq { .. }
                        | Value::SynthDict { .. }
                        | Value::FrozenSet { .. }
                        | Value::Generator { .. }
                ) || matches!(&inferred, Value::Node(g)
                    if self.kind_is(*g, |k| matches!(k,
                        NodeKind::List{..} | NodeKind::Tuple{..} | NodeKind::Set{..} | NodeKind::Dict{..})));
                let d = if !is_const && is_instance {
                    // descriptor check: instance of a class with __get__
                    if let Some(pcls) = self.proxied_class(&inferred) {
                        let get_sym = self.sym("__get__");
                        if self.class_getattr(pcls, get_sym, Some(ctx), true).is_ok() {
                            sink(Value::Uninferable)
                        } else {
                            sink(inferred)
                        }
                    } else {
                        sink(inferred)
                    }
                } else if let Value::Property { func, .. } = &inferred {
                    let func = *func;
                    if !class_context {
                        if ctx.callcontext.borrow().is_none() && setter.is_none() {
                            let args = self.func_arg_nodes(func);
                            *ctx.callcontext.borrow_mut() = Some(Rc::new(CallCtx {
                                id: self.next_callctx_id(),
                                args: std::cell::RefCell::new(
                                    args.into_iter().map(crate::value::NV::N).collect(),
                                ),
                                keywords: std::cell::RefCell::new(Vec::new()),
                                callee: std::cell::RefCell::new(Some(inferred.clone())),
                            }));
                        }
                        let mut inner_stop = false;
                        let _ = self.function_infer_call_result_to(func, Some(cls), Some(ctx), &mut |v| {
                            let d = sink(v);
                            if let Drive::Stop = d {
                                inner_stop = true;
                            }
                            d
                        });
                        if inner_stop {
                            Drive::Stop
                        } else {
                            Drive::Go
                        }
                    } else if metaclass.is_some() && {
                        let fscope = self.parent(func).map(|p| self.scope(p));
                        match (&fscope, metaclass) {
                            (Some(fs), Some(Value::Node(mg))) => fs == mg,
                            _ => false,
                        }
                    } {
                        let mut inner_stop = false;
                        let _ = self.function_infer_call_result_to(func, Some(cls), Some(ctx), &mut |v| {
                            let d = sink(v);
                            if let Drive::Stop = d {
                                inner_stop = true;
                            }
                            d
                        });
                        if inner_stop {
                            Drive::Stop
                        } else {
                            Drive::Go
                        }
                    } else {
                        sink(inferred)
                    }
                } else {
                    sink(self.function_to_method(&inferred, cls))
                };
                if let Drive::Stop = d {
                    *stopped = true;
                }
                d
            })
        };
        if stopped {
            return End::Stopped;
        }
        end
    }

    fn class_igetattr_fallback_to(
        &self,
        cls: GNode,
        name: GSym,
        ctx: &Rc<Ctx>,
        _e: ErrKind,
        sink: &mut Sink,
    ) -> End {
        let name_str = self.sname(name);
        if !name_str.starts_with("__") && self.has_dynamic_getattr(cls, ctx) {
            yield_v!(sink, Value::Uninferable);
            End::Done
        } else {
            End::Raised(ErrKind::Inference)
        }
    }

    /// scoped_nodes.py:166-174 function_to_method
    fn function_to_method(&self, v: &Value, klass: GNode) -> Value {
        if let Value::Node(g) = v {
            if self.kind_is(*g, |k| {
                matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
            }) {
                return match self.func_type(*g) {
                    FType::ClassMethod => Value::BoundMethod {
                        func: *g,
                        bound: Rc::new(Value::Node(klass)),
                    },
                    FType::StaticMethod => v.clone(),
                    _ => Value::UnboundMethod { func: *g },
                };
            }
        }
        v.clone()
    }

    /// scoped_nodes.py:2516-2538 has_dynamic_getattr
    pub fn has_dynamic_getattr(&self, cls: GNode, ctx: &Rc<Ctx>) -> bool {
        // _valid_getattr(self.getattr(name, context)[0]) — the FIRST attr
        // only (scoped_nodes.py:2516-2538): IPlugin(threading.local)'s
        // first __getattribute__ comes from C `_thread._local` (not
        // pure_python) -> False -> the missing-attr lookup stays ERR even
        // though _threading_local.local's pure-python one is further down.
        // __getattribute__ is consulted ONLY when __getattr__ is not found
        // (the first `return` short-circuits an invalid __getattr__).
        let look = |name: &str| -> Option<bool> {
            let sym = self.sym(name);
            match self.class_getattr(cls, sym, Some(ctx), true) {
                Ok(attrs) => Some(match attrs.first() {
                    Some(NV::N(g)) => {
                        let md = self.md(g.m);
                        md.pure_python && md.name != "builtins"
                    }
                    _ => false,
                }),
                Err(_) => None,
            }
        };
        match look("__getattr__") {
            Some(v) => v,
            None => look("__getattribute__").unwrap_or(false),
        }
    }

    // ---------- Instance getattr / igetattr (§12.2-12.3) ----------

    /// ClassDef.instance_attr (scoped_nodes.py:2281-2301)
    /// ClassDef.instance_attr + instance_attr_ancestors
    /// (scoped_nodes.py): the ancestors walk gets the caller's context —
    /// including the lookupname mutated by Instance.igetattr (bases.py:281)
    pub fn instance_attr(&self, cls: GNode, name: GSym, ctx: Option<&Rc<Ctx>>) -> Result<Vec<GNode>, ErrKind> {
        self.instance_attr_of(cls, None, name, ctx)
    }

    /// owner-aware variant: instances of the object_type PROXY classes
    /// read the per-(class, InstId) attrs (astroid: fresh
    /// _build_proxy_class per evaluation — the shared snapshot class never
    /// holds entries; the fresh class has NO bases so its ancestors walk is
    /// just [object])
    pub fn instance_attr_of(
        &self,
        cls: GNode,
        inst: Option<crate::value::InstId>,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Vec<GNode>, ErrKind> {
        if self.is_object_type_proxy_cls(cls) {
            let mut values: Vec<GNode> = match inst {
                Some(id) => self
                    .proxy_iattrs
                    .borrow()
                    .get(&(cls, id))
                    .and_then(|m| m.get(&name))
                    .cloned()
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            // ancestors of the fresh proxy class: [builtins.object]
            let obj = self.builtins().object;
            if let Some(v) = self.iattrs.borrow().get(&obj).and_then(|m| m.get(&name)) {
                values.extend(v.iter().copied());
            }
            values.retain(|g| !self.kind_is(*g, |k| matches!(k, NodeKind::DelAttr { .. })));
            return if values.is_empty() {
                Err(ErrKind::Attribute)
            } else {
                Ok(values)
            };
        }
        let mut values: Vec<GNode> = self
            .iattrs
            .borrow()
            .get(&cls)
            .and_then(|m| m.get(&name))
            .cloned()
            .unwrap_or_default();
        for anc in self.ancestors(cls, true, ctx) {
            if let Some(v) = self.iattrs.borrow().get(&anc).and_then(|m| m.get(&name)) {
                values.extend(v.iter().copied());
            }
        }
        values.retain(|g| !self.kind_is(*g, |k| matches!(k, NodeKind::DelAttr { .. })));
        if values.is_empty() {
            Err(ErrKind::Attribute)
        } else {
            Ok(values)
        }
    }

    /// BaseInstance.getattr (bases.py:243-272)
    pub fn instance_getattr(
        &self,
        owner: &Value,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
        lookupclass: bool,
    ) -> Result<Vec<NV>, ErrKind> {
        let cls = match self.proxied_class(owner) {
            Some(c) => c,
            None => return Err(ErrKind::Attribute),
        };
        let inst_id = match owner {
            Value::Inst { id, .. } | Value::ExcInst { id, .. } => Some(*id),
            _ => None,
        };
        match self.instance_attr_of(cls, inst_id, name, ctx) {
            Err(_) => {
                let name_str = self.sname(name);
                if let Some(v) = self.instance_special_attr(owner, &name_str, ctx) {
                    return Ok(vec![NV::V(v)]);
                }
                if lookupclass {
                    return self.class_getattr(cls, name, ctx, false);
                }
                Err(ErrKind::Attribute)
            }
            Ok(values) => {
                let mut out: Vec<NV> = values.into_iter().map(NV::N).collect();
                if lookupclass {
                    if let Ok(cv) = self.class_getattr(cls, name, ctx, false) {
                        out.extend(cv);
                    }
                }
                Ok(out)
            }
        }
    }

    /// eager shim
    pub fn instance_igetattr(
        &self,
        owner: &Value,
        name: GSym,
        ctx_in: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        let mut vals = Vec::new();
        let end = self.instance_igetattr_to(owner, name, ctx_in, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        match end {
            End::Raised(e) if vals.is_empty() => Err(e),
            e => Ok(Flow {
                vals,
                err: e.err_opt(),
            }),
        }
    }

    /// BaseInstance.igetattr (bases.py:274-297)
    pub fn instance_igetattr_to(
        &self,
        owner: &Value,
        name: GSym,
        ctx_in: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        let ctx = match ctx_in {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        ctx.lookupname.set(Some(name));
        match self.instance_getattr(owner, name, Some(&ctx), false) {
            Ok(attrs) => {
                // bases.py:283-285: `_infer_stmts(self._wrap_attr(get_attr,
                // context), context, frame=self)` — _wrap_attr runs over the
                // RAW attr list BEFORE inference: AssignAttr nodes pass
                // through untouched, only UnboundMethod VALUES get
                // bound/property-resolved. The INFERRED results are yielded
                // RAW — an instance attr holding `StreamOutput.recv` stays
                // UM (core stream/conftest _original_recv).
                let mut wrapped: Vec<NV> = Vec::new();
                for a in &attrs {
                    match a {
                        NV::V(Value::UnboundMethod { func }) => {
                            if self.is_property(*func, None) {
                                let mut vals: Vec<Value> = Vec::new();
                                let _ = self.function_infer_call_result_to(
                                    *func,
                                    None,
                                    Some(&ctx),
                                    &mut |x| {
                                        vals.push(x);
                                        Drive::Go
                                    },
                                );
                                wrapped.extend(vals.into_iter().map(NV::V));
                            } else {
                                wrapped.push(NV::V(Value::BoundMethod {
                                    func: *func,
                                    bound: Rc::new(owner.clone()),
                                }));
                            }
                        }
                        other => wrapped.push(other.clone()),
                    }
                }
                self.infer_stmts_to(&wrapped, Some(&ctx), None, sink)
            }
            Err(_) => {
                // fallback to class igetattr (descriptor logic)
                let cls = match self.proxied_class(owner) {
                    Some(c) => c,
                    None => return End::Raised(ErrKind::Inference),
                };
                let mut stopped = false;
                let mut any = false;
                let end = {
                    let stopped = &mut stopped;
                    let any = &mut any;
                    let ctx2 = Rc::clone(&ctx);
                    self.class_igetattr_to(cls, name, Some(&ctx), false, &mut |v| {
                        *any = true;
                        let d = self.wrap_value_to(owner, v, &ctx2, sink);
                        if let Drive::Stop = d {
                            *stopped = true;
                        }
                        d
                    })
                };
                if stopped {
                    return End::Stopped;
                }
                match end {
                    End::Raised(_) if !any => End::Raised(ErrKind::Inference),
                    e => e,
                }
            }
        }
    }

    /// _wrap_attr (bases.py:299-315) applied per inferred value; streams
    /// property call results.
    fn wrap_value_to(&self, owner: &Value, v: Value, ctx: &Rc<Ctx>, sink: &mut Sink) -> Drive {
        match &v {
            Value::UnboundMethod { func } => {
                // _is_property(attr) — context None (bases.py:305)
                if self.is_property(*func, None) {
                    let mut stopped = false;
                    let _ = self.function_infer_call_result_to(*func, None, Some(ctx), &mut |x| {
                        let d = sink(x);
                        if let Drive::Stop = d {
                            stopped = true;
                        }
                        d
                    });
                    if stopped {
                        Drive::Stop
                    } else {
                        Drive::Go
                    }
                } else {
                    sink(Value::BoundMethod {
                        func: *func,
                        bound: Rc::new(owner.clone()),
                    })
                }
            }
            Value::BoundMethod { func, .. } => {
                // bases.py:304: `isinstance(attr, UnboundMethod)` — BoundMethod
                // IS an UnboundMethod subclass! Classmethods arriving from the
                // class-igetattr fallback (function_to_method wrapped them as
                // BoundMethod(n, klass)) re-run the FULL _is_property walk
                // here (decoratornames + safe_infer pulls, all UNCACHED in
                // astroid), then get re-wrapped `BoundMethod(attr, self)` —
                // the outer bind is overridden by the inner one at call time
                // (BoundMethod.infer_call_result rebinds to self.bound), so
                // we keep the inner BM value.
                if self.is_property(*func, None) {
                    // bases.py:306 `yield from attr.infer_call_result(self,
                    // context)` — through the BM's bind semantics
                    let mut stopped = false;
                    let _ = self.infer_call_result_to(&v, None, Some(ctx), &mut |x| {
                        let d = sink(x);
                        if let Drive::Stop = d {
                            stopped = true;
                        }
                        d
                    });
                    if stopped {
                        Drive::Stop
                    } else {
                        Drive::Go
                    }
                } else {
                    sink(v)
                }
            }
            Value::Node(g)
                if self.kind_is(*g, |k| matches!(k, NodeKind::Lambda(_))) =>
            {
                // bind lambdas whose first arg is literally `self`
                let md = self.md(g.m);
                let first_arg_self = match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Lambda(d) => match &md.tree.nodes[d.args.idx()].kind {
                        NodeKind::Arguments(a) => a.args.first().map(|&arg| {
                            match &md.tree.nodes[arg.idx()].kind {
                                NodeKind::AssignName { name } => {
                                    md.tree.s(*name) == "self"
                                }
                                _ => false,
                            }
                        }) == Some(true),
                        _ => false,
                    },
                    _ => false,
                };
                if first_arg_self {
                    sink(Value::BoundMethod {
                        func: *g,
                        bound: Rc::new(owner.clone()),
                    })
                } else {
                    sink(v)
                }
            }
            _ => sink(v),
        }
    }

    fn instance_special_attr(
        &self,
        owner: &Value,
        name: &str,
        _ctx: Option<&Rc<Ctx>>,
    ) -> Option<Value> {
        let cls = self.proxied_class(owner)?;
        // Exception instance extras first
        if let Value::ExcInst { exceptions, .. } = owner {
            match name {
                "args" => {
                    return Some(Value::SynthSeq {
                        kind: SeqKind::Tuple,
                        elems: Rc::new(Vec::new()),
                    })
                }
                "__traceback__" => {
                    let tb = self.builtins().traceback;
                    return Some(Value::Inst { cls: tb, id: crate::value::fresh_inst_id() });
                }
                "exceptions" => {
                    if let Some(ex) = exceptions {
                        return Some(Value::SynthSeq {
                            kind: SeqKind::List,
                            elems: Rc::new(ex.to_vec()),
                        });
                    }
                    // GroupExceptionInstanceModel.attr_exceptions
                    // (objectmodel.py:773-776): a FRESH empty Tuple per
                    // access — EXACT qname builtins.ExceptionGroup only
                    // (BUILTIN_EXCEPTIONS, objectmodel.py:813)
                    if self.qname(cls) == "builtins.ExceptionGroup" {
                        return Some(Value::SynthSeq {
                            kind: SeqKind::Tuple,
                            elems: Rc::new(Vec::new()),
                        });
                    }
                }
                "text" => {
                    // SyntaxError model
                    if self.qname(cls) == "builtins.SyntaxError" {
                        return Some(Value::SynthConst(Rc::new(ConstValue::Str("".into()))));
                    }
                }
                // OSErrorInstanceModel (objectmodel.py:779-792): selected
                // for the EXACT qnames in BUILTIN_EXCEPTIONS (OSError +
                // aliases/subclasses listed there)
                "filename" | "strerror" | "filename2" | "errno" => {
                    const OSERROR_QNAMES: &[&str] = &[
                        "builtins.OSError",
                        "builtins.BlockingIOError",
                        "builtins.BrokenPipeError",
                        "builtins.ChildProcessError",
                        "builtins.ConnectionAbortedError",
                        "builtins.ConnectionError",
                        "builtins.ConnectionRefusedError",
                        "builtins.ConnectionResetError",
                        "builtins.FileExistsError",
                        "builtins.FileNotFoundError",
                        "builtins.InterruptedError",
                        "builtins.IsADirectoryError",
                        "builtins.NotADirectoryError",
                        "builtins.PermissionError",
                        "builtins.ProcessLookupError",
                        "builtins.TimeoutError",
                    ];
                    if OSERROR_QNAMES.contains(&self.qname(cls).as_str()) {
                        return Some(Value::SynthConst(Rc::new(if name == "errno" {
                            ConstValue::Int(IntValue::Small(0))
                        } else {
                            ConstValue::Str("".into())
                        })));
                    }
                }
                // ImportErrorInstanceModel (objectmodel.py:795-803)
                "name" | "path" => {
                    if self.qname(cls) == "builtins.ImportError" {
                        return Some(Value::SynthConst(Rc::new(ConstValue::Str("".into()))));
                    }
                }
                // UnicodeDecodeErrorInstanceModel (objectmodel.py:805-808)
                "object" => {
                    if self.qname(cls) == "builtins.UnicodeDecodeError" {
                        return Some(Value::SynthConst(Rc::new(ConstValue::Bytes(
                            Vec::new().into(),
                        ))));
                    }
                }
                _ => {}
            }
        }
        // DictModel (objectmodel.py:840-889): items/keys/values resolve to
        // a special BoundMethod proxying `next(dict_cls.igetattr(name))`
        // (inference side effect!) whose infer_call_result yields the
        // DictItems/DictKeys/DictValues object — see the qname carve-out in
        // bound_method_infer_call_result_to.
        let dict_owner = matches!(owner, Value::SynthDict { .. })
            || matches!(owner, Value::Node(g) if self.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })));
        if dict_owner {
            if matches!(name, "items" | "keys" | "values") {
                let dict_cls = self.builtins().dict;
                let sym = self.sym(name);
                // `next(self._instance._proxied.igetattr(name), None)` —
                // class_context=True wraps the method as UnboundMethod
                // (function_to_method) before the single pull
                let meth = self.class_igetattr_first(dict_cls, sym, None, true).ok().flatten();
                match meth {
                    Some(Value::Node(f)) | Some(Value::UnboundMethod { func: f }) => {
                        return Some(Value::BoundMethod {
                            func: f,
                            bound: Rc::new(owner.clone()),
                        });
                    }
                    _ => return None,
                }
            }
        }
        // Dict literals/instances use DictModel (objects.py:256, set on the
        // Dict node class), which has ONLY __class__ + items/keys/values —
        // __module__/__doc__/__dict__ fall through to the class lookup
        // (`{}.__doc__` is an InferenceError in astroid).
        if dict_owner && matches!(name, "__module__" | "__doc__" | "__dict__") {
            return None;
        }
        // GeneratorBaseModel mixes in ContextManagerModel
        // (objectmodel.py:640-684,696): gen.__enter__/__exit__ resolve to
        // fresh synthetic FunctionDefs PARENTED TO builtins.object
        // (extract_node per access), wrapped as BoundMethods bound to
        // _get_bound_node(model) = the generator class. Calling them yields
        // Const(None) (`...` body, no returns).
        if let Value::Generator { is_async, .. } = owner {
            if matches!(name, "__enter__" | "__exit__") {
                let src = if name == "__enter__" {
                    "def __enter__(self): ...\n"
                } else {
                    "def __exit__(self, exc_type, exc_value, traceback): ...\n"
                };
                // template module named builtins.object so the func qname
                // composes to builtins.object.__enter__ (like the
                // ObjectModel __new__/__init__ templates)
                let mid = self.build_template_module(src, "builtins.object")?;
                let f = {
                    let fmd = self.md(mid);
                    let locals = fmd.locals.borrow();
                    locals
                        .get(&pyast::NodeId::MODULE)
                        .and_then(|l| l.get(&self.sym(name)))
                        .and_then(|v| v.first())
                        .copied()?
                };
                let b = self.builtins();
                let bound = if *is_async { b.async_generator } else { b.generator };
                return Some(Value::BoundMethod {
                    func: f,
                    bound: Rc::new(Value::Node(bound)),
                });
            }
        }
        match name {
            "__class__" => Some(Value::Node(cls)),
            // InstanceModel.attr___module__ = Const(self._instance.root()
            // .qname()) (objectmodel.py:735-737): for node-backed instances
            // (Const/List/Tuple/Set literals subclass bases.Instance!) the
            // root is the module CONTAINING THE LITERAL, not the proxied
            // builtin class's module (django: `backend_class = None` →
            // None.__module__ == 'tests.mail.test_backends')
            "__module__" => {
                let modname = match owner {
                    Value::Node(g) => self.md(g.m).name.clone(),
                    _ => self.md(cls.m).name.clone(),
                };
                Some(Value::SynthConst(Rc::new(ConstValue::Str(modname.into()))))
            }
            // InstanceModel.attr___doc__ (objectmodel.py:744-746):
            // Const(getattr(self._instance.doc_node, "value", None)) —
            // Instance proxies doc_node to the CLASS docstring
            "__doc__" => {
                let doc = {
                    let md = self.md(cls.m);
                    match &md.tree.nodes[cls.n.idx()].kind {
                        NodeKind::ClassDef(d) => {
                            d.doc_node.and_then(|doc| match &md.tree.nodes[doc.idx()].kind {
                                NodeKind::Const(c) => Some(c.clone()),
                                _ => None,
                            })
                        }
                        _ => None,
                    }
                };
                Some(Value::SynthConst(Rc::new(doc.unwrap_or(ConstValue::None))))
            }
            // InstanceModel.attr___dict__ (objectmodel.py:747-749):
            // _dunder_dict(instance, instance.instance_attrs) — keys are
            // Const(attr name), values the LAST assignment node per attr
            // (objectmodel.py:49-68; instance_attrs is the proxied class's
            // OWN dict, no ancestors)
            "__dict__" => {
                let collect = |attrs: &IndexMap<GSym, Vec<GNode>>| -> Vec<(Value, Value)> {
                    attrs
                        .iter()
                        .filter(|(_, v)| !v.is_empty())
                        .map(|(&k, v)| {
                            (
                                Value::SynthConst(Rc::new(ConstValue::Str(
                                    self.sname(k).into(),
                                ))),
                                Value::Node(*v.last().unwrap()),
                            )
                        })
                        .collect()
                };
                // object_type proxy-class instances read their
                // per-evaluation class attrs (fresh _build_proxy_class)
                let items: Vec<(Value, Value)> = if self.is_object_type_proxy_cls(cls) {
                    match owner {
                        Value::Inst { id, .. } | Value::ExcInst { id, .. } => self
                            .proxy_iattrs
                            .borrow()
                            .get(&(cls, *id))
                            .map(|m| collect(m))
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    }
                } else {
                    self.iattrs
                        .borrow()
                        .get(&cls)
                        .map(|m| collect(m))
                        .unwrap_or_default()
                };
                Some(Value::SynthDict {
                    items: Rc::new(items),
                })
            }
            // ObjectModel.attr___new__/attr___init__ (objectmodel.py:136-164):
            // synthetic FunctionDefs parented to builtins.object, wrapped as
            // BoundMethod(proxy=node, bound=instance)
            "__new__" => {
                let (new_fn, _) = self.obj_model_func_nodes()?;
                Some(Value::BoundMethod {
                    func: new_fn,
                    bound: Rc::new(owner.clone()),
                })
            }
            "__init__" => {
                let (_, init_fn) = self.obj_model_func_nodes()?;
                Some(Value::BoundMethod {
                    func: init_fn,
                    bound: Rc::new(owner.clone()),
                })
            }
            _ => None,
        }
    }

    /// lazily build the ObjectModel __new__/__init__ template module
    /// (objectmodel.py:136-164); the host module is named builtins.object so
    /// qname() composes to builtins.object.__new__ / builtins.object.__init__
    fn obj_model_func_nodes(&self) -> Option<(GNode, GNode)> {
        if let Some(p) = *self.obj_model_funcs.borrow() {
            return Some(p);
        }
        let mid = self.build_template_module(
            "def __new__(self, cls): return cls()\ndef __init__(self, *args, **kwargs): return None\n",
            "builtins.object",
        )?;
        let md = self.md(mid);
        let locals = md.locals.borrow();
        let map = locals.get(&NodeId::MODULE)?;
        let new_sym = self.interner.borrow_mut().intern("__new__");
        let init_sym = self.interner.borrow_mut().intern("__init__");
        let new_fn = *map.get(&new_sym)?.first()?;
        let init_fn = *map.get(&init_sym)?.first()?;
        drop(locals);
        *self.obj_model_funcs.borrow_mut() = Some((new_fn, init_fn));
        Some((new_fn, init_fn))
    }

    // ---------- FunctionDef getattr ----------

    /// STREAMING FunctionDef.igetattr: the instance_attrs branch relays
    /// values lazily through _infer_stmts (scoped_nodes.py:1298-1311 —
    /// `bases._infer_stmts(self.getattr(...))` is a generator: each value
    /// reaches the outer Attribute/Call relays BEFORE the next stmt is
    /// pulled, so resume bumps land in astroid's order; salt
    /// `set_logging_options_dict.__options_dict__` chains).
    fn function_igetattr_to(
        &self,
        func: GNode,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        let vals = self
            .iattrs
            .borrow()
            .get(&func)
            .and_then(|m| m.get(&name))
            .cloned();
        if let Some(vals) = vals {
            if !vals.is_empty() {
                let mut nv: Vec<NV> = vals.into_iter().map(NV::N).collect();
                // FunctionDef.getattr APPENDS the special-attribute model
                // result to the instance_attrs list (scoped_nodes.py:
                // 1303-1306: `found_attrs.append(special_attributes
                // .lookup(name))`) — `view.__doc__` yields the AssignAttr
                // value AND the model Const (django as_view pair)
                let name_str = self.sname(name);
                if let Some(mv) = self.function_model_attr(func, &name_str) {
                    nv.push(match &mv {
                        Value::Node(g) => NV::N(*g),
                        _ => NV::V(mv),
                    });
                }
                let ctx2 = copy_context(ctx);
                ctx2.lookupname.set(Some(name));
                return self.infer_stmts_to(&nv, Some(&ctx2), None, sink);
            }
        }
        match self.function_igetattr_rest(func, name, ctx) {
            Ok(flow) => {
                for v in flow.vals {
                    yield_v!(sink, v);
                }
                match flow.err {
                    Some(e) => End::Raised(e),
                    None => End::Done,
                }
            }
            Err(e) => End::Raised(e),
        }
    }

    fn function_igetattr(
        &self,
        func: GNode,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        // instance_attrs + appended model attr (scoped_nodes.py:1298-1311)
        if let Some(vals) = self
            .iattrs
            .borrow()
            .get(&func)
            .and_then(|m| m.get(&name))
            .cloned()
        {
            if !vals.is_empty() {
                let mut nv: Vec<NV> = vals.into_iter().map(NV::N).collect();
                let name_str = self.sname(name);
                if let Some(mv) = self.function_model_attr(func, &name_str) {
                    nv.push(match &mv {
                        Value::Node(g) => NV::N(*g),
                        _ => NV::V(mv),
                    });
                }
                let ctx2 = copy_context(ctx);
                ctx2.lookupname.set(Some(name));
                return Ok(self.infer_stmts(&nv, Some(&ctx2), None));
            }
        }
        self.function_igetattr_rest(func, name, ctx)
    }

    /// model-attr / lru-model portion of FunctionDef.igetattr (all
    /// single-value results -- eager is bump-equivalent)
    fn function_igetattr_rest(
        &self,
        func: GNode,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        let name_str = self.sname(name);
        // LruWrappedModel (brain_functools.py:26-62): replaces the
        // FunctionModel for lru_cache-decorated functions
        if self.lru_wrapped.borrow().contains(&func) {
            match name_str.as_str() {
                "__wrapped__" => return Ok(Flow::one(Value::Node(func))),
                "cache_clear" => {
                    let f = self.lru_cache_clear_template();
                    let bound = self.parent(func).map(|p| self.scope(p));
                    return Ok(Flow::one(Value::BoundMethod {
                        func: f,
                        bound: Rc::new(match bound {
                            Some(b) => Value::Node(b),
                            None => Value::Uninferable,
                        }),
                    }));
                }
                "cache_info" => {
                    // attr_cache_info (brain_functools.py:38-56): a FRESH
                    // `_CacheInfo(0, 0, 0, 0)` extract_node template per
                    // access; the CacheInfoBoundMethod proxies the FUNCTION
                    // (renders BM:<func qname>, bound = the function) and
                    // its infer_call_result yields safe_infer(cache_info)
                    // (Inst:__astroid_synthetic.CacheInfo via the
                    // namedtuple brain on functools._CacheInfo).
                    if let Some(call) = self.lru_cacheinfo_template() {
                        self.cacheinfo_calls.borrow_mut().insert(func, call);
                    }
                    return Ok(Flow::one(Value::BoundMethod {
                        func,
                        bound: Rc::new(Value::Node(func)),
                    }));
                }
                _ => {}
            }
        }
        if let Some(v) = self.function_model_attr(func, &name_str) {
            // FunctionDef.igetattr runs the model result through
            // _infer_stmts (scoped_nodes.py:1298-1311): fresh nodes
            // (Unknown -> U, Const/Tuple/Dict) get a full stmt.infer hop
            // (+1 bump); proxies (DescBM) pass through hop-free. The
            // Bound/UnboundMethod model path yields RAW instead — see
            // method_igetattr.
            let nv = vec![match &v {
                Value::Node(g) => NV::N(*g),
                _ => NV::V(v),
            }];
            let ctx2 = copy_context(ctx);
            ctx2.lookupname.set(Some(name));
            return Ok(self.infer_stmts(&nv, Some(&ctx2), None));
        }
        Err(ErrKind::Inference)
    }

    /// extract_node("def cache_clear(self): pass") — module name '' so the
    /// BM renders as BM:.cache_clear
    fn lru_cache_clear_template(&self) -> GNode {
        if let Some(g) = *self.lru_cache_clear_fn.borrow() {
            return g;
        }
        let g = self
            .build_template_module("def cache_clear(self): pass\n", "")
            .map(|mid| {
                let md = self.md(mid);
                let locals = md.locals.borrow();
                locals
                    .get(&pyast::NodeId::MODULE)
                    .and_then(|l| l.get(&self.sym("cache_clear")))
                    .and_then(|v| v.first())
                    .copied()
            })
            .flatten()
            .unwrap_or(GNode {
                m: crate::value::ModId(0),
                n: pyast::NodeId::MODULE,
            });
        *self.lru_cache_clear_fn.borrow_mut() = Some(g);
        g
    }

    /// extract_node("from functools import _CacheInfo\n_CacheInfo(0,0,0,0)")
    /// — built FRESH per attr_cache_info access (brain_functools.py:39-44);
    /// returns the template's Call node.
    fn lru_cacheinfo_template(&self) -> Option<GNode> {
        let mid = self.build_template_module(
            "from functools import _CacheInfo\n_CacheInfo(0, 0, 0, 0)\n",
            "",
        )?;
        let md = self.md(mid);
        let body: Vec<pyast::NodeId> = match &md.tree.nodes[pyast::NodeId::MODULE.idx()].kind {
            NodeKind::Module(m) => m.body.clone(),
            _ => return None,
        };
        let last = *body.last()?;
        match &md.tree.nodes[last.idx()].kind {
            NodeKind::Expr { value } => Some(GNode { m: mid, n: *value }),
            _ => None,
        }
    }

    fn method_igetattr(
        &self,
        func: GNode,
        bound: Option<&Rc<Value>>,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        let name_str = self.sname(name);
        match name_str.as_str() {
            "__func__" => {
                // BoundMethodModel.attr___func__ (objectmodel.py:688-691) is
                // `self._instance._proxied._proxied` — when the BM proxies a
                // FunctionDef DIRECTLY (function_to_method classmethod wrap,
                // metaclass lookup, Super) the second `._proxied` raises
                // AttributeError -> the owner is skipped by _infer_attribute
                // (pandas ExtensionArray._from_sequence_of_strings.__func__
                // -> ERR). Instance-access BMs wrap an UnboundMethod
                // (bases._wrap_attr) so `._proxied._proxied` lands on the
                // function. We encode the wrap chain by bound kind: bound to
                // a ClassDef/FunctionDef/Module node => direct FunctionDef
                // proxy => AttributeError; everything else (instances,
                // consts, ...) came through _wrap_attr.
                if let Some(b) = bound {
                    if let Value::Node(g) = &**b {
                        let classish = self.kind_is(*g, |k| {
                            matches!(
                                k,
                                NodeKind::ClassDef(_)
                                    | NodeKind::FunctionDef(_)
                                    | NodeKind::AsyncFunctionDef(_)
                                    | NodeKind::Lambda(_)
                                    | NodeKind::Module(_)
                            )
                        });
                        if classish {
                            return Err(ErrKind::Attribute);
                        }
                    }
                }
                return Ok(Flow::one(Value::Node(func)));
            }
            "__self__" => {
                return Ok(Flow::one(match bound {
                    Some(b) => (**b).clone(),
                    None => Value::SynthConst(Rc::new(ConstValue::None)),
                }))
            }
            _ => {}
        }
        // UnboundMethodModel is ObjectModel-based (objectmodel.py:620-638):
        // ONLY __class__/__func__/__self__/im_* (+ ObjectModel
        // __new__/__init__) are model attrs. Everything else falls through
        // to self._proxied.igetattr (bases.py:470) — the FunctionDef path,
        // where model results hop through _infer_stmts (+1 each).
        // BoundMethodModel(FunctionModel) keeps the full function model.
        if bound.is_none() {
            return match name_str.as_str() {
                "im_func" => Ok(Flow::one(Value::Node(func))),
                "im_self" => {
                    Ok(Flow::one(Value::SynthConst(Rc::new(ConstValue::None))))
                }
                // attr___class__ = helpers.object_type(UM) -> proxy class
                // 'function' (objectmodel.py:622-627, helpers.py:44-57)
                "im_class" | "__class__" => {
                    Ok(Flow::one(Value::Node(self.builtins().function)))
                }
                "__new__" | "__init__" => match self.function_model_attr(func, &name_str)
                {
                    Some(v) => Ok(Flow::one(v)),
                    None => self.function_igetattr(func, name, ctx),
                },
                _ => self.function_igetattr(func, name, ctx),
            };
        }
        // BoundMethod.igetattr yields special-attribute model results RAW
        // (bases.py:466-469 `iter((self.special_attributes
        // .lookup(name),))` — NO _infer_stmts hop): `bm.__code__` renders
        // 'Unknown', `bm.__get__` is a DescriptorBoundMethod wrapping the
        // BOUND method value.
        if let Some(v) = self.function_model_attr(func, &name_str) {
            if let (Value::DescBM { func: f, .. }, Some(b)) = (&v, bound) {
                return Ok(Flow::one(Value::DescBM {
                    func: *f,
                    inner: Rc::new(Value::BoundMethod {
                        func: *f,
                        bound: Rc::clone(b),
                    }),
                }));
            }
            return Ok(Flow::one(v));
        }
        self.function_igetattr(func, name, ctx)
    }

    /// objects.Property getattr: special_attributes is PropertyModel ONLY
    /// (fget/fset/setter/getter/deleter + ObjectModel __new__/__init__) —
    /// anything else raises AttributeInferenceError -> InferenceError
    /// (`prop.__doc__` is ERR, unlike plain functions).
    fn property_igetattr(
        &self,
        owner: &Value,
        func: GNode,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        let _ = owner;
        let name_str = self.sname(name);
        let hop = |synth: GNode| -> Result<Flow, ErrKind> {
            // model result through _infer_stmts -> stmt.infer hop (+1)
            let c = match ctx {
                Some(c) => Rc::clone(c),
                None => Ctx::new(),
            };
            Ok(self.infer(synth, &c))
        };
        match name_str.as_str() {
            "fget" => {
                // attr_fget (objectmodel.py:921-950): PropertyFuncAccessor
                // named 'fget' parented to the Property (qname composes as
                // <property qname>.fget); calling it requires exactly ONE
                // caller arg and delegates to the wrapped function.
                let synth = self.alloc_synth_funcdef("fget", func);
                self.prop_accessors.borrow_mut().insert(synth, (func, 1));
                hop(synth)
            }
            "fset" => {
                // attr_fset (objectmodel.py:952-986): find the sibling
                // `@<name>.setter`-decorated def with the same name; no
                // setter -> InferenceError. find_setter's comprehension
                // `[t for t in func.parent.get_children() if t.name == ...]`
                // evaluates `.name` on EVERY child — any child kind without
                // a `name` attribute (Assign targets, Attribute bases,
                // Keyword, ...) raises AttributeError, which escapes
                // attr_fset and is swallowed per-owner by _infer_attribute
                // (django ChoiceField.choices.fset -> ERR: `widget = Select`
                // class attrs have no .name).
                if let Value::Property { synth: true, .. } = owner {
                    // infer_property products are parented to
                    // SYNTHETIC_ROOT (no children) -> find_setter None
                    return Err(ErrKind::Inference);
                }
                let fname = self.node_name(func).unwrap_or_default();
                let parent = self.parent(func);
                let mut setter: Option<GNode> = None;
                if let Some(p) = parent {
                    let kids: Vec<GNode> = {
                        let md = self.md(p.m);
                        let mut buf = Vec::new();
                        md.tree.push_children(p.n, &mut buf);
                        buf.into_iter().map(|n| GNode { m: p.m, n }).collect()
                    };
                    // pass 1: `.name` access on every child (AttributeError
                    // on kinds without it — astroid nodes with a name attr:
                    // Module/FunctionDef/ClassDef/Lambda/Name/AssignName/
                    // DelName)
                    for child in &kids {
                        let has_name_attr = self.kind_is(*child, |k| {
                            matches!(
                                k,
                                NodeKind::Module(_)
                                    | NodeKind::FunctionDef(_)
                                    | NodeKind::AsyncFunctionDef(_)
                                    | NodeKind::ClassDef(_)
                                    | NodeKind::Lambda(_)
                                    | NodeKind::Name { .. }
                                    | NodeKind::AssignName { .. }
                                    | NodeKind::DelName { .. }
                            )
                        });
                        if !has_name_attr {
                            return Err(ErrKind::Attribute); // AttributeError
                        }
                    }
                    for child in kids {
                        if self.node_name(child).as_deref() != Some(fname.as_str()) {
                            continue;
                        }
                        for dec in self.decoratornames(child, None).into_iter().flatten() {
                            if dec.ends_with(&format!("{fname}.setter")) {
                                setter = Some(child);
                                break;
                            }
                        }
                        if setter.is_some() {
                            break;
                        }
                    }
                }
                match setter {
                    Some(s) => {
                        let synth = self.alloc_synth_funcdef("fset", func);
                        self.prop_accessors.borrow_mut().insert(synth, (s, 2));
                        hop(synth)
                    }
                    None => Err(ErrKind::Inference),
                }
            }
            "setter" | "deleter" | "getter" => {
                // PropertyModel attr_setter/deleter/getter
                // (objectmodel.py:988-998): a FRESH empty FunctionDef named
                // after the accessor, parented to the Property.
                let synth = self.alloc_synth_funcdef(&name_str, func);
                hop(synth)
            }
            // ObjectModel base attrs still resolve (__new__/__init__ BMs)
            "__new__" | "__init__" => self.function_igetattr(func, name, ctx),
            _ => Err(ErrKind::Inference),
        }
    }

    fn function_model_attr(&self, func: GNode, name: &str) -> Option<Value> {
        let md = self.md(func.m);
        match name {
            // ObjectModel attr___new__/attr___init__ (objectmodel.py:136-164):
            // synthetic `def __init__(self,*a,**kw): return None` parented to
            // builtins.object, bound to _get_bound_node(model) — for
            // function/UM/BM models that resolves to the FUNCTION node
            // (cls._dataclass.__init__ -> BM:builtins.object.__init__).
            "__new__" => {
                return self.obj_model_func_nodes().map(|(f, _)| Value::BoundMethod {
                    func: f,
                    bound: Rc::new(Value::Node(func)),
                })
            }
            "__init__" => {
                return self.obj_model_func_nodes().map(|(_, f)| Value::BoundMethod {
                    func: f,
                    bound: Rc::new(Value::Node(func)),
                })
            }
            _ => {}
        }
        match name {
            "__name__" => self
                .node_name(func)
                .map(|n| Value::SynthConst(Rc::new(ConstValue::Str(n.into())))),
            "__qualname__" => Some(Value::SynthConst(Rc::new(ConstValue::Str(
                self.qname(func).into(),
            )))),
            "__module__" => Some(Value::SynthConst(Rc::new(ConstValue::Str(
                md.name.clone().into(),
            )))),
            "__doc__" => {
                let doc = match &md.tree.nodes[func.n.idx()].kind {
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d
                        .doc_node
                        .and_then(|doc| match &md.tree.nodes[doc.idx()].kind {
                            NodeKind::Const(c) => Some(c.clone()),
                            _ => None,
                        }),
                    _ => None,
                };
                Some(Value::SynthConst(Rc::new(doc.unwrap_or(ConstValue::None))))
            }
            "__dict__" | "__globals__" => Some(Value::SynthDict {
                items: Rc::new(Vec::new()),
            }),
            // attr___defaults__ (objectmodel.py:261-268): Const(None) when
            // no defaults, else a fresh Tuple of the default value nodes
            "__defaults__" => {
                let defaults = self.func_defaults(func);
                if defaults.is_empty() {
                    Some(Value::SynthConst(Rc::new(ConstValue::None)))
                } else {
                    Some(Value::SynthSeq {
                        kind: SeqKind::Tuple,
                        elems: Rc::new(defaults.into_iter().map(Value::Node).collect()),
                    })
                }
            }
            // attr___kwdefaults__ (objectmodel.py:322-345): Dict of kwonly
            // (Const name, default node) pairs
            "__kwdefaults__" => Some(Value::SynthDict {
                items: Rc::new(
                    self.func_kwonly_defaults(func)
                        .into_iter()
                        .map(|(n, d)| {
                            (
                                Value::SynthConst(Rc::new(ConstValue::Str(n.into()))),
                                Value::Node(d),
                            )
                        })
                        .collect(),
                ),
            }),
            // attr___annotations__ (objectmodel.py:270-309): Dict of
            // (Const name, annotation node) incl. 'return'
            "__annotations__" => Some(Value::SynthDict {
                items: Rc::new(
                    self.func_annotation_pairs(func)
                        .into_iter()
                        .map(|(n, a)| {
                            (
                                Value::SynthConst(Rc::new(ConstValue::Str(n.into()))),
                                Value::Node(a),
                            )
                        })
                        .collect(),
                ),
            }),
            // attr___get__ (objectmodel.py:352-460): DescriptorBoundMethod
            "__get__" => Some(Value::DescBM {
                func,
                inner: Rc::new(Value::Node(func)),
            }),
            // the attr___ne__ Unknown family (objectmodel.py:462-485):
            // fresh Unknown nodes — infer to Uninferable through the
            // FunctionDef.igetattr hop, render 'Unknown' raw on the
            // Bound/UnboundMethod model path. NOTE astroid's
            // attr___setattr___/attr___delattr___ strip to THREE-underscore
            // names that never match real lookups (bug kept: '__setattr__'
            // and '__delattr__' are NOT in the model).
            "__ne__" | "__subclasshook__" | "__str__" | "__sizeof__" | "__repr__"
            | "__reduce__" | "__reduce_ex__" | "__lt__" | "__eq__" | "__gt__"
            | "__format__" | "__getattribute__" | "__hash__" | "__dir__" | "__call__"
            | "__class__" | "__closure__" | "__code__" => {
                Some(Value::Node(self.alloc_synth_node(NodeKind::Unknown)))
            }
            _ => None,
        }
    }

    /// args.defaults value nodes
    fn func_defaults(&self, func: GNode) -> Vec<GNode> {
        let md = self.md(func.m);
        let args = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
            NodeKind::Lambda(d) => d.args,
            _ => return Vec::new(),
        };
        match &md.tree.nodes[args.idx()].kind {
            NodeKind::Arguments(a) => {
                a.defaults.iter().map(|&n| GNode { m: func.m, n }).collect()
            }
            _ => Vec::new(),
        }
    }

    /// kwonly (name, default node) pairs with a default present
    fn func_kwonly_defaults(&self, func: GNode) -> Vec<(String, GNode)> {
        let md = self.md(func.m);
        let args = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
            NodeKind::Lambda(d) => d.args,
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        if let NodeKind::Arguments(a) = &md.tree.nodes[args.idx()].kind {
            for (arg, def) in a.kwonlyargs.iter().zip(a.kw_defaults.iter()) {
                let (Some(def), NodeKind::AssignName { name }) =
                    (def, &md.tree.nodes[arg.idx()].kind)
                else {
                    continue;
                };
                out.push((md.tree.s(*name).to_string(), GNode { m: func.m, n: *def }));
            }
        }
        out
    }

    /// (arg name, annotation node) pairs incl. 'return'
    /// (objectmodel.py:285-299; later duplicates overwrite)
    fn func_annotation_pairs(&self, func: GNode) -> Vec<(String, GNode)> {
        let md = self.md(func.m);
        let (args, returns) = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => (d.args, d.returns),
            _ => return Vec::new(),
        };
        let mut out: Vec<(String, GNode)> = Vec::new();
        let push = |name: String, ann: GNode, out: &mut Vec<(String, GNode)>| {
            if let Some(pos) = out.iter().position(|(n, _)| *n == name) {
                out[pos] = (name, ann);
            } else {
                out.push((name, ann));
            }
        };
        if let NodeKind::Arguments(a) = &md.tree.nodes[args.idx()].kind {
            let pairs = a
                .args
                .iter()
                .zip(a.annotations.iter())
                .chain(a.kwonlyargs.iter().zip(a.kwonlyargs_annotations.iter()))
                .chain(a.posonlyargs.iter().zip(a.posonlyargs_annotations.iter()));
            for (arg, ann) in pairs {
                let (Some(ann), NodeKind::AssignName { name }) =
                    (ann, &md.tree.nodes[arg.idx()].kind)
                else {
                    continue;
                };
                push(
                    md.tree.s(*name).to_string(),
                    GNode { m: func.m, n: *ann },
                    &mut out,
                );
            }
        }
        if let Some(r) = returns {
            push("return".to_string(), GNode { m: func.m, n: r }, &mut out);
        }
        out
    }

    // ---------- Slice ----------

    fn slice_igetattr(&self, g: GNode, name: GSym, ctx: Option<&Rc<Ctx>>) -> Result<Flow, ErrKind> {
        let md = self.md(g.m);
        let (lower, upper, step) = match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Slice { lower, upper, step } => (*lower, *upper, *step),
            _ => return Err(ErrKind::Attribute),
        };
        let name_str = self.sname(name);
        let child = match name_str.as_str() {
            "start" => lower,
            "stop" => upper,
            "step" => step,
            _ => {
                // class getattr on builtin slice
                let cls = self.builtins().slice;
                return self
                    .class_igetattr(cls, name, ctx, false)
                    .map_err(|_| ErrKind::Inference);
            }
        };
        match child {
            Some(c) => Ok(self.infer(GNode { m: g.m, n: c }, &copy_context(ctx))),
            None => Ok(Flow::one(Value::SynthConst(Rc::new(ConstValue::None)))),
        }
    }

    fn synth_slice_igetattr(
        &self,
        owner: &Value,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        let bounds = match owner {
            Value::SynthSlice { bounds } => bounds,
            _ => return Err(ErrKind::Attribute),
        };
        let name_str = self.sname(name);
        let idx = match name_str.as_str() {
            "start" => 0,
            "stop" => 1,
            "step" => 2,
            _ => {
                let cls = self.builtins().slice;
                return self
                    .class_igetattr(cls, name, ctx, false)
                    .map_err(|_| ErrKind::Inference);
            }
        };
        Ok(Flow::one(Value::SynthConst(Rc::new(
            bounds[idx].clone().unwrap_or(ConstValue::None),
        ))))
    }

    // ---------- Super (§12.8) ----------

    pub fn super_mro(&self, owner: &Value) -> Result<Vec<GNode>, ErrKind> {
        let (mro_pointer, mro_type) = match owner {
            Value::Super {
                mro_pointer,
                mro_type,
                ..
            } => (*mro_pointer, mro_type),
            _ => return Err(ErrKind::Super),
        };
        let cls = match &**mro_type {
            Value::Node(g) => *g,
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => *cls,
            _ => return Err(ErrKind::Super),
        };
        let mro = self.mro(cls, None).map_err(|_| ErrKind::Super)?;
        match mro.iter().position(|&c| c == mro_pointer) {
            Some(i) => Ok(mro[i + 1..].to_vec()),
            None => Err(ErrKind::Super),
        }
    }

    fn super_igetattr_to(
        &self,
        owner: &Value,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        let name_str = self.sname(name);
        let (mro_type, scope) = match owner {
            Value::Super { mro_type, scope, .. } => (Rc::clone(mro_type), *scope),
            _ => return End::Raised(ErrKind::Attribute),
        };
        if name_str == "__class__" {
            yield_v!(sink, Value::Node(self.builtins().super_));
            return End::Done;
        }
        let mro = match self.super_mro(owner) {
            Ok(m) => m,
            Err(_) => return End::Raised(ErrKind::Attribute),
        };
        let mut found = false;
        for cls in mro {
            // objects.py:184-189: only cls[name] (the FIRST local) is
            // inferred, and bound methods bind to the mro class `cls`.
            let locs = self.class_locals_get(cls, name);
            let Some(&first_loc) = locs.first() else { continue };
            found = true;
            let ctx2 = copy_context(ctx);
            ctx2.lookupname.set(Some(name));
            let mut stopped = false;
            let end = {
                let stopped = &mut stopped;
                let ctx3 = Rc::clone(&ctx2);
                let mro_type = &mro_type;
                self.infer_stmts_to(&[NV::N(first_loc)], Some(&ctx2), None, &mut |inferred| {
                    let d = match &inferred {
                        Value::Node(g)
                            if self.kind_is(*g, |k| {
                                matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                            }) =>
                        {
                            let ft = self.func_type(*g);
                            let caller_is_classmethod =
                                self.func_type(scope) == FType::ClassMethod;
                            let class_based = matches!(
                                &**mro_type,
                                Value::Node(g2) if self.kind_is(*g2, |k| matches!(k, NodeKind::ClassDef(_)))
                            );
                            if ft == FType::ClassMethod {
                                sink(Value::BoundMethod {
                                    func: *g,
                                    bound: Rc::new(Value::Node(cls)),
                                })
                            } else if caller_is_classmethod && ft == FType::Method {
                                sink(inferred.clone())
                            } else if class_based || ft == FType::StaticMethod {
                                sink(inferred.clone())
                            // bases._is_property(inferred) — context None
                            // (objects.py:211)
                            } else if self.is_property(*g, None) {
                                let mut any = false;
                                let mut inner_stop = false;
                                let _ = self.function_infer_call_result_to(*g, None, ctx, &mut |x| {
                                    any = true;
                                    let d = sink(x);
                                    if let Drive::Stop = d {
                                        inner_stop = true;
                                    }
                                    d
                                });
                                if inner_stop {
                                    Drive::Stop
                                } else if !any {
                                    sink(Value::Uninferable)
                                } else {
                                    Drive::Go
                                }
                            } else {
                                sink(Value::BoundMethod {
                                    func: *g,
                                    bound: Rc::new(Value::Node(cls)),
                                })
                            }
                        }
                        Value::Property { func, .. } => {
                            let func = *func;
                            let mut any = false;
                            let mut inner_stop = false;
                            let _ = self.function_infer_call_result_to(func, None, ctx, &mut |x| {
                                any = true;
                                let d = sink(x);
                                if let Drive::Stop = d {
                                    inner_stop = true;
                                }
                                d
                            });
                            if inner_stop {
                                Drive::Stop
                            } else if !any {
                                sink(Value::Uninferable)
                            } else {
                                Drive::Go
                            }
                        }
                        _ => sink(inferred),
                    };
                    if let Drive::Stop = d {
                        *stopped = true;
                    }
                    d
                })
            };
            if stopped {
                return End::Stopped;
            }
            if let End::Raised(e) = end {
                if !e.is_inference() {
                    return End::Raised(e);
                }
            }
        }
        if !found {
            // objects.py:166-169: `if not found and name in
            // self.special_attributes: yield ...` — SuperModel
            // (__thisclass__/__self_class__/__self__/__class__) + the
            // inherited ObjectModel __new__/__init__ bound methods
            let (mro_pointer, self_class) = match owner {
                Value::Super { mro_pointer, self_class, .. } => (*mro_pointer, *self_class),
                _ => return End::Raised(ErrKind::Attribute),
            };
            let model: Option<Value> = match name_str.as_str() {
                "__thisclass__" => Some(Value::Node(mro_pointer)),
                "__self_class__" => Some(Value::Node(self_class)),
                "__self__" => Some((*mro_type).clone()),
                "__new__" => self.obj_model_func_nodes().map(|(f, _)| Value::BoundMethod {
                    func: f,
                    bound: Rc::new(owner.clone()),
                }),
                "__init__" => self.obj_model_func_nodes().map(|(_, f)| Value::BoundMethod {
                    func: f,
                    bound: Rc::new(owner.clone()),
                }),
                _ => None,
            };
            let _ = scope;
            match model {
                Some(v) => {
                    yield_v!(sink, v);
                    return End::Done;
                }
                None => return End::Raised(ErrKind::Attribute),
            }
        }
        End::Done
    }

    // ---------- MRO / ancestors (§13) ----------

    /// ClassDef.hide (scoped_nodes.py:1849; set only at :1603 for the
    /// with_metaclass temporary_class)
    pub fn is_hidden_class(&self, g: GNode) -> bool {
        self.hidden_classes.borrow().contains(&g)
    }

    pub fn class_bases(&self, cls: GNode) -> Vec<GNode> {
        let md = self.md(cls.m);
        match &md.tree.nodes[cls.n.idx()].kind {
            NodeKind::ClassDef(d) => d
                .bases
                .iter()
                .map(|&b| GNode { m: cls.m, n: b })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// ancestors() (scoped_nodes.py:2167-2211) — prefix DFS; eager shim.
    pub fn ancestors(&self, cls: GNode, recurs: bool, ctx: Option<&Rc<Ctx>>) -> Vec<GNode> {
        let mut out = Vec::new();
        let _ = self.ancestors_to(cls, recurs, ctx, &mut |g| {
            out.push(g);
            Drive::Go
        });
        out
    }

    /// streaming ancestors(): one frame of scoped_nodes.py:2167-2211. Each
    /// recursion level is its own generator with its OWN `yielded` set
    /// (fresh `{self}`); base inference is pulled value-by-value (the base
    /// generator stays SUSPENDED while grandparents are walked), and
    /// consumers can abandon early (is_subtype_of / metaclass search).
    pub fn ancestors_to(
        &self,
        cls: GNode,
        recurs: bool,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut dyn FnMut(GNode) -> Drive,
    ) -> End {
        self.twalk("ANC", cls);
        // scoped_nodes.py:2167-2180 — ancestors() does NOT clone the
        // context: `if context is None: context = InferenceContext()`.
        // lookupname set by callers (e.g. igetattr's '__slots__') is
        // preserved into base inference cache keys.
        let ctx = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        self.ancestors_frame(cls, recurs, &ctx, 0, sink)
    }

    fn ancestors_frame(
        &self,
        cls: GNode,
        recurs: bool,
        ctx: &Rc<Ctx>,
        depth: u32,
        sink: &mut dyn FnMut(GNode) -> Drive,
    ) -> End {
        if depth > 100 {
            // stand-in for Python's RecursionError on cyclic bases
            return End::Done;
        }
        let mut yielded: rustc_hash::FxHashSet<GNode> = Default::default();
        yielded.insert(cls);
        let bases = self.class_bases(cls);
        if bases.is_empty() {
            if self.qname(cls) != "builtins.object" {
                let obj = self.builtins().object;
                if let Drive::Stop = sink(obj) {
                    return End::Stopped;
                }
            }
            return End::Done;
        }
        for base in bases {
            // with context.restore_path(): per-base snapshot
            let saved_path = ctx.path.borrow().clone();
            let mut consumer_stop = false;
            let _ = {
                let yielded = &mut yielded;
                let consumer_stop = &mut consumer_stop;
                self.infer_to(base, ctx, &mut |baseobj| {
                    // scoped_nodes.py:2185-2189: non-ClassDef baseobjs that
                    // are astroid `Instance`s (incl. Const/containers!)
                    // unproxy to their class (Const None -> NoneType -> its
                    // ancestors walk yields object BEFORE later bases)
                    let basecls = match &baseobj {
                        Value::Node(g)
                            if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) =>
                        {
                            *g
                        }
                        other => match self.instance_unproxy(other) {
                            Some(c) => c,
                            None => return Drive::Go,
                        },
                    };
                    if yielded.insert(basecls) {
                        if let Drive::Stop = sink(basecls) {
                            *consumer_stop = true;
                            return Drive::Stop;
                        }
                    }
                    if !recurs {
                        return Drive::Go;
                    }
                    // for grandpa in baseobj.ancestors(recurs=True, context):
                    // fresh generator (own yielded set); `grandpa is self`
                    // breaks the INNER loop only
                    let mut inner_stop = false;
                    let _ = self.ancestors_frame(basecls, true, ctx, depth + 1, &mut |gp| {
                        if gp == cls {
                            return Drive::Stop; // break
                        }
                        if !yielded.insert(gp) {
                            return Drive::Go; // continue
                        }
                        let d = sink(gp);
                        if let Drive::Stop = d {
                            inner_stop = true;
                        }
                        d
                    });
                    if inner_stop {
                        *consumer_stop = true;
                        return Drive::Stop;
                    }
                    Drive::Go
                })
            };
            *ctx.path.borrow_mut() = saved_path;
            if consumer_stop {
                return End::Stopped;
            }
            // InferenceError from a base -> continue with next base
        }
        End::Done
    }

    /// _compute_mro / mro() (scoped_nodes.py:2837-2863) — C3
    pub fn mro(&self, cls: GNode, ctx: Option<&Rc<Ctx>>) -> Result<Vec<GNode>, ErrKind> {
        self.compute_mro(cls, ctx, 0)
    }

    fn compute_mro(&self, cls: GNode, ctx: Option<&Rc<Ctx>>, depth: u32) -> Result<Vec<GNode>, ErrKind> {
        if depth > 100 {
            return Err(ErrKind::Mro);
        }
        if self.qname(cls) == "builtins.object" {
            return Ok(vec![cls]);
        }
        let inferred_bases = self.inferred_bases(cls, ctx);
        let mut bases_mro: Vec<Vec<GNode>> = Vec::new();
        for &base in &inferred_bases {
            if base == cls {
                continue;
            }
            let mro = self.compute_mro(base, ctx, depth + 1)?;
            bases_mro.push(mro);
        }
        let mut unmerged: Vec<Vec<GNode>> = Vec::new();
        unmerged.push(vec![cls]);
        unmerged.extend(bases_mro);
        unmerged.push(inferred_bases);
        // clean_duplicates_mro: dedupe key (lineno, qname)
        for seq in &unmerged {
            let mut seen: rustc_hash::FxHashSet<(u32, String)> = Default::default();
            for &node in seq {
                let key = (self.fromlineno(node), self.qname(node));
                if !seen.insert(key) {
                    return Err(ErrKind::Mro);
                }
            }
        }
        self.clean_typing_generic_mro(&mut unmerged);
        c3_merge(unmerged).ok_or(ErrKind::Mro)
    }

    fn clean_typing_generic_mro(&self, sequences: &mut Vec<Vec<GNode>>) {
        let n = sequences.len();
        if n < 2 {
            return;
        }
        let pos_in_inferred = {
            let inferred = &sequences[n - 1];
            inferred
                .iter()
                .position(|&b| self.qname(b) == "typing.Generic")
        };
        let pos = match pos_in_inferred {
            Some(p) => p,
            None => return,
        };
        let mut found = false;
        for (i, seq) in sequences[1..n - 1].iter().enumerate() {
            if i == pos {
                continue;
            }
            if seq.iter().any(|&b| self.qname(b) == "typing.Generic") {
                found = true;
                break;
            }
        }
        if !found {
            return;
        }
        // remove from inferred_bases and its bases_mro entry
        sequences[n - 1].remove(pos);
        if pos + 1 < n - 1 {
            sequences.remove(pos + 1);
        }
    }

    /// _inferred_bases (scoped_nodes.py:2803-2835)
    fn inferred_bases(&self, cls: GNode, ctx: Option<&Rc<Ctx>>) -> Vec<GNode> {
        self.twalk("INFBASES", cls);
        let bases = self.class_bases(cls);
        if bases.is_empty() {
            if self.qname(cls) != "builtins.object" {
                return vec![self.builtins().object];
            }
            return Vec::new();
        }
        // scoped_nodes.py:2817-2818 — context normalized ONCE for the whole
        // bases walk (a fresh InferenceContext when None); _infer_last then
        // clones per base. Clones SHARE the nodes_inferred cell, so counter
        // bumps accumulate across one class's bases (but NOT across the
        // _compute_mro recursion, which passes the caller's `context` —
        // still None — to base._compute_mro).
        let base_root = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        let mut out = Vec::new();
        for base in bases {
            // _infer_last with a cloned context
            let c = base_root.clone_ctx();
            let flow = self.infer(base, &c);
            let last = flow.vals.last().cloned();
            let Some(last) = last else { continue };
            // scoped_nodes.py:2828-2831: Instance baseobjs (incl. Const/
            // containers) unproxy to their class before the ClassDef check
            let basecls = match last {
                Value::Node(g) if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) => g,
                other => match self.instance_unproxy(&other) {
                    Some(c) => c,
                    None => continue,
                },
            };
            out.push(basecls);
        }
        out
    }

    /// is_subtype_of (scoped_nodes.py:2004-2015): `any(...)` abandons the
    /// ancestors generator on the first match.
    pub fn is_subtype_of(&self, cls: GNode, type_name: &str, ctx: Option<&Rc<Ctx>>) -> bool {
        self.twalk("SUBTYPE", cls);
        if self.qname(cls) == type_name {
            return true;
        }
        let mut found = false;
        let _ = self.ancestors_to(cls, true, ctx, &mut |a| {
            if self.qname(a) == type_name {
                found = true;
                Drive::Stop
            } else {
                Drive::Go
            }
        });
        found
    }

    // ---------- metaclass ----------

    /// declared_metaclass (scoped_nodes.py:2626-2661). The bases loop runs
    /// on EVERY call (even with no metaclass keyword): each base is fully
    /// materialized via base.infer(context) — counter bumps included — and
    /// a hidden baseobj (six.with_metaclass temporary_class) persistently
    /// overwrites self._metaclass (scoped_nodes.py:2638-2645).
    pub fn declared_metaclass(&self, cls: GNode, ctx: Option<&Rc<Ctx>>) -> Option<Value> {
        self.twalk("DECLMETA", cls);
        // for base in self.bases: for baseobj in base.infer(context):
        // (context passed through unchanged, NOT copied). With context=None
        // each base.infer(None) builds its OWN fresh InferenceContext —
        // counters do NOT accumulate across bases (scoped_nodes.py:2640-2648)
        for base in self.class_bases(cls) {
            let base_ctx = match ctx {
                Some(c) => Rc::clone(c),
                None => Ctx::new(),
            };
            let _ = self.infer_to(base, &base_ctx, &mut |baseobj| {
                if let Value::Node(g) = &baseobj {
                    if self.is_hidden_class(*g) {
                        // self._metaclass = baseobj._metaclass;
                        // self._metaclass_hack = True; break (inner loop)
                        if let Some(meta) = self.class_metaclass_node(*g) {
                            self.meta_override.borrow_mut().insert(cls, meta);
                        }
                        return Drive::Stop;
                    }
                }
                Drive::Go
            });
        }
        let meta: GNode = match self.meta_override.borrow().get(&cls).copied() {
            Some(g) => g,
            None => self.class_metaclass_node(cls)?,
        };
        // next(node for node in self._metaclass.infer(context=context) if
        // not Uninferable) — context passed through unchanged
        // (scoped_nodes.py:2651-2658); abandons the generator on the first
        // non-Uninferable value.
        let c = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        let mut found: Option<Value> = None;
        let _ = {
            let found = &mut found;
            self.infer_to(meta, &c, &mut |v| {
                if v.is_uninferable() {
                    Drive::Go
                } else {
                    *found = Some(v);
                    Drive::Stop
                }
            })
        };
        found
    }

    /// the `metaclass=` keyword node of a ClassDef (TreeRebuilder
    /// _metaclass), before any with_metaclass override
    fn class_metaclass_node(&self, cls: GNode) -> Option<GNode> {
        if let Some(meta) = self.meta_override.borrow().get(&cls).copied() {
            return Some(meta);
        }
        let md = self.md(cls.m);
        match &md.tree.nodes[cls.n.idx()].kind {
            NodeKind::ClassDef(d) => d.metaclass.map(|n| GNode { m: cls.m, n }),
            _ => None,
        }
    }

    pub fn metaclass(&self, cls: GNode, ctx: Option<&Rc<Ctx>>) -> Option<Value> {
        self.find_metaclass(cls, &mut Default::default(), ctx)
    }

    fn find_metaclass(
        &self,
        cls: GNode,
        seen: &mut rustc_hash::FxHashSet<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Option<Value> {
        self.twalk("FINDMETA", cls);
        seen.insert(cls);
        if let Some(k) = self.declared_metaclass(cls, ctx) {
            return Some(k);
        }
        // `return klass` inside the loop abandons the ancestors generator
        let mut found: Option<Value> = None;
        let _ = self.ancestors_to(cls, true, ctx, &mut |parent| {
            if !seen.contains(&parent) {
                // scoped_nodes.py:2673 `parent._find_metaclass(seen)` —
                // the context is DROPPED on recursion (bug-for-bug)
                if let Some(k) = self.find_metaclass(parent, seen, None) {
                    found = Some(k);
                    return Drive::Stop;
                }
            }
            Drive::Go
        });
        found
    }

    // ---------- ClassDef.type / instantiate_class ----------

    /// instantiate_class (scoped_nodes.py:2303-2316)
    pub fn instantiate_class(&self, cls: GNode) -> Value {
        if let Ok(mro) = self.mro(cls, None) {
            let is_exc = mro.iter().any(|&c| {
                self.node_name(c)
                    .map(|n| n == "Exception" || n == "BaseException")
                    .unwrap_or(false)
            });
            if is_exc {
                return Value::ExcInst {
                    id: crate::value::fresh_inst_id(),
                    cls,
                    exceptions: None,
                };
            }
        }
        Value::Inst { cls, id: crate::value::fresh_inst_id() }
    }

    /// FunctionDef.type (scoped_nodes.py:1313-1384), cached
    pub fn func_type(&self, func: GNode) -> FType {
        if let Some(&t) = self.ftype_cache.borrow().get(&func) {
            return t;
        }
        let md = self.md(func.m);
        if let Some(&t) = md.ftype.get(&func.n) {
            self.ftype_cache.borrow_mut().insert(func, t);
            return t;
        }
        let t = self.compute_func_type(func);
        self.ftype_cache.borrow_mut().insert(func, t);
        t
    }

    fn compute_func_type(&self, func: GNode) -> FType {
        let md = self.md(func.m);
        let (name, decorators) = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                (md.tree.s(d.name).to_string(), d.decorators)
            }
            NodeKind::Lambda(d) => {
                // Lambda.type (scoped_nodes.py:908-918): "method" when the
                // first argument is literally named `self` and the lambda's
                // parent scope is a ClassDef
                let first_is_self = match &md.tree.nodes[d.args.idx()].kind {
                    NodeKind::Arguments(a) => {
                        let first = a.posonlyargs.first().or(a.args.first());
                        first
                            .map(|&arg| match &md.tree.nodes[arg.idx()].kind {
                                NodeKind::AssignName { name } => md.tree.s(*name) == "self",
                                _ => false,
                            })
                            .unwrap_or(false)
                    }
                    _ => false,
                };
                if first_is_self {
                    let in_class = self
                        .parent(func)
                        .map(|p| {
                            let s = self.scope(p);
                            self.kind_is(s, |k| matches!(k, NodeKind::ClassDef(_)))
                        })
                        .unwrap_or(false);
                    if in_class {
                        return FType::Method;
                    }
                }
                return FType::Function;
            }
            _ => return FType::Function,
        };
        // extra_decorators: `meth = staticmethod(meth)` in the class body
        let parent_frame = self.parent(func).map(|p| self.frame(p));
        let in_class = parent_frame
            .map(|f| self.kind_is(f, |k| matches!(k, NodeKind::ClassDef(_))))
            .unwrap_or(false);
        if in_class {
            for call in self.extra_decorators(func, parent_frame.unwrap(), &name) {
                let cmd = self.md(call.m);
                if let NodeKind::Call { func: cf, .. } = &cmd.tree.nodes[call.n.idx()].kind {
                    if let NodeKind::Name { name: cn } = &cmd.tree.nodes[cf.idx()].kind {
                        match cmd.tree.s(*cn) {
                            "classmethod" => return FType::ClassMethod,
                            "staticmethod" => return FType::StaticMethod,
                            _ => {}
                        }
                    }
                }
            }
        }
        let mut type_ = FType::Function;
        if in_class {
            if matches!(name.as_str(), "__new__" | "__init_subclass__" | "__class_getitem__") {
                return FType::ClassMethod;
            }
            type_ = FType::Method;
        }
        let Some(dec) = decorators else { return type_ };
        let dec_nodes: Vec<NodeId> = match &md.tree.nodes[dec.idx()].kind {
            NodeKind::Decorators { nodes } => nodes.clone(),
            _ => Vec::new(),
        };
        for dn in dec_nodes {
            match &md.tree.nodes[dn.idx()].kind {
                NodeKind::Name { name: n } => match md.tree.s(*n) {
                    "classmethod" => return FType::ClassMethod,
                    "staticmethod" => return FType::StaticMethod,
                    _ => {}
                },
                NodeKind::Attribute { expr, attrname, .. } => {
                    if let NodeKind::Name { name: en } = &md.tree.nodes[expr.idx()].kind {
                        if md.tree.s(*en) == "builtins" {
                            match md.tree.s(*attrname) {
                                "classmethod" => return FType::ClassMethod,
                                "staticmethod" => return FType::StaticMethod,
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
            // decorator call chains + inferred classes: infer the decorator.
            // BOTH pulls are no-context calls in astroid (scoped_nodes.py:
            // 1358 `next(node.func.infer())`, :1366 `node.infer()`) — each
            // materializes its OWN InferenceContext; sharing one ctx lets
            // the abandoned func pull path-block the decorator re-infer.
            let g = GNode { m: func.m, n: dn };
            if let NodeKind::Call { func: cf, .. } = &md.tree.nodes[dn.idx()].kind {
                let cg = GNode { m: func.m, n: *cf };
                // next(node.func.infer()) — single pull, fresh ctx
                if let Ok(Some(current)) = self.first_value(cg, &Ctx::new()) {
                    if let Some(t) = self.infer_decorator_callchain(&current) {
                        return t;
                    }
                }
            }
            let flow = self.infer(g, &Ctx::new());
            if flow.err.map(|e| e.is_inference()).unwrap_or(false) && flow.vals.is_empty() {
                continue;
            }
            for inferred in &flow.vals {
                if let Some(t) = self.infer_decorator_callchain(inferred) {
                    return t;
                }
                let icls = match inferred {
                    Value::Node(g2)
                        if self.kind_is(*g2, |k| matches!(k, NodeKind::ClassDef(_))) =>
                    {
                        *g2
                    }
                    _ => continue,
                };
                for ancestor in self.ancestors(icls, true, None) {
                    if self.is_subtype_of(ancestor, "builtins.classmethod", None) {
                        return FType::ClassMethod;
                    }
                    if self.is_subtype_of(ancestor, "builtins.staticmethod", None) {
                        return FType::StaticMethod;
                    }
                }
            }
        }
        type_
    }

    /// _infer_decorator_callchain (scoped_nodes.py:846-881)
    fn infer_decorator_callchain(&self, v: &Value) -> Option<FType> {
        // isinstance(node, FunctionDef) — objects.PartialFunction subclasses
        // FunctionDef, so partial values run THEIR infer_call_result here
        // (functools.wraps(f) decorators pull update_wrapper's `return
        // wrapper`; scoped_nodes.py:846-857)
        let callee: Value = match v {
            Value::Node(g)
                if self.kind_is(*g, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                }) =>
            {
                Value::Node(*g)
            }
            Value::Partial { .. } => v.clone(),
            _ => return None,
        };
        // next(node.infer_call_result(caller=None), None) — single pull
        let result = self
            .infer_call_result_first(&callee, None, None)
            .ok()
            .flatten()?;
        let result = &result;
        let rescls = match result {
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(*cls),
            Value::Node(g) if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) => Some(*g),
            _ => None,
        };
        if let Some(c) = rescls {
            if self.is_subtype_of(c, "builtins.classmethod", None) {
                return Some(FType::ClassMethod);
            }
            if self.is_subtype_of(c, "builtins.staticmethod", None) {
                return Some(FType::StaticMethod);
            }
        }
        if let Value::Node(g) = result {
            let md = self.md(g.m);
            if let NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) =
                &md.tree.nodes[g.n.idx()].kind
            {
                if let Some(dec) = d.decorators {
                    if let NodeKind::Decorators { nodes } = &md.tree.nodes[dec.idx()].kind {
                        for dn in nodes {
                            match &md.tree.nodes[dn.idx()].kind {
                                NodeKind::Name { name } => match md.tree.s(*name) {
                                    "classmethod" => return Some(FType::ClassMethod),
                                    "staticmethod" => return Some(FType::StaticMethod),
                                    _ => {}
                                },
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// extra_decorators (scoped_nodes.py:1221-1259): Assign nodes in the
    /// class body of form `name = callable(name)`.
    fn extra_decorators(&self, func: GNode, frame: GNode, name: &str) -> Vec<GNode> {
        let md = self.md(frame.m);
        let mut out = Vec::new();
        // _assign_nodes_in_scope: all Assigns in the class scope (recursive
        // through non-frame statements)
        let mut stack: Vec<NodeId> = Vec::new();
        let mut buf = Vec::new();
        md.tree.push_children(frame.n, &mut buf);
        stack.extend(buf.iter().copied());
        while let Some(n) = stack.pop() {
            if matches!(
                md.tree.nodes[n.idx()].kind,
                NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::ClassDef(_)
                    | NodeKind::Lambda(_)
            ) {
                continue;
            }
            if let NodeKind::Assign { targets, value } = &md.tree.nodes[n.idx()].kind {
                let value_is_call_of_name = match &md.tree.nodes[value.idx()].kind {
                    NodeKind::Call { func: cf, .. } => {
                        matches!(md.tree.nodes[cf.idx()].kind, NodeKind::Name { .. })
                    }
                    _ => false,
                };
                if value_is_call_of_name {
                    for t in targets {
                        if let NodeKind::AssignName { name: tn } = &md.tree.nodes[t.idx()].kind {
                            if md.tree.s(*tn) == name {
                                // `meth = frame[self.name]` — LocalsDictNodeNG
                                // __getitem__ = locals[name][0]: the FIRST
                                // local (the FunctionDef), not the last
                                // (which is the AssignName of the rebind)
                                let sym = self.g(&md, *tn);
                                let locs = self.class_locals_get(frame, sym);
                                if let Some(meth) = locs.first() {
                                    if self.kind_is(*meth, |k| {
                                        matches!(
                                            k,
                                            NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                                        )
                                    }) && func.m == frame.m
                                    {
                                        out.push(GNode { m: frame.m, n: *value });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            buf.clear();
            md.tree.push_children(n, &mut buf);
            stack.extend(buf.iter().copied());
        }
        out
    }

    // ---------- property detection (§11.2) ----------

    pub fn decoratornames(&self, func: GNode, ctx: Option<&Rc<Ctx>>) -> Vec<Option<String>> {
        let md = self.md(func.m);
        let mut result = Vec::new();
        let decorators = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.decorators,
            _ => None,
        };
        let mut decnodes: Vec<GNode> = Vec::new();
        if let Some(dec) = decorators {
            if let NodeKind::Decorators { nodes } = &md.tree.nodes[dec.idx()].kind {
                decnodes.extend(nodes.iter().map(|&n| GNode { m: func.m, n }));
            }
        }
        // decoratornames += self.extra_decorators (scoped_nodes.py:1459)
        let parent_frame = self.parent(func).map(|p| self.frame(p));
        if let Some(frame) = parent_frame {
            if self.kind_is(frame, |k| matches!(k, NodeKind::ClassDef(_))) {
                let fname = match &md.tree.nodes[func.n.idx()].kind {
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                        md.tree.s(d.name).to_string()
                    }
                    _ => String::new(),
                };
                if !fname.is_empty() {
                    decnodes.extend(self.extra_decorators(func, frame, &fname));
                }
            }
        }
        for dn in decnodes {
            // scoped_nodes.py:1462 `decnode.infer(context=context)` — the
            // caller's context AS-IS (lookupname intact -> cache keys are
            // per-attribute-name in the igetattr setter scan!)
            let flow = match ctx {
                Some(c) => self.infer(dn, c),
                None => self.infer(dn, &crate::ctx::Ctx::new()),
            };
            for v in &flow.vals {
                result.push(self.value_qname(v));
            }
        }
        result
    }

    /// qname of an inference result (proxies forward to _proxied)
    pub fn value_qname(&self, v: &Value) -> Option<String> {
        match v {
            Value::Uninferable => None,
            Value::Node(g) => Some(self.qname(*g)),
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(self.qname(*cls)),
            Value::BoundMethod { func, .. }
            | Value::UnboundMethod { func }
            | Value::Property { func, .. }
            | Value::Partial { func, .. } => Some(self.qname(*func)),
            Value::Generator { is_async, .. } => Some(if *is_async {
                "builtins.async_generator".to_string()
            } else {
                "builtins.generator".to_string()
            }),
            v => self
                .proxied_class(v)
                .map(|c| self.qname(c)),
        }
    }

    /// bases._is_property (bases.py:69-108). All astroid call sites pass
    /// context=None (bases.py:305, scoped_nodes.py:1526/:2467,
    /// objects.py:211) — decorator inference then runs under FRESH contexts.
    pub fn is_property(&self, func: GNode, ctx: Option<&Rc<Ctx>>) -> bool {
        let md = self.md(func.m);
        let decorators = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.decorators,
            _ => return false,
        };
        // decoratornames runs FIRST, even without syntactic decorators
        // (extra_decorators feed it) — bases.py:72
        let decnames = self.decoratornames(func, ctx);
        for n in decnames.iter().flatten() {
            if PROPERTIES.contains(&n.as_str()) {
                return true;
            }
        }
        for n in decnames.iter().flatten() {
            let stripped = n.rsplit('.').next().unwrap_or("");
            if POSSIBLE_PROPERTIES.contains(&stripped) {
                return true;
            }
        }
        // bases.py:83: no syntactic decorators -> False before phase 3
        if decorators.is_none() {
            return false;
        }
        // decorator classes subtyping a property class
        let dec = decorators.unwrap();
        let dec_nodes: Vec<NodeId> = match &md.tree.nodes[dec.idx()].kind {
            NodeKind::Decorators { nodes } => nodes.clone(),
            _ => Vec::new(),
        };
        for dn in dec_nodes {
            let g = GNode { m: func.m, n: dn };
            // safe_infer(decorator, context=context) — the ctx AS-IS
            // (None -> fresh per pull)
            let c = match ctx {
                Some(c) => Rc::clone(c),
                None => crate::ctx::Ctx::new(),
            };
            let inferred = self.safe_infer(g, &c);
            let Some(inferred) = inferred else { continue };
            if let Value::Node(icls) = inferred {
                if self.kind_is(icls, |k| matches!(k, NodeKind::ClassDef(_))) {
                    // inferred.is_subtype_of(pclass) — context None
                    // (bases.py:92)
                    if PROPERTIES
                        .iter()
                        .any(|p| self.is_subtype_of(icls, p, None))
                    {
                        return true;
                    }
                    // Subscript functools.cached_property base
                    let cmd = self.md(icls.m);
                    if let NodeKind::ClassDef(d) = &cmd.tree.nodes[icls.n.idx()].kind {
                        for &b in &d.bases {
                            if let NodeKind::Subscript { value, .. } =
                                &cmd.tree.nodes[b.idx()].kind
                            {
                                let vg = GNode { m: icls.m, n: *value };
                                let c2 = match ctx {
                                    Some(c) => Rc::clone(c),
                                    None => crate::ctx::Ctx::new(),
                                };
                                if let Some(Value::Node(vcls)) = self.safe_infer(vg, &c2) {
                                    if self.node_name(vcls).as_deref() == Some("cached_property")
                                        && self.md(vcls.m).name == "functools"
                                    {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// arguments node ids of a function (for the property callcontext hack)
    pub fn func_arg_nodes(&self, func: GNode) -> Vec<GNode> {
        let md = self.md(func.m);
        let args = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
            NodeKind::Lambda(d) => d.args,
            _ => return Vec::new(),
        };
        match &md.tree.nodes[args.idx()].kind {
            NodeKind::Arguments(a) => {
                let mut v: Vec<GNode> = Vec::new();
                v.extend(a.posonlyargs.iter().map(|&n| GNode { m: func.m, n }));
                v.extend(a.args.iter().map(|&n| GNode { m: func.m, n }));
                if let Some(vn) = a.vararg_node {
                    v.push(GNode { m: func.m, n: vn });
                }
                v.extend(a.kwonlyargs.iter().map(|&n| GNode { m: func.m, n }));
                if let Some(kn) = a.kwarg_node {
                    v.push(GNode { m: func.m, n: kn });
                }
                v
            }
            _ => Vec::new(),
        }
    }

    /// instance_attrs map handle for tests/checkers
    pub fn instance_attrs_of(&self, cls: GNode) -> IndexMap<GSym, Vec<GNode>> {
        self.iattrs.borrow().get(&cls).cloned().unwrap_or_default()
    }
}

const MODULE_MODEL_ATTRS: [&str; 12] = [
    "__name__",
    "__doc__",
    "__file__",
    "__dict__",
    "__package__",
    "__path__",
    "__spec__",
    "__loader__",
    "__cached__",
    "builtins",
    "__init__",
    "__new__",
];

const CLASS_MODEL_ATTRS: [&str; 14] = [
    "__module__",
    "__name__",
    "__qualname__",
    "__doc__",
    "__mro__",
    "mro",
    "__bases__",
    "__class__",
    "__subclasses__",
    "__dict__",
    "__call__",
    "__annotations__",
    "__init__",
    "__new__",
];

impl Engine {
    /// get-or-create the implicit class local placeholder (scoped_nodes.py:
    /// 1911-1933 + objectmodel ClassModel attrs evaluated at construction:
    /// __module__ = Const(root().qname()), __qualname__ = Const(qname()),
    /// __annotations__ = Unknown -> Uninferable)
    pub fn implicit_class_local(&self, cls: GNode, which: u8) -> GNode {
        if let Some(g) = self.implicit_locals.borrow().get(&(cls, which)) {
            return *g;
        }
        let kind = match which {
            0 => NodeKind::Const(ConstValue::Str(self.md(cls.m).name.clone().into())),
            1 => NodeKind::Const(ConstValue::Str(self.qname(cls).into())),
            _ => NodeKind::Unknown,
        };
        let ph = self.alloc_synth_node(kind);
        self.implicit_owner.borrow_mut().insert(ph, cls);
        // parent = the class (the model Const's statement() resolves to it
        // during _filter_stmts when the name is looked up from class scope)
        self.reparents.borrow_mut().insert(ph, cls);
        self.implicit_locals.borrow_mut().insert((cls, which), ph);
        ph
    }

    fn class_model_attr(&self, cls: GNode, name: &str, ctx: Option<&Rc<Ctx>>) -> Value {
        match name {
            "__name__" => Value::SynthConst(Rc::new(ConstValue::Str(
                self.node_name(cls).unwrap_or_default().into(),
            ))),
            "__qualname__" => {
                let q = self.qname(cls);
                let q = q
                    .split_once('.')
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or(q);
                Value::SynthConst(Rc::new(ConstValue::Str(q.into())))
            }
            "__module__" => Value::SynthConst(Rc::new(ConstValue::Str(
                self.md(cls.m).name.clone().into(),
            ))),
            // ClassModel.attr___doc__ (objectmodel.py:511-513):
            // Const(getattr(self._instance.doc_node, "value", None))
            "__doc__" => {
                let doc = {
                    let md = self.md(cls.m);
                    match &md.tree.nodes[cls.n.idx()].kind {
                        NodeKind::ClassDef(d) => d.doc_node.and_then(|dn| {
                            match &md.tree.nodes[dn.idx()].kind {
                                NodeKind::Const(c) => Some(c.clone()),
                                _ => None,
                            }
                        }),
                        _ => None,
                    }
                };
                Value::SynthConst(Rc::new(doc.unwrap_or(ConstValue::None)))
            }
            "__mro__" => match self.mro(cls, ctx) {
                Ok(m) => Value::SynthSeq {
                    kind: SeqKind::Tuple,
                    elems: Rc::new(m.into_iter().map(Value::Node).collect()),
                },
                Err(_) => Value::Uninferable,
            },
            "__bases__" => {
                let elems: Vec<Value> = self
                    .inferred_bases(cls, ctx)
                    .into_iter()
                    .map(Value::Node)
                    .collect();
                Value::SynthSeq {
                    kind: SeqKind::Tuple,
                    elems: Rc::new(elems),
                }
            }
            "__class__" => match self.metaclass(cls, ctx) {
                Some(m) => m,
                None => Value::Node(self.builtins().type_),
            },
            // ClassModel.attr___call__ (objectmodel.py:707-710): calling a
            // class A() returns an instance of A — feeds the igetattr
            // descriptor check (getattr('__get__') metaclass walk burns
            // shared-counter bumps before the InferenceError)
            "__call__" => self.instantiate_class(cls),
            // ObjectModel.attr___new__/attr___init__ (objectmodel.py:
            // 135-165): synthetic BoundMethods on the extracted template
            // defs, bound to _get_bound_node(self) = the class itself
            "__new__" => self
                .obj_model_func_nodes()
                .map(|(f, _)| Value::BoundMethod {
                    func: f,
                    bound: Rc::new(Value::Node(cls)),
                })
                .unwrap_or(Value::Uninferable),
            "__init__" => self
                .obj_model_func_nodes()
                .map(|(_, f)| Value::BoundMethod {
                    func: f,
                    bound: Rc::new(Value::Node(cls)),
                })
                .unwrap_or(Value::Uninferable),
            // ClassModel.attr_mro (objectmodel.py:521-541): an
            // MroBoundMethod proxying implicit_metaclass.locals["mro"][0]
            // (renders BM:builtins.type.mro); calling it yields
            // attr___mro__ — a Tuple of the class's mro. We carry the
            // modeled CLASS in `bound` so the call interception can
            // compute its mro (see bound_method_infer_call_result_to).
            "mro" => {
                let b = self.builtins();
                let mro_fn = self
                    .class_locals_get(b.type_, self.sym("mro"))
                    .first()
                    .copied();
                match mro_fn {
                    Some(f) => Value::BoundMethod {
                        func: f,
                        bound: Rc::new(Value::Node(cls)),
                    },
                    None => Value::Uninferable,
                }
            }
            "__subclasses__" => Value::Uninferable,
            "__dict__" => Value::SynthDict {
                items: Rc::new(Vec::new()),
            },
            "__annotations__" => Value::Uninferable,
            _ => Value::Uninferable,
        }
    }
}

/// _c3_merge (scoped_nodes.py:72-107)
fn c3_merge(mut sequences: Vec<Vec<GNode>>) -> Option<Vec<GNode>> {
    let mut result: Vec<GNode> = Vec::new();
    loop {
        sequences.retain(|s| !s.is_empty());
        if sequences.is_empty() {
            return Some(result);
        }
        let mut head: Option<GNode> = None;
        'search: for s1 in &sequences {
            let candidate = s1[0];
            for s2 in &sequences {
                if s2[1..].contains(&candidate) {
                    continue 'search;
                }
            }
            head = Some(candidate);
            break;
        }
        let head = head?;
        result.push(head);
        for seq in &mut sequences {
            if seq.first() == Some(&head) {
                seq.remove(0);
            }
        }
    }
}
