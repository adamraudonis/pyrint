//! Call inference: Call._infer, infer_call_result per callable kind,
//! CallSite + infer_argument, Arguments inference.
//! Ports: node_classes.py:1744-1784, scoped_nodes.py:1555-1636 + 2071-2102,
//! bases.py:317-345 + 472-674, arguments.py, protocols.py:352-444.

use std::cell::RefCell;
use std::rc::Rc;

use pyast::tree::{ConstValue, NodeKind};
use pyast::NodeId;

use crate::ctx::{bind_context_to_node, copy_context, CallCtx, Ctx};
use crate::graph::{Engine, FType};
use crate::value::{ErrKind, Flow, GNode, GSym, SeqKind, Value, NV};

pub struct ArgSpec {
    pub args: Vec<GNode>,
    pub args_unknown: bool,
    pub posonlyargs: Vec<GNode>,
    pub kwonlyargs: Vec<GNode>,
    pub vararg: Option<GSym>,
    pub vararg_node: Option<GNode>,
    pub kwarg: Option<GSym>,
    pub kwarg_node: Option<GNode>,
    pub defaults: Vec<GNode>,
    pub kw_defaults: Vec<Option<GNode>>,
    pub arguments_node: GNode,
}

impl ArgSpec {
    /// Arguments.arguments: posonly + args + vararg + kwonly + kwarg
    pub fn arguments(&self) -> Vec<GNode> {
        let mut v = self.posonlyargs.clone();
        v.extend(self.args.iter().copied());
        if let Some(vn) = self.vararg_node {
            v.push(vn);
        }
        v.extend(self.kwonlyargs.iter().copied());
        if let Some(kn) = self.kwarg_node {
            v.push(kn);
        }
        v
    }
}

impl Engine {
    pub fn arg_spec(&self, func: GNode) -> Option<ArgSpec> {
        let md = self.md(func.m);
        let args_id = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
            NodeKind::Lambda(d) => d.args,
            _ => return None,
        };
        self.arg_spec_of_arguments(GNode { m: func.m, n: args_id })
    }

    pub fn arg_spec_of_arguments(&self, ga: GNode) -> Option<ArgSpec> {
        let md = self.md(ga.m);
        match &md.tree.nodes[ga.n.idx()].kind {
            NodeKind::Arguments(a) => Some(ArgSpec {
                args: a.args.iter().map(|&n| GNode { m: ga.m, n }).collect(),
                args_unknown: *md.args_unknown.get(&ga.n).unwrap_or(&false),
                posonlyargs: a.posonlyargs.iter().map(|&n| GNode { m: ga.m, n }).collect(),
                kwonlyargs: a.kwonlyargs.iter().map(|&n| GNode { m: ga.m, n }).collect(),
                vararg: a.vararg.map(|s| self.g(&md, s)),
                vararg_node: a.vararg_node.map(|n| GNode { m: ga.m, n }),
                kwarg: a.kwarg.map(|s| self.g(&md, s)),
                kwarg_node: a.kwarg_node.map(|n| GNode { m: ga.m, n }),
                defaults: a.defaults.iter().map(|&n| GNode { m: ga.m, n }).collect(),
                kw_defaults: a
                    .kw_defaults
                    .iter()
                    .map(|o| o.map(|n| GNode { m: ga.m, n }))
                    .collect(),
                arguments_node: ga,
            }),
            _ => None,
        }
    }

    pub fn assign_name_of(&self, g: GNode) -> Option<GSym> {
        let md = self.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::AssignName { name } => Some(self.g(&md, *name)),
            _ => None,
        }
    }

    // ---------- Call._infer (node_classes.py:1744-1784) ----------

    pub fn infer_call(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (func, args, keywords) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, args, keywords } => (
                GNode { m: node.m, n: *func },
                args.iter().map(|&a| GNode { m: node.m, n: a }).collect::<Vec<_>>(),
                keywords.clone(),
            ),
            _ => return Flow::err(ErrKind::Inference),
        };
        let callctx = copy_context(Some(ctx));
        *callctx.boundnode.borrow_mut() = None;
        // _populate_context_lookup with context.clone()
        {
            let clone = ctx.clone_ctx();
            let mut extra: rustc_hash::FxHashMap<GNode, Rc<Ctx>> = Default::default();
            for &a in &args {
                let key = match &md.tree.nodes[a.n.idx()].kind {
                    NodeKind::Starred { value, .. } => GNode { m: node.m, n: *value },
                    _ => a,
                };
                extra.insert(key, Rc::clone(&clone));
            }
            for &kw in &keywords {
                if let NodeKind::Keyword { value, .. } = &md.tree.nodes[kw.idx()].kind {
                    extra.insert(GNode { m: node.m, n: *value }, Rc::clone(&clone));
                }
            }
            *callctx.extra_context.borrow_mut() = Rc::new(extra);
        }
        let kw_pairs: Vec<(Option<GSym>, GNode)> = keywords
            .iter()
            .filter_map(|&kw| match &md.tree.nodes[kw.idx()].kind {
                NodeKind::Keyword { arg, value } => Some((
                    arg.map(|s| self.g(&md, s)),
                    GNode { m: node.m, n: *value },
                )),
                _ => None,
            })
            .collect();
        let callees = self.infer(func, ctx);
        let mut out: Vec<Value> = Vec::new();
        for callee in &callees.vals {
            if callee.is_uninferable() {
                out.push(Value::Uninferable);
                continue;
            }
            if !self.has_infer_call_result(callee) {
                continue;
            }
            *callctx.callcontext.borrow_mut() = Some(Rc::new(CallCtx {
                id: self.next_callctx_id(),
                args: RefCell::new(args.iter().map(|&a| NV::N(a)).collect()),
                keywords: RefCell::new(kw_pairs.clone()),
                callee: RefCell::new(Some(callee.clone())),
            }));
            let f = self.infer_call_result(callee, Some(node), Some(&callctx));
            out.extend(f.vals);
            if let Some(e) = f.err {
                if !e.is_inference() {
                    return Flow { vals: out, err: Some(e) };
                }
                // InferenceError from one callee: continue
            }
        }
        if let Some(e) = callees.err {
            return Flow { vals: out, err: Some(e) };
        }
        Flow::ok(out)
    }

    // ---------- infer_call_result dispatch ----------

    pub fn infer_call_result(
        &self,
        callee: &Value,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Flow {
        if self.depth.get() >= self.max_depth {
            return Flow::err(ErrKind::Recursion);
        }
        self.depth.set(self.depth.get() + 1);
        let r = self.infer_call_result_inner(callee, caller, ctx);
        self.depth.set(self.depth.get() - 1);
        r
    }

    fn infer_call_result_inner(
        &self,
        callee: &Value,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Flow {
        match callee {
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => {
                        self.function_infer_call_result(*g, caller, ctx)
                    }
                    NodeKind::Lambda(d) => {
                        let body = GNode { m: g.m, n: d.body };
                        let c = match ctx {
                            Some(c) => Rc::clone(c),
                            None => Ctx::new(),
                        };
                        self.infer(body, &c)
                    }
                    NodeKind::ClassDef(_) => self.class_infer_call_result(*g, caller, ctx),
                    // Const / containers -> BaseInstance
                    NodeKind::Const(_)
                    | NodeKind::List { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::Set { .. }
                    | NodeKind::Dict { .. } => {
                        self.base_instance_infer_call_result(callee, caller, ctx)
                    }
                    _ => Flow::err(ErrKind::Inference),
                }
            }
            Value::BoundMethod { func, bound } => {
                self.bound_method_infer_call_result(*func, bound, caller, ctx)
            }
            Value::UnboundMethod { func } => {
                self.unbound_method_infer_call_result(*func, caller, ctx)
            }
            Value::Property { .. } => Flow::err(ErrKind::Inference),
            Value::Partial {
                func,
                filled_args,
                filled_keywords,
            } => {
                if let Some(c) = ctx {
                    if let Some(cc) = c.callcontext.borrow().as_ref() {
                        let current: Vec<Option<GSym>> = cc
                            .keywords
                            .borrow()
                            .iter()
                            .map(|(k, _)| *k)
                            .collect();
                        for (k, v) in filled_keywords.iter() {
                            if !current.contains(&Some(*k)) {
                                cc.keywords.borrow_mut().push((Some(*k), *v));
                            }
                        }
                        let mut new_args: Vec<NV> =
                            filled_args.iter().map(|&a| NV::N(a)).collect();
                        new_args.extend(cc.args.borrow().iter().cloned());
                        *cc.args.borrow_mut() = new_args;
                    }
                }
                self.function_infer_call_result(*func, caller, ctx)
            }
            Value::Inst { .. }
            | Value::ExcInst { .. }
            | Value::SynthConst(_)
            | Value::SynthSeq { .. }
            | Value::SynthDict { .. }
            | Value::FrozenSet { .. }
            | Value::Generator { .. } => {
                self.base_instance_infer_call_result(callee, caller, ctx)
            }
            _ => Flow::err(ErrKind::Inference),
        }
    }

    // ---------- FunctionDef.infer_call_result (scoped_nodes.py:1555-1636) ----------

    pub fn function_infer_call_result(
        &self,
        func: GNode,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Flow {
        let _ = caller;
        let ctx = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        let md = self.md(func.m);
        let is_async = matches!(
            md.tree.nodes[func.n.idx()].kind,
            NodeKind::AsyncFunctionDef(_)
        );
        if self.is_generator(func) {
            return Flow::one(Value::Generator {
                func,
                is_async,
            });
        }
        // NOTE: the six `with_metaclass` hack (scoped_nodes.py:1577-1615)
        // is not ported yet; none of the pinned corpora rely on it for the
        // -E message set (revisit on diff evidence).
        let body: Vec<NodeId> = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
            _ => return Flow::err(ErrKind::Inference),
        };
        let returns = self.return_nodes_skip_functions(func);
        if returns.is_empty() {
            if !body.is_empty() {
                if self.is_abstract(func, true, true) {
                    return Flow::uninferable();
                }
                return Flow::one(Value::SynthConst(Rc::new(ConstValue::None)));
            }
            return Flow::err(ErrKind::Inference);
        }
        let mut out = Vec::new();
        for ret in returns {
            let rmd = self.md(ret.m);
            let value = match &rmd.tree.nodes[ret.n.idx()].kind {
                NodeKind::Return { value } => *value,
                _ => None,
            };
            match value {
                None => out.push(Value::SynthConst(Rc::new(ConstValue::None))),
                Some(v) => {
                    let f = self.infer(GNode { m: ret.m, n: v }, &ctx);
                    out.extend(f.vals);
                    if let Some(e) = f.err {
                        if e.is_inference() {
                            out.push(Value::Uninferable);
                        } else {
                            return Flow { vals: out, err: Some(e) };
                        }
                    }
                }
            }
        }
        Flow::ok(out)
    }

    /// is_generator (scoped_nodes.py:1511-1519): a Yield/YieldFrom not
    /// nested in another function or lambda.
    pub fn is_generator(&self, func: GNode) -> bool {
        let md = self.md(func.m);
        let body: Vec<NodeId> = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
            _ => return false,
        };
        let mut stack = body;
        let mut buf = Vec::new();
        while let Some(n) = stack.pop() {
            match &md.tree.nodes[n.idx()].kind {
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::Lambda(_) => {
                    continue
                }
                NodeKind::Yield { .. } | NodeKind::YieldFrom { .. } => return true,
                _ => {}
            }
            buf.clear();
            md.tree.push_children(n, &mut buf);
            stack.extend(buf.iter().copied());
        }
        false
    }

    /// _get_return_nodes_skip_functions over multi-line block fields
    fn return_nodes_skip_functions(&self, func: GNode) -> Vec<GNode> {
        let md = self.md(func.m);
        let body: Vec<NodeId> = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        for stmt in body {
            self.collect_returns(GNode { m: func.m, n: stmt }, &mut out);
        }
        out
    }

    fn collect_returns(&self, node: GNode, out: &mut Vec<GNode>) {
        let md = self.md(node.m);
        let blocks: Vec<Vec<NodeId>> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Return { .. } => {
                out.push(node);
                return;
            }
            // is_function children are skipped by the caller below
            NodeKind::If { body, orelse, .. } => vec![body.clone(), orelse.clone()],
            NodeKind::For(d) | NodeKind::AsyncFor(d) => vec![d.body.clone(), d.orelse.clone()],
            NodeKind::While { body, orelse, .. } => vec![body.clone(), orelse.clone()],
            NodeKind::Try(d) | NodeKind::TryStar(d) => vec![
                d.body.clone(),
                d.handlers.clone(),
                d.orelse.clone(),
                d.finalbody.clone(),
            ],
            NodeKind::ExceptHandler { body, .. } => vec![body.clone()],
            NodeKind::With(d) | NodeKind::AsyncWith(d) => vec![d.body.clone()],
            NodeKind::Match { cases, .. } => vec![cases.clone()],
            NodeKind::MatchCase { body, .. } => vec![body.clone()],
            NodeKind::ClassDef(d) => vec![d.body.clone()],
            _ => return,
        };
        for block in blocks {
            for child in block {
                let g = GNode { m: node.m, n: child };
                if self.kind_is(g, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                }) {
                    continue;
                }
                self.collect_returns(g, out);
            }
        }
    }

    /// is_abstract (scoped_nodes.py:1475-1509) — first-statement-only quirk
    pub fn is_abstract(&self, func: GNode, pass_is_abstract: bool, any_raise: bool) -> bool {
        let md = self.md(func.m);
        let (decorators, body) = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                (d.decorators, d.body.clone())
            }
            _ => return false,
        };
        if let Some(dec) = decorators {
            let nodes: Vec<NodeId> = match &md.tree.nodes[dec.idx()].kind {
                NodeKind::Decorators { nodes } => nodes.clone(),
                _ => Vec::new(),
            };
            for dn in nodes {
                let g = GNode { m: func.m, n: dn };
                let f = self.infer(g, &Ctx::new());
                if let Some(first) = f.vals.first() {
                    if !first.is_uninferable() {
                        if let Some(q) = self.value_qname(first) {
                            if q == "abc.abstractproperty" || q == "abc.abstractmethod" {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        for child in &body {
            let k = &md.tree.nodes[child.idx()].kind;
            if let NodeKind::Raise { exc, .. } = k {
                if any_raise {
                    return true;
                }
                // raises_not_implemented: any Name "NotImplementedError" in exc
                if let Some(exc) = exc {
                    if self.contains_name(GNode { m: func.m, n: *exc }, "NotImplementedError") {
                        return true;
                    }
                }
            }
            return pass_is_abstract && matches!(k, NodeKind::Pass);
        }
        pass_is_abstract
    }

    fn contains_name(&self, node: GNode, name: &str) -> bool {
        let md = self.md(node.m);
        let mut stack = vec![node.n];
        let mut buf = Vec::new();
        while let Some(n) = stack.pop() {
            if let NodeKind::Name { name: ns } = &md.tree.nodes[n.idx()].kind {
                if md.tree.s(*ns) == name {
                    return true;
                }
            }
            buf.clear();
            md.tree.push_children(n, &mut buf);
            stack.extend(buf.iter().copied());
        }
        false
    }

    // ---------- ClassDef.infer_call_result (scoped_nodes.py:2071-2102) ----------

    fn class_infer_call_result(&self, cls: GNode, caller: Option<GNode>, ctx: Option<&Rc<Ctx>>) -> Flow {
        // type("X", bases, attrs) — _infer_type_call not ported yet
        let _ = caller;
        let mut dunder_call: Option<Value> = None;
        if let Some(Value::Node(meta)) = self.metaclass(cls, ctx) {
            let call_sym = self.sym("__call__");
            if !self.class_locals_get(meta, call_sym).is_empty() {
                if let Ok(f) = self.class_igetattr(meta, call_sym, ctx, true) {
                    dunder_call = f.vals.into_iter().next();
                }
            }
        }
        if let Some(dc) = dunder_call {
            let qn = self.value_qname(&dc);
            if qn.as_deref() != Some("builtins.type.__call__") {
                let ctx2 = bind_context_to_node(ctx, Value::Node(cls));
                if let Some(cc) = ctx2.callcontext.borrow().as_ref() {
                    *cc.callee.borrow_mut() = Some(dc.clone());
                }
                return self.infer_call_result(&dc, caller, Some(&ctx2));
            }
        }
        Flow::one(self.instantiate_class(cls))
    }

    // ---------- Bound/Unbound method ----------

    fn bound_method_infer_call_result(
        &self,
        func: GNode,
        bound: &Rc<Value>,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Flow {
        let ctx2 = bind_context_to_node(ctx, (**bound).clone());
        // type.__new__(mcs, name, bases, attrs) — _infer_type_new_call not
        // ported yet (synthetic class building); falls through.
        self.unbound_method_infer_call_result_with(func, caller, Some(&ctx2))
    }

    fn unbound_method_infer_call_result(
        &self,
        func: GNode,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Flow {
        self.unbound_method_infer_call_result_with(func, caller, ctx)
    }

    fn unbound_method_infer_call_result_with(
        &self,
        func: GNode,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Flow {
        let name = self.node_name(func);
        if name.as_deref() == Some("__new__") {
            let frame = self.parent(func).map(|p| self.frame(p));
            if let Some(frame) = frame {
                let q = self.qname(frame);
                if q.starts_with("builtins.") && q != "builtins.type" {
                    return self.infer_builtin_new(caller, ctx);
                }
            }
        }
        self.function_infer_call_result(func, caller, ctx)
    }

    /// bases.py:497-530 _infer_builtin_new
    fn infer_builtin_new(&self, caller: Option<GNode>, ctx: Option<&Rc<Ctx>>) -> Flow {
        let Some(caller) = caller else {
            return Flow::empty();
        };
        let md = self.md(caller.m);
        let args: Vec<NodeId> = match &md.tree.nodes[caller.n.idx()].kind {
            NodeKind::Call { args, .. } => args.clone(),
            _ => return Flow::empty(),
        };
        if args.is_empty() {
            return Flow::empty();
        }
        let ctx = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        if args.len() > 1 {
            let a1 = GNode { m: caller.m, n: args[1] };
            let cv = match &md.tree.nodes[args[1].idx()].kind {
                NodeKind::Const(c) => Some(c.clone()),
                _ => {
                    let f = self.infer(a1, &ctx);
                    f.vals.first().and_then(|v| self.value_const(v))
                }
            };
            if let Some(cv) = cv {
                if !matches!(cv, ConstValue::None) {
                    return Flow::one(Value::SynthConst(Rc::new(cv)));
                }
            }
        }
        let a0 = GNode { m: caller.m, n: args[0] };
        let node_ctx = ctx
            .extra_context
            .borrow()
            .get(&a0)
            .cloned()
            .unwrap_or_else(Ctx::new);
        let f = self.infer(a0, &node_ctx);
        let mut out = Vec::new();
        if let Some(first) = f.vals.first() {
            if first.is_uninferable() {
                out.push(Value::Uninferable);
            }
            if let Value::Node(g) = first {
                if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) {
                    out.push(Value::Inst { cls: *g });
                }
            }
            return Flow {
                vals: out,
                err: Some(ErrKind::Inference),
            };
        }
        Flow::empty()
    }

    // ---------- BaseInstance.infer_call_result (bases.py:317-345) ----------

    fn base_instance_infer_call_result(
        &self,
        instance: &Value,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Flow {
        let ctx2 = bind_context_to_node(ctx, instance.clone());
        let mut inferred = false;
        let mut out: Vec<Value> = Vec::new();
        // attribute-call shortcut
        if let Some(caller) = caller {
            let md = self.md(caller.m);
            if let NodeKind::Call { func, .. } = &md.tree.nodes[caller.n.idx()].kind {
                if let NodeKind::Attribute { attrname, .. } = &md.tree.nodes[func.idx()].kind {
                    let sym = self.g(&md, *attrname);
                    if let Ok(f) = self.igetattr_value(instance, sym, Some(&ctx2)) {
                        if !f.vals.is_empty() {
                            inferred = true;
                        }
                        out.extend(f.vals);
                    }
                }
            }
        }
        let proxied = self.proxied_class(instance);
        if let Some(cls) = proxied {
            let call_sym = self.sym("__call__");
            if let Ok(f) = self.class_igetattr(cls, call_sym, Some(&ctx2), true) {
                for node in f.vals {
                    if node.is_uninferable() || !self.value_callable(&node, &ctx2) {
                        continue;
                    }
                    // recursion prevention: instance of same class
                    let same = self.proxied_class(&node) == proxied
                        && matches!(node, Value::Inst { .. });
                    if same {
                        inferred = true;
                        out.push(node);
                        continue;
                    }
                    let sub = self.infer_call_result(&node, caller, Some(&ctx2));
                    if !sub.vals.is_empty() {
                        inferred = true;
                    }
                    out.extend(sub.vals);
                }
            }
        }
        if !inferred {
            return Flow::err(ErrKind::Inference);
        }
        Flow::ok(out)
    }

    // ---------- Arguments._infer + protocols (§10) ----------

    pub fn infer_arguments_node(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return Flow::err(ErrKind::Inference),
        };
        self.arguments_infer_argname(node, Some(name), ctx)
    }

    /// protocols.py:352-413 _arguments_infer_argname
    pub fn arguments_infer_argname(
        &self,
        arguments_node: GNode,
        name: Option<GSym>,
        ctx: &Rc<Ctx>,
    ) -> Flow {
        let Some(spec) = self.arg_spec_of_arguments(arguments_node) else {
            return Flow::err(ErrKind::Inference);
        };
        let all_args = spec.arguments();
        if all_args.is_empty() {
            return Flow::uninferable();
        }
        let func = match self.parent(arguments_node) {
            Some(f) => f,
            None => return Flow::uninferable(),
        };
        let functype = self.func_type(func);
        // args excluding vararg/kwarg names
        let args: Vec<GNode> = all_args
            .iter()
            .copied()
            .filter(|&a| {
                let n = self.assign_name_of(a);
                n != spec.vararg && n != spec.kwarg || n.is_none()
            })
            .collect();
        let first_name = all_args.first().and_then(|&a| self.assign_name_of(a));
        if !args.is_empty() && first_name == name && functype != FType::StaticMethod {
            let parent_scope = self
                .parent(func)
                .map(|p| self.scope(p));
            let mut cls_value: Option<Value> = parent_scope.map(Value::Node);
            let is_metaclass = parent_scope
                .map(|c| {
                    self.kind_is(c, |k| matches!(k, NodeKind::ClassDef(_)))
                        && self.class_type(c) == "metaclass"
                })
                .unwrap_or(false);
            if let Some(bn) = ctx.boundnode.borrow().as_ref() {
                if let Value::Inst { cls } | Value::ExcInst { cls, .. } = bn {
                    cls_value = Some(Value::Node(*cls));
                }
            }
            if is_metaclass || functype == FType::ClassMethod {
                return Flow::ok(cls_value.into_iter().collect());
            }
            if functype == FType::Method {
                let inst = match cls_value {
                    Some(Value::Node(c))
                        if self.kind_is(c, |k| matches!(k, NodeKind::ClassDef(_))) =>
                    {
                        Some(self.instantiate_class(c))
                    }
                    other => other,
                };
                return Flow::ok(inst.into_iter().collect());
            }
        }
        // call-context path
        let cc_opt = ctx.callcontext.borrow().clone();
        if let Some(cc) = cc_opt {
            let callee = cc.callee.borrow().clone();
            let callee_name = callee.as_ref().and_then(|c| match c {
                Value::Node(g) => self.node_name(*g),
                Value::BoundMethod { func, .. }
                | Value::UnboundMethod { func }
                | Value::Property { func }
                | Value::Partial { func, .. } => self.node_name(*func),
                _ => None,
            });
            if callee_name.is_some() && callee_name == self.node_name(func) {
                let site = self.call_site_from(&cc, ctx);
                return match name {
                    Some(n) => self.infer_argument(&site, func, n, ctx),
                    None => Flow::err(ErrKind::Inference),
                };
            }
        }
        let name_str = name.map(|n| self.sname(n));
        if name.is_some() && name == spec.vararg {
            let mut elems: Vec<Value> = Vec::new();
            if args.is_empty()
                && self.node_name(func).as_deref() == Some("__init__")
            {
                if let Some(scope) = self.parent(func).map(|p| self.scope(p)) {
                    if self.kind_is(scope, |k| matches!(k, NodeKind::ClassDef(_))) {
                        elems.push(self.instantiate_class(scope));
                    }
                }
            }
            return Flow::one(Value::SynthSeq {
                kind: SeqKind::Tuple,
                elems: Rc::new(elems),
            });
        }
        if name.is_some() && name == spec.kwarg {
            return Flow::one(Value::SynthDict {
                items: Rc::new(Vec::new()),
            });
        }
        let _ = name_str;
        // default value + Uninferable
        match name.and_then(|n| self.default_value(&spec, n)) {
            Some(def) => {
                let c = copy_context(Some(ctx));
                let mut f = self.infer(def, &c);
                f.vals.push(Value::Uninferable);
                Flow::ok(f.vals)
            }
            None => Flow::uninferable(),
        }
    }

    /// Arguments.default_value (node_classes.py:930-955)
    pub fn default_value(&self, spec: &ArgSpec, name: GSym) -> Option<GNode> {
        // kwonly first
        if let Some(i) = spec
            .kwonlyargs
            .iter()
            .position(|&a| self.assign_name_of(a) == Some(name))
        {
            if spec.kw_defaults.len() > i {
                return spec.kw_defaults[i]; // None -> NoDefault
            }
            return None;
        }
        let args: Vec<GNode> = spec
            .arguments()
            .into_iter()
            .filter(|&a| {
                let n = self.assign_name_of(a);
                !(n.is_some() && (n == spec.vararg || n == spec.kwarg))
            })
            .collect();
        if let Some(index) = args
            .iter()
            .position(|&a| self.assign_name_of(a) == Some(name))
        {
            let idx = index as i64
                - (args.len() as i64 - spec.defaults.len() as i64 - spec.kw_defaults.len() as i64);
            if idx >= 0 && (idx as usize) < spec.defaults.len() {
                return Some(spec.defaults[idx as usize]);
            }
        }
        None
    }

    /// arguments_assigned_stmts (protocols.py:416-444) is implemented in
    /// protocols.rs; CallSite below.

    // ---------- CallSite (arguments.py) ----------

    pub fn call_site_from(&self, cc: &CallCtx, ctx: &Rc<Ctx>) -> CallSite {
        // unpack args
        let mut unpacked_args: Vec<NV> = Vec::new();
        for arg in cc.args.borrow().iter() {
            let arg_node = match arg {
                NV::N(g) => *g,
                NV::V(v) => {
                    unpacked_args.push(NV::V(v.clone()));
                    continue;
                }
            };
            let md = self.md(arg_node.m);
            match &md.tree.nodes[arg_node.n.idx()].kind {
                NodeKind::Starred { value, .. } => {
                    let v = self.safe_infer(GNode { m: arg_node.m, n: *value }, &ctx.clone_ctx());
                    match v.and_then(|v| self.value_elts(&v).map(|e| (v, e))) {
                        Some((_, elts)) => {
                            for e in elts {
                                unpacked_args.push(match e {
                                    Value::Node(g) => NV::N(g),
                                    other => NV::V(other),
                                });
                            }
                        }
                        None => unpacked_args.push(NV::V(Value::Uninferable)),
                    }
                }
                _ => unpacked_args.push(NV::N(arg_node)),
            }
        }
        // unpack keywords
        let mut unpacked_kwargs: Vec<(GSym, NV)> = Vec::new();
        let mut duplicated: Vec<GSym> = Vec::new();
        for (name, value) in cc.keywords.borrow().iter() {
            match name {
                None => {
                    let v = self.safe_infer(*value, &ctx.clone_ctx());
                    match v.and_then(|v| self.value_dict_items(&v)) {
                        Some(items) => {
                            for (k, val) in items {
                                let key_inferred = match &k {
                                    Value::Node(g) => self.safe_infer(*g, &ctx.clone_ctx()),
                                    other => Some(other.clone()),
                                };
                                match key_inferred.and_then(|kv| self.value_const(&kv)) {
                                    Some(ConstValue::Str(s)) => {
                                        let sym = self.sym(&s);
                                        if unpacked_kwargs.iter().any(|(k2, _)| *k2 == sym) {
                                            duplicated.push(sym);
                                            if let Some(e) = unpacked_kwargs
                                                .iter_mut()
                                                .find(|(k2, _)| *k2 == sym)
                                            {
                                                e.1 = NV::V(Value::Uninferable);
                                            }
                                        } else {
                                            unpacked_kwargs.push((
                                                sym,
                                                match val {
                                                    Value::Node(g) => NV::N(g),
                                                    other => NV::V(other),
                                                },
                                            ));
                                        }
                                    }
                                    _ => {
                                        // non-Const-str key: entry Uninferable
                                        let sym = self.sym("**");
                                        unpacked_kwargs.push((sym, NV::V(Value::Uninferable)));
                                    }
                                }
                            }
                        }
                        None => {
                            let sym = self.sym("**");
                            unpacked_kwargs.push((sym, NV::V(Value::Uninferable)));
                        }
                    }
                }
                Some(n) => {
                    if unpacked_kwargs.iter().any(|(k2, _)| k2 == n) {
                        duplicated.push(*n);
                        if let Some(e) = unpacked_kwargs.iter_mut().find(|(k2, _)| k2 == n) {
                            e.1 = NV::V(Value::Uninferable);
                        }
                    } else {
                        unpacked_kwargs.push((*n, NV::N(*value)));
                    }
                }
            }
        }
        CallSite {
            unpacked_args,
            unpacked_kwargs,
            duplicated_keywords: duplicated,
        }
    }

    /// CallSite.infer_argument (arguments.py:141-309)
    pub fn infer_argument(
        &self,
        site: &CallSite,
        funcnode: GNode,
        name: GSym,
        ctx: &Rc<Ctx>,
    ) -> Flow {
        let is_func = self.kind_is(funcnode, |k| {
            matches!(
                k,
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::Lambda(_)
            )
        });
        if !is_func {
            return Flow::err(ErrKind::Inference);
        }
        if site.duplicated_keywords.contains(&name) {
            return Flow::err(ErrKind::Inference);
        }
        // keywords first
        if let Some((_, v)) = site
            .keyword_arguments()
            .into_iter()
            .find(|(k, _)| *k == name)
        {
            return self.infer_nv(&v, ctx);
        }
        let Some(spec) = self.arg_spec(funcnode) else {
            return Flow::err(ErrKind::Inference);
        };
        if spec.args_unknown {
            return Flow::err(ErrKind::Inference);
        }
        let positional_all = site.positional_arguments();
        if positional_all.len() > spec.args.len()
            && spec.vararg.is_none()
            && spec.posonlyargs.is_empty()
        {
            return Flow::err(ErrKind::Inference);
        }
        let mut positional: Vec<NV> = positional_all
            .iter()
            .take(spec.args.len())
            .cloned()
            .collect();
        let vararg: Vec<NV> = positional_all
            .iter()
            .skip(spec.args.len())
            .cloned()
            .collect();
        let argindex: Option<usize> = if Some(name) == spec.vararg || Some(name) == spec.kwarg {
            None
        } else {
            spec.arguments()
                .iter()
                .position(|&a| self.assign_name_of(a) == Some(name))
        };
        let kwonly: Vec<GSym> = spec
            .kwonlyargs
            .iter()
            .filter_map(|&a| self.assign_name_of(a))
            .collect();
        let mut kwargs: Vec<(GSym, NV)> = site
            .keyword_arguments()
            .into_iter()
            .filter(|(k, _)| !kwonly.contains(k))
            .collect();
        if positional.len() < spec.args.len() {
            for &func_arg in &spec.args {
                if let Some(an) = self.assign_name_of(func_arg) {
                    if let Some(pos) = kwargs.iter().position(|(k, _)| *k == an) {
                        let (_, v) = kwargs.remove(pos);
                        positional.push(v);
                    }
                }
            }
        }
        let functype = self.func_type(funcnode);
        if let Some(argindex) = argindex {
            let mut boundnode = ctx.boundnode.borrow().clone();
            if argindex == 0 && matches!(functype, FType::Method | FType::ClassMethod) {
                if boundnode.is_none() && functype == FType::Method && !positional.is_empty() {
                    return self.infer_nv(&positional[0], ctx);
                }
                if boundnode.is_none() {
                    boundnode = self.parent(funcnode).map(|p| Value::Node(self.frame(p)));
                }
                if let Some(Value::Node(bcls)) = &boundnode {
                    if self.kind_is(*bcls, |k| matches!(k, NodeKind::ClassDef(_))) {
                        let method_scope = self.parent(funcnode).map(|p| self.scope(p));
                        let meta = self.metaclass(*bcls, Some(ctx));
                        if let (Some(ms), Some(Value::Node(mg))) = (method_scope, &meta) {
                            if ms == *mg {
                                return Flow::ok(boundnode.into_iter().collect());
                            }
                        }
                    }
                }
                if functype == FType::Method {
                    let bn = match boundnode {
                        Some(Value::Node(c))
                            if self.kind_is(c, |k| matches!(k, NodeKind::ClassDef(_))) =>
                        {
                            Some(self.instantiate_class(c))
                        }
                        other => other,
                    };
                    return Flow::ok(bn.into_iter().collect());
                }
                if functype == FType::ClassMethod {
                    return Flow::ok(boundnode.into_iter().collect());
                }
            }
            let mut argindex = argindex;
            if matches!(functype, FType::Method | FType::ClassMethod)
                && ctx.boundnode.borrow().is_some()
            {
                argindex = argindex.saturating_sub(1);
            }
            if argindex < positional_all.len() {
                return self.infer_nv(&positional_all[argindex], ctx);
            }
        }
        if spec.kwarg == Some(name) {
            if site.has_invalid_keywords() {
                return Flow::err(ErrKind::Inference);
            }
            let items: Vec<(Value, Value)> = kwargs
                .into_iter()
                .map(|(k, v)| {
                    (
                        Value::SynthConst(Rc::new(ConstValue::Str(self.sname(k).into()))),
                        match v {
                            NV::N(g) => Value::Node(g),
                            NV::V(val) => val,
                        },
                    )
                })
                .collect();
            return Flow::one(Value::SynthDict {
                items: Rc::new(items),
            });
        }
        if spec.vararg == Some(name) {
            if site.has_invalid_arguments() {
                return Flow::err(ErrKind::Inference);
            }
            let elems: Vec<Value> = vararg
                .into_iter()
                .map(|v| match v {
                    NV::N(g) => Value::Node(g),
                    NV::V(val) => val,
                })
                .collect();
            return Flow::one(Value::SynthSeq {
                kind: SeqKind::Tuple,
                elems: Rc::new(elems),
            });
        }
        match self.default_value(&spec, name) {
            Some(def) => self.infer(def, ctx),
            None => Flow::err(ErrKind::Inference),
        }
    }

    pub fn infer_nv(&self, nv: &NV, ctx: &Rc<Ctx>) -> Flow {
        match nv {
            NV::N(g) => self.infer(*g, ctx),
            NV::V(v) => Flow::one(v.clone()),
        }
    }

    /// _class_type subset for the metaclass check in _arguments_infer
    /// (scoped_nodes.py:1750-1785).
    pub fn class_type(&self, cls: GNode) -> &'static str {
        if self.is_metaclass_class(cls, 0) {
            return "metaclass";
        }
        if self
            .node_name(cls)
            .map(|n| n.ends_with("Exception"))
            .unwrap_or(false)
        {
            return "exception";
        }
        "class"
    }

    fn is_metaclass_class(&self, cls: GNode, depth: u32) -> bool {
        if depth > 50 {
            return false;
        }
        if self.node_name(cls).as_deref() == Some("type")
            && self.md(cls.m).name == "builtins"
        {
            return true;
        }
        for base in self.class_bases(cls) {
            let f = self.infer(base, &Ctx::new());
            for v in &f.vals {
                if let Value::Node(g) = v {
                    if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_)))
                        && self.is_metaclass_class(*g, depth + 1)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}

pub struct CallSite {
    pub unpacked_args: Vec<NV>,
    pub unpacked_kwargs: Vec<(GSym, NV)>,
    pub duplicated_keywords: Vec<GSym>,
}

impl CallSite {
    pub fn positional_arguments(&self) -> Vec<NV> {
        self.unpacked_args
            .iter()
            .filter(|v| !matches!(v, NV::V(Value::Uninferable)))
            .cloned()
            .collect()
    }
    pub fn keyword_arguments(&self) -> Vec<(GSym, NV)> {
        self.unpacked_kwargs
            .iter()
            .filter(|(_, v)| !matches!(v, NV::V(Value::Uninferable)))
            .cloned()
            .collect()
    }
    pub fn has_invalid_arguments(&self) -> bool {
        self.positional_arguments().len() != self.unpacked_args.len()
    }
    pub fn has_invalid_keywords(&self) -> bool {
        self.keyword_arguments().len() != self.unpacked_kwargs.len()
    }
}
