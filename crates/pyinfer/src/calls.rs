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
use crate::infer::Sink;
use crate::yield_v;
use crate::value::{Drive, End, ErrKind, Flow, GNode, GSym, SeqKind, Value, NV};

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

    pub fn infer_call_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let md = self.md(node.m);
        let (func, args, keywords) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, args, keywords } => (
                GNode { m: node.m, n: *func },
                args.iter().map(|&a| GNode { m: node.m, n: a }).collect::<Vec<_>>(),
                keywords.clone(),
            ),
            _ => return End::Raised(ErrKind::Inference),
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
        // callees pulled one at a time (node_classes.py:1764-1782)
        let mut hard_err: Option<ErrKind> = None;
        let end = {
            let hard_err = &mut hard_err;
            let callctx = &callctx;
            let args = &args;
            let kw_pairs = &kw_pairs;
            self.infer_to(func, ctx, &mut |callee| {
                if callee.is_uninferable() {
                    return sink(Value::Uninferable);
                }
                if !self.has_infer_call_result(&callee) {
                    return Drive::Go; // silently skipped
                }
                *callctx.callcontext.borrow_mut() = Some(Rc::new(CallCtx {
                    id: self.next_callctx_id(),
                    args: RefCell::new(args.iter().map(|&a| NV::N(a)).collect()),
                    keywords: RefCell::new(kw_pairs.clone()),
                    callee: RefCell::new(Some(callee.clone())),
                }));
                let mut stopped = false;
                let e = self.infer_call_result_to(&callee, Some(node), Some(callctx), &mut |v| {
                    let d = sink(v);
                    if let Drive::Stop = d {
                        stopped = true;
                    }
                    d
                });
                if stopped {
                    return Drive::Stop;
                }
                match e {
                    End::Raised(err) if err.is_inference() => Drive::Go, // continue
                    End::Raised(err) => {
                        *hard_err = Some(err);
                        Drive::Stop
                    }
                    _ => Drive::Go,
                }
            })
        };
        if let Some(e) = hard_err {
            return End::Raised(e);
        }
        end
    }

    // ---------- infer_call_result dispatch ----------

    /// eager shim
    pub fn infer_call_result(
        &self,
        callee: &Value,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_call_result_to(callee, caller, ctx, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    pub fn infer_call_result_to(
        &self,
        callee: &Value,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        if self.depth.get() >= self.max_depth {
            return End::Raised(ErrKind::Recursion);
        }
        self.depth.set(self.depth.get() + 1);
        let r = self.infer_call_result_inner(callee, caller, ctx, sink);
        self.depth.set(self.depth.get() - 1);
        r
    }

    fn infer_call_result_inner(
        &self,
        callee: &Value,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        match callee {
            Value::Node(g) => {
                let mut lambda_body: Option<GNode> = None;
                let kind_tag = {
                    let md = self.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => 1,
                        NodeKind::Lambda(d) => {
                            lambda_body = Some(GNode { m: g.m, n: d.body });
                            4
                        }
                        NodeKind::ClassDef(_) => 2,
                        // Const / containers -> BaseInstance
                        NodeKind::Const(_)
                        | NodeKind::List { .. }
                        | NodeKind::Tuple { .. }
                        | NodeKind::Set { .. }
                        | NodeKind::Dict { .. } => 3,
                        _ => 0,
                    }
                };
                match kind_tag {
                    1 => self.function_infer_call_result_to(*g, caller, ctx, sink),
                    2 => self.class_infer_call_result_to(*g, caller, ctx, sink),
                    3 => self.base_instance_infer_call_result_to(callee, caller, ctx, sink),
                    4 => {
                        // Lambda.infer_call_result: return self.body.infer
                        let c = match ctx {
                            Some(c) => Rc::clone(c),
                            None => Ctx::new(),
                        };
                        self.infer_to(lambda_body.unwrap(), &c, sink)
                    }
                    _ => End::Raised(ErrKind::Inference),
                }
            }
            Value::BoundMethod { func, bound } => {
                self.bound_method_infer_call_result_to(*func, bound, caller, ctx, sink)
            }
            Value::UnboundMethod { func } => {
                self.unbound_method_infer_call_result_with_to(*func, caller, ctx, sink)
            }
            Value::Property { .. } => End::Raised(ErrKind::Inference),
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
                self.function_infer_call_result_to(*func, caller, ctx, sink)
            }
            Value::Inst { .. }
            | Value::ExcInst { .. }
            | Value::SynthConst(_)
            | Value::SynthSeq { .. }
            | Value::SynthDict { .. }
            | Value::FrozenSet { .. }
            | Value::Generator { .. } => {
                self.base_instance_infer_call_result_to(callee, caller, ctx, sink)
            }
            _ => End::Raised(ErrKind::Inference),
        }
    }

    // ---------- FunctionDef.infer_call_result (scoped_nodes.py:1555-1636) ----------

    /// eager shim
    pub fn function_infer_call_result(
        &self,
        func: GNode,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Flow {
        let mut vals = Vec::new();
        let end = self.function_infer_call_result_to(func, caller, ctx, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    pub fn function_infer_call_result_to(
        &self,
        func: GNode,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
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
            // bases.Generator.__init__ captures copy_context(context) as
            // _call_context (bases.py:698); a fresh object per call
            yield_v!(
                sink,
                Value::Generator {
                    func,
                    is_async,
                    call_ctx: copy_context(Some(&ctx)),
                }
            );
            return End::Done;
        }
        // NOTE: the six `with_metaclass` hack (scoped_nodes.py:1577-1615)
        // is not ported yet; none of the pinned corpora rely on it for the
        // -E message set (revisit on diff evidence).
        let body: Vec<NodeId> = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
            _ => return End::Raised(ErrKind::Inference),
        };
        drop(md);
        let returns = self.return_nodes_skip_functions(func);
        if returns.is_empty() {
            if !body.is_empty() {
                if self.is_abstract(func, true, true) {
                    yield_v!(sink, Value::Uninferable);
                } else {
                    yield_v!(sink, Value::SynthConst(Rc::new(ConstValue::None)));
                }
                return End::Done;
            }
            return End::Raised(ErrKind::Inference);
        }
        for ret in returns {
            let value = {
                let rmd = self.md(ret.m);
                match &rmd.tree.nodes[ret.n.idx()].kind {
                    NodeKind::Return { value } => *value,
                    _ => None,
                }
            };
            match value {
                None => yield_v!(sink, Value::SynthConst(Rc::new(ConstValue::None))),
                Some(v) => {
                    // `yield from returnnode.value.infer(context)` with
                    // `except InferenceError: yield Uninferable` per return
                    let mut stopped = false;
                    let e = self.infer_to(GNode { m: ret.m, n: v }, &ctx, &mut |val| {
                        let d = sink(val);
                        if let Drive::Stop = d {
                            stopped = true;
                        }
                        d
                    });
                    if stopped {
                        return End::Stopped;
                    }
                    match e {
                        End::Raised(err) if err.is_inference() => {
                            yield_v!(sink, Value::Uninferable);
                        }
                        End::Raised(err) => return End::Raised(err),
                        _ => {}
                    }
                }
            }
        }
        End::Done
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

    fn class_infer_call_result_to(
        &self,
        cls: GNode,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        // type("X", bases, attrs) (scoped_nodes.py:2076-2079).
        // ORDER MATTERS: is_subtype_of(context) runs FIRST (its ancestors()
        // walk infers base expressions under the SHARED context — counter
        // bumps happen even when the call is not 3-arg).
        if self.is_subtype_of(cls, "builtins.type", ctx) {
            if let Some(call) = caller {
                let n_args = match &self.md(call.m).tree.nodes[call.n.idx()].kind {
                    NodeKind::Call { args, .. } => args.len(),
                    _ => 0,
                };
                if n_args == 3 {
                    return match self.infer_type_call(call, ctx) {
                        Ok(v) => {
                            yield_v!(sink, v);
                            End::Done
                        }
                        Err(e) => End::Raised(e),
                    };
                }
            }
        }
        let mut dunder_call: Option<Value> = None;
        if let Some(Value::Node(meta)) = self.metaclass(cls, ctx) {
            let call_sym = self.sym("__call__");
            if !self.class_locals_get(meta, call_sym).is_empty() {
                // next(metaclass.igetattr("__call__", context)) — one pull
                if let Ok(f) = self.class_igetattr_first(meta, call_sym, ctx, true) {
                    dunder_call = f;
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
                return self.infer_call_result_to(&dc, caller, Some(&ctx2), sink);
            }
        }
        yield_v!(sink, self.instantiate_class(cls));
        End::Done
    }

    // ---------- _infer_type_call / _infer_type_new_call ----------

    /// first inferred value of an argument node, astroid `next(x.infer(ctx))`
    /// (StopIteration -> InferenceError). Single pull: the generator is
    /// abandoned afterwards (no cache write, no bump for the value).
    pub fn infer_first(&self, node: GNode, ctx: Option<&Rc<Ctx>>) -> Result<Value, ErrKind> {
        let c = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        let mut first: Option<Value> = None;
        let end = {
            let first = &mut first;
            self.infer_to(node, &c, &mut |v| {
                *first = Some(v);
                Drive::Stop
            })
        };
        match first {
            Some(v) => Ok(v),
            None => Err(end.err_opt().unwrap_or(ErrKind::Inference)),
        }
    }

    /// `next(x.infer(), None)` — single pull, fresh context. Ok(None) on
    /// StopIteration; raised errors propagate (Err).
    pub fn infer_first_fresh(&self, node: GNode) -> Result<Option<Value>, ErrKind> {
        let c = Ctx::new();
        let mut first: Option<Value> = None;
        let end = {
            let first = &mut first;
            self.infer_to(node, &c, &mut |v| {
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

    /// container elements as NVs (`itered()` over Tuple/List nodes or the
    /// synthetic sequences our brains produce).
    fn container_elts(&self, v: &Value) -> Option<Vec<NV>> {
        match v {
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => Some(
                        elts.iter().map(|&e| NV::N(GNode { m: g.m, n: e })).collect(),
                    ),
                    _ => None,
                }
            }
            Value::SynthSeq { kind, elems } if !matches!(kind, SeqKind::Set) => {
                Some(elems.iter().cloned().map(NV::V).collect())
            }
            _ => None,
        }
    }

    /// scoped_nodes.py:2017-2069 _infer_type_call — reconstruct the class a
    /// 3-arg `type()` call (or metaclass call) creates.
    fn infer_type_call(&self, caller: GNode, ctx: Option<&Rc<Ctx>>) -> Result<Value, ErrKind> {
        let md = self.md(caller.m);
        let args: Vec<GNode> = match &md.tree.nodes[caller.n.idx()].kind {
            NodeKind::Call { args, .. } => {
                args.iter().map(|&a| GNode { m: caller.m, n: a }).collect()
            }
            _ => return Err(ErrKind::Inference),
        };
        // name: first inferred value of args[0]; non-Const-str -> Uninferable
        let name_v = self.infer_first(args[0], ctx)?;
        let name = match self.value_const(&name_v) {
            Some(ConstValue::Str(s)) => s.to_string(),
            _ => return Ok(Value::Uninferable),
        };
        // bases: first inferred value of args[1]; must be Tuple/List
        let bases_v = self.infer_first(args[1], ctx)?;
        let Some(base_elts) = self.container_elts(&bases_v) else {
            return Ok(Value::Uninferable);
        };
        // each base becomes EvaluatedObject(original, first-inferred value);
        // skipped when inference yields nothing or a falsy value
        // (`if inferred:` — Uninferable is falsy, nodes are objects).
        let mut eval_bases: Vec<Value> = Vec::new();
        for elt in &base_elts {
            let first = match elt {
                NV::N(g) => {
                    let c = match ctx {
                        Some(c) => Rc::clone(c),
                        None => Ctx::new(),
                    };
                    self.infer(*g, &c).vals.into_iter().next()
                }
                NV::V(v) => Some(v.clone()),
            };
            if let Some(v) = first {
                if !v.is_uninferable() {
                    eval_bases.push(v);
                }
            }
        }
        // members: first inferred value of args[2]; errors -> None
        let members = self.infer_first(args[2], ctx).ok();
        let mut locals: Vec<(GSym, NV)> = Vec::new();
        if let Some(m) = &members {
            if let Some(items) = self.value_dict_items_nv(m) {
                for (k, v) in items {
                    let kc = match &k {
                        NV::N(g) => self.value_const(&Value::Node(*g)),
                        NV::V(v) => self.value_const(v),
                    };
                    if let Some(ConstValue::Str(s)) = kc {
                        locals.push((self.sym(&s), v));
                    }
                }
            }
        }
        // parent=caller.parent — qname prefix is its frame's qname
        // (ClassDef created with lineno=0)
        let parent_frame = match self.parent(caller) {
            Some(p) => self.frame(p),
            None => self.frame(caller),
        };
        let modname = self.qname(parent_frame);
        let n_extra = locals.iter().filter(|(_, v)| matches!(v, NV::V(_))).count();
        let (cls, base_slots, _, extra_slots) =
            self.build_synth_class(&modname, &name, 0, 0, eval_bases.len(), false, n_extra);
        {
            let mut red = self.redirects.borrow_mut();
            for (slot, v) in base_slots.iter().zip(eval_bases) {
                red.insert(GNode { m: cls.m, n: *slot }, NV::V(v));
            }
        }
        // locals: dict-assignment REPLACE semantics per name
        let cmd = self.md(cls.m);
        let mut lmap = cmd.locals.borrow_mut();
        let entry = lmap.entry(cls.n).or_default();
        let mut extra_iter = extra_slots.into_iter();
        for (sym, v) in locals {
            let g = match v {
                NV::N(g) => g,
                NV::V(val) => {
                    let slot = extra_iter.next().expect("extra slot");
                    let g = GNode { m: cls.m, n: slot };
                    self.redirects.borrow_mut().insert(g, NV::V(val));
                    g
                }
            };
            entry.insert(sym, vec![g]);
        }
        Ok(Value::Node(cls))
    }

    /// Dict items as NV pairs (Dict nodes or SynthDict values).
    fn value_dict_items_nv(&self, v: &Value) -> Option<Vec<(NV, NV)>> {
        match v {
            Value::SynthDict { items } => Some(
                items
                    .iter()
                    .map(|(k, val)| (NV::V(k.clone()), NV::V(val.clone())))
                    .collect(),
            ),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => Some(
                        items
                            .iter()
                            .map(|&(k, val)| {
                                (
                                    NV::N(GNode { m: g.m, n: k }),
                                    NV::N(GNode { m: g.m, n: val }),
                                )
                            })
                            .collect(),
                    ),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// bases.py:555-654 _infer_type_new_call — type.__new__(mcs, name,
    /// bases, attrs). Returns Ok(None) when any validation falls through
    /// (-> normal call inference).
    fn infer_type_new_call(
        &self,
        caller: GNode,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Option<Value>, ErrKind> {
        let md = self.md(caller.m);
        let args: Vec<GNode> = match &md.tree.nodes[caller.n.idx()].kind {
            NodeKind::Call { args, .. } => {
                args.iter().map(|&a| GNode { m: caller.m, n: a }).collect()
            }
            _ => return Ok(None),
        };
        // mcs: ClassDef subtype of builtins.type
        let mcs_v = self.infer_first(args[0], ctx)?;
        let mcs = match &mcs_v {
            Value::Node(g) if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) => *g,
            _ => return Ok(None),
        };
        if !self.is_subtype_of(mcs, "builtins.type", None) {
            return Ok(None);
        }
        // name: Const str
        let name_v = self.infer_first(args[1], ctx)?;
        let name = match self.value_const(&name_v) {
            Some(ConstValue::Str(s)) => s.to_string(),
            _ => return Ok(None),
        };
        // bases: Tuple of ClassDefs (raw elts kept as the class bases)
        let bases_v = self.infer_first(args[2], ctx)?;
        let base_elts: Vec<NV> = match &bases_v {
            Value::Node(g)
                if self.kind_is(*g, |k| matches!(k, NodeKind::Tuple { .. })) =>
            {
                let bmd = self.md(g.m);
                match &bmd.tree.nodes[g.n.idx()].kind {
                    NodeKind::Tuple { elts, .. } => {
                        elts.iter().map(|&e| NV::N(GNode { m: g.m, n: e })).collect()
                    }
                    _ => return Ok(None),
                }
            }
            Value::SynthSeq {
                kind: SeqKind::Tuple,
                elems,
            } => elems.iter().cloned().map(NV::V).collect(),
            _ => return Ok(None),
        };
        for elt in &base_elts {
            let first = match elt {
                NV::N(g) => Some(self.infer_first(*g, ctx)?),
                NV::V(v) => Some(v.clone()),
            };
            match first {
                Some(Value::Node(g))
                    if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) => {}
                _ => return Ok(None),
            }
        }
        // attrs: Dict; keys/values fully inferred (defaultdict(list) APPEND)
        let attrs_v = self.infer_first(args[3], ctx)?;
        let Some(items) = self.value_dict_items_nv(&attrs_v) else {
            return Ok(None);
        };
        let mut locals: Vec<(GSym, Value)> = Vec::new();
        for (k, v) in items {
            let kv = match &k {
                NV::N(g) => self.infer_first(*g, ctx)?,
                NV::V(val) => val.clone(),
            };
            let vv = match &v {
                NV::N(g) => self.infer_first(*g, ctx)?,
                NV::V(val) => val.clone(),
            };
            if let Some(ConstValue::Str(s)) = self.value_const(&kv) {
                locals.push((self.sym(&s), vv));
            }
        }
        // build: parent=caller, lineno=caller.lineno, metaclass=mcs
        let modname = self.qname(self.frame(caller));
        let lineno = self.fromlineno(caller);
        let col = self.md(caller.m).tree.nodes[caller.n.idx()].col_offset;
        let (cls, base_slots, meta_slot, extra_slots) =
            self.build_synth_class(&modname, &name, lineno, col, base_elts.len(), true, locals.len());
        {
            let mut red = self.redirects.borrow_mut();
            for (slot, v) in base_slots.iter().zip(base_elts) {
                red.insert(GNode { m: cls.m, n: *slot }, v);
            }
            if let Some(ms) = meta_slot {
                red.insert(GNode { m: cls.m, n: ms }, NV::V(Value::Node(mcs)));
            }
        }
        let cmd = self.md(cls.m);
        let mut lmap = cmd.locals.borrow_mut();
        let entry = lmap.entry(cls.n).or_default();
        let mut extra_iter = extra_slots.into_iter();
        for (sym, val) in locals {
            let slot = extra_iter.next().expect("extra slot");
            let g = GNode { m: cls.m, n: slot };
            self.redirects.borrow_mut().insert(g, NV::V(val));
            entry.entry(sym).or_default().push(g);
        }
        Ok(Some(Value::Node(cls)))
    }

    // ---------- Bound/Unbound method ----------

    fn bound_method_infer_call_result_to(
        &self,
        func: GNode,
        bound: &Rc<Value>,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        // DictMethodBoundMethod (objectmodel.py:840-852): a dict-model BM
        // (bound to a Dict literal/synth, func = builtins.dict.items/keys/
        // values) yields the DictItems/Keys/Values object directly
        {
            let dict_like = matches!(&**bound, Value::SynthDict { .. })
                || matches!(&**bound, Value::Node(g)
                    if self.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })));
            if dict_like {
                let q = self.qname(func);
                let dr = || match &**bound {
                    Value::SynthDict { items } => crate::value::DictRef::Synth(Rc::clone(items)),
                    Value::Node(g) => crate::value::DictRef::Node(*g),
                    _ => unreachable!(),
                };
                match q.as_str() {
                    "builtins.dict.items" => {
                        yield_v!(sink, Value::DictItems(Rc::new(dr())));
                        return End::Done;
                    }
                    "builtins.dict.keys" => {
                        yield_v!(sink, Value::DictKeys(Rc::new(dr())));
                        return End::Done;
                    }
                    "builtins.dict.values" => {
                        yield_v!(sink, Value::DictValues(Rc::new(dr())));
                        return End::Done;
                    }
                    _ => {}
                }
            }
        }
        let ctx2 = bind_context_to_node(ctx, (**bound).clone());
        // type.__new__(mcs, name, bases, attrs) (bases.py:656-674)
        if let Some(call) = caller {
            let is_type_new = matches!(&**bound, Value::Node(b)
                    if self.kind_is(*b, |k| matches!(k, NodeKind::ClassDef(_)))
                        && self.node_name(*b).as_deref() == Some("type"))
                && self.node_name(func).as_deref() == Some("__new__")
                && matches!(
                    &self.md(call.m).tree.nodes[call.n.idx()].kind,
                    NodeKind::Call { args, .. } if args.len() == 4
                );
            if is_type_new {
                match self.infer_type_new_call(call, Some(&ctx2)) {
                    Ok(Some(v)) => {
                        yield_v!(sink, v);
                        return End::Done;
                    }
                    Ok(None) => {}
                    Err(e) => return End::Raised(e),
                }
            }
        }
        self.unbound_method_infer_call_result_with_to(func, caller, Some(&ctx2), sink)
    }

    fn unbound_method_infer_call_result_with_to(
        &self,
        func: GNode,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        let name = self.node_name(func);
        if name.as_deref() == Some("__new__") {
            let frame = self.parent(func).map(|p| self.frame(p));
            if let Some(frame) = frame {
                let q = self.qname(frame);
                if q.starts_with("builtins.") && q != "builtins.type" {
                    return self.infer_builtin_new_to(caller, ctx, sink);
                }
            }
        }
        self.function_infer_call_result_to(func, caller, ctx, sink)
    }

    /// bases.py:497-530 _infer_builtin_new — note the unconditional
    /// `raise InferenceError` after handling the FIRST inferred value of
    /// args[0] (the generator is abandoned: no cache write for args[0]).
    fn infer_builtin_new_to(
        &self,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        let Some(caller) = caller else {
            return End::Done;
        };
        let args: Vec<NodeId> = {
            let md = self.md(caller.m);
            match &md.tree.nodes[caller.n.idx()].kind {
                NodeKind::Call { args, .. } => args.clone(),
                _ => return End::Done,
            }
        };
        if args.is_empty() {
            return End::Done;
        }
        let ctx = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        if args.len() > 1 {
            let a1 = GNode { m: caller.m, n: args[1] };
            let is_const = {
                let md = self.md(caller.m);
                matches!(&md.tree.nodes[args[1].idx()].kind, NodeKind::Const(_))
            };
            let cv = if is_const {
                let md = self.md(caller.m);
                match &md.tree.nodes[args[1].idx()].kind {
                    NodeKind::Const(c) => Some(c.clone()),
                    _ => None,
                }
            } else {
                // next(caller.args[1].infer(), None) — NO context, one pull;
                // raised errors propagate out of _infer_builtin_new
                match self.infer_first_fresh(a1) {
                    Ok(v) => v.and_then(|v| self.value_const(&v)),
                    Err(e) => return End::Raised(e),
                }
            };
            if let Some(cv) = cv {
                if !matches!(cv, ConstValue::None) {
                    yield_v!(sink, Value::SynthConst(Rc::new(cv)));
                    return End::Done;
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
        // first value only, then unconditional raise (loop abandons a0's
        // generator on the raise)
        let mut to_yield: Vec<Value> = Vec::new();
        let mut got_one = false;
        let end = {
            let to_yield = &mut to_yield;
            let got_one = &mut got_one;
            self.infer_to(a0, &node_ctx, &mut |first| {
                *got_one = true;
                if first.is_uninferable() {
                    to_yield.push(Value::Uninferable);
                }
                if let Value::Node(g) = &first {
                    if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) {
                        to_yield.push(Value::Inst { cls: *g, id: crate::value::fresh_inst_id() });
                    }
                }
                Drive::Stop // raise InferenceError abandons the generator
            })
        };
        if got_one {
            for v in to_yield {
                yield_v!(sink, v);
            }
            return End::Raised(ErrKind::Inference);
        }
        match end {
            End::Raised(e) => End::Raised(e), // error from a0's generator propagates
            _ => End::Done,
        }
    }

    // ---------- BaseInstance.infer_call_result (bases.py:317-345) ----------

    fn base_instance_infer_call_result_to(
        &self,
        instance: &Value,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
        sink: &mut Sink,
    ) -> End {
        let ctx2 = bind_context_to_node(ctx, instance.clone());
        let mut inferred = false;
        // attribute-call shortcut: infer the attribute itself
        if let Some(caller) = caller {
            let sym = {
                let md = self.md(caller.m);
                if let NodeKind::Call { func, .. } = &md.tree.nodes[caller.n.idx()].kind {
                    if let NodeKind::Attribute { attrname, .. } = &md.tree.nodes[func.idx()].kind {
                        Some(self.g(&md, *attrname))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(sym) = sym {
                let mut stopped = false;
                let end = self.igetattr_value_to(instance, sym, Some(&ctx2), &mut |v| {
                    inferred = true;
                    let d = sink(v);
                    if let Drive::Stop = d {
                        stopped = true;
                    }
                    d
                });
                if stopped {
                    return End::Stopped;
                }
                if let End::Raised(e) = end {
                    // bases.py:327-330: the shortcut's igetattr is NOT
                    // wrapped -- a missing attribute on the instance
                    // aborts the whole call inference (InferenceError;
                    // Instance.igetattr converts AttributeInferenceError)
                    return End::Raised(if e == ErrKind::Attribute {
                        ErrKind::Inference
                    } else {
                        e
                    });
                }
            }
        }
        let proxied = self.proxied_class(instance);
        if let Some(cls) = proxied {
            let call_sym = self.sym("__call__");
            let mut stopped = false;
            let mut hard_err: Option<ErrKind> = None;
            let _ = {
                let inferred = &mut inferred;
                let stopped = &mut stopped;
                let hard_err = &mut hard_err;
                let ctx2 = &ctx2;
                self.class_igetattr_to(cls, call_sym, Some(ctx2), true, &mut |node| {
                    if node.is_uninferable() || !self.value_callable(&node, ctx2) {
                        return Drive::Go;
                    }
                    // recursion prevention: instance of same class
                    let same = self.proxied_class(&node) == proxied
                        && matches!(node, Value::Inst { .. });
                    if same {
                        *inferred = true;
                        let d = sink(node);
                        if let Drive::Stop = d {
                            *stopped = true;
                        }
                        return d;
                    }
                    let e = self.infer_call_result_to(&node, caller, Some(ctx2), &mut |v| {
                        *inferred = true;
                        let d = sink(v);
                        if let Drive::Stop = d {
                            *stopped = true;
                        }
                        d
                    });
                    if *stopped {
                        return Drive::Stop;
                    }
                    match e {
                        End::Raised(err) => {
                            // no try/except here in astroid — errors from a
                            // dunder's infer_call_result propagate
                            *hard_err = Some(err);
                            Drive::Stop
                        }
                        _ => Drive::Go,
                    }
                })
            };
            if stopped {
                return End::Stopped;
            }
            if let Some(e) = hard_err {
                return End::Raised(e);
            }
        }
        if !inferred {
            return End::Raised(ErrKind::Inference);
        }
        End::Done
    }

    // ---------- Arguments._infer + protocols (§10) ----------

    pub fn infer_arguments_node_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return End::Raised(ErrKind::Inference),
        };
        self.arguments_infer_argname_to(node, Some(name), ctx, sink)
    }

    /// eager shim
    pub fn arguments_infer_argname(
        &self,
        arguments_node: GNode,
        name: Option<GSym>,
        ctx: &Rc<Ctx>,
    ) -> Flow {
        let mut vals = Vec::new();
        let end = self.arguments_infer_argname_to(arguments_node, name, ctx, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    /// protocols.py:352-413 _arguments_infer_argname
    pub fn arguments_infer_argname_to(
        &self,
        arguments_node: GNode,
        name: Option<GSym>,
        ctx: &Rc<Ctx>,
        sink: &mut Sink,
    ) -> End {
        let Some(spec) = self.arg_spec_of_arguments(arguments_node) else {
            return End::Raised(ErrKind::Inference);
        };
        let all_args = spec.arguments();
        if all_args.is_empty() {
            yield_v!(sink, Value::Uninferable);
            return End::Done;
        }
        let func = match self.parent(arguments_node) {
            Some(f) => f,
            None => {
                yield_v!(sink, Value::Uninferable);
                return End::Done;
            }
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
                if let Value::Inst { cls, .. } | Value::ExcInst { cls, .. } = bn {
                    cls_value = Some(Value::Node(*cls));
                }
            }
            if is_metaclass || functype == FType::ClassMethod {
                for v in cls_value {
                    yield_v!(sink, v);
                }
                return End::Done;
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
                for v in inst {
                    yield_v!(sink, v);
                }
                return End::Done;
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
                // protocols.py:387-389: CallSite(context.callcontext,
                // context.extra_context) — context=None: the unpack
                // safe_infers run under FRESH contexts (cc/bn None, own
                // counters), with the caller's extra_context as the
                // argument_context_map; infer_argument then gets the LIVE
                // context (callcontext still set)
                let fresh = Ctx::new();
                let map = ctx.extra_context.borrow().clone();
                let site = self.call_site_from_map(&cc, &fresh, map);
                return match name {
                    Some(n) => self.infer_argument_to(&site, func, n, ctx, sink),
                    None => End::Raised(ErrKind::Inference),
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
            yield_v!(sink, Value::SynthSeq {
                kind: SeqKind::Tuple,
                elems: Rc::new(elems),
            });
            return End::Done;
        }
        if name.is_some() && name == spec.kwarg {
            yield_v!(sink, Value::SynthDict {
                items: Rc::new(Vec::new()),
            });
            return End::Done;
        }
        let _ = name_str;
        // default value + Uninferable (protocols.py:404-413):
        // `yield from default.infer(context); yield Uninferable` — errors
        // from the default's inference propagate (only NoDefault is caught).
        match name.and_then(|n| self.default_value(&spec, n)) {
            Some(def) => {
                let c = copy_context(Some(ctx));
                match self.infer_to(def, &c, sink) {
                    End::Done => {
                        yield_v!(sink, Value::Uninferable);
                        End::Done
                    }
                    e => e,
                }
            }
            None => {
                yield_v!(sink, Value::Uninferable);
                End::Done
            }
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
        self.call_site_from_map(cc, ctx, Rc::new(rustc_hash::FxHashMap::default()))
    }

    /// CallSite(callcontext, argument_context_map, context): _unpack_args/
    /// _unpack_keywords MUTATE the passed context: `context.extra_context =
    /// self.argument_context_map` (arguments.py:95/:135). With the default
    /// empty map (arguments_assigned_stmts path) the populated Call._infer
    /// map is CLOBBERED; _arguments_infer_argname instead passes context=
    /// None (FRESH unpack contexts) + the caller's extra_context as the map.
    pub fn call_site_from_map(
        &self,
        cc: &CallCtx,
        ctx: &Rc<Ctx>,
        map: Rc<rustc_hash::FxHashMap<GNode, Rc<Ctx>>>,
    ) -> CallSite {
        *ctx.extra_context.borrow_mut() = map;
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

    /// eager shim
    pub fn infer_argument(
        &self,
        site: &CallSite,
        funcnode: GNode,
        name: GSym,
        ctx: &Rc<Ctx>,
    ) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_argument_to(site, funcnode, name, ctx, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    /// CallSite.infer_argument (arguments.py:141-309)
    pub fn infer_argument_to(
        &self,
        site: &CallSite,
        funcnode: GNode,
        name: GSym,
        ctx: &Rc<Ctx>,
        sink: &mut Sink,
    ) -> End {
        let is_func = self.kind_is(funcnode, |k| {
            matches!(
                k,
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::Lambda(_)
            )
        });
        if !is_func {
            return End::Raised(ErrKind::Inference);
        }
        if site.duplicated_keywords.contains(&name) {
            return End::Raised(ErrKind::Inference);
        }
        // keywords first
        if let Some((_, v)) = site
            .keyword_arguments()
            .into_iter()
            .find(|(k, _)| *k == name)
        {
            return self.infer_nv_to(&v, ctx, sink);
        }
        let Some(spec) = self.arg_spec(funcnode) else {
            return End::Raised(ErrKind::Inference);
        };
        if spec.args_unknown {
            return End::Raised(ErrKind::Inference);
        }
        let positional_all = site.positional_arguments();
        if positional_all.len() > spec.args.len()
            && spec.vararg.is_none()
            && spec.posonlyargs.is_empty()
        {
            return End::Raised(ErrKind::Inference);
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
                    return self.infer_nv_to(&positional[0], ctx, sink);
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
                                for v in boundnode {
                                    yield_v!(sink, v);
                                }
                                return End::Done;
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
                    for v in bn {
                        yield_v!(sink, v);
                    }
                    return End::Done;
                }
                if functype == FType::ClassMethod {
                    for v in boundnode {
                        yield_v!(sink, v);
                    }
                    return End::Done;
                }
            }
            let mut argindex = argindex;
            if matches!(functype, FType::Method | FType::ClassMethod)
                && ctx.boundnode.borrow().is_some()
            {
                argindex = argindex.saturating_sub(1);
            }
            if argindex < positional_all.len() {
                return self.infer_nv_to(&positional_all[argindex], ctx, sink);
            }
        }
        if spec.kwarg == Some(name) {
            if site.has_invalid_keywords() {
                return End::Raised(ErrKind::Inference);
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
            yield_v!(sink, Value::SynthDict {
                items: Rc::new(items),
            });
            return End::Done;
        }
        if spec.vararg == Some(name) {
            if site.has_invalid_arguments() {
                return End::Raised(ErrKind::Inference);
            }
            let elems: Vec<Value> = vararg
                .into_iter()
                .map(|v| match v {
                    NV::N(g) => Value::Node(g),
                    NV::V(val) => val,
                })
                .collect();
            yield_v!(sink, Value::SynthSeq {
                kind: SeqKind::Tuple,
                elems: Rc::new(elems),
            });
            return End::Done;
        }
        match self.default_value(&spec, name) {
            Some(def) => self.infer_to(def, ctx, sink),
            None => End::Raised(ErrKind::Inference),
        }
    }

    pub fn infer_nv(&self, nv: &NV, ctx: &Rc<Ctx>) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_nv_to(nv, ctx, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    /// arg.infer(context) where the arg may be an already-inferred value
    /// (operator-protocol call contexts): values infer to themselves.
    pub fn infer_nv_to(&self, nv: &NV, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        match nv {
            NV::N(g) => self.infer_to(*g, ctx, sink),
            NV::V(v) => {
                yield_v!(sink, v.clone());
                End::Done
            }
        }
    }

    /// _class_type subset for the metaclass check in _arguments_infer
    /// (scoped_nodes.py:1750-1785).
    /// ClassDef.type — _class_type (scoped_nodes.py:1750-1785), memoized
    /// on the class for the whole run.
    pub fn class_type(&self, cls: GNode) -> &'static str {
        self.class_type_inner(cls, &mut Default::default())
    }

    fn class_type_inner(
        &self,
        cls: GNode,
        ancestors: &mut rustc_hash::FxHashSet<String>,
    ) -> &'static str {
        if let Some(&t) = self.cls_type_cache.borrow().get(&cls) {
            return t;
        }
        let mut ty: Option<&'static str> = None;
        if self.is_metaclass_class(cls, &mut Default::default()) {
            ty = Some("metaclass");
        } else if self
            .node_name(cls)
            .map(|n| n.ends_with("Exception"))
            .unwrap_or(false)
        {
            ty = Some("exception");
        } else {
            let qn = self.qname(cls);
            if ancestors.contains(&qn) {
                // ancestor loop -> "class" (memoized)
                self.cls_type_cache.borrow_mut().insert(cls, "class");
                return "class";
            }
            ancestors.insert(qn);
            // for base in klass.ancestors(recurs=False): break abandons
            let mut found: Option<&'static str> = None;
            let _ = self.ancestors_to(cls, false, None, &mut |base| {
                let name = self.class_type_inner(base, ancestors);
                if name != "class" {
                    if name == "metaclass" {
                        // don't propagate metaclass to non-metaclasses
                        return crate::value::Drive::Go;
                    }
                    found = Some(name);
                    return crate::value::Drive::Stop;
                }
                crate::value::Drive::Go
            });
            ty = found;
        }
        let t = ty.unwrap_or("class");
        self.cls_type_cache.borrow_mut().insert(cls, t);
        t
    }

    /// _is_metaclass (scoped_nodes.py:1714-1747): infers base expressions
    /// (fresh contexts), seen-set keyed by qname, abandons base generators
    /// on early returns.
    fn is_metaclass_class(
        &self,
        cls: GNode,
        seen: &mut rustc_hash::FxHashSet<String>,
    ) -> bool {
        if self.node_name(cls).as_deref() == Some("type") {
            return true;
        }
        for base in self.class_bases(cls) {
            let mut verdict: Option<bool> = None;
            let _ = {
                let verdict = &mut verdict;
                let seen2: *mut rustc_hash::FxHashSet<String> = seen;
                self.infer_to(base, &Ctx::new(), &mut |baseobj| {
                    // SAFETY: sequential single-threaded access
                    let seen = unsafe { &mut *seen2 };
                    let qn = match self.value_qname(&baseobj) {
                        Some(q) => q,
                        None => match &baseobj {
                            Value::Uninferable => "Uninferable".to_string(),
                            _ => return crate::value::Drive::Go,
                        },
                    };
                    if seen.contains(&qn) {
                        return crate::value::Drive::Go;
                    }
                    seen.insert(qn);
                    // isinstance(baseobj, bases.Instance) — Const and the
                    // container literals are Instance subclasses too
                    let is_instance = matches!(
                        baseobj,
                        Value::Inst { .. }
                            | Value::ExcInst { .. }
                            | Value::SynthConst(_)
                            | Value::SynthSeq { .. }
                            | Value::SynthDict { .. }
                            | Value::FrozenSet { .. }
                            | Value::Generator { .. }
                    ) || matches!(&baseobj, Value::Node(g)
                        if self.kind_is(*g, |k| matches!(k,
                            NodeKind::Const(_) | NodeKind::List { .. } | NodeKind::Tuple { .. }
                                | NodeKind::Set { .. } | NodeKind::Dict { .. })));
                    if is_instance {
                        *verdict = Some(false);
                        return crate::value::Drive::Stop;
                    }
                    let g = match &baseobj {
                        Value::Node(g)
                            if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) =>
                        {
                            *g
                        }
                        _ => return crate::value::Drive::Go,
                    };
                    if g == cls {
                        return crate::value::Drive::Go;
                    }
                    if self.cls_type_cache.borrow().get(&g) == Some(&"metaclass")
                        || self.is_metaclass_class(g, seen)
                    {
                        *verdict = Some(true);
                        return crate::value::Drive::Stop;
                    }
                    crate::value::Drive::Go
                })
            };
            match verdict {
                Some(v) => return v,
                None => continue, // InferenceError -> continue too
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
