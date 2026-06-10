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
                        NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                        | NodeKind::Lambda(_) => 3,
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
                    3 => stream_result(self.function_igetattr(*g, name, ctx), sink),
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
            Value::BoundMethod { func, bound } => {
                stream_result(self.method_igetattr(*func, Some(bound), name, ctx), sink)
            }
            Value::UnboundMethod { func } => {
                stream_result(self.method_igetattr(*func, None, name, ctx), sink)
            }
            Value::Property { func } | Value::Partial { func, .. } => {
                stream_result(self.property_igetattr(owner, *func, name, ctx), sink)
            }
            Value::Super { .. } => self.super_igetattr_to(owner, name, ctx, sink),
            Value::DictItems(_) | Value::DictKeys(_) | Value::DictValues(_) => {
                End::Raised(ErrKind::Attribute)
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
            return Ok(vec![NV::V(self.class_model_attr(cls, &name_str, ctx))]);
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
    fn metaclass_lookup_attribute(&self, cls: GNode, name: GSym, ctx: Option<&Rc<Ctx>>) -> Vec<NV> {
        let mut out = Vec::new();
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
                                    out.push(NV::V(Value::BoundMethod {
                                        func: *g,
                                        bound: Rc::new(Value::Node(frame)),
                                    }));
                                }
                                FType::StaticMethod => out.push(NV::V(attr.clone())),
                                _ => out.push(NV::V(Value::BoundMethod {
                                    func: *g,
                                    bound: Rc::new(Value::Node(cls)),
                                })),
                            }
                        }
                        _ => out.push(NV::V(attr.clone())),
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
                    | NV::V(Value::Property { func })
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
                } else if let Value::Property { func } = &inferred {
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
        let look = |name: &str| -> bool {
            let sym = self.sym(name);
            match self.class_getattr(cls, sym, Some(ctx), true) {
                Ok(attrs) => attrs.iter().any(|a| match a {
                    NV::N(g) => {
                        let md = self.md(g.m);
                        md.pure_python && md.name != "builtins"
                    }
                    NV::V(_) => false,
                }),
                Err(_) => false,
            }
        };
        look("__getattr__") || look("__getattribute__")
    }

    // ---------- Instance getattr / igetattr (§12.2-12.3) ----------

    /// ClassDef.instance_attr (scoped_nodes.py:2281-2301)
    /// ClassDef.instance_attr + instance_attr_ancestors
    /// (scoped_nodes.py): the ancestors walk gets the caller's context —
    /// including the lookupname mutated by Instance.igetattr (bases.py:281)
    pub fn instance_attr(&self, cls: GNode, name: GSym, ctx: Option<&Rc<Ctx>>) -> Result<Vec<GNode>, ErrKind> {
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
        match self.instance_attr(cls, name, ctx) {
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
                // _infer_stmts(self._wrap_attr(get_attr, ...), ...) with a
                // post-inference wrap (see wrap note below)
                let mut stopped = false;
                let end = {
                    let stopped = &mut stopped;
                    let ctx2 = Rc::clone(&ctx);
                    self.infer_stmts_to(&attrs, Some(&ctx), None, &mut |v| {
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
                end
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
        if matches!(owner, Value::SynthDict { .. })
            || matches!(owner, Value::Node(g) if self.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })))
        {
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
        match name {
            "__class__" => Some(Value::Node(cls)),
            "__module__" => Some(Value::SynthConst(Rc::new(ConstValue::Str(
                self.md(cls.m).name.clone().into(),
            )))),
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
            "__dict__" => Some(Value::SynthDict {
                items: Rc::new(Vec::new()),
            }),
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

    fn function_igetattr(
        &self,
        func: GNode,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        // instance_attrs first (scoped_nodes.py:1298-1311)
        if let Some(vals) = self
            .iattrs
            .borrow()
            .get(&func)
            .and_then(|m| m.get(&name))
            .cloned()
        {
            if !vals.is_empty() {
                let nv: Vec<NV> = vals.into_iter().map(NV::N).collect();
                let ctx2 = copy_context(ctx);
                ctx2.lookupname.set(Some(name));
                return Ok(self.infer_stmts(&nv, Some(&ctx2), None));
            }
        }
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
                    // CacheInfoBoundMethod proxying the function; calling it
                    // yields a _CacheInfo namedtuple instance — approximate
                    // the BM with the function itself (render parity:
                    // BM:<func qname>) — calls fall back to normal result
                    return Ok(Flow::one(Value::BoundMethod {
                        func,
                        bound: Rc::new(Value::Node(func)),
                    }));
                }
                _ => {}
            }
        }
        if let Some(v) = self.function_model_attr(func, &name_str) {
            return Ok(Flow::one(v));
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

    fn method_igetattr(
        &self,
        func: GNode,
        bound: Option<&Rc<Value>>,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        let name_str = self.sname(name);
        match name_str.as_str() {
            "__func__" => return Ok(Flow::one(Value::Node(func))),
            "__self__" => {
                return Ok(Flow::one(match bound {
                    Some(b) => (**b).clone(),
                    None => Value::SynthConst(Rc::new(ConstValue::None)),
                }))
            }
            _ => {}
        }
        self.function_igetattr(func, name, ctx)
    }

    fn property_igetattr(
        &self,
        owner: &Value,
        func: GNode,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Flow, ErrKind> {
        let name_str = self.sname(name);
        match name_str.as_str() {
            "fget" => return Ok(Flow::one(Value::Node(func))),
            "fset" | "deleter" | "getter" | "setter" => {
                // PropertyModel: functions; approximate with the function
                let _ = owner;
                return Ok(Flow::one(Value::Node(func)));
            }
            _ => {}
        }
        self.function_igetattr(func, name, ctx)
    }

    fn function_model_attr(&self, func: GNode, name: &str) -> Option<Value> {
        let md = self.md(func.m);
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
            "__dict__" => Some(Value::SynthDict {
                items: Rc::new(Vec::new()),
            }),
            "__defaults__" | "__kwdefaults__" | "__annotations__" => Some(Value::Uninferable),
            "__class__" => Some(Value::Node(self.builtins().function)),
            _ => None,
        }
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
                        Value::Property { func } => {
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
                    let basecls = match &baseobj {
                        Value::Node(g)
                            if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) =>
                        {
                            *g
                        }
                        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => *cls,
                        _ => return Drive::Go,
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
        let bases = self.class_bases(cls);
        if bases.is_empty() {
            if self.qname(cls) != "builtins.object" {
                return vec![self.builtins().object];
            }
            return Vec::new();
        }
        let mut out = Vec::new();
        for base in bases {
            // _infer_last with a cloned context
            let c = match ctx {
                Some(c) => c.clone_ctx(),
                None => Ctx::new(),
            };
            let flow = self.infer(base, &c);
            let last = flow.vals.last().cloned();
            let Some(last) = last else { continue };
            let basecls = match last {
                Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => cls,
                Value::Node(g) if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) => g,
                _ => continue,
            };
            out.push(basecls);
        }
        out
    }

    /// is_subtype_of (scoped_nodes.py:2004-2015): `any(...)` abandons the
    /// ancestors generator on the first match.
    pub fn is_subtype_of(&self, cls: GNode, type_name: &str, ctx: Option<&Rc<Ctx>>) -> bool {
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
        // for base in self.bases: for baseobj in base.infer(context):
        // (context passed through unchanged, NOT copied)
        let base_ctx = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        for base in self.class_bases(cls) {
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
            NodeKind::Lambda(_) => return FType::Function,
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
            // decorator call chains + inferred classes: infer the decorator
            let g = GNode { m: func.m, n: dn };
            let ctx = Ctx::new();
            if let NodeKind::Call { func: cf, .. } = &md.tree.nodes[dn.idx()].kind {
                let cg = GNode { m: func.m, n: *cf };
                // next(node.func.infer()) — single pull
                if let Ok(Some(current)) = self.first_value(cg, &ctx) {
                    if let Some(t) = self.infer_decorator_callchain(&current) {
                        return t;
                    }
                }
            }
            let flow = self.infer(g, &ctx);
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
        let func = match v {
            Value::Node(g)
                if self.kind_is(*g, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                }) =>
            {
                *g
            }
            _ => return None,
        };
        // next(node.infer_call_result(caller=None), None) — single pull
        let result = self
            .infer_call_result_first(&Value::Node(func), None, None)
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
                                // meth must be a FunctionDef in this frame
                                let sym = self.g(&md, *tn);
                                let locs = self.class_locals_get(frame, sym);
                                if let Some(meth) = locs.last() {
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
            | Value::Property { func }
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
            "__doc__" => Value::SynthConst(Rc::new(ConstValue::None)),
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
            "__subclasses__" | "mro" | "__call__" | "__new__" | "__init__" => Value::Uninferable,
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
