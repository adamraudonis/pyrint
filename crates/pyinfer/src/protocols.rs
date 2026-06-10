//! assigned_stmts protocol (astroid/protocols.py), Subscript inference +
//! getitem implementations (notes/07 §15), operator protocols (§14).

use std::cell::RefCell;
use std::rc::Rc;

use pyast::tree::{ConstValue, Ctx as ExprCtx, IntValue, NodeKind};
use pyast::NodeId;

use crate::ctx::{bind_context_to_node, copy_context, CallCtx, Ctx};
use crate::graph::Engine;
use crate::infer::Sink;
use crate::yield_v;
use crate::value::{Drive, End, ErrKind, Flow, GNode, GSym, SeqKind, Value, NV};

/// assigned_stmts results are re-run through _infer_stmts by
/// infer_assign (protocols.py + node_classes.py): NODE results get a full
/// stmt.infer() hop (counter bump + cache write) while proxy values
/// (Instance/BoundMethod/...) pass through via Proxy.infer (yield self).
fn nvify(v: Value) -> NV {
    match v {
        Value::Node(g) => NV::N(g),
        other => NV::V(other),
    }
}

impl Engine {
    // ================= assigned_stmts =================

    /// node.assigned_stmts(context, assign_path) — `node` is the TARGET
    /// (AssignName/AssignAttr) or a container/Starred in store context.
    pub fn assigned_stmts(
        &self,
        node: GNode,
        ctx: Option<&Rc<Ctx>>,
        path: Option<Vec<usize>>,
    ) -> Result<Vec<NV>, ErrKind> {
        let md = self.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::AssignName { .. } | NodeKind::AssignAttr { .. } => {
                let parent = self.parent(node).ok_or(ErrKind::Inference)?;
                self.parent_assigned(parent, node, ctx, path)
            }
            // containers / Starred delegate from their own parent
            _ => {
                let parent = self.parent(node).ok_or(ErrKind::Inference)?;
                self.parent_assigned(parent, node, ctx, path)
            }
        }
    }

    /// dispatch on the assignment-parent kind
    pub fn parent_assigned(
        &self,
        parent: GNode,
        child: GNode,
        ctx: Option<&Rc<Ctx>>,
        path: Option<Vec<usize>>,
    ) -> Result<Vec<NV>, ErrKind> {
        let md = self.md(parent.m);
        match &md.tree.nodes[parent.n.idx()].kind {
            NodeKind::Assign { value, .. } | NodeKind::AugAssign { value, .. } => {
                let value = GNode { m: parent.m, n: *value };
                self.assign_assigned(value, ctx, path)
            }
            NodeKind::TypeAlias { value, .. } => {
                let value = GNode { m: parent.m, n: *value };
                self.assign_assigned(value, ctx, path)
            }
            NodeKind::AnnAssign { value, .. } => match value {
                None => Ok(vec![NV::V(Value::Uninferable)]),
                Some(v) => {
                    let value = GNode { m: parent.m, n: *v };
                    self.assign_assigned(value, ctx, path)
                }
            },
            NodeKind::Tuple { elts, ctx: ec } | NodeKind::List { elts, ctx: ec } => {
                if *ec != ExprCtx::Store {
                    return Err(ErrKind::Inference);
                }
                let index = elts
                    .iter()
                    .position(|&e| e == child.n)
                    .ok_or(ErrKind::Inference)?;
                let mut path = path.unwrap_or_default();
                path.insert(0, index);
                let gparent = self.parent(parent).ok_or(ErrKind::Inference)?;
                self.parent_assigned(gparent, parent, ctx, Some(path))
            }
            NodeKind::For(d) => {
                let iter = GNode { m: parent.m, n: d.iter };
                self.for_assigned(iter, false, ctx, path)
            }
            NodeKind::AsyncFor(_) => Err(ErrKind::Inference),
            NodeKind::Comprehension { iter, is_async, .. } => {
                let iter = GNode { m: parent.m, n: *iter };
                self.for_assigned(iter, *is_async, ctx, path)
            }
            NodeKind::With(d) | NodeKind::AsyncWith(d) => {
                self.with_assigned(parent, &d.items, child, ctx, path)
            }
            NodeKind::Starred { .. } => self.starred_assigned(parent, child, ctx, path),
            NodeKind::Arguments(_) => self.arguments_assigned(parent, child, ctx),
            NodeKind::ExceptHandler { .. } => self.excepthandler_assigned(parent, ctx),
            NodeKind::NamedExpr { target, value } => {
                if *target == child.n {
                    let f = self.infer(GNode { m: parent.m, n: *value }, &copy_context(ctx));
                    if f.vals.is_empty() {
                        return Err(f.err.unwrap_or(ErrKind::Inference));
                    }
                    Ok(f.vals.into_iter().map(nvify).collect())
                } else {
                    Err(ErrKind::Inference)
                }
            }
            NodeKind::MatchMapping { .. } | NodeKind::MatchStar { .. } => {
                // yes_if_nothing_inferred over empty
                Ok(vec![NV::V(Value::Uninferable)])
            }
            NodeKind::MatchAs { pattern, .. } => {
                // bare capture: MatchCase -> Match -> subject
                if pattern.is_none() {
                    if let Some(case) = self.parent(parent) {
                        let cmd = self.md(case.m);
                        if let NodeKind::MatchCase { pattern: cp, .. } =
                            &cmd.tree.nodes[case.n.idx()].kind
                        {
                            if *cp == parent.n {
                                if let Some(match_node) = self.parent(case) {
                                    if let NodeKind::Match { subject, .. } =
                                        &cmd.tree.nodes[match_node.n.idx()].kind
                                    {
                                        return Ok(vec![NV::N(GNode {
                                            m: parent.m,
                                            n: *subject,
                                        })]);
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(vec![NV::V(Value::Uninferable)])
            }
            NodeKind::TypeVar { .. } | NodeKind::TypeVarTuple { .. } | NodeKind::ParamSpec { .. } => {
                Ok(vec![NV::V(Value::SynthConst(Rc::new(ConstValue::None)))])
            }
            NodeKind::Delete { .. } => Err(ErrKind::Inference),
            _ => Err(ErrKind::Inference),
        }
    }

    /// assign_assigned_stmts (protocols.py:447-466), raise_if_nothing
    fn assign_assigned(
        &self,
        value: GNode,
        ctx: Option<&Rc<Ctx>>,
        path: Option<Vec<usize>>,
    ) -> Result<Vec<NV>, ErrKind> {
        match path {
            None => Ok(vec![NV::N(value)]),
            Some(path) if path.is_empty() => Ok(vec![NV::N(value)]),
            Some(path) => {
                let c = match ctx {
                    Some(c) => Rc::clone(c),
                    None => Ctx::new(),
                };
                let parts = self.infer(value, &c);
                let out = self.resolve_assignment_parts(&parts.vals, &path, &c);
                if out.is_empty() {
                    return Err(ErrKind::Inference);
                }
                Ok(out)
            }
        }
    }

    /// _resolve_assignment_parts (protocols.py:482-519)
    fn resolve_assignment_parts(&self, parts: &[Value], path: &[usize], ctx: &Rc<Ctx>) -> Vec<NV> {
        let mut path = path.to_vec();
        let index = path.remove(0);
        let mut out = Vec::new();
        for part in parts {
            let assigned: Option<NV> = match part {
                Value::Node(g)
                    if self.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })) =>
                {
                    let md = self.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::Dict { items } => match items.get(index) {
                            Some((k, _)) => Some(NV::N(GNode { m: g.m, n: *k })),
                            None => return out,
                        },
                        _ => None,
                    }
                }
                Value::SynthDict { items } => match items.get(index) {
                    Some((k, _)) => Some(match k {
                        Value::Node(g) => NV::N(*g),
                        other => NV::V(other.clone()),
                    }),
                    None => return out,
                },
                _ => {
                    let idx = Value::SynthConst(Rc::new(ConstValue::Int(IntValue::Small(
                        index as i64,
                    ))));
                    match self.getitem(part, &idx, ctx) {
                        Ok(nv) => Some(nv),
                        Err(ErrKind::AstroidType) | Err(ErrKind::AstroidIndex) => return out,
                        Err(_) => None,
                    }
                }
            };
            let assigned = match assigned {
                Some(a) => a,
                None => return out,
            };
            // `if not assigned` — None or Uninferable
            if matches!(assigned, NV::V(Value::Uninferable)) && !path.is_empty() {
                return out;
            }
            if path.is_empty() {
                out.push(assigned);
            } else {
                let flow = self.infer_nv(&assigned, ctx);
                if flow.err.map(|e| e.is_inference()).unwrap_or(false) && flow.vals.is_empty() {
                    return out;
                }
                out.extend(self.resolve_assignment_parts(&flow.vals, &path, ctx));
            }
        }
        out
    }

    /// for_assigned_stmts (protocols.py:290-316), raise_if_nothing
    fn for_assigned(
        &self,
        iter: GNode,
        is_async: bool,
        ctx: Option<&Rc<Ctx>>,
        path: Option<Vec<usize>>,
    ) -> Result<Vec<NV>, ErrKind> {
        if is_async {
            return Err(ErrKind::Inference);
        }
        let c = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        let parts = self.infer(iter, &c);
        // `for lst in self.iter.infer(context)`: an error raised on the
        // first pull (e.g. NameInferenceError from a class-scope name not
        // visible in the genexp) propagates AS-IS out of assigned_stmts
        // (protocols.py:290-316; raise_if_nothing_inferred only converts
        // StopIteration). Preserving the kind matters: _infer_stmts skips
        // NameInferenceError silently but yields U for InferenceError.
        if parts.vals.is_empty() {
            if let Some(e) = parts.err {
                return Err(e);
            }
        }
        let mut out: Vec<NV> = Vec::new();
        match path {
            None => {
                for lst in &parts.vals {
                    match lst {
                        Value::Node(g) => {
                            let md = self.md(g.m);
                            match &md.tree.nodes[g.n.idx()].kind {
                                NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => {
                                    out.extend(
                                        elts.iter().map(|&e| NV::N(GNode { m: g.m, n: e })),
                                    );
                                }
                                _ => {}
                            }
                        }
                        Value::SynthSeq { kind, elems }
                            if matches!(kind, SeqKind::Tuple | SeqKind::List) =>
                        {
                            out.extend(elems.iter().map(|v| match v {
                                Value::Node(g) => NV::N(*g),
                                other => NV::V(other.clone()),
                            }));
                        }
                        _ => {}
                    }
                }
            }
            Some(path) => {
                out = self.resolve_looppart(&parts.vals, &path, &c);
            }
        }
        if out.is_empty() {
            return Err(ErrKind::Inference);
        }
        Ok(out)
    }

    /// _resolve_looppart (protocols.py:249-287)
    fn resolve_looppart(&self, parts: &[Value], path: &[usize], ctx: &Rc<Ctx>) -> Vec<NV> {
        let mut path = path.to_vec();
        let index = path.remove(0);
        let mut out = Vec::new();
        for part in parts {
            if part.is_uninferable() {
                continue;
            }
            let Some(itered) = self.value_itered(part) else { continue };
            let mut itered: Vec<Value> = itered;
            if let Some(v) = itered.get(index) {
                let is_const_or_name = match v {
                    Value::SynthConst(_) => true,
                    Value::Node(g) => self.kind_is(*g, |k| {
                        matches!(k, NodeKind::Const(_) | NodeKind::Name { .. })
                    }),
                    _ => false,
                };
                if is_const_or_name {
                    itered = vec![part.clone()];
                }
            }
            for stmt in &itered {
                let idx = Value::SynthConst(Rc::new(ConstValue::Int(IntValue::Small(
                    index as i64,
                ))));
                let assigned = match self.getitem(stmt, &idx, ctx) {
                    Ok(nv) => nv,
                    Err(_) => continue,
                };
                if path.is_empty() {
                    out.push(assigned);
                } else if matches!(assigned, NV::V(Value::Uninferable)) {
                    break;
                } else {
                    let flow = self.infer_nv(&assigned, ctx);
                    if flow.err.map(|e| e.is_inference()).unwrap_or(false)
                        && flow.vals.is_empty()
                    {
                        break;
                    }
                    out.extend(self.resolve_looppart(&flow.vals, &path, ctx));
                }
            }
        }
        out
    }

    /// with_assigned_stmts (protocols.py:605-682), raise_if_nothing
    fn with_assigned(
        &self,
        with_node: GNode,
        items: &[(NodeId, Option<NodeId>)],
        child: GNode,
        ctx: Option<&Rc<Ctx>>,
        path: Option<Vec<usize>>,
    ) -> Result<Vec<NV>, ErrKind> {
        let mgr = items
            .iter()
            .find(|(_, vars)| *vars == Some(child.n))
            .map(|(m, _)| GNode { m: with_node.m, n: *m });
        let Some(mgr) = mgr else {
            return Err(ErrKind::Inference);
        };
        let c = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        let results = self.infer_context_manager(mgr, &c)?;
        match path {
            None => {
                if results.is_empty() {
                    Err(ErrKind::Inference)
                } else {
                    Ok(results)
                }
            }
            Some(path) => {
                let mut out = Vec::new();
                for result in results {
                    let flow = self.infer_nv(&result, &c);
                    for obj in flow.vals {
                        let mut cur = obj;
                        let mut ok = true;
                        for &index in &path {
                            let elts = self.value_elts(&cur);
                            match elts.and_then(|e| e.get(index).cloned()) {
                                Some(v) => cur = v,
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if ok {
                            out.push(match cur {
                                Value::Node(g) => NV::N(g),
                                other => NV::V(other),
                            });
                        } else {
                            return Err(ErrKind::Inference);
                        }
                    }
                }
                if out.is_empty() {
                    Err(ErrKind::Inference)
                } else {
                    Ok(out)
                }
            }
        }
    }

    /// _infer_context_manager (protocols.py:567-602)
    fn infer_context_manager(&self, mgr: GNode, ctx: &Rc<Ctx>) -> Result<Vec<NV>, ErrKind> {
        // next(mgr.infer(context)) — single pull
        let inferred = self
            .first_value(mgr, ctx)
            .ok()
            .flatten()
            .ok_or(ErrKind::Inference)?;
        match &inferred {
            Value::Generator { func, call_ctx, .. } => {
                // only contextlib.contextmanager-decorated generators
                let md = self.md(func.m);
                let decorators = match &md.tree.nodes[func.n.idx()].kind {
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.decorators,
                    _ => None,
                };
                let Some(dec) = decorators else {
                    return Err(ErrKind::Inference);
                };
                let dec_nodes: Vec<NodeId> = match &md.tree.nodes[dec.idx()].kind {
                    NodeKind::Decorators { nodes } => nodes.clone(),
                    _ => Vec::new(),
                };
                let mut is_cm = false;
                for dn in dec_nodes {
                    // next(decorator_node.infer(context), None) — single
                    // pull with the SAME context object (protocols.py:582)
                    if let Ok(Some(first)) =
                        self.first_value(GNode { m: func.m, n: dn }, ctx)
                    {
                        if self.value_qname(&first).as_deref() == Some("contextlib.contextmanager") {
                            is_cm = true;
                            break;
                        }
                    }
                }
                if !is_cm {
                    return Err(ErrKind::Inference);
                }
                // next(inferred.infer_yield_types()) — single pull with the
                // generator's CAPTURED creation context (bases.py:703-704)
                let v = self.infer_yield_first(*func, call_ctx)?;
                Ok(vec![NV::V(v)])
            }
            Value::Inst { .. } | Value::ExcInst { .. } => {
                let enter_sym = self.sym("__enter__");
                // next(inferred.igetattr("__enter__", context)) — single pull
                let enter = self
                    .igetattr_first(&inferred, enter_sym, Some(ctx))
                    .ok()
                    .flatten()
                    .ok_or(ErrKind::Inference)?;
                match &enter {
                    Value::BoundMethod { .. } => {
                        // yield from enter.infer_call_result(self, context)
                        // — BoundMethod binds context.boundnode to the
                        // instance (bases.py BoundMethod.infer_call_result),
                        // so `return self` infers to the SUBCLASS instance
                        let res = self.infer_call_result(&enter, None, Some(ctx));
                        Ok(res.vals.into_iter().map(nvify).collect())
                    }
                    _ => Err(ErrKind::Inference),
                }
            }
            _ => Err(ErrKind::Inference),
        }
    }

    /// `next(gen.infer_yield_types())` — single lazy pull of
    /// FunctionDef.infer_yield_result (scoped_nodes.py:1543-1553).
    /// nodes_of_class(Yield) includes YieldFrom (a Yield subclass,
    /// node_classes.py:4607). The consumer abandons the generator after
    /// the first value (no further counter burn); an empty per-yield infer
    /// falls through to the next yield; exhaustion -> InferenceError.
    pub fn infer_yield_first(&self, func: GNode, ctx: &Rc<Ctx>) -> Result<Value, ErrKind> {
        let md = self.md(func.m);
        let mut stack = vec![func.n];
        let mut buf = Vec::new();
        let mut yields: Vec<NodeId> = Vec::new();
        while let Some(n) = stack.pop() {
            if matches!(
                &md.tree.nodes[n.idx()].kind,
                NodeKind::Yield { .. } | NodeKind::YieldFrom { .. }
            ) {
                yields.push(n);
            }
            buf.clear();
            md.tree.push_children(n, &mut buf);
            stack.extend(buf.iter().copied());
        }
        yields.sort();
        drop(md);
        for y in yields {
            let md = self.md(func.m);
            let value = match &md.tree.nodes[y.idx()].kind {
                NodeKind::Yield { value } => *value,
                NodeKind::YieldFrom { value } => Some(*value),
                _ => None,
            };
            drop(md);
            match value {
                // `yield` (no value): Const(None), NO scope check
                // (scoped_nodes.py:1550-1551)
                None => return Ok(Value::SynthConst(Rc::new(ConstValue::None))),
                Some(v) => {
                    // elif yield_.scope() == self
                    let g = GNode { m: func.m, n: y };
                    if self.frame(g) != func {
                        continue;
                    }
                    let mut first: Option<Value> = None;
                    let end = {
                        let first = &mut first;
                        self.infer_to(GNode { m: func.m, n: v }, ctx, &mut |val| {
                            *first = Some(val);
                            Drive::Stop
                        })
                    };
                    match (first, end) {
                        (Some(v), _) => return Ok(v),
                        (None, End::Raised(e)) => return Err(e),
                        (None, _) => continue, // empty infer -> next yield
                    }
                }
            }
        }
        Err(ErrKind::Inference) // StopIteration -> InferenceError(node=func)
    }

    /// excepthandler_assigned_stmts (protocols.py:522-564)
    fn excepthandler_assigned(
        &self,
        handler: GNode,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Vec<NV>, ErrKind> {
        let md = self.md(handler.m);
        let type_ = match &md.tree.nodes[handler.n.idx()].kind {
            NodeKind::ExceptHandler { type_, .. } => *type_,
            _ => None,
        };
        let c = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        let mut assigned: Vec<NV> = Vec::new();
        if let Some(t) = type_ {
            for v in self.unpack_infer(GNode { m: handler.m, n: t }, &c)? {
                match v {
                    Value::Node(g)
                        if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) =>
                    {
                        assigned.push(NV::V(Value::ExcInst {
                            cls: g,
                            exceptions: None,
                        }));
                    }
                    other => assigned.push(NV::V(other)),
                }
            }
        }
        // except* -> ExceptionGroup instance
        let parent_is_trystar = self
            .parent(handler)
            .map(|p| self.kind_is(p, |k| matches!(k, NodeKind::TryStar(_))))
            .unwrap_or(false);
        if parent_is_trystar {
            let eg_sym = self.sym("ExceptionGroup");
            let (_, eg) = self.builtin_lookup(eg_sym);
            if let Some(NV::N(cls)) = eg.first() {
                let exceptions: Vec<Value> = assigned
                    .iter()
                    .map(|nv| match nv {
                        NV::V(v) => v.clone(),
                        NV::N(g) => Value::Node(*g),
                    })
                    .collect();
                return Ok(vec![NV::V(Value::ExcInst {
                    cls: *cls,
                    exceptions: Some(Rc::new(vec![Value::SynthSeq {
                        kind: SeqKind::List,
                        elems: Rc::new(exceptions),
                    }])),
                })]);
            }
        }
        if assigned.is_empty() {
            return Err(ErrKind::Inference);
        }
        Ok(assigned)
    }

    /// node_classes.py:89-113 unpack_infer
    pub fn unpack_infer(&self, stmt: GNode, ctx: &Rc<Ctx>) -> Result<Vec<Value>, ErrKind> {
        let md = self.md(stmt.m);
        match &md.tree.nodes[stmt.n.idx()].kind {
            NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } => {
                let mut out = Vec::new();
                for &e in elts {
                    out.extend(self.unpack_infer(GNode { m: stmt.m, n: e }, ctx)?);
                }
                Ok(out)
            }
            _ => {
                let flow = self.infer(stmt, ctx);
                let mut out = Vec::new();
                for v in flow.vals {
                    match &v {
                        Value::Node(g) if *g == stmt => out.push(v),
                        Value::Uninferable => out.push(v),
                        Value::Node(g) => {
                            out.extend(self.unpack_infer(*g, ctx)?);
                        }
                        _ => out.push(v),
                    }
                }
                if out.is_empty() {
                    return Err(flow.err.unwrap_or(ErrKind::Inference));
                }
                Ok(out)
            }
        }
    }

    /// arguments_assigned_stmts (protocols.py:416-444)
    fn arguments_assigned(
        &self,
        arguments_node: GNode,
        child: GNode,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Vec<NV>, ErrKind> {
        let node_name = self.assign_name_of(child);
        let cc_opt = ctx.and_then(|c| c.callcontext.borrow().clone());
        if let (Some(c), Some(cc)) = (ctx, cc_opt) {
            let callee = cc.callee.borrow().clone();
            let callee_func = callee.as_ref().and_then(|v| match v {
                Value::Node(g) => Some(*g),
                Value::BoundMethod { func, .. }
                | Value::UnboundMethod { func }
                | Value::Property { func }
                | Value::Partial { func, .. } => Some(*func),
                _ => None,
            });
            let frame = self.frame(child);
            let callee_name = callee_func.and_then(|f| self.node_name(f));
            if callee_name.is_some() && callee_name == self.node_name(frame) {
                // reset call context, bind args via CallSite
                let new_ctx = copy_context(Some(c));
                *new_ctx.callcontext.borrow_mut() = None;
                let site = self.call_site_from(&cc, &new_ctx);
                let func = self.parent(arguments_node).ok_or(ErrKind::Inference)?;
                let name = node_name.ok_or(ErrKind::Inference)?;
                let f = self.infer_argument(&site, func, name, &new_ctx);
                if f.vals.is_empty() {
                    return Err(f.err.unwrap_or(ErrKind::Inference));
                }
                return Ok(f.vals.into_iter().map(nvify).collect());
            }
            let f = self.arguments_infer_argname(arguments_node, node_name, c);
            if f.vals.is_empty() {
                return Err(f.err.unwrap_or(ErrKind::Inference));
            }
            return Ok(f.vals.into_iter().map(nvify).collect());
        }
        let c = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        let f = self.arguments_infer_argname(arguments_node, node_name, &c);
        if f.vals.is_empty() {
            return Err(f.err.unwrap_or(ErrKind::Inference));
        }
        Ok(f.vals.into_iter().map(nvify).collect())
    }

    /// starred_assigned_stmts (protocols.py:704-899), yes_if_nothing
    fn starred_assigned(
        &self,
        starred: GNode,
        _child: GNode,
        ctx: Option<&Rc<Ctx>>,
        _path: Option<Vec<usize>>,
    ) -> Result<Vec<NV>, ErrKind> {
        let stmt = self.statement(starred).ok_or(ErrKind::Inference)?;
        let md = self.md(stmt.m);
        let c = match ctx {
            Some(c) => Rc::clone(c),
            None => Ctx::new(),
        };
        match &md.tree.nodes[stmt.n.idx()].kind {
            NodeKind::Assign { targets, value } => {
                let lhs = targets.first().copied().ok_or(ErrKind::Inference)?;
                let lhs_elts: Vec<NodeId> = match &md.tree.nodes[lhs.idx()].kind {
                    NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => elts.clone(),
                    _ => return Ok(vec![NV::V(Value::Uninferable)]),
                };
                // count Starred nodes in the whole lhs
                let mut starred_count = 0;
                let mut stack = vec![lhs];
                let mut buf = Vec::new();
                while let Some(n) = stack.pop() {
                    if matches!(md.tree.nodes[n.idx()].kind, NodeKind::Starred { .. }) {
                        starred_count += 1;
                    }
                    buf.clear();
                    md.tree.push_children(n, &mut buf);
                    stack.extend(buf.iter().copied());
                }
                if starred_count > 1 {
                    return Err(ErrKind::Inference);
                }
                // rhs = next(value.infer(context)) — single pull
                let rhs = match self.first_value(GNode { m: stmt.m, n: *value }, &c) {
                    Ok(Some(v)) => v,
                    _ => return Ok(vec![NV::V(Value::Uninferable)]),
                };
                let elts = match self.value_itered(&rhs) {
                    Some(e) => e,
                    None => return Ok(vec![NV::V(Value::Uninferable)]),
                };
                let mut elts: std::collections::VecDeque<Value> = elts.into();
                for (index, &left_node) in lhs_elts.iter().enumerate() {
                    if !matches!(md.tree.nodes[left_node.idx()].kind, NodeKind::Starred { .. }) {
                        if elts.is_empty() {
                            break;
                        }
                        elts.pop_front();
                        continue;
                    }
                    let rest: Vec<NodeId> = lhs_elts[index..].iter().rev().copied().collect();
                    for &right_node in &rest {
                        if !matches!(
                            md.tree.nodes[right_node.idx()].kind,
                            NodeKind::Starred { .. }
                        ) {
                            if elts.is_empty() {
                                break;
                            }
                            elts.pop_back();
                            continue;
                        }
                        return Ok(vec![NV::V(Value::SynthSeq {
                            kind: SeqKind::List,
                            elems: Rc::new(elts.iter().cloned().collect()),
                        })]);
                    }
                    break;
                }
                Ok(vec![NV::V(Value::Uninferable)])
            }
            NodeKind::For(d) => {
                // next(self.iter.infer(context)) — single pull
                let inferred_iterable = match self.first_value(GNode { m: stmt.m, n: d.iter }, &c) {
                    Ok(Some(v)) => v,
                    _ => return Ok(vec![NV::V(Value::Uninferable)]),
                };
                let itered = match self.value_itered(&inferred_iterable) {
                    Some(i) => i,
                    None => return Ok(vec![NV::V(Value::Uninferable)]),
                };
                let target = GNode { m: stmt.m, n: d.target };
                if !self.kind_is(target, |k| matches!(k, NodeKind::Tuple { .. })) {
                    return Err(ErrKind::Inference);
                }
                let mut lookups: Vec<(usize, usize)> = Vec::new();
                self.determine_starred_lookups(starred, target, &mut lookups);
                if lookups.is_empty() {
                    return Err(ErrKind::Inference);
                }
                let (last_index, last_len) = *lookups.last().unwrap();
                let is_starred_last = last_index == last_len - 1;
                for element in itered {
                    let mut element = element;
                    let mut found: Option<Vec<Value>> = None;
                    for (i, lookup) in lookups.iter().enumerate() {
                        let Some(inner) = self.value_itered(&element) else { break };
                        if i + 1 == lookups.len() {
                            // slice
                            let end = if is_starred_last {
                                inner.len()
                            } else {
                                (last_len - last_index).min(inner.len())
                            };
                            if last_index > inner.len() {
                                break;
                            }
                            let sliced: Vec<Value> =
                                inner[last_index.min(inner.len())..end].to_vec();
                            found = Some(sliced.clone());
                            // (element no longer needed)
                            break;
                        } else {
                            match inner.get(lookup.0) {
                                Some(e) => {
                                    element = e.clone();
                                    found = None;
                                }
                                None => break,
                            }
                        }
                    }
                    return Ok(vec![NV::V(Value::SynthSeq {
                        kind: SeqKind::List,
                        elems: Rc::new(found.unwrap_or_default()),
                    })]);
                }
                Ok(vec![NV::V(Value::Uninferable)])
            }
            _ => Err(ErrKind::Inference),
        }
    }

    fn determine_starred_lookups(
        &self,
        starred: GNode,
        target: GNode,
        lookups: &mut Vec<(usize, usize)>,
    ) {
        let md = self.md(target.m);
        let elts: Vec<NodeId> = match &md.tree.nodes[target.n.idx()].kind {
            NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => elts.clone(),
            _ => return,
        };
        let starred_name = match &md.tree.nodes[starred.n.idx()].kind {
            NodeKind::Starred { value, .. } => match &md.tree.nodes[value.idx()].kind {
                NodeKind::AssignName { name } | NodeKind::Name { name } => {
                    Some(md.tree.s(*name).to_string())
                }
                _ => None,
            },
            _ => None,
        };
        for (index, &element) in elts.iter().enumerate() {
            match &md.tree.nodes[element.idx()].kind {
                NodeKind::Starred { value, .. } => {
                    let elem_name = match &md.tree.nodes[value.idx()].kind {
                        NodeKind::AssignName { name } | NodeKind::Name { name } => {
                            Some(md.tree.s(*name).to_string())
                        }
                        _ => None,
                    };
                    if elem_name == starred_name {
                        lookups.push((index, elts.len()));
                        break;
                    }
                }
                NodeKind::Tuple { elts: inner, .. } => {
                    lookups.push((index, inner.len()));
                    self.determine_starred_lookups(starred, GNode { m: target.m, n: element }, lookups);
                }
                _ => {}
            }
        }
    }

    // ================= itered =================

    /// `.itered()`: List/Tuple/Set -> elts; Dict -> keys; Const str/bytes ->
    /// char Consts; FrozenSet -> elts. None => no attribute / TypeError.
    /// (key, value) pairs of a DictRef as Values (Dict literal children
    /// stay nodes)
    pub fn dictref_pairs(&self, dr: &crate::value::DictRef) -> Vec<(Value, Value)> {
        match dr {
            crate::value::DictRef::Synth(items) => items.as_ref().clone(),
            crate::value::DictRef::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => items
                        .iter()
                        .map(|&(k, v)| {
                            (
                                Value::Node(GNode { m: g.m, n: k }),
                                Value::Node(GNode { m: g.m, n: v }),
                            )
                        })
                        .collect(),
                    _ => Vec::new(),
                }
            }
        }
    }

    pub fn value_itered(&self, v: &Value) -> Option<Vec<Value>> {
        match v {
            Value::SynthSeq { elems, .. } | Value::FrozenSet { elems } => Some(elems.to_vec()),
            Value::SynthDict { items } => {
                Some(items.iter().map(|(k, _)| k.clone()).collect())
            }
            Value::SynthConst(c) => const_itered(c),
            // DictItems proxies a List of Tuple(key, value) nodes built by
            // DictModel.attr_items (objectmodel.py:855-867); Keys/Values
            // proxy Lists of the keys/values
            Value::DictItems(dr) => Some(
                self.dictref_pairs(dr)
                    .into_iter()
                    .map(|(k, v)| Value::SynthSeq {
                        kind: SeqKind::Tuple,
                        elems: Rc::new(vec![k, v]),
                    })
                    .collect(),
            ),
            Value::DictKeys(dr) => {
                Some(self.dictref_pairs(dr).into_iter().map(|(k, _)| k).collect())
            }
            Value::DictValues(dr) => {
                Some(self.dictref_pairs(dr).into_iter().map(|(_, v)| v).collect())
            }
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
                    NodeKind::Dict { items } => Some(
                        items
                            .iter()
                            .map(|&(k, _)| Value::Node(GNode { m: g.m, n: k }))
                            .collect(),
                    ),
                    NodeKind::Const(c) => const_itered(c),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ================= Subscript (§15) =================

    /// eager shim (infer_lhs path)
    pub fn infer_subscript(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_subscript_to(node, ctx, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    /// Subscript._infer_subscript (node_classes.py:3729-3795), streaming.
    pub fn infer_subscript_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let md = self.md(node.m);
        let (value, slice) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Subscript { value, slice, .. } => (
                GNode { m: node.m, n: *value },
                GNode { m: node.m, n: *slice },
            ),
            _ => return End::Raised(ErrKind::Inference),
        };
        // outcome of the whole generator decided inside the nested pulls
        let mut finished = false; // yielded U + `return` (abandons outer)
        let mut raised: Option<ErrKind> = None;
        let mut stopped = false;
        let end = {
            let finished = &mut finished;
            let raised = &mut raised;
            let stopped = &mut stopped;
            self.infer_to(value, ctx, &mut |val| {
                if val.is_uninferable() {
                    let _ = sink(Value::Uninferable);
                    *finished = true;
                    return Drive::Stop;
                }
                // inner: self.slice.infer(context), pulled per value
                let inner_end = {
                    let finished = &mut *finished;
                    let raised = &mut *raised;
                    let stopped = &mut *stopped;
                    let val = &val;
                    self.infer_to(slice, ctx, &mut |index| {
                        if index.is_uninferable() {
                            let _ = sink(Value::Uninferable);
                            *finished = true;
                            return Drive::Stop;
                        }
                        // determine index value
                        let index_value: Value = if matches!(val, Value::Inst { .. }) {
                            // exact-class Instance: raw index NODE
                            Value::Node(slice)
                        } else if matches!(index, Value::Inst { .. }) {
                            match self.class_instance_as_index(&index) {
                                Some(v) => v,
                                None => {
                                    *raised = Some(ErrKind::Inference);
                                    return Drive::Stop;
                                }
                            }
                        } else {
                            index.clone()
                        };
                        let assigned = match self.getitem(val, &index_value, ctx) {
                            Ok(nv) => nv,
                            Err(_) => {
                                *raised = Some(ErrKind::Inference);
                                return Drive::Stop;
                            }
                        };
                        match &assigned {
                            NV::N(g) if *g == node => {
                                let _ = sink(Value::Uninferable);
                                *finished = true;
                                Drive::Stop
                            }
                            NV::V(Value::Uninferable) => {
                                let _ = sink(Value::Uninferable);
                                *finished = true;
                                Drive::Stop
                            }
                            _ => {
                                // yield from assigned.infer(context) — errors
                                // propagate (no try in astroid)
                                let mut inner_stop = false;
                                let e = self.infer_nv_to(&assigned, ctx, &mut |v| {
                                    let d = sink(v);
                                    if let Drive::Stop = d {
                                        inner_stop = true;
                                    }
                                    d
                                });
                                if inner_stop {
                                    *stopped = true;
                                    return Drive::Stop;
                                }
                                match e {
                                    End::Raised(err) => {
                                        *raised = Some(err);
                                        Drive::Stop
                                    }
                                    _ => Drive::Go,
                                }
                            }
                        }
                    })
                };
                if *finished || *stopped || raised.is_some() {
                    return Drive::Stop;
                }
                match inner_end {
                    End::Raised(e) => {
                        *raised = Some(e);
                        Drive::Stop
                    }
                    _ => Drive::Go,
                }
            })
        };
        if stopped {
            return End::Stopped;
        }
        if finished {
            return End::Done;
        }
        if let Some(e) = raised {
            return End::Raised(e);
        }
        end
    }

    /// helpers.class_instance_as_index
    fn class_instance_as_index(&self, node: &Value) -> Option<Value> {
        let ctx = Ctx::new();
        let sym = self.sym("__index__");
        let flow = self.igetattr_value(node, sym, Some(&ctx)).ok()?;
        for inferred in &flow.vals {
            if let Value::BoundMethod { func, .. } = inferred {
                *ctx.boundnode.borrow_mut() = Some(node.clone());
                *ctx.callcontext.borrow_mut() = Some(Rc::new(CallCtx {
                    id: self.next_callctx_id(),
                    args: RefCell::new(Vec::new()),
                    keywords: RefCell::new(Vec::new()),
                    callee: RefCell::new(Some(inferred.clone())),
                }));
                let res = self.function_infer_call_result(*func, None, Some(&ctx));
                for r in &res.vals {
                    if let Some(ConstValue::Int(_)) = self.value_const(r) {
                        return Some(r.clone());
                    }
                }
            }
        }
        None
    }

    /// getitem dispatch (notes/07 §15.2). Returns the assigned NV
    /// (un-inferred node where astroid yields nodes).
    pub fn getitem(&self, value: &Value, index: &Value, ctx: &Rc<Ctx>) -> Result<NV, ErrKind> {
        match value {
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => self.const_getitem(c, index),
                    NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } => {
                        let elems: Vec<NV> = elts
                            .iter()
                            .map(|&e| NV::N(GNode { m: g.m, n: e }))
                            .collect();
                        let kind = if matches!(md.tree.nodes[g.n.idx()].kind, NodeKind::List { .. })
                        {
                            SeqKind::List
                        } else {
                            SeqKind::Tuple
                        };
                        self.container_getitem(&elems, kind, index)
                    }
                    NodeKind::Dict { items } => {
                        let pairs: Vec<(NV, NV)> = items
                            .iter()
                            .map(|&(k, v)| {
                                (
                                    NV::N(GNode { m: g.m, n: k }),
                                    NV::N(GNode { m: g.m, n: v }),
                                )
                            })
                            .collect();
                        self.dict_getitem(g.m, &pairs, index, ctx)
                    }
                    NodeKind::ClassDef(_) => self.class_getitem(*g, index, ctx),
                    _ => Err(ErrKind::AstroidType),
                }
            }
            Value::SynthConst(c) => self.const_getitem(c, index),
            Value::SynthSeq { kind, elems } => {
                let elems: Vec<NV> = elems
                    .iter()
                    .map(|v| match v {
                        Value::Node(g) => NV::N(*g),
                        other => NV::V(other.clone()),
                    })
                    .collect();
                self.container_getitem(&elems, *kind, index)
            }
            Value::SynthDict { items } => {
                let pairs: Vec<(NV, NV)> = items
                    .iter()
                    .map(|(k, v)| {
                        (
                            match k {
                                Value::Node(g) => NV::N(*g),
                                o => NV::V(o.clone()),
                            },
                            match v {
                                Value::Node(g) => NV::N(*g),
                                o => NV::V(o.clone()),
                            },
                        )
                    })
                    .collect();
                self.dict_getitem_values(&pairs, index, ctx)
            }
            Value::Inst { .. } | Value::ExcInst { .. } => self.instance_getitem(value, index, ctx),
            _ => Err(ErrKind::AstroidType),
        }
    }

    fn const_getitem(&self, c: &ConstValue, index: &Value) -> Result<NV, ErrKind> {
        let idx_const = self.value_const(index);
        match c {
            ConstValue::Str(s) => {
                let chars: Vec<char> = s.chars().collect();
                match &idx_const {
                    Some(ConstValue::Int(IntValue::Small(i))) => {
                        let i = norm_index(*i, chars.len()).ok_or(ErrKind::AstroidIndex)?;
                        Ok(NV::V(Value::SynthConst(Rc::new(ConstValue::Str(
                            chars[i].to_string().into(),
                        )))))
                    }
                    Some(ConstValue::Bool(b)) => {
                        let i = norm_index(*b as i64, chars.len())
                            .ok_or(ErrKind::AstroidIndex)?;
                        Ok(NV::V(Value::SynthConst(Rc::new(ConstValue::Str(
                            chars[i].to_string().into(),
                        )))))
                    }
                    _ => match self.value_slice(index) {
                        Some(sl) => {
                            let sliced: String =
                                slice_seq(&chars, &sl).into_iter().collect();
                            Ok(NV::V(Value::SynthConst(Rc::new(ConstValue::Str(
                                sliced.into(),
                            )))))
                        }
                        None => Err(ErrKind::AstroidType),
                    },
                }
            }
            ConstValue::Bytes(b) => match &idx_const {
                Some(ConstValue::Int(IntValue::Small(i))) => {
                    let i = norm_index(*i, b.len()).ok_or(ErrKind::AstroidIndex)?;
                    Ok(NV::V(Value::SynthConst(Rc::new(ConstValue::Int(
                        IntValue::Small(b[i] as i64),
                    )))))
                }
                _ => match self.value_slice(index) {
                    Some(sl) => {
                        let v: Vec<u8> = slice_seq(b, &sl);
                        Ok(NV::V(Value::SynthConst(Rc::new(ConstValue::Bytes(
                            v.into(),
                        )))))
                    }
                    None => Err(ErrKind::AstroidType),
                },
            },
            _ => Err(ErrKind::AstroidType),
        }
    }

    fn container_getitem(
        &self,
        elems: &[NV],
        kind: SeqKind,
        index: &Value,
    ) -> Result<NV, ErrKind> {
        match self.value_const(index) {
            Some(ConstValue::Int(IntValue::Small(i))) => {
                let i = norm_index(i, elems.len()).ok_or(ErrKind::AstroidIndex)?;
                Ok(elems[i].clone())
            }
            Some(ConstValue::Bool(b)) => {
                let i = norm_index(b as i64, elems.len()).ok_or(ErrKind::AstroidIndex)?;
                Ok(elems[i].clone())
            }
            _ => match self.value_slice(index) {
                Some(sl) => {
                    let sliced = slice_seq(elems, &sl);
                    let vals: Vec<Value> = sliced
                        .into_iter()
                        .map(|nv| match nv {
                            NV::N(g) => Value::Node(g),
                            NV::V(v) => v,
                        })
                        .collect();
                    Ok(NV::V(Value::SynthSeq {
                        kind,
                        elems: Rc::new(vals),
                    }))
                }
                None => Err(ErrKind::AstroidType),
            },
        }
    }

    fn dict_getitem(
        &self,
        m: crate::value::ModId,
        items: &[(NV, NV)],
        index: &Value,
        ctx: &Rc<Ctx>,
    ) -> Result<NV, ErrKind> {
        let _ = m;
        self.dict_getitem_values(items, index, ctx)
    }

    fn dict_getitem_values(
        &self,
        items: &[(NV, NV)],
        index: &Value,
        ctx: &Rc<Ctx>,
    ) -> Result<NV, ErrKind> {
        let index_const = self.value_const(index);
        for (k, v) in items {
            // DictUnpack keys: recurse into the unpacked dict
            if let NV::N(kg) = k {
                if self.kind_is(*kg, |kk| matches!(kk, NodeKind::DictUnpack)) {
                    if let NV::N(vg) = v {
                        if let Some(inner) = self.safe_infer(*vg, &ctx.clone_ctx()) {
                            if let Some(pairs) = self.value_dict_items(&inner) {
                                let nv_pairs: Vec<(NV, NV)> = pairs
                                    .iter()
                                    .map(|(a, b)| {
                                        (
                                            match a {
                                                Value::Node(g) => NV::N(*g),
                                                o => NV::V(o.clone()),
                                            },
                                            match b {
                                                Value::Node(g) => NV::N(*g),
                                                o => NV::V(o.clone()),
                                            },
                                        )
                                    })
                                    .collect();
                                if let Ok(found) =
                                    self.dict_getitem_values(&nv_pairs, index, ctx)
                                {
                                    return Ok(found);
                                }
                            }
                        }
                    }
                    continue;
                }
            }
            let key_flow = self.infer_nv(k, ctx);
            for ik in &key_flow.vals {
                if ik.is_uninferable() {
                    continue;
                }
                if let (Some(kc), Some(ic)) = (self.value_const(ik), index_const.as_ref()) {
                    if const_eq(&kc, ic) {
                        return Ok(v.clone());
                    }
                }
            }
        }
        Err(ErrKind::AstroidIndex)
    }

    /// Instance.getitem (bases.py:416-435)
    fn instance_getitem(&self, instance: &Value, index: &Value, ctx: &Rc<Ctx>) -> Result<NV, ErrKind> {
        let new_ctx = bind_context_to_node(Some(ctx), instance.clone());
        let sym = self.sym("__getitem__");
        // method = next(self.igetattr("__getitem__", context)) — single pull
        let method = self
            .igetattr_first(instance, sym, Some(&new_ctx))
            .ok()
            .flatten()
            .ok_or(ErrKind::Inference)?;
        let func = match &method {
            Value::BoundMethod { func, .. } => *func,
            _ => return Err(ErrKind::Inference),
        };
        // must have exactly 2 parameters
        if let Some(spec) = self.arg_spec(func) {
            if spec.arguments().len() != 2 {
                return Err(ErrKind::AstroidType);
            }
        }
        *new_ctx.callcontext.borrow_mut() = Some(Rc::new(CallCtx {
            id: self.next_callctx_id(),
            args: RefCell::new(vec![NV::V(index.clone())]),
            keywords: RefCell::new(Vec::new()),
            callee: RefCell::new(Some(method.clone())),
        }));
        let res = Flow {
            vals: self
                .infer_call_result_first(&method, None, Some(&new_ctx))
                .ok()
                .flatten()
                .into_iter()
                .collect(),
            err: None,
        };
        match res.vals.into_iter().next() {
            Some(v) => Ok(NV::V(v)),
            None => Ok(NV::V(Value::Uninferable)),
        }
    }

    /// ClassDef.getitem (scoped_nodes.py:2540-2590)
    fn class_getitem(&self, cls: GNode, index: &Value, ctx: &Rc<Ctx>) -> Result<NV, ErrKind> {
        let sym = self.sym("__getitem__");
        let mut methods = self.dunder_lookup_class(cls, sym);
        let mut from_class_getitem = false;
        if methods.is_empty() {
            let cg = self.sym("__class_getitem__");
            match self.class_getattr(cls, cg, Some(ctx), true) {
                Ok(attrs) => {
                    methods = attrs
                        .into_iter()
                        .filter_map(|a| match a {
                            NV::N(g) => Some(g),
                            _ => None,
                        })
                        .collect();
                    from_class_getitem = true;
                }
                Err(_) => return Err(ErrKind::AstroidType),
            }
        }
        let _ = from_class_getitem;
        let Some(&method) = methods.first() else {
            return Err(ErrKind::AstroidType);
        };
        // EmptyNode method on a builtin class: list[int] -> the class itself
        if self.kind_is(method, |k| matches!(k, NodeKind::EmptyNode)) {
            return Ok(NV::N(cls));
        }
        let new_ctx = bind_context_to_node(Some(ctx), Value::Node(cls));
        *new_ctx.callcontext.borrow_mut() = Some(Rc::new(CallCtx {
            id: self.next_callctx_id(),
            args: RefCell::new(vec![NV::V(index.clone())]),
            keywords: RefCell::new(Vec::new()),
            callee: RefCell::new(Some(Value::Node(method))),
        }));
        // next(methods[0].infer_call_result(self, ctx), ...) — single pull;
        // InferenceError -> Uninferable (scoped_nodes.py:2575-2590)
        match self.infer_call_result_first(&Value::Node(method), None, Some(&new_ctx)) {
            Ok(Some(v)) => Ok(NV::V(v)),
            _ => Ok(NV::V(Value::Uninferable)),
        }
    }

    /// slice value of an index (Slice node / SynthSlice)
    fn value_slice(&self, index: &Value) -> Option<PySlice> {
        match index {
            Value::SynthSlice { bounds } => Some(PySlice {
                start: const_as_int(bounds[0].as_ref()),
                stop: const_as_int(bounds[1].as_ref()),
                step: const_as_int(bounds[2].as_ref()),
            }),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Slice { lower, upper, step } => {
                        // _infer_slice: bounds must be Const int/None
                        let get = |o: &Option<NodeId>| -> Option<Option<i64>> {
                            match o {
                                None => Some(None),
                                Some(n) => {
                                    let gg = GNode { m: g.m, n: *n };
                                    match &md.tree.nodes[n.idx()].kind {
                                        NodeKind::Const(ConstValue::Int(IntValue::Small(i))) => {
                                            Some(Some(*i))
                                        }
                                        NodeKind::Const(ConstValue::None) => Some(None),
                                        _ => {
                                            let f =
                                                self.infer(gg, &Ctx::new());
                                            match f.vals.first().and_then(|v| self.value_const(v))
                                            {
                                                Some(ConstValue::Int(IntValue::Small(i))) => {
                                                    Some(Some(i))
                                                }
                                                Some(ConstValue::None) => Some(None),
                                                _ => None,
                                            }
                                        }
                                    }
                                }
                            }
                        };
                        Some(PySlice {
                            start: get(lower)?,
                            stop: get(upper)?,
                            step: get(step)?,
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ================= operators (§14) =================

    /// BinOp._infer with Bad-message filtering (-> Uninferable)
    pub fn infer_binop_filtered(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let f = self.infer_binop_raw(node, ctx);
        Flow {
            vals: f
                .vals
                .into_iter()
                .map(|v| if matches!(v, Value::Uninferable) { v } else { v })
                .collect(),
            err: f.err,
        }
    }

    fn infer_binop_raw(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (left, op, right) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::BinOp { left, op, right } => (
                GNode { m: node.m, n: *left },
                op.clone(),
                GNode { m: node.m, n: *right },
            ),
            _ => return Flow::err(ErrKind::Inference),
        };
        let lhs_ctx = copy_context(Some(ctx));
        let rhs_ctx = copy_context(Some(ctx));
        // itertools.product materializes both iterators before yielding
        // anything (node_classes.py:1549): an error from either side
        // propagates with no values produced.
        let lhs_flow = self.infer(left, &lhs_ctx);
        if let Some(e) = lhs_flow.err {
            return Flow::err(e);
        }
        let rhs_flow = self.infer(right, &rhs_ctx);
        if let Some(e) = rhs_flow.err {
            return Flow::err(e);
        }
        let mut out = Vec::new();
        'outer: for lhs in &lhs_flow.vals {
            for rhs in &rhs_flow.vals {
                if lhs.is_uninferable() || rhs.is_uninferable() {
                    out.push(Value::Uninferable);
                    break 'outer;
                }
                match self.infer_binary_operation(lhs, rhs, &op, node, ctx, false) {
                    Ok(mut vals) => out.append(&mut vals),
                    Err(_) => out.push(Value::Uninferable),
                }
            }
        }
        Flow::ok(out)
    }

    pub fn infer_augassign_filtered(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (target, op, value) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::AugAssign { target, op, value } => (
                GNode { m: node.m, n: *target },
                op.clone(),
                GNode { m: node.m, n: *value },
            ),
            _ => return Flow::err(ErrKind::Inference),
        };
        // lhs: target.infer_lhs; product materializes both iterators
        // (node_classes.py:1430) — errors propagate before any yield.
        let lhs_flow = self.infer_lhs(target, ctx);
        if let Some(e) = lhs_flow.err {
            return Flow::err(e);
        }
        let rhs_ctx = ctx.clone_ctx();
        let rhs_flow = self.infer(value, &rhs_ctx);
        if let Some(e) = rhs_flow.err {
            return Flow::err(e);
        }
        let mut out = Vec::new();
        'outer: for lhs in &lhs_flow.vals {
            for rhs in &rhs_flow.vals {
                if lhs.is_uninferable() || rhs.is_uninferable() {
                    out.push(Value::Uninferable);
                    break 'outer;
                }
                match self.infer_binary_operation(lhs, rhs, &op, node, ctx, true) {
                    Ok(mut vals) => out.append(&mut vals),
                    Err(_) => out.push(Value::Uninferable),
                }
            }
        }
        Flow::ok(out)
    }

    /// AssignName.infer_lhs / Subscript.infer_lhs / AssignAttr.infer_lhs
    pub fn infer_lhs(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::AssignName { name } => {
                // same algorithm as infer_name, no path wrapper
                let name_sym = self.g(&md, *name);
                let looked = self.lookup(node, name_sym);
                let (frame, stmts) = (looked.0, looked.1.clone());
                if stmts.is_empty() {
                    return Flow::err(ErrKind::NameError);
                }
                let ctx2 = copy_context(Some(ctx));
                ctx2.lookupname.set(Some(name_sym));
                let cs = self.get_constraints(node, frame);
                ctx2.constraints.borrow_mut().insert(name_sym, Rc::new(cs));
                self.infer_stmts(&stmts, Some(&ctx2), Some(frame)).raise_if_nothing()
            }
            NodeKind::AssignAttr { .. } => self.infer_attribute_load(node, ctx).raise_if_nothing(),
            NodeKind::Subscript { .. } => self.infer_subscript(node, ctx).raise_if_nothing(),
            _ => self.infer(node, ctx),
        }
    }

    /// _infer_binary_operation (_base_nodes.py:620-672). Returns Err for
    /// BadBinaryOperationMessage (callers map to Uninferable in _infer;
    /// type_errors() reads them directly later).
    fn infer_binary_operation(
        &self,
        left: &Value,
        right: &Value,
        op: &str,
        _opnode: GNode,
        ctx: &Rc<Ctx>,
        aug: bool,
    ) -> Result<Vec<Value>, ErrKind> {
        let left_type = self.object_type(left, ctx);
        let right_type = self.object_type(right, ctx);
        // method flow
        struct Try {
            instance: Value,
            method_name: String,
            other: Value,
        }
        // AugAssign ops arrive as "+="; the binary method table is keyed on
        // the base operator (astroid keeps separate AUGMENTED_OP_METHOD).
        let base_op = if aug { op.trim_end_matches('=') } else { op };
        let bin_method = bin_op_method(base_op).ok_or(ErrKind::Inference)?;
        let reflected = reflected_name(bin_method);
        let mut methods: Vec<Try> = Vec::new();
        let same_type = match (&left_type, &right_type) {
            (Some(l), Some(r)) => self.qname(*l) == self.qname(*r),
            _ => false,
        };
        if aug {
            let aug_method = format!("__i{}__", &bin_method[2..bin_method.len() - 2]);
            methods.push(Try {
                instance: left.clone(),
                method_name: aug_method,
                other: right.clone(),
            });
        }
        if same_type {
            methods.push(Try {
                instance: left.clone(),
                method_name: bin_method.to_string(),
                other: right.clone(),
            });
        } else {
            let subtype = match (&left_type, &right_type) {
                (Some(l), Some(r)) => self.is_subtype(*l, *r),
                _ => Err(ErrKind::Inference),
            };
            let supertype = match (&left_type, &right_type) {
                (Some(l), Some(r)) => self.is_supertype(*l, *r),
                _ => Err(ErrKind::Inference),
            };
            match (subtype, supertype) {
                (Ok(true), _) => {
                    methods.push(Try {
                        instance: left.clone(),
                        method_name: bin_method.to_string(),
                        other: right.clone(),
                    });
                }
                (_, Ok(true)) => {
                    methods.push(Try {
                        instance: right.clone(),
                        method_name: reflected.clone(),
                        other: left.clone(),
                    });
                    methods.push(Try {
                        instance: left.clone(),
                        method_name: bin_method.to_string(),
                        other: right.clone(),
                    });
                }
                (Err(ErrKind::Inference), _) | (_, Err(ErrKind::Inference))
                    if left_type.is_none() || right_type.is_none() =>
                {
                    methods.push(Try {
                        instance: left.clone(),
                        method_name: bin_method.to_string(),
                        other: right.clone(),
                    });
                    methods.push(Try {
                        instance: right.clone(),
                        method_name: reflected.clone(),
                        other: left.clone(),
                    });
                }
                (Err(ErrKind::AstroidType), _) | (_, Err(ErrKind::AstroidType)) => {
                    // _NonDeducibleTypeHierarchy -> Uninferable
                    return Ok(vec![Value::Uninferable]);
                }
                _ => {
                    methods.push(Try {
                        instance: left.clone(),
                        method_name: bin_method.to_string(),
                        other: right.clone(),
                    });
                    methods.push(Try {
                        instance: right.clone(),
                        method_name: reflected.clone(),
                        other: left.clone(),
                    });
                }
            }
        }
        // PEP 604: X | Y on classes
        if op == "|" {
            let both_union_able = self.is_union_able(left) && self.is_union_able(right);
            if both_union_able {
                methods.push(Try {
                    instance: Value::UnionType,
                    method_name: "__or_union__".to_string(),
                    other: right.clone(),
                });
            }
        }
        for m in methods {
            if m.method_name == "__or_union__" {
                return Ok(vec![Value::UnionType]);
            }
            match self.invoke_binop_inference(&m.instance, base_op, &m.other, &m.method_name, ctx) {
                Ok(results) => {
                    if results.iter().any(|r| r.is_uninferable()) {
                        return Ok(vec![Value::Uninferable]);
                    }
                    let n_notimpl = results
                        .iter()
                        .filter(|r|

                            matches!(self.value_const(r), Some(ConstValue::NotImplemented)))
                        .count();
                    if n_notimpl == results.len() && !results.is_empty() {
                        continue;
                    }
                    if n_notimpl > 0 {
                        return Ok(vec![Value::Uninferable]);
                    }
                    return Ok(results);
                }
                Err(ErrKind::Attribute) => continue,
                Err(ErrKind::Inference) => return Ok(vec![Value::Uninferable]),
                Err(e) => return Err(e),
            }
        }
        // BadBinaryOperationMessage — public inference yields Uninferable
        Ok(vec![Value::Uninferable])
    }

    fn is_union_able(&self, v: &Value) -> bool {
        match v {
            Value::UnionType => true,
            Value::Node(g) => {
                let md = self.md(g.m);
                matches!(
                    md.tree.nodes[g.n.idx()].kind,
                    NodeKind::ClassDef(_) | NodeKind::Const(ConstValue::None)
                )
            }
            Value::SynthConst(c) => matches!(**c, ConstValue::None),
            _ => false,
        }
    }

    /// _invoke_binop_inference (_base_nodes.py:386-423)
    fn invoke_binop_inference(
        &self,
        instance: &Value,
        op: &str,
        other: &Value,
        method_name: &str,
        ctx: &Rc<Ctx>,
    ) -> Result<Vec<Value>, ErrKind> {
        let sym = self.sym(method_name);
        let methods = self.dunder_lookup(instance, sym)?;
        let context = bind_context_to_node(Some(ctx), instance.clone());
        let method = *methods.first().ok_or(ErrKind::Attribute)?;
        *context.callcontext.borrow_mut() = Some(Rc::new(CallCtx {
            id: self.next_callctx_id(),
            args: RefCell::new(vec![NV::V(other.clone())]),
            keywords: RefCell::new(Vec::new()),
            callee: RefCell::new(Some(Value::Node(method))),
        }));
        // str % special-case
        if op == "%" {
            if let Some(ConstValue::Str(fmt)) = self.value_const(instance) {
                return Ok(self.infer_old_style_string_formatting(&fmt, other, &context));
            }
        }
        // inferred = next(method.infer(context)) — single pull
        // (_base_nodes.py:404-409)
        let inferred = self
            .first_value(method, &context)
            .ok()
            .flatten()
            .ok_or(ErrKind::Inference)?;
        if inferred.is_uninferable() {
            return Err(ErrKind::Inference);
        }
        // per-type infer_binary_op
        self.infer_binary_op_impl(instance, op, other, &context, &inferred)
    }

    /// dunder_lookup.lookup (interpreter/dunder_lookup.py)
    fn dunder_lookup(&self, v: &Value, name: GSym) -> Result<Vec<GNode>, ErrKind> {
        match v {
            // literal containers + Const: proxied class locals only
            Value::SynthConst(_) | Value::SynthSeq { .. } | Value::SynthDict { .. }
            | Value::FrozenSet { .. } => {
                let cls = self.proxied_class(v).ok_or(ErrKind::Attribute)?;
                let res = self.class_locals_get(cls, name);
                if res.is_empty() {
                    Err(ErrKind::Attribute)
                } else {
                    Ok(res)
                }
            }
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(_)
                    | NodeKind::List { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::Set { .. }
                    | NodeKind::Dict { .. } => {
                        let cls = self.proxied_class(v).ok_or(ErrKind::Attribute)?;
                        let res = self.class_locals_get(cls, name);
                        if res.is_empty() {
                            Err(ErrKind::Attribute)
                        } else {
                            Ok(res)
                        }
                    }
                    NodeKind::ClassDef(_) => {
                        let res = self.dunder_lookup_class(*g, name);
                        if res.is_empty() {
                            Err(ErrKind::Attribute)
                        } else {
                            Ok(res)
                        }
                    }
                    _ => Err(ErrKind::Attribute),
                }
            }
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
                let mut res = self.class_locals_get(*cls, name);
                for anc in self.ancestors(*cls, true, None) {
                    res.extend(self.class_locals_get(anc, name));
                }
                if res.is_empty() {
                    Err(ErrKind::Attribute)
                } else {
                    Ok(res)
                }
            }
            Value::Generator { .. } => {
                let cls = self.proxied_class(v).ok_or(ErrKind::Attribute)?;
                let mut res = self.class_locals_get(cls, name);
                for anc in self.ancestors(cls, true, None) {
                    res.extend(self.class_locals_get(anc, name));
                }
                if res.is_empty() {
                    Err(ErrKind::Attribute)
                } else {
                    Ok(res)
                }
            }
            _ => Err(ErrKind::Attribute),
        }
    }

    /// _class_lookup: dunders on a class go to its METACLASS
    fn dunder_lookup_class(&self, cls: GNode, name: GSym) -> Vec<GNode> {
        match self.metaclass(cls, None) {
            Some(Value::Node(meta)) => {
                let mut res = self.class_locals_get(meta, name);
                if res.is_empty() {
                    for anc in self.ancestors(meta, true, None) {
                        res.extend(self.class_locals_get(anc, name));
                    }
                }
                res
            }
            _ => Vec::new(),
        }
    }

    /// per-type infer_binary_op
    fn infer_binary_op_impl(
        &self,
        instance: &Value,
        op: &str,
        other: &Value,
        ctx: &Rc<Ctx>,
        method_inferred: &Value,
    ) -> Result<Vec<Value>, ErrKind> {
        // Const
        if let Some(lc) = self.value_const(instance) {
            if let Some(rc) = self.value_const(other) {
                return Ok(vec![const_binop_fold(&lc, op, &rc)]);
            }
            // str % nonconst handled earlier; other type -> NotImplemented
            if matches!(lc, ConstValue::Str(_)) && op == "%" {
                return Ok(vec![Value::Uninferable]);
            }
            return Ok(vec![Value::SynthConst(Rc::new(ConstValue::NotImplemented))]);
        }
        // Tuple / List
        if let Some((kind, elems)) = self.value_seq_parts(instance) {
            if matches!(kind, SeqKind::Tuple | SeqKind::List) {
                if op == "+" {
                    if let Some((okind, oelems)) = self.value_seq_parts(other) {
                        if okind == kind {
                            let mut all = Vec::new();
                            for e in elems.iter().chain(oelems.iter()) {
                                // _filter_uninferable_nodes infers each elt
                                let inferred = match e {
                                    NV::N(g) => self
                                        .infer(*g, &ctx.clone_ctx())
                                        .vals
                                        .first()
                                        .cloned()
                                        .unwrap_or(Value::Uninferable),
                                    NV::V(v) => v.clone(),
                                };
                                all.push(inferred);
                            }
                            return Ok(vec![Value::SynthSeq {
                                kind,
                                elems: Rc::new(all),
                            }]);
                        }
                    }
                    return Ok(vec![Value::SynthConst(Rc::new(ConstValue::NotImplemented))]);
                }
                if op == "*" {
                    if let Some(ConstValue::Int(IntValue::Small(n))) = self.value_const(other) {
                        let n = n.max(0) as usize;
                        if elems.len().saturating_mul(n) > 100_000_000 {
                            return Ok(vec![Value::Uninferable]);
                        }
                        let mut all = Vec::new();
                        for _ in 0..n {
                            for e in &elems {
                                all.push(match e {
                                    NV::N(g) => self
                                        .infer(*g, &ctx.clone_ctx())
                                        .vals
                                        .first()
                                        .cloned()
                                        .unwrap_or(Value::Uninferable),
                                    NV::V(v) => v.clone(),
                                });
                            }
                        }
                        return Ok(vec![Value::SynthSeq {
                            kind,
                            elems: Rc::new(all),
                        }]);
                    }
                    return Ok(vec![Value::SynthConst(Rc::new(ConstValue::NotImplemented))]);
                }
                return Ok(vec![Value::SynthConst(Rc::new(ConstValue::NotImplemented))]);
            }
        }
        // Instance / ClassDef: infer the dunder's return
        match method_inferred {
            m if self.has_infer_call_result(m) => {
                let f = self.infer_call_result(m, None, Some(ctx));
                if f.vals.is_empty() {
                    Ok(vec![Value::Uninferable])
                } else {
                    Ok(f.vals)
                }
            }
            _ => Ok(vec![Value::Uninferable]),
        }
    }

    fn value_seq_parts(&self, v: &Value) -> Option<(SeqKind, Vec<NV>)> {
        match v {
            Value::SynthSeq { kind, elems } => Some((
                *kind,
                elems
                    .iter()
                    .map(|e| match e {
                        Value::Node(g) => NV::N(*g),
                        o => NV::V(o.clone()),
                    })
                    .collect(),
            )),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::List { elts, .. } => Some((
                        SeqKind::List,
                        elts.iter().map(|&e| NV::N(GNode { m: g.m, n: e })).collect(),
                    )),
                    NodeKind::Tuple { elts, .. } => Some((
                        SeqKind::Tuple,
                        elts.iter().map(|&e| NV::N(GNode { m: g.m, n: e })).collect(),
                    )),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// _infer_old_style_string_formatting (subset: Const rhs / all-Const
    /// Tuple / Const-keyed Dict folds; anything else Uninferable)
    fn infer_old_style_string_formatting(
        &self,
        fmt: &str,
        other: &Value,
        ctx: &Rc<Ctx>,
    ) -> Vec<Value> {
        // exact port of _base_nodes.py:350-384
        enum Branch {
            Tuple(Vec<Value>),          // safe-inferred positional elements
            Dict(Vec<(GNode, GNode)>),  // raw item nodes
            SynthDictV(Vec<(Value, Value)>),
            One(ConstValue),
            Other,
        }
        let branch = match other {
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Branch::One(c.clone()),
                    NodeKind::Tuple { elts, .. } => {
                        let elem_nodes: Vec<GNode> =
                            elts.iter().map(|&e| GNode { m: g.m, n: e }).collect();
                        drop(md);
                        let mut inferred = Vec::new();
                        for e in elem_nodes {
                            // util.safe_infer(i, context); None (ambiguous /
                            // error) is NOT a Const -> values = None
                            match self.safe_infer(e, &copy_context(Some(ctx))) {
                                Some(v) => inferred.push(v),
                                None => inferred.push(Value::Uninferable),
                            }
                        }
                        Branch::Tuple(inferred)
                    }
                    NodeKind::Dict { items, .. } => Branch::Dict(
                        items
                            .iter()
                            .map(|(k, v)| (GNode { m: g.m, n: *k }, GNode { m: g.m, n: *v }))
                            .collect(),
                    ),
                    _ => Branch::Other,
                }
            }
            Value::SynthConst(c) => Branch::One((**c).clone()),
            Value::SynthSeq { kind: SeqKind::Tuple, elems } => {
                // synthetic Tuples (CallSite varargs) hold values already;
                // `util.Uninferable in other.elts` -> single U
                if elems.iter().any(|e| e.is_uninferable()) {
                    return vec![Value::Uninferable];
                }
                Branch::Tuple(
                    elems
                        .iter()
                        .map(|e| self.safe_infer_value(e).unwrap_or(Value::Uninferable))
                        .collect(),
                )
            }
            Value::SynthDict { items } => Branch::SynthDictV(items.to_vec()),
            _ => Branch::Other,
        };
        let args: Option<PctArgs> = match branch {
            Branch::One(c) => Some(PctArgs::One(c)),
            Branch::Tuple(vals) => {
                let mut consts = Vec::new();
                let mut all_const = true;
                for v in &vals {
                    match self.value_const(v) {
                        Some(c) => consts.push(c),
                        None => {
                            all_const = false;
                            break;
                        }
                    }
                }
                if all_const {
                    Some(PctArgs::Many(consts))
                } else {
                    // values = None -> `fmt % None` (single None value)
                    Some(PctArgs::One(ConstValue::None))
                }
            }
            Branch::Dict(items) => {
                let mut map: Vec<(String, ConstValue)> = Vec::new();
                for (k, v) in items {
                    let kc = self
                        .safe_infer(k, &copy_context(Some(ctx)))
                        .and_then(|x| self.value_const(&x));
                    let Some(ConstValue::Str(ks)) = kc else {
                        // non-Const key -> (Uninferable,) ... astroid also
                        // requires Const but ANY const key works as mapping
                        // key; non-str Const keys can't be addressed by
                        // %(name)s anyway — treat non-Const as U
                        match kc {
                            Some(_) => return vec![Value::Uninferable],
                            None => return vec![Value::Uninferable],
                        }
                    };
                    let vc = self
                        .safe_infer(v, &copy_context(Some(ctx)))
                        .and_then(|x| self.value_const(&x));
                    match vc {
                        Some(c) => map.push((ks.to_string(), c)),
                        None => return vec![Value::Uninferable],
                    }
                }
                Some(PctArgs::Mapping(map))
            }
            Branch::SynthDictV(items) => {
                let mut map: Vec<(String, ConstValue)> = Vec::new();
                for (k, v) in items {
                    let kc = self
                        .safe_infer_value(&k)
                        .and_then(|x| self.value_const(&x));
                    let Some(ConstValue::Str(ks)) = kc else {
                        return vec![Value::Uninferable];
                    };
                    match self.safe_infer_value(&v).and_then(|x| self.value_const(&x)) {
                        Some(c) => map.push((ks.to_string(), c)),
                        None => return vec![Value::Uninferable],
                    }
                }
                Some(PctArgs::Mapping(map))
            }
            Branch::Other => None, // -> (Uninferable,)
        };
        match args.and_then(|a| pct_format(fmt, &a)) {
            Some(s) => vec![Value::SynthConst(Rc::new(ConstValue::Str(s.into())))],
            None => vec![Value::Uninferable],
        }
    }

    /// UnaryOp._infer with message filtering
    pub fn infer_unaryop_filtered(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (op, operand) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::UnaryOp { op, operand } => {
                (op.clone(), GNode { m: node.m, n: *operand })
            }
            _ => return Flow::err(ErrKind::Inference),
        };
        let flow = self.infer(operand, ctx);
        let mut out = Vec::new();
        for operand_v in &flow.vals {
            if operand_v.is_uninferable() {
                out.push(Value::Uninferable);
                continue;
            }
            // Const/containers: direct infer_unary_op
            if let Some(c) = self.value_const(operand_v) {
                match const_unary_fold(&c, &op) {
                    Some(v) => out.push(v),
                    None => out.push(Value::Uninferable), // Bad message -> U
                }
                continue;
            }
            if op.as_ref() == "not" {
                match self.bool_value(operand_v, ctx) {
                    Some(b) => out.push(Value::SynthConst(Rc::new(ConstValue::Bool(!b)))),
                    None => out.push(Value::Uninferable),
                }
                continue;
            }
            // containers with unary op -> TypeError -> Bad -> U
            if self.value_elts(operand_v).is_some() {
                out.push(Value::Uninferable);
                continue;
            }
            // Instance/ClassDef: dunder lookup
            let meth_name = match op.as_ref() {
                "+" => "__pos__",
                "-" => "__neg__",
                "~" => "__invert__",
                _ => {
                    out.push(Value::Uninferable);
                    continue;
                }
            };
            let is_inst_or_class = matches!(operand_v, Value::Inst { .. } | Value::ExcInst { .. })
                || matches!(operand_v, Value::Node(g)
                    if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))));
            if !is_inst_or_class {
                out.push(Value::Uninferable); // BadUnaryOperationMessage
                continue;
            }
            let sym = self.sym(meth_name);
            match self.dunder_lookup(operand_v, sym) {
                Err(_) => out.push(Value::Uninferable), // Bad message
                Ok(methods) => {
                    let Some(&meth) = methods.first() else {
                        out.push(Value::Uninferable);
                        continue;
                    };
                    // inferred = next(meth.infer()) — single pull
                    let Ok(Some(inferred)) = self.first_value(meth, &copy_context(Some(ctx)))
                    else {
                        continue;
                    };
                    if inferred.is_uninferable() || !self.value_callable(&inferred, ctx) {
                        continue;
                    }
                    let c2 = bind_context_to_node(Some(ctx), operand_v.clone());
                    *c2.callcontext.borrow_mut() = Some(Rc::new(CallCtx {
                        id: self.next_callctx_id(),
                        args: RefCell::new(Vec::new()),
                        keywords: RefCell::new(Vec::new()),
                        callee: RefCell::new(Some(inferred.clone())),
                    }));
                    // result = next(inferred.infer_call_result(self, ctx), None)
                    match self.infer_call_result_first(&inferred, None, Some(&c2)) {
                        Ok(Some(r)) => out.push(r),
                        Ok(None) => out.push(operand_v.clone()),
                        Err(e) if e.is_inference() => out.push(Value::Uninferable),
                        Err(_) => out.push(Value::Uninferable),
                    }
                }
            }
        }
        if out.is_empty() {
            if let Some(e) = flow.err {
                return Flow::err(e);
            }
        }
        Flow {
            vals: out,
            err: flow.err,
        }
    }

    /// helpers.object_type — None == Uninferable
    /// helpers.object_type over a NODE: full inference, set-collapse.
    pub fn object_type_of_node(&self, node: GNode, ctx: &Rc<Ctx>) -> Value {
        let b = self.builtins();
        let flow = self.infer(node, &copy_context(Some(ctx)));
        if flow.err.map(|e| e.is_inference()).unwrap_or(false) {
            // InferenceError anywhere in _object_type -> Uninferable
            return Value::Uninferable;
        }
        #[derive(PartialEq, Eq, Hash)]
        enum Entry {
            Class(GNode),
            Fresh(u32), // unique per occurrence (fresh build_class)
            Uninferable,
        }
        let mut set: rustc_hash::FxHashSet<Entry> = Default::default();
        let mut fresh = 0u32;
        let mut last: Option<Value> = None;
        for v in &flow.vals {
            let entry = match v {
                Value::Uninferable => {
                    last = match set.contains(&Entry::Uninferable) {
                        true => last,
                        false => Some(Value::Uninferable),
                    };
                    Entry::Uninferable
                }
                Value::Node(g)
                    if self.kind_is(*g, |k| matches!(k, NodeKind::Unknown)) =>
                {
                    return Value::Uninferable; // raise InferenceError
                }
                _ => match self.object_type(v, ctx) {
                    Some(t) => {
                        let is_fresh = {
                            // function/method/module proxies are fresh objects
                            t == b.function
                                || t == b.builtin_function_or_method
                                || t == b.method
                                || t == b.module
                        };
                        last = Some(Value::Node(t));
                        if is_fresh {
                            fresh += 1;
                            Entry::Fresh(fresh)
                        } else {
                            Entry::Class(t)
                        }
                    }
                    None => return Value::Uninferable,
                },
            };
            set.insert(entry);
        }
        if set.len() != 1 {
            return Value::Uninferable;
        }
        last.unwrap_or(Value::Uninferable)
    }

    pub fn object_type(&self, v: &Value, ctx: &Rc<Ctx>) -> Option<GNode> {
        let b = self.builtins();
        match v {
            Value::Uninferable => None,
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::ClassDef(_) => match self.metaclass(*g, Some(ctx)) {
                        Some(Value::Node(meta)) => Some(meta),
                        _ => Some(b.type_),
                    },
                    NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::Lambda(_) => {
                        // _function_type (helpers.py): builtins-rooted
                        // functions proxy builtin_function_or_method
                        if md.name == "builtins" {
                            Some(b.builtin_function_or_method)
                        } else {
                            Some(b.function)
                        }
                    }
                    NodeKind::Module(_) => Some(b.module),
                    NodeKind::Unknown => None,
                    NodeKind::Slice { .. } => Some(b.slice),
                    _ => self.proxied_class(v),
                }
            }
            Value::BoundMethod { .. } => Some(b.method),
            Value::UnboundMethod { .. } => Some(b.function),
            Value::Property { .. } | Value::Partial { .. } => Some(b.function),
            Value::Super { .. } => Some(b.super_),
            _ => self.proxied_class(v),
        }
    }

    /// helpers.is_subtype / is_supertype with _NonDeducibleTypeHierarchy
    /// (-> Err(AstroidType) stands in for the sentinel)
    fn is_subtype(&self, t1: GNode, t2: GNode) -> Result<bool, ErrKind> {
        self.type_check(t2, t1)
    }
    fn is_supertype(&self, t1: GNode, t2: GNode) -> Result<bool, ErrKind> {
        self.type_check(t1, t2)
    }
    fn type_check(&self, target: GNode, klass: GNode) -> Result<bool, ErrKind> {
        if !self.has_known_bases(target, 0) || !self.has_known_bases(klass, 0) {
            return Err(ErrKind::AstroidType);
        }
        match self.mro(klass, None) {
            Ok(mro) => {
                let upto = mro.len().saturating_sub(1);
                Ok(mro[..upto].contains(&target))
            }
            Err(_) => Err(ErrKind::AstroidType),
        }
    }

    pub fn has_known_bases(&self, cls: GNode, depth: u32) -> bool {
        if depth > 50 {
            return false;
        }
        for base in self.class_bases(cls) {
            let v = self.safe_infer(base, &Ctx::new());
            match v {
                Some(Value::Node(g))
                    if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) && g != cls =>
                {
                    if !self.has_known_bases(g, depth + 1) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

// ---------- helpers ----------

pub struct PySlice {
    pub start: Option<i64>,
    pub stop: Option<i64>,
    pub step: Option<i64>,
}

fn const_as_int(c: Option<&ConstValue>) -> Option<i64> {
    match c {
        Some(ConstValue::Int(IntValue::Small(i))) => Some(*i),
        _ => None,
    }
}

fn norm_index(i: i64, len: usize) -> Option<usize> {
    let len = len as i64;
    let idx = if i < 0 { i + len } else { i };
    if idx < 0 || idx >= len {
        None
    } else {
        Some(idx as usize)
    }
}

/// Python slice semantics over a vec
fn slice_seq<T: Clone>(v: &[T], sl: &PySlice) -> Vec<T> {
    let len = v.len() as i64;
    let step = sl.step.unwrap_or(1);
    if step == 0 {
        return Vec::new();
    }
    let (mut start, stop) = if step > 0 {
        (
            clamp_index(sl.start.unwrap_or(0), len, false),
            clamp_index(sl.stop.unwrap_or(len), len, false),
        )
    } else {
        (
            clamp_index(sl.start.unwrap_or(len - 1), len, true),
            match sl.stop {
                None => -1,
                Some(s) => clamp_index(s, len, true),
            },
        )
    };
    let mut out = Vec::new();
    if step > 0 {
        while start < stop {
            if start >= 0 && start < len {
                out.push(v[start as usize].clone());
            }
            start += step;
        }
    } else {
        while start > stop {
            if start >= 0 && start < len {
                out.push(v[start as usize].clone());
            }
            start += step;
        }
    }
    out
}

fn clamp_index(i: i64, len: i64, neg_step: bool) -> i64 {
    let mut idx = if i < 0 { i + len } else { i };
    if neg_step {
        if idx < -1 {
            idx = -1;
        }
        if idx > len - 1 {
            idx = len - 1;
        }
    } else {
        if idx < 0 {
            idx = 0;
        }
        if idx > len {
            idx = len;
        }
    }
    idx
}

fn const_itered(c: &ConstValue) -> Option<Vec<Value>> {
    match c {
        ConstValue::Str(s) => Some(
            s.chars()
                .map(|ch| {
                    Value::SynthConst(Rc::new(ConstValue::Str(ch.to_string().into())))
                })
                .collect(),
        ),
        ConstValue::Bytes(b) => Some(
            b.iter()
                .map(|&byte| {
                    Value::SynthConst(Rc::new(ConstValue::Int(IntValue::Small(byte as i64))))
                })
                .collect(),
        ),
        _ => None,
    }
}

pub fn const_eq(a: &ConstValue, b: &ConstValue) -> bool {
    use ConstValue::*;
    match (a, b) {
        (Str(x), Str(y)) => x == y,
        (Bytes(x), Bytes(y)) => x == y,
        (None, None) => true,
        (Ellipsis, Ellipsis) => true,
        _ => {
            let na = num_of(a);
            let nb = num_of(b);
            match (na, nb) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            }
        }
    }
}

fn num_of(c: &ConstValue) -> Option<f64> {
    match c {
        ConstValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        ConstValue::Int(IntValue::Small(i)) => Some(*i as f64),
        ConstValue::Float(f) => Some(*f),
        _ => None,
    }
}

fn bin_op_method(op: &str) -> Option<&'static str> {
    Some(match op {
        "+" => "__add__",
        "-" => "__sub__",
        "*" => "__mul__",
        "/" => "__truediv__",
        "//" => "__floordiv__",
        "%" => "__mod__",
        "**" => "__pow__",
        "<<" => "__lshift__",
        ">>" => "__rshift__",
        "&" => "__and__",
        "|" => "__or__",
        "^" => "__xor__",
        "@" => "__matmul__",
        _ => return None,
    })
}

fn reflected_name(method: &str) -> String {
    format!("__r{}", &method[2..])
}

/// BIN_OP_IMPL const folding (protocols.py:103-136)
/// int-like operand for sequence repetition: Some(Some(n)) for small
/// ints/bools, Some(None) for ints overflowing Py_ssize_t (OverflowError),
/// None for non-ints
fn int_index(c: &ConstValue) -> Option<Option<i64>> {
    match c {
        ConstValue::Int(IntValue::Small(i)) => Some(Some(*i)),
        ConstValue::Bool(b) => Some(Some(*b as i64)),
        ConstValue::Int(IntValue::Big(_)) => Some(None),
        _ => None,
    }
}

fn const_binop_fold(l: &ConstValue, op: &str, r: &ConstValue) -> Value {
    use ConstValue::*;
    let notimpl = || Value::SynthConst(Rc::new(NotImplemented));
    let int = |i: i64| Value::SynthConst(Rc::new(Int(IntValue::Small(i))));
    let float = |f: f64| Value::SynthConst(Rc::new(Float(f)));
    let sstr = |s: String| Value::SynthConst(Rc::new(Str(s.into())));
    // ** anti-DoS guard
    if op == "**" {
        let big = |c: &ConstValue| match c {
            Int(IntValue::Small(i)) => *i > 100_000,
            Float(f) => *f > 1e5,
            _ => false,
        };
        if big(r) {
            return notimpl();
        }
    }
    let num_l = num_of(l);
    let num_r = num_of(r);
    let is_int = |c: &ConstValue| matches!(c, Int(_) | Bool(_));
    match op {
        "+" => match (l, r) {
            (Str(a), Str(b)) => sstr(format!("{a}{b}")),
            (Bytes(a), Bytes(b)) => {
                let mut v = a.to_vec();
                v.extend(b.iter());
                Value::SynthConst(Rc::new(Bytes(v.into())))
            }
            _ => match (num_l, num_r) {
                (Some(a), Some(b)) => {
                    if is_int(l) && is_int(r) {
                        int((a + b) as i64)
                    } else {
                        float(a + b)
                    }
                }
                _ => notimpl(),
            },
        },
        "-" => match (num_l, num_r) {
            (Some(a), Some(b)) => {
                if is_int(l) && is_int(r) {
                    int((a - b) as i64)
                } else {
                    float(a - b)
                }
            }
            _ => notimpl(),
        },
        "*" => match (l, r) {
            // seq * int / int * seq: python evaluates natively
            // (BIN_OP_IMPL "*", protocols.py const_infer_binary_op);
            // bool counts as int (True * "x" == "x"); ints beyond
            // Py_ssize_t raise OverflowError -> except Exception ->
            // Uninferable; giant results approximate MemoryError -> U
            (Str(a), n) | (n, Str(a)) if int_index(n).is_some() => match int_index(n).unwrap()
            {
                Option::Some(k) => {
                    let k = k.max(0) as usize;
                    if a.len().saturating_mul(k) > 100_000_000 {
                        Value::Uninferable
                    } else {
                        sstr(a.repeat(k))
                    }
                }
                Option::None => Value::Uninferable,
            },
            (Bytes(b), n) | (n, Bytes(b)) if int_index(n).is_some() => match int_index(n)
                .unwrap()
            {
                Option::Some(k) => {
                    let k = k.max(0) as usize;
                    if b.len().saturating_mul(k) > 100_000_000 {
                        Value::Uninferable
                    } else {
                        Value::SynthConst(Rc::new(Bytes(b.repeat(k).into())))
                    }
                }
                Option::None => Value::Uninferable,
            },
            _ => match (num_l, num_r) {
                (Some(a), Some(b)) => {
                    if is_int(l) && is_int(r) {
                        int((a * b) as i64)
                    } else {
                        float(a * b)
                    }
                }
                _ => notimpl(),
            },
        },
        "/" => match (num_l, num_r) {
            (Some(_), Some(b)) if b == 0.0 => Value::Uninferable, // ZeroDivisionError
            (Some(a), Some(b)) => float(a / b),
            _ => notimpl(),
        },
        "//" => match (num_l, num_r) {
            (Some(_), Some(b)) if b == 0.0 => Value::Uninferable,
            (Some(a), Some(b)) => {
                if is_int(l) && is_int(r) {
                    int((a / b).floor() as i64)
                } else {
                    float((a / b).floor())
                }
            }
            _ => notimpl(),
        },
        "%" => match (num_l, num_r) {
            (Some(_), Some(b)) if b == 0.0 => Value::Uninferable,
            (Some(a), Some(b)) => {
                let rem = a - (a / b).floor() * b;
                if is_int(l) && is_int(r) {
                    int(rem as i64)
                } else {
                    float(rem)
                }
            }
            _ => notimpl(),
        },
        "**" => match (num_l, num_r) {
            (Some(a), Some(b)) => {
                if is_int(l) && is_int(r) && b >= 0.0 {
                    int(a.powf(b) as i64)
                } else {
                    float(a.powf(b))
                }
            }
            _ => notimpl(),
        },
        "<<" | ">>" | "&" | "|" | "^" => match (l, r) {
            (Int(IntValue::Small(a)), Int(IntValue::Small(b))) => {
                let v = match op {
                    "<<" => {
                        if *b > 10_000 {
                            return Value::Uninferable;
                        }
                        a.checked_shl(*b as u32).unwrap_or(0)
                    }
                    ">>" => a.checked_shr(*b as u32).unwrap_or(0),
                    "&" => a & b,
                    "|" => a | b,
                    "^" => a ^ b,
                    _ => unreachable!(),
                };
                int(v)
            }
            _ => notimpl(),
        },
        _ => notimpl(),
    }
}

fn const_unary_fold(c: &ConstValue, op: &str) -> Option<Value> {
    use ConstValue as C;
    let some = |c: ConstValue| Some(Value::SynthConst(Rc::new(c)));
    match op {
        "not" => some(C::Bool(!crate::infer::const_truth(c))),
        "-" => match c {
            C::Int(IntValue::Small(i)) => some(C::Int(IntValue::Small(-i))),
            C::Float(f) => some(C::Float(-f)),
            C::Bool(b) => some(C::Int(IntValue::Small(if *b { -1 } else { 0 }))),
            C::NotImplemented => some(C::NotImplemented),
            _ => Option::None, // TypeError -> BadUnaryOperationMessage
        },
        "+" => match c {
            C::Int(_) | C::Float(_) | C::Bool(_) => some(c.clone()),
            C::NotImplemented => some(C::NotImplemented),
            _ => Option::None,
        },
        "~" => match c {
            C::Int(IntValue::Small(i)) => some(C::Int(IntValue::Small(!i))),
            C::Bool(b) => some(C::Int(IntValue::Small(if *b { -2 } else { -1 }))),
            C::NotImplemented => some(C::NotImplemented),
            _ => Option::None,
        },
        _ => Option::None,
    }
}

#[allow(dead_code)]
enum PctArgs {
    One(ConstValue),
    Many(Vec<ConstValue>),
    Mapping(Vec<(String, ConstValue)>),
}

/// minimal %-format folding for Const operands
fn pct_format(fmt: &str, args: &PctArgs) -> Option<String> {
    let (values, mapping): (Vec<&ConstValue>, Option<&Vec<(String, ConstValue)>>) = match args {
        PctArgs::One(c) => (vec![c], None),
        PctArgs::Many(v) => (v.iter().collect(), None),
        PctArgs::Mapping(m) => (Vec::new(), Some(m)),
    };
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut vi = 0usize;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '%' {
            out.push(c);
            continue;
        }
        if i < chars.len() && chars[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        // %(key) for mappings
        let mut key: Option<String> = None;
        if i < chars.len() && chars[i] == '(' {
            i += 1;
            let mut k = String::new();
            loop {
                if i >= chars.len() {
                    return None; // ValueError: incomplete format key
                }
                if chars[i] == ')' {
                    i += 1;
                    break;
                }
                k.push(chars[i]);
                i += 1;
            }
            key = Some(k);
        }
        // flags
        let mut minus = false;
        let mut plus = false;
        let mut space = false;
        let mut zero = false;
        let mut alt = false;
        while i < chars.len() {
            match chars[i] {
                '-' => minus = true,
                '+' => plus = true,
                ' ' => space = true,
                '0' => zero = true,
                '#' => alt = true,
                _ => break,
            }
            i += 1;
        }
        // width
        let mut width = 0usize;
        let mut has_width = false;
        while i < chars.len() && chars[i].is_ascii_digit() {
            has_width = true;
            width = width * 10 + (chars[i] as usize - '0' as usize);
            i += 1;
        }
        // precision
        let mut prec: Option<usize> = None;
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            let mut p = 0usize;
            while i < chars.len() && chars[i].is_ascii_digit() {
                p = p * 10 + (chars[i] as usize - '0' as usize);
                i += 1;
            }
            prec = Some(p);
        }
        // length modifiers (h/l/L) — ignored by Python
        while i < chars.len() && matches!(chars[i], 'h' | 'l' | 'L') {
            i += 1;
        }
        if i >= chars.len() {
            return None; // ValueError: incomplete format
        }
        let conv = chars[i];
        i += 1;
        let v: &ConstValue = match (&key, mapping) {
            (Some(k), Some(m)) => m.iter().rev().find(|(mk, _)| mk == k).map(|(_, mv)| mv)?,
            (None, Some(_)) => return None, // TypeError: format requires a mapping
            (Some(_), None) => return None,
            (None, None) => {
                let v = values.get(vi)?;
                vi += 1;
                v
            }
        };
        let body: String = match conv {
            's' => {
                let mut t = const_str(v)?;
                if let Some(p) = prec {
                    t.truncate(p);
                }
                t
            }
            'r' => {
                let mut t = match v {
                    ConstValue::Str(s) => pyast::pyrepr::repr_str(s),
                    _ => const_str(v)?,
                };
                if let Some(p) = prec {
                    t.truncate(p);
                }
                t
            }
            'd' | 'i' | 'u' => {
                let n: i64 = match v {
                    ConstValue::Int(IntValue::Small(i)) => *i,
                    ConstValue::Bool(b) => *b as i64,
                    ConstValue::Float(f) => *f as i64,
                    _ => return None,
                };
                format_int_directive(n, plus, space, 10, false, alt)
            }
            'x' | 'X' | 'o' => {
                let n: i64 = match v {
                    ConstValue::Int(IntValue::Small(i)) => *i,
                    ConstValue::Bool(b) => *b as i64,
                    _ => return None, // TypeError for floats/strs
                };
                let base = if conv == 'o' { 8 } else { 16 };
                let mut t = format_int_directive(n, plus, space, base, conv == 'X', alt);
                if alt {
                    // '#': 0x/0o prefix — insert after sign
                    let prefix = match conv {
                        'x' => "0x",
                        'X' => "0X",
                        _ => "0o",
                    };
                    let sign_len = usize::from(t.starts_with(['-', '+', ' ']));
                    t.insert_str(sign_len, prefix);
                }
                t
            }
            'f' | 'F' => {
                let f: f64 = match v {
                    ConstValue::Float(f) => *f,
                    ConstValue::Int(IntValue::Small(i)) => *i as f64,
                    ConstValue::Bool(b) => (*b as i64) as f64,
                    _ => return None,
                };
                let p = prec.unwrap_or(6);
                let mut t = format!("{:.*}", p, f);
                if f >= 0.0 {
                    if plus {
                        t.insert(0, '+');
                    } else if space {
                        t.insert(0, ' ');
                    }
                }
                t
            }
            'c' => match v {
                ConstValue::Str(s) if s.chars().count() == 1 => s.to_string(),
                ConstValue::Int(IntValue::Small(i)) => {
                    char::from_u32(*i as u32).map(String::from)?
                }
                _ => return None,
            },
            _ => return None, // unsupported conversion (e/g/*-width...)
        };
        // width padding
        let padded = if has_width && body.chars().count() < width {
            let pad = width - body.chars().count();
            if minus {
                format!("{}{}", body, " ".repeat(pad))
            } else if zero && matches!(conv, 'd' | 'i' | 'u' | 'x' | 'X' | 'o' | 'f' | 'F') {
                // zero-pad after any sign
                let sign_len = usize::from(body.starts_with(['-', '+', ' ']));
                let (sign, rest) = body.split_at(sign_len);
                format!("{}{}{}", sign, "0".repeat(pad), rest)
            } else {
                format!("{}{}", " ".repeat(pad), body)
            }
        } else {
            body
        };
        out.push_str(&padded);
    }
    if mapping.is_none() && vi != values.len() {
        return None; // TypeError: not all arguments converted
    }
    Some(out)
}

fn format_int_directive(
    n: i64,
    plus: bool,
    space: bool,
    base: u32,
    upper: bool,
    _alt: bool,
) -> String {
    let mag = (n as i128).unsigned_abs();
    let digits = match base {
        8 => format!("{:o}", mag),
        16 => {
            if upper {
                format!("{:X}", mag)
            } else {
                format!("{:x}", mag)
            }
        }
        _ => format!("{}", mag),
    };
    if n < 0 {
        format!("-{digits}")
    } else if plus {
        format!("+{digits}")
    } else if space {
        format!(" {digits}")
    } else {
        digits
    }
}

fn const_str(c: &ConstValue) -> Option<String> {
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
