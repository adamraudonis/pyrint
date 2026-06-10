//! NodeNG.infer dispatch + per-node `_infer` implementations.
//! Ports: astroid/nodes/node_ng.py:121-176, decorators.py, bases.py
//! _infer_stmts, node_classes.py per-node inference (notes/07 §4,6,8,16,17).

use std::rc::Rc;

use pyast::tree::{ConstValue, Ctx as ExprCtx, IntValue, NodeKind};
use pyast::NodeId;

use crate::ctx::{bind_context_to_node, copy_context, CallCtx, Ctx, MAX_INFERABLE_VALUES, MAX_INFERRED};
use crate::graph::Engine;
use crate::snapshot::EInf;
use crate::value::{value_key, Drive, End, ErrKind, Flow, GNode, GSym, SeqKind, Value, NV};

/// Streaming consumer for generator-exact inference (notes/07 §4): each
/// call is one `yield`; returning `Drive::Stop` abandons the producer just
/// like dropping a suspended Python generator.
pub type Sink<'a> = dyn FnMut(Value) -> Drive + 'a;

/// yield one value to a sink, propagating consumer abandonment.
#[macro_export]
macro_rules! yield_v {
    ($sink:expr, $v:expr) => {
        if let Drive::Stop = $sink($v) {
            return End::Stopped;
        }
    };
}

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub enum DedupKey {
    Node(GNode),
    Uninferable,
    /// Python object identity for synthetic values: clones of the same Rc
    /// (e.g. BoolOp's materialized operand lists reused across product
    /// pairs — node_classes.py:1668 itertools.product) are the SAME object
    /// in astroid and dedup in path_wrapper; independently const_factory'd
    /// values are distinct objects and do NOT dedup.
    Ptr(usize),
}

/// path_wrapper dedup identity (decorators.py:25-54): exact-class Instance
/// unproxies to its ClassDef *node*; node-backed values use node identity;
/// Uninferable is a singleton; every other proxy is a fresh object.
pub fn dedup_key(v: &Value) -> Option<DedupKey> {
    match v {
        Value::Node(g) => Some(DedupKey::Node(*g)),
        Value::Inst { cls } => Some(DedupKey::Node(*cls)),
        Value::Uninferable => Some(DedupKey::Uninferable),
        Value::SynthConst(rc) => Some(DedupKey::Ptr(std::rc::Rc::as_ptr(rc) as usize)),
        Value::SynthSeq { elems, .. } => Some(DedupKey::Ptr(std::rc::Rc::as_ptr(elems) as usize)),
        Value::SynthDict { items } => Some(DedupKey::Ptr(std::rc::Rc::as_ptr(items) as usize)),
        Value::FrozenSet { elems } => Some(DedupKey::Ptr(std::rc::Rc::as_ptr(elems) as usize)),
        _ => None,
    }
}

impl Engine {
    // ---------- entry point (NodeNG.infer) ----------

    /// Eager shim over `infer_to` for consumers that exhaust the generator
    /// (astroid `list(node.infer())` semantics).
    pub fn infer(&self, node: GNode, ctx_in: &Rc<Ctx>) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_to(node, ctx_in, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    /// NodeNG.infer (node_ng.py:121-176), streaming.
    pub fn infer_to(&self, node: GNode, ctx_in: &Rc<Ctx>, sink: &mut Sink) -> End {
        if self.depth.get() >= self.max_depth {
            return End::Raised(ErrKind::Recursion);
        }
        // synthetic-class base placeholders standing for raw cross-module
        // nodes (bases.py _infer_type_new_call stores the original Tuple
        // elts as bases): inference goes straight to the original node.
        let node = match self.redirects.borrow().get(&node) {
            Some(NV::N(g)) => *g,
            _ => node,
        };
        self.depth.set(self.depth.get() + 1);
        let r = self.infer_entry_to(node, ctx_in, sink);
        self.depth.set(self.depth.get() - 1);
        r
    }

    fn infer_entry_to(&self, node: GNode, ctx_in: &Rc<Ctx>, sink: &mut Sink) -> End {
        // debug trace (PRYLINT_TRACE_INFER) mirroring the astroid
        // NodeNG.infer monkeypatch used for bump-parity debugging
        if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
            let md = self.md(node.m);
            let kind = crate::treeutil::kind_label(&md.tree.nodes[node.n.idx()].kind);
            let name = self.node_name(node).unwrap_or_default();
            let d = self.depth.get() as usize;
            let ccid = ctx_in.callcontext.borrow().as_ref().map(|c| c.id);
            let bn = ctx_in.boundnode.borrow().is_some();
            eprintln!(
                "{}> {} {} ln={:?} cc={:?} bn={}",
                "  ".repeat(d),
                kind,
                name,
                ctx_in.lookupname.get().map(|s| self.sname(s)),
                ccid,
                bn
            );
            let mut wrapped = |v: Value| -> Drive {
                eprintln!("{}  yield {}", "  ".repeat(d), crate::dump::render(self, &v));
                sink(v)
            };
            return self.infer_entry_to_inner(node, ctx_in, &mut wrapped);
        }
        self.infer_entry_to_inner(node, ctx_in, sink)
    }

    fn infer_entry_to_inner(&self, node: GNode, ctx_in: &Rc<Ctx>, sink: &mut Sink) -> End {
        // extra_context swap (node_ng.py:125-128)
        let ctx = {
            let extra = ctx_in.extra_context.borrow();
            match extra.get(&node) {
                Some(c) => Rc::clone(c),
                None => Rc::clone(ctx_in),
            }
        };
        // explicit inference (inference tips). astroid materializes tip
        // results (inference_tip.py:64-66 list(func(...))), then replays:
        // `context.nodes_inferred += 1; yield result` — bump BEFORE yield.
        if let Some(flow) = self.explicit_inference(node, &ctx) {
            for v in flow.vals {
                ctx.bump_inferred();
                yield_v!(sink, v);
            }
            return match flow.err {
                Some(e) => End::Raised(e),
                None => End::Done,
            };
        }
        let key = (
            node,
            ctx.lookupname.get(),
            ctx.callcontext.borrow().as_ref().map(|c| c.id),
            ctx.boundnode.borrow().as_ref().map(value_key),
        );
        let cached = self.inf_cache.borrow().get(&key).cloned();
        if let Some(cached) = cached {
            // replay without bumping nodes_inferred (node_ng.py:155-157)
            for v in cached.iter() {
                yield_v!(sink, v.clone());
            }
            return End::Done;
        }
        // limit loop (node_ng.py:160-176)
        let mut results: Vec<Value> = Vec::new();
        let mut i: usize = 0;
        let mut truncated = false;
        let mut cache_after_trunc = false;
        let end = {
            let results = &mut results;
            let i = &mut i;
            let truncated = &mut truncated;
            let cache_after_trunc = &mut cache_after_trunc;
            let ctx2 = Rc::clone(&ctx);
            self.infer_dispatch_to(node, &ctx, &mut |v| {
                if *i >= MAX_INFERABLE_VALUES || ctx2.nodes_inferred.get() > MAX_INFERRED {
                    results.push(Value::Uninferable);
                    let d = sink(Value::Uninferable);
                    *truncated = true;
                    // node_ng.py:164-167: `yield Uninferable` SUSPENDS
                    // before `break` — the cache write below the loop only
                    // runs if the consumer pulls again (Drive::Go). A
                    // consumer abandoning at this yield drops the generator
                    // while suspended: NO cache write (probe: os.path attr
                    // chain stays uncached after a capped abspath call).
                    *cache_after_trunc = matches!(d, Drive::Go);
                    return Drive::Stop;
                }
                results.push(v.clone());
                let d = sink(v);
                if let Drive::Stop = d {
                    // consumer abandoned at the yield: the post-yield
                    // `context.nodes_inferred += 1` never runs, and the
                    // cache write is skipped (generator dropped).
                    return Drive::Stop;
                }
                ctx2.bump_inferred();
                *i += 1;
                Drive::Go
            })
        };
        match end {
            End::Done => {
                self.inf_cache.borrow_mut().insert(key, Rc::new(results));
                End::Done
            }
            End::Stopped => {
                if truncated && cache_after_trunc {
                    self.inf_cache.borrow_mut().insert(key, Rc::new(results));
                    End::Done
                } else {
                    End::Stopped
                }
            }
            End::Raised(e) => End::Raised(e), // nothing cached
        }
    }

    /// decorators.py:25-54 path_wrapper, streaming.
    fn path_wrapped_to<F>(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink, f: F) -> End
    where
        F: FnOnce(&Self, &mut Sink) -> End,
    {
        if ctx.push(node) {
            return End::Done; // already on path -> EMPTY generator
        }
        let mut yielded: rustc_hash::FxHashSet<DedupKey> = Default::default();
        let mut wrapped = |v: Value| -> Drive {
            match dedup_key(&v) {
                Some(k) => {
                    if yielded.insert(k) {
                        sink(v)
                    } else {
                        Drive::Go // duplicate: keep pulling
                    }
                }
                None => sink(v),
            }
        };
        f(self, &mut wrapped)
    }

    /// decorators.py:68-96 raise_if_nothing_inferred, streaming.
    fn rin_to<F>(&self, sink: &mut Sink, f: F) -> End
    where
        F: FnOnce(&Self, &mut Sink) -> End,
    {
        let mut any = false;
        let end = {
            let any = &mut any;
            let mut wrapped = |v: Value| -> Drive {
                *any = true;
                sink(v)
            };
            f(self, &mut wrapped)
        };
        match end {
            End::Done if !any => End::Raised(ErrKind::Inference),
            End::Raised(ErrKind::Recursion) if !any => End::Raised(ErrKind::Inference),
            e => e,
        }
    }

    /// decorators.py:57-66 yes_if_nothing_inferred, streaming.
    fn yin_to<F>(&self, sink: &mut Sink, f: F) -> End
    where
        F: FnOnce(&Self, &mut Sink) -> End,
    {
        let mut any = false;
        let end = {
            let any = &mut any;
            let mut wrapped = |v: Value| -> Drive {
                *any = true;
                sink(v)
            };
            f(self, &mut wrapped)
        };
        match end {
            End::Done if !any => {
                let _ = sink(Value::Uninferable);
                End::Done
            }
            e => e,
        }
    }

    /// stream an eagerly-computed Flow (used for node kinds whose astroid
    /// `_infer` materializes everything before yielding anyway).
    fn stream_flow(&self, flow: Flow, sink: &mut Sink) -> End {
        for v in flow.vals {
            yield_v!(sink, v);
        }
        match flow.err {
            Some(e) => End::Raised(e),
            None => End::Done,
        }
    }

    // ---------- per-kind dispatch with decorator table (notes/07 §4.1) ----------

    fn infer_dispatch_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        // EvaluatedObject._infer (node_classes.py EvaluatedObject: yields the
        // stored value) and proxy values stored in synthetic-class locals
        // (enum members): undecorated, yields the value as-is.
        let red = self.redirects.borrow().get(&node).cloned();
        if let Some(NV::V(v)) = red {
            yield_v!(sink, v);
            return End::Done;
        }
        let kind_tag = {
            let md = self.md(node.m);
            std::mem::discriminant(&md.tree.nodes[node.n.idx()].kind);
            // clone the small data we need to avoid holding md across calls
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Name { .. } => 1,
                NodeKind::AssignName { .. } => 2,
                NodeKind::AssignAttr { .. } => 3,
                NodeKind::Attribute { .. } => 4,
                NodeKind::Subscript { .. } => 5,
                NodeKind::Call { .. } => 6,
                NodeKind::AugAssign { .. } => 7,
                NodeKind::BinOp { .. } => 8,
                NodeKind::BoolOp { .. } => 9,
                NodeKind::UnaryOp { .. } => 10,
                NodeKind::Import { .. } => 11,
                NodeKind::ImportFrom { .. } => 12,
                NodeKind::Global { .. } => 13,
                NodeKind::EmptyNode => 14,
                NodeKind::IfExp { .. } => 15,
                NodeKind::List { .. } | NodeKind::Tuple { .. } | NodeKind::Set { .. } => 16,
                NodeKind::Arguments(_) => 17,
                NodeKind::Const(_)
                | NodeKind::Slice { .. }
                | NodeKind::Module(_)
                | NodeKind::ClassDef(_)
                | NodeKind::Lambda(_) => 18,
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => 19,
                NodeKind::Dict { .. } => 20,
                NodeKind::Compare { .. } => 21,
                NodeKind::JoinedStr { .. } => 22,
                NodeKind::FormattedValue { .. } => 23,
                NodeKind::Unknown => 24,
                _ => 0,
            }
        };
        match kind_tag {
            1 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_name_to(node, ctx, s))
            }),
            2 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_assign_name_to(node, ctx, s))
            }),
            3 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_assign_attr_to(node, ctx, s))
            }),
            4 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_attribute_load_to(node, ctx, s))
            }),
            5 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_subscript_to(node, ctx, s))
            }),
            6 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_call_to(node, ctx, s))
            }),
            7 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| {
                    let f = e.infer_augassign_filtered(node, ctx);
                    e.stream_flow(f, s)
                })
            }),
            8 => self.yin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| {
                    let f = e.infer_binop_filtered(node, ctx);
                    e.stream_flow(f, s)
                })
            }),
            9 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| {
                    let f = e.infer_boolop(node, ctx);
                    e.stream_flow(f, s)
                })
            }),
            10 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| {
                    let f = e.infer_unaryop_filtered(node, ctx);
                    e.stream_flow(f, s)
                })
            }),
            11 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| {
                    let f = e.infer_import(node, ctx);
                    e.stream_flow(f, s)
                })
            }),
            12 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_import_from_to(node, ctx, s))
            }),
            13 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_global_to(node, ctx, s))
            }),
            14 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_empty_node_to(node, ctx, s))
            }),
            15 => self.rin_to(sink, |e, s| e.infer_ifexp_to(node, ctx, s)),
            16 => self.rin_to(sink, |e, s| {
                let f = e.infer_container(node, ctx);
                e.stream_flow(f, s)
            }),
            17 => self.rin_to(sink, |e, s| e.infer_arguments_node_to(node, ctx, s)),
            18 => {
                yield_v!(sink, Value::Node(node));
                End::Done
            }
            19 => self.stream_flow(self.infer_functiondef(node, ctx), sink),
            20 => self.stream_flow(self.infer_dict(node, ctx), sink),
            21 => self.stream_flow(self.infer_compare(node, ctx), sink),
            22 => self.stream_flow(self.infer_joinedstr(node, ctx), sink),
            23 => self.stream_flow(self.infer_formatted_value(node, ctx), sink),
            24 => {
                yield_v!(sink, Value::Uninferable);
                End::Done
            }
            // NamedExpr is not directly inferable in astroid 4 (no _infer);
            // default: InferenceError
            _ => End::Raised(ErrKind::Inference),
        }
    }

    // ---------- _infer_stmts (bases.py:153-204) ----------

    /// eager shim
    pub fn infer_stmts(&self, stmts: &[NV], ctx_in: Option<&Rc<Ctx>>, frame: Option<GNode>) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_stmts_to(stmts, ctx_in, frame, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    pub fn infer_stmts_to(
        &self,
        stmts: &[NV],
        ctx_in: Option<&Rc<Ctx>>,
        frame: Option<GNode>,
        sink: &mut Sink,
    ) -> End {
        let mut inferred = false;
        let mut constraint_failed = false;
        let (name, ctx, constraints) = match ctx_in {
            Some(c) => {
                let name = c.lookupname.get();
                let clone = c.clone_ctx();
                let constraints = match name {
                    Some(n) => clone
                        .constraints
                        .borrow()
                        .get(&n)
                        .cloned()
                        .unwrap_or_else(|| Rc::new(Vec::new())),
                    None => Rc::new(Vec::new()),
                };
                (name, clone, constraints)
            }
            None => (None, Ctx::new(), Rc::new(Vec::new())),
        };
        for stmt in stmts {
            let stmt_node = match stmt {
                NV::V(v) => {
                    // proxies / object-model values infer to themselves
                    // (Proxy.infer yields self — no bump, no cache)
                    yield_v!(sink, v.clone());
                    inferred = true;
                    continue;
                }
                NV::N(g) => *g,
            };
            ctx.lookupname.set(self.infer_name_of_stmt(stmt_node, frame, name));
            // constraints whose If does not contain the stmt
            let mut stmt_constraints: Vec<&crate::constraint::Constraint> = Vec::new();
            for (cstmt, cs) in constraints.iter() {
                if !self.parent_of(*cstmt, stmt_node) {
                    stmt_constraints.extend(cs.iter());
                }
            }
            let end = {
                let inferred = &mut inferred;
                let constraint_failed = &mut constraint_failed;
                let stmt_constraints = &stmt_constraints;
                let ctx2 = Rc::clone(&ctx);
                self.infer_to(stmt_node, &ctx, &mut |inf| {
                    if stmt_constraints
                        .iter()
                        .all(|c| self.constraint_satisfied(c, &inf, &ctx2))
                    {
                        *inferred = true;
                        sink(inf)
                    } else {
                        *constraint_failed = true;
                        Drive::Go
                    }
                })
            };
            match end {
                End::Stopped => return End::Stopped,
                End::Raised(ErrKind::NameError) => continue,
                End::Raised(ErrKind::Inference) => {
                    yield_v!(sink, Value::Uninferable);
                    inferred = true;
                }
                End::Raised(other) => return End::Raised(other),
                End::Done => {}
            }
        }
        if !inferred && constraint_failed {
            yield_v!(sink, Value::Uninferable);
        } else if !inferred {
            return End::Raised(ErrKind::Inference);
        }
        End::Done
    }

    /// stmt._infer_name(frame, name)
    fn infer_name_of_stmt(&self, stmt: GNode, frame: Option<GNode>, name: Option<GSym>) -> Option<GSym> {
        let md = self.md(stmt.m);
        match &md.tree.nodes[stmt.n.idx()].kind {
            NodeKind::Import { .. }
            | NodeKind::ImportFrom { .. }
            | NodeKind::Global { .. }
            | NodeKind::Try(_)
            | NodeKind::TryStar(_) => name,
            NodeKind::Arguments(_) => {
                let parent = self.parent(stmt);
                if parent.is_some() && parent == frame {
                    name
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    // ---------- Name (§8.1) ----------

    fn infer_name_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let md = self.md(node.m);
        let name_sym = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Name { name } => self.g(&md, *name),
            _ => return End::Raised(ErrKind::Inference),
        };
        let looked = self.lookup(node, name_sym);
        let (frame, mut stmts) = (looked.0, looked.1.clone());
        if stmts.is_empty() {
            // helpers._higher_function_scope
            if let Some(pf) = self.higher_function_scope(self.scope(node)) {
                let looked2 = self.lookup_in(pf, node, name_sym);
                stmts = looked2;
            }
            if stmts.is_empty() {
                return End::Raised(ErrKind::NameError);
            }
        }
        let ctx2 = copy_context(Some(ctx));
        ctx2.lookupname.set(Some(name_sym));
        let constraints = self.get_constraints(node, frame);
        ctx2.constraints
            .borrow_mut()
            .insert(name_sym, Rc::new(constraints));
        self.infer_stmts_to(&stmts, Some(&ctx2), Some(frame), sink)
    }

    /// parent_function.lookup(name) — without going through node.scope()
    fn lookup_in(&self, scope: GNode, node: GNode, name: GSym) -> Vec<NV> {
        self.scope_lookup(scope, node, name, 0).1
    }

    fn higher_function_scope(&self, scope: GNode) -> Option<GNode> {
        let mut current = scope;
        loop {
            let parent = self.parent(current)?;
            if self.kind_is(parent, |k| {
                matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
            }) {
                return Some(parent);
            }
            current = parent;
        }
    }

    // ---------- AssignName / AssignAttr (§8.2) ----------

    fn infer_assign_name_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let parent = self.parent(node);
        if let Some(p) = parent {
            if self.kind_is(p, |k| matches!(k, NodeKind::AugAssign { .. })) {
                return self.infer_to(p, ctx, sink);
            }
        }
        // astroid materializes: `stmts = list(self.assigned_stmts(...))`
        let stmts = match self.assigned_stmts(node, Some(ctx), None) {
            Ok(s) => s,
            Err(e) => return End::Raised(e),
        };
        self.infer_stmts_to(&stmts, Some(ctx), None, sink)
    }

    fn infer_assign_attr_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let stmts = match self.assigned_stmts(node, Some(ctx), None) {
            Ok(s) => s,
            Err(e) => return End::Raised(e),
        };
        self.infer_stmts_to(&stmts, Some(ctx), None, sink)
    }

    // ---------- Attribute(Load) (§12.1) ----------

    /// eager shim (AssignAttr.infer_lhs path)
    pub fn infer_attribute_load(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_attribute_load_to(node, ctx, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    pub fn infer_attribute_load_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let md = self.md(node.m);
        let (expr, attrname) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Attribute { expr, attrname, .. } => {
                (GNode { m: node.m, n: *expr }, self.g(&md, *attrname))
            }
            NodeKind::AssignAttr { expr, attrname } => {
                (GNode { m: node.m, n: *expr }, self.g(&md, *attrname))
            }
            _ => return End::Raised(ErrKind::Inference),
        };
        // `context = copy_context(context)` re-binds the loop variable: each
        // iteration copies the PREVIOUS copy (node_classes.py:1081).
        let mut cur_ctx = Rc::clone(ctx);
        let end = {
            let cur_ctx = &mut cur_ctx;
            self.infer_to(expr, ctx, &mut |owner| {
                if owner.is_uninferable() {
                    return sink(Value::Uninferable);
                }
                *cur_ctx = copy_context(Some(cur_ctx));
                let old_bound = cur_ctx.boundnode.borrow().clone();
                *cur_ctx.boundnode.borrow_mut() = Some(owner.clone());
                // constraints when owner is ClassDef or Instance
                let frame_for_constraints: Option<GNode> = match &owner {
                    Value::Node(g)
                        if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) =>
                    {
                        Some(*g)
                    }
                    Value::Inst { cls } | Value::ExcInst { cls, .. } => Some(*cls),
                    _ => None,
                };
                if let Some(frame) = frame_for_constraints {
                    let cs = self.get_constraints(node, frame);
                    cur_ctx
                        .constraints
                        .borrow_mut()
                        .insert(attrname, Rc::new(cs));
                }
                // hardcoded sys.argv (node_classes.py:1084-1086)
                let is_sys_argv = self.sname(attrname) == "argv"
                    && matches!(&owner, Value::Node(g)
                        if self.kind_is(*g, |k| matches!(k, NodeKind::Module(_)))
                            && self.md(g.m).name == "sys");
                let drive = if is_sys_argv {
                    sink(Value::Uninferable)
                } else {
                    // per-owner errors swallowed (AttributeInferenceError,
                    // InferenceError, AttributeError — node_classes.py:1100)
                    let mut stopped = false;
                    let _ = self.igetattr_value_to(&owner, attrname, Some(cur_ctx), &mut |v| {
                        let d = sink(v);
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
                };
                *cur_ctx.boundnode.borrow_mut() = old_bound;
                drive
            })
        };
        // owner-generator errors propagate (after any yields)
        end
    }

    // ---------- Import / ImportFrom / Global (§20.3) ----------

    fn infer_import(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return Flow::err(ErrKind::Inference),
        };
        let md = self.md(node.m);
        let names: Vec<(GSym, Option<GSym>)> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Import { names } => names
                .iter()
                .map(|(a, b)| (self.g(&md, *a), b.map(|s| self.g(&md, s))))
                .collect(),
            _ => return Flow::err(ErrKind::Inference),
        };
        let real = match self.real_name(&names, name) {
            Some(r) => r,
            None => return Flow::err(ErrKind::Inference),
        };
        match self.do_import_module(node, Some(&real)) {
            Ok(mid) => Flow::one(Value::Node(GNode {
                m: mid,
                n: NodeId::MODULE,
            })),
            Err(_) => Flow::err(ErrKind::Inference),
        }
    }

    /// _base_nodes.py:174-188 real_name
    fn real_name(&self, names: &[(GSym, Option<GSym>)], asname: GSym) -> Option<String> {
        let asname_str = self.sname(asname);
        for (name, _asname) in names {
            let name_str = self.sname(*name);
            if name_str == "*" {
                return Some(asname_str);
            }
            let (eff_name, eff_asname) = match _asname {
                Some(a) => (name_str.clone(), self.sname(*a)),
                None => {
                    let first = name_str.split('.').next().unwrap_or("").to_string();
                    (first.clone(), first)
                }
            };
            if asname_str == eff_asname {
                return Some(eff_name);
            }
        }
        None
    }

    fn infer_import_from_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return End::Raised(ErrKind::Inference),
        };
        let md = self.md(node.m);
        let names: Vec<(GSym, Option<GSym>)> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ImportFrom { names, .. } => names
                .iter()
                .map(|(a, b)| (self.g(&md, *a), b.map(|s| self.g(&md, s))))
                .collect(),
            _ => return End::Raised(ErrKind::Inference),
        };
        let real = match self.real_name(&names, name) {
            Some(r) => r,
            None => return End::Raised(ErrKind::Inference),
        };
        let module = match self.do_import_module(node, None) {
            Ok(m) => m,
            Err(_) => return End::Raised(ErrKind::Inference),
        };
        let ctx2 = copy_context(Some(ctx));
        let real_sym = self.sym(&real);
        ctx2.lookupname.set(Some(real_sym));
        let ignore_locals = module == node.m; // module is self.root()
        match self.module_getattr(module, real_sym, ignore_locals) {
            Ok(stmts) => self.infer_stmts_to(&stmts, Some(&ctx2), None, sink),
            Err(_) => End::Raised(ErrKind::Inference),
        }
    }

    fn infer_global_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return End::Raised(ErrKind::Inference),
        };
        match self.module_getattr(node.m, name, false) {
            Ok(stmts) => self.infer_stmts_to(&stmts, Some(ctx), None, sink),
            Err(_) => End::Raised(ErrKind::Inference),
        }
    }

    // ---------- FunctionDef (§11.2) ----------

    fn infer_functiondef(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        if self.is_property(node, ctx) {
            Flow::one(Value::Property { func: node })
        } else {
            Flow::one(Value::Node(node))
        }
    }

    // ---------- containers (§17) ----------

    fn infer_container(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (elts, kind) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::List { elts, .. } => (elts.clone(), SeqKind::List),
            NodeKind::Tuple { elts, .. } => (elts.clone(), SeqKind::Tuple),
            NodeKind::Set { elts } => (elts.clone(), SeqKind::Set),
            _ => return Flow::err(ErrKind::Inference),
        };
        let has_special = elts.iter().any(|&e| {
            matches!(
                md.tree.nodes[e.idx()].kind,
                NodeKind::Starred { .. } | NodeKind::NamedExpr { .. }
            )
        });
        if !has_special {
            return Flow::one(Value::Node(node));
        }
        // _infer_sequence_helper (node_classes.py:364-386)
        let mut values: Vec<Value> = Vec::new();
        for e in elts {
            let g = GNode { m: node.m, n: e };
            match &md.tree.nodes[e.idx()].kind {
                NodeKind::Starred { value, .. } => {
                    let starred = self.safe_infer(GNode { m: node.m, n: *value }, ctx);
                    match starred {
                        Some(v) => match self.value_elts(&v) {
                            Some(sub) => values.extend(sub),
                            None => return Flow::err(ErrKind::Inference),
                        },
                        None => return Flow::err(ErrKind::Inference),
                    }
                }
                NodeKind::NamedExpr { value, .. } => {
                    let v = self.safe_infer(GNode { m: node.m, n: *value }, ctx);
                    match v {
                        Some(v) => values.push(v),
                        None => return Flow::err(ErrKind::Inference),
                    }
                }
                _ => values.push(Value::Node(g)),
            }
        }
        Flow::one(Value::SynthSeq {
            kind,
            elems: Rc::new(values),
        })
    }

    fn infer_dict(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let items: Vec<(NodeId, NodeId)> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Dict { items } => items.clone(),
            _ => return Flow::err(ErrKind::Inference),
        };
        let has_unpack = items
            .iter()
            .any(|(k, _)| matches!(md.tree.nodes[k.idx()].kind, NodeKind::DictUnpack));
        if !has_unpack {
            return Flow::one(Value::Node(node));
        }
        // _infer_map (node_classes.py:2485-2506) with update-replace
        // semantics keyed by as_string; we key Const values by value and
        // others by identity.
        let mut out: Vec<(Value, Value)> = Vec::new();
        let mut replace = |k: Value, v: Value, out: &mut Vec<(Value, Value)>| {
            let kc = self.value_const(&k);
            if let Some(kc) = kc {
                if let Some(pos) = out
                    .iter()
                    .position(|(ek, _)| self.value_const(ek).as_ref() == Some(&kc))
                {
                    out.remove(pos);
                }
            }
            out.push((k, v));
        };
        for (k, v) in items {
            if matches!(md.tree.nodes[k.idx()].kind, NodeKind::DictUnpack) {
                let inner = self.safe_infer(GNode { m: node.m, n: v }, ctx);
                match inner {
                    Some(val) => match self.value_dict_items(&val) {
                        Some(pairs) => {
                            for (ik, iv) in pairs {
                                replace(ik, iv, &mut out);
                            }
                        }
                        None => return Flow::err(ErrKind::Inference),
                    },
                    None => return Flow::err(ErrKind::Inference),
                }
            } else {
                let ik = self.safe_infer(GNode { m: node.m, n: k }, ctx);
                let iv = self.safe_infer(GNode { m: node.m, n: v }, ctx);
                match (ik, iv) {
                    (Some(ik), Some(iv)) => {
                        if ik.is_uninferable() || iv.is_uninferable() {
                            return Flow::err(ErrKind::Inference);
                        }
                        replace(ik, iv, &mut out);
                    }
                    _ => return Flow::err(ErrKind::Inference),
                }
            }
        }
        Flow::one(Value::SynthDict {
            items: Rc::new(out),
        })
    }

    // ---------- BoolOp (§16.4) ----------

    fn infer_boolop(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (op, values) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::BoolOp { op, values } => (op.clone(), values.clone()),
            _ => return Flow::err(ErrKind::Inference),
        };
        if values.len() < 2 {
            return Flow::err(ErrKind::Inference);
        }
        let shortcircuit = op.as_ref() == "or";
        // infer each operand fully (node_classes.py:1655-1663)
        let mut inferred_ops: Vec<Vec<Value>> = Vec::new();
        for v in &values {
            let f = self.infer(GNode { m: node.m, n: *v }, ctx);
            if f.vals.is_empty() {
                return Flow::err(ErrKind::Inference);
            }
            inferred_ops.push(f.vals);
        }
        // cartesian product
        let mut out = Vec::new();
        let mut idx = vec![0usize; inferred_ops.len()];
        'outer: loop {
            let pair: Vec<&Value> = idx.iter().enumerate().map(|(i, &j)| &inferred_ops[i][j]).collect();
            if pair.iter().any(|v| v.is_uninferable()) {
                out.push(Value::Uninferable);
            } else {
                let mut yielded = false;
                for (i, value) in pair.iter().enumerate() {
                    if i == pair.len() - 1 {
                        break;
                    }
                    match self.bool_value(value, ctx) {
                        None => {
                            out.push(Value::Uninferable);
                            yielded = true;
                            break;
                        }
                        Some(b) => {
                            if b == shortcircuit {
                                out.push((*value).clone());
                                yielded = true;
                                break;
                            }
                        }
                    }
                }
                if !yielded {
                    out.push(pair[pair.len() - 1].clone());
                }
            }
            // increment cartesian counter
            let mut i = idx.len();
            loop {
                if i == 0 {
                    break 'outer;
                }
                i -= 1;
                idx[i] += 1;
                if idx[i] < inferred_ops[i].len() {
                    break;
                }
                idx[i] = 0;
            }
        }
        Flow::ok(out)
    }

    // ---------- IfExp (§16.5) ----------

    fn infer_ifexp_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let md = self.md(node.m);
        let (test, body, orelse) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::IfExp { test, body, orelse } => (*test, *body, *orelse),
            _ => return End::Raised(ErrKind::Inference),
        };
        // node_classes.py:3115-3117: branch contexts copied UP FRONT
        let lhs = copy_context(Some(ctx));
        let rhs = copy_context(Some(ctx));
        // condition scan (node_classes.py:3121-3136): pulls test values one
        // at a time; `break` abandons the test generator.
        let mut condition: Option<bool> = None;
        let mut decided = false; // broke out with condition=None
        {
            let condition = &mut condition;
            let decided = &mut decided;
            let tctx = ctx.clone_ctx();
            let end = self.infer_to(GNode { m: node.m, n: test }, &tctx, &mut |v| {
                if v.is_uninferable() {
                    *condition = None;
                    *decided = true;
                    return Drive::Stop;
                }
                // test.bool_value() — no context (bases.py:388)
                match self.bool_value(&v, &Ctx::new()) {
                    None => {
                        *condition = None;
                        *decided = true;
                        Drive::Stop
                    }
                    Some(b) => {
                        match *condition {
                            None if !*decided => {
                                *condition = Some(b);
                                *decided = true; // first value recorded
                                Drive::Go
                            }
                            Some(prev) if prev != b => {
                                *condition = None;
                                Drive::Stop
                            }
                            _ => Drive::Go,
                        }
                    }
                }
            });
            if let End::Raised(e) = end {
                if e.is_inference() {
                    *condition = None;
                } else {
                    return End::Raised(e);
                }
            }
        }
        if condition == Some(true) || condition.is_none() {
            match self.infer_to(GNode { m: node.m, n: body }, &lhs, sink) {
                End::Done => {}
                e => return e, // errors / consumer stop propagate immediately
            }
        }
        if condition == Some(false) || condition.is_none() {
            return self.infer_to(GNode { m: node.m, n: orelse }, &rhs, sink);
        }
        End::Done
    }

    // ---------- Compare (§16.3) ----------

    fn infer_compare(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (left, ops) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Compare { left, ops } => (*left, ops.clone()),
            _ => return Flow::err(ErrKind::Inference),
        };
        let mut retval: Option<bool> = None;
        let lhs_flow = self.infer(GNode { m: node.m, n: left }, &ctx.clone_ctx());
        if lhs_flow.is_err() {
            return Flow { vals: lhs_flow.vals, err: lhs_flow.err };
        }
        let mut lhs = lhs_flow.vals;
        for (op, right) in &ops {
            let rhs_flow = self.infer(GNode { m: node.m, n: *right }, &ctx.clone_ctx());
            if rhs_flow.is_err() {
                return Flow { vals: Vec::new(), err: rhs_flow.err };
            }
            let rhs = rhs_flow.vals;
            // _do_compare
            match self.do_compare(&lhs, op, &rhs) {
                None => return Flow::uninferable(),
                Some(r) => retval = Some(r),
            }
            if retval == Some(false) {
                break; // short-circuit
            }
            lhs = rhs;
        }
        match retval {
            Some(b) => Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(b)))),
            None => Flow::uninferable(),
        }
    }

    /// node_classes.py:1859-1905 _do_compare — literal folding only.
    /// Returns None for Uninferable.
    fn do_compare(&self, lefts: &[Value], op: &str, rights: &[Value]) -> Option<bool> {
        if op == "is" || op == "is not" {
            return None;
        }
        let mut retval: Option<bool> = None;
        for left in lefts {
            for right in rights {
                let lc = self.value_const(left)?;
                let rc = self.value_const(right)?;
                let r = compare_consts(&lc, op, &rc)?;
                match retval {
                    None => retval = Some(r),
                    Some(prev) if prev == r => {}
                    _ => return None, // mixed True/False
                }
            }
        }
        retval
    }

    // ---------- f-strings (§16.6) ----------

    fn infer_formatted_value(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (value, format_spec) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::FormattedValue {
                value, format_spec, ..
            } => (*value, *format_spec),
            _ => return Flow::err(ErrKind::Inference),
        };
        drop(md);
        let mut out = Vec::new();
        let mut uninferable_already = false;
        // format_specs = Const("") if None else format_spec; .infer() — the
        // synthetic Const("") infer bumps once (fresh node, cache miss)
        let specs: Vec<Value> = match format_spec {
            None => {
                ctx.bump_inferred();
                vec![Value::SynthConst(Rc::new(ConstValue::Str("".into())))]
            }
            Some(fs) => self.infer(GNode { m: node.m, n: fs }, ctx).vals,
        };
        for spec_v in specs {
            let spec = match self.value_const(&spec_v) {
                Some(ConstValue::Str(sp)) => Some(sp.to_string()),
                Some(_) => None, // non-str Const spec: format() TypeError per value
                None => {
                    // not a Const at all -> single Uninferable
                    if !uninferable_already {
                        out.push(Value::Uninferable);
                        uninferable_already = true;
                    }
                    continue;
                }
            };
            let vf = self.infer(GNode { m: node.m, n: value }, ctx);
            for v in &vf.vals {
                // format(value_to_format, spec): Const values use python
                // format(); other inference results are formatted as their
                // astroid str() — Instance: "Instance of {root}.{name}"
                // (bases.py:373), Uninferable: "Uninferable"
                let formatted: Option<String> = match (&spec, v) {
                    (Some(sp), _) if self.value_const(v).is_some() => {
                        format_const(&self.value_const(v).unwrap(), sp)
                    }
                    (Some(sp), Value::Inst { cls } | Value::ExcInst { cls, .. })
                        if sp.is_empty() =>
                    {
                        let root = self.md(cls.m).name.clone();
                        let name = self.node_name(*cls).unwrap_or_default();
                        Some(format!("Instance of {root}.{name}"))
                    }
                    (Some(sp), Value::Uninferable) if sp.is_empty() => {
                        Some("Uninferable".to_string())
                    }
                    _ => None,
                };
                match formatted {
                    Some(sf) => {
                        out.push(Value::SynthConst(Rc::new(ConstValue::Str(sf.into()))))
                    }
                    None => {
                        out.push(Value::Uninferable);
                        uninferable_already = true;
                    }
                }
            }
        }
        Flow::ok(out)
    }

    /// JoinedStr._infer_from_values (node_classes.py:4822-4844), recursive
    /// cartesian concatenation; parts inferred via node._infer (NO
    /// NodeNG.infer wrapper: no cache, no bumps — _safe_infer_from_node)
    fn joinedstr_parts(&self, values: &[NodeId], m: crate::value::ModId, ctx: &Rc<Ctx>) -> Vec<Value> {
        if values.is_empty() {
            return Vec::new();
        }
        // _safe_infer_from_node: node._infer; InferenceError -> single U
        let safe = |n: NodeId| -> Vec<Value> {
            let g = GNode { m, n };
            let mut vals = Vec::new();
            let end = self.infer_dispatch_to(g, ctx, &mut |v| {
                vals.push(v);
                Drive::Go
            });
            if vals.is_empty() {
                if let End::Raised(_) = end {
                    return vec![Value::Uninferable];
                }
            }
            vals
        };
        if values.len() == 1 {
            let mut out = Vec::new();
            for v in safe(values[0]) {
                if self.value_const(&v).is_some() {
                    out.push(v);
                } else {
                    out.push(Value::SynthConst(Rc::new(ConstValue::Str(
                        "{Uninferable}".into(),
                    ))));
                }
            }
            return out;
        }
        let mut out = Vec::new();
        for prefix in safe(values[0]) {
            for suffix in self.joinedstr_parts(&values[1..], m, ctx) {
                let mut result = String::new();
                for part in [&prefix, &suffix] {
                    match self.value_const(part) {
                        Some(c) => result.push_str(&const_str_value(&c)),
                        None => result.push_str("{Uninferable}"),
                    }
                }
                out.push(Value::SynthConst(Rc::new(ConstValue::Str(result.into()))));
            }
        }
        out
    }

    fn infer_joinedstr(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let values: Vec<NodeId> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::JoinedStr { values } => values.clone(),
            _ => return Flow::err(ErrKind::Inference),
        };
        drop(md);
        if values.is_empty() {
            return Flow::one(Value::SynthConst(Rc::new(ConstValue::Str("".into()))));
        }
        // _infer_with_values: failed = U or Const str containing the
        // "{Uninferable}" marker; only the FIRST failure yields U — later
        // failures fall through and yield the raw marker Const
        // (node_classes.py:4807-4820, bug-for-bug)
        let mut out = Vec::new();
        let mut uninferable_already = false;
        for v in self.joinedstr_parts(&values, node.m, ctx) {
            let failed = v.is_uninferable()
                || matches!(self.value_const(&v), Some(ConstValue::Str(sv)) if sv.contains("{Uninferable}"));
            if failed && !uninferable_already {
                uninferable_already = true;
                out.push(Value::Uninferable);
                continue;
            }
            out.push(v);
        }
        Flow::ok(out)
    }

    // ---------- EmptyNode ----------

    /// EmptyNode._infer (node_classes.py:2568-2581) →
    /// AstroidManager.infer_ast_from_something (manager.py): resolves the
    /// live object's class via `modastroid.igetattr(name, context)` — under
    /// the SHARED context, so the lookup work bumps nodes_inferred exactly
    /// like astroid. The snapshot einf descriptor supplies klass.__module__
    /// + __name__ (qname) and whether obj was an instance (instantiate
    /// branch) or the class/function itself.
    fn infer_empty_node_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let (descs, ek) = {
            let md = self.md(node.m);
            (md.einf.get(&node.n).cloned(), md.eklass.get(&node.n).cloned())
        };
        let Some(ek) = ek else {
            // no underlying object -> Uninferable; legacy einf-only
            // snapshots replay recorded values
            match descs {
                Some(descs) if !descs.is_empty() => {
                    for d in &descs {
                        yield_v!(sink, self.resolve_einf(d));
                    }
                }
                _ => yield_v!(sink, Value::Uninferable),
            }
            return End::Done;
        };
        let (modname, name, instantiate) = (ek.module, ek.name, ek.instance);
        let mid = match self.ast_from_module_name(&modname, true) {
            Ok(m) => m,
            Err(_) => {
                // AstroidError -> Uninferable
                yield_v!(sink, Value::Uninferable);
                return End::Done;
            }
        };
        let sym = self.sym(&name);
        let owner = Value::Node(GNode {
            m: mid,
            n: NodeId::MODULE,
        });
        let mut stopped = false;
        let end = {
            let stopped = &mut stopped;
            self.igetattr_value_to(&owner, sym, Some(ctx), &mut |v| {
                let out = if instantiate {
                    match &v {
                        // Uninferable.instantiate_class() is Uninferable
                        Value::Uninferable => Value::Uninferable,
                        Value::Node(g)
                            if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) =>
                        {
                            self.instantiate_class(*g)
                        }
                        other => other.clone(),
                    }
                } else {
                    v
                };
                let d = sink(out);
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
            // InferenceError is an AstroidError -> yield Uninferable
            End::Raised(_) => {
                yield_v!(sink, Value::Uninferable);
                End::Done
            }
            e => e,
        }
    }

    fn resolve_einf(&self, d: &EInf) -> Value {
        match d {
            EInf::Uninferable => Value::Uninferable,
            EInf::Const(c) => Value::SynthConst(Rc::new(c.clone())),
            EInf::Class(q) | EInf::Inst(q) | EInf::Func(q) => {
                match self.resolve_qname(q) {
                    Some(g) => match d {
                        EInf::Inst(_) => Value::Inst { cls: g },
                        _ => Value::Node(g),
                    },
                    None => Value::Uninferable,
                }
            }
        }
    }

    /// resolve "mod.sub.Class.attr" by importing the longest module prefix
    /// then walking locals.
    pub fn resolve_qname(&self, q: &str) -> Option<GNode> {
        let parts: Vec<&str> = q.split('.').collect();
        for split in (1..parts.len()).rev() {
            let modname = parts[..split].join(".");
            if let Ok(mid) = self.ast_from_module_name(&modname, true) {
                let mut cur = GNode {
                    m: mid,
                    n: NodeId::MODULE,
                };
                let mut ok = true;
                for seg in &parts[split..] {
                    let sym = self.sym(seg);
                    let md = self.md(cur.m);
                    let locals = md.locals.borrow();
                    match locals.get(&cur.n).and_then(|l| l.get(&sym)).and_then(|v| v.first()) {
                        Some(&g) => cur = g,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    return Some(cur);
                }
            }
        }
        None
    }

    /// `next(node.infer(ctx), None)`-style single pull. Ok(None) =
    /// StopIteration before any value; Err = raised before the first value.
    pub fn first_value(&self, node: GNode, ctx: &Rc<Ctx>) -> Result<Option<Value>, ErrKind> {
        let mut first: Option<Value> = None;
        let end = {
            let first = &mut first;
            self.infer_to(node, ctx, &mut |v| {
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

    /// `next(value.igetattr(name, ctx))` — single pull, abandoning the
    /// attribute generator. Ok(None) = StopIteration; Err = raised before
    /// the first value.
    pub fn igetattr_first(
        &self,
        owner: &Value,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Option<Value>, ErrKind> {
        let mut first: Option<Value> = None;
        let end = {
            let first = &mut first;
            self.igetattr_value_to(owner, name, ctx, &mut |v| {
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

    /// `next(callee.infer_call_result(caller, ctx), None)` — single pull.
    pub fn infer_call_result_first(
        &self,
        callee: &Value,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Option<Value>, ErrKind> {
        let mut first: Option<Value> = None;
        let end = {
            let first = &mut first;
            self.infer_call_result_to(callee, caller, ctx, &mut |v| {
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

    // ---------- safe_infer (§5) ----------

    /// util.safe_infer: pulls at most TWO values then abandons the
    /// generator (no cache write / no bump for the second value).
    pub fn safe_infer(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Value> {
        let mut first: Option<Value> = None;
        let mut ambiguous = false;
        let end = {
            let first = &mut first;
            let ambiguous = &mut ambiguous;
            self.infer_to(node, ctx, &mut |v| {
                if first.is_none() {
                    *first = Some(v);
                    Drive::Go
                } else {
                    *ambiguous = true;
                    Drive::Stop
                }
            })
        };
        match end {
            End::Stopped => None,                      // second value -> ambiguity
            End::Raised(_) => None,                    // error on first OR second pull
            End::Done => first,                        // exactly one (or zero -> None)
        }
    }

    pub fn safe_infer_value(&self, v: &Value) -> Option<Value> {
        // values infer to themselves
        Some(v.clone())
    }

    // ---------- helpers on values ----------

    pub fn value_const(&self, v: &Value) -> Option<ConstValue> {
        match v {
            Value::SynthConst(c) => Some((**c).clone()),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(c.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// elements of a container value, as Values (for starred unpacking)
    pub fn value_elts(&self, v: &Value) -> Option<Vec<Value>> {
        match v {
            Value::SynthSeq { elems, .. } | Value::FrozenSet { elems } => {
                Some(elems.to_vec())
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
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn value_dict_items(&self, v: &Value) -> Option<Vec<(Value, Value)>> {
        match v {
            Value::SynthDict { items } => Some(items.to_vec()),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => Some(
                        items
                            .iter()
                            .map(|&(k, val)| {
                                (
                                    Value::Node(GNode { m: g.m, n: k }),
                                    Value::Node(GNode { m: g.m, n: val }),
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

    /// the builtins ClassDef a value is an instance of (Instance._proxied)
    pub fn proxied_class(&self, v: &Value) -> Option<GNode> {
        let b = self.builtins();
        match v {
            Value::Inst { cls } | Value::ExcInst { cls, .. } => Some(*cls),
            Value::SynthConst(c) => Some(self.const_class(c)),
            Value::SynthSeq { kind, .. } => Some(match kind {
                SeqKind::List => b.list,
                SeqKind::Tuple => b.tuple,
                SeqKind::Set => b.set,
            }),
            Value::SynthDict { .. } => Some(b.dict),
            Value::SynthSlice { .. } => Some(b.slice),
            Value::FrozenSet { .. } => Some(b.frozenset),
            Value::Generator { is_async, .. } => Some(if *is_async {
                b.async_generator
            } else {
                b.generator
            }),
            Value::UnionType => Some(b.union_type),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(self.const_class(c)),
                    NodeKind::List { .. } => Some(b.list),
                    NodeKind::Tuple { .. } => Some(b.tuple),
                    NodeKind::Set { .. } => Some(b.set),
                    NodeKind::Dict { .. } => Some(b.dict),
                    NodeKind::Slice { .. } => Some(b.slice),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn const_class(&self, c: &ConstValue) -> GNode {
        let b = self.builtins();
        match c {
            ConstValue::None => b.none_type,
            ConstValue::NotImplemented => b.notimpl_type,
            ConstValue::Ellipsis => b.ellipsis_type,
            ConstValue::Bool(_) => b.bool_,
            ConstValue::Int(_) => b.int,
            ConstValue::Float(_) => b.float,
            ConstValue::Complex { .. } => b.complex,
            ConstValue::Str(_) | ConstValue::StrSurrogate(_) => b.str_,
            ConstValue::Bytes(_) => b.bytes,
        }
    }

    /// §16.1/16.2 bool_value. None == Uninferable.
    pub fn bool_value(&self, v: &Value, ctx: &Rc<Ctx>) -> Option<bool> {
        match v {
            Value::Uninferable => None,
            Value::SynthConst(c) => Some(const_truth(c)),
            Value::SynthSeq { elems, .. } => Some(!elems.is_empty()),
            Value::SynthDict { items } => Some(!items.is_empty()),
            Value::FrozenSet { elems } => Some(!elems.is_empty()),
            Value::SynthSlice { .. } => None,
            Value::Generator { .. }
            | Value::BoundMethod { .. }
            | Value::UnboundMethod { .. }
            | Value::UnionType
            | Value::Property { .. }
            | Value::Partial { .. }
            | Value::Super { .. } => Some(true),
            Value::DictItems(_) | Value::DictKeys(_) | Value::DictValues(_) => None,
            Value::Inst { .. } | Value::ExcInst { .. } => self.instance_bool_value(v, ctx),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(const_truth(c)),
                    NodeKind::List { elts, .. }
                    | NodeKind::Tuple { elts, .. }
                    | NodeKind::Set { elts } => Some(!elts.is_empty()),
                    NodeKind::Dict { items } => Some(!items.is_empty()),
                    NodeKind::Module(_)
                    | NodeKind::ClassDef(_)
                    | NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::Lambda(_)
                    | NodeKind::GeneratorExp(_) => Some(true),
                    _ => None,
                }
            }
        }
    }

    /// bases.py:388-414 Instance.bool_value
    fn instance_bool_value(&self, v: &Value, ctx: &Rc<Ctx>) -> Option<bool> {
        *ctx.boundnode.borrow_mut() = Some(v.clone());
        match self.infer_method_result_truth(v, "__bool__", ctx) {
            Ok(r) => r,
            Err(_) => match self.infer_method_result_truth(v, "__len__", ctx) {
                Ok(r) => r,
                Err(_) => Some(true),
            },
        }
    }

    /// bases.py:207-228 _infer_method_result_truth
    fn infer_method_result_truth(
        &self,
        instance: &Value,
        name: &str,
        ctx: &Rc<Ctx>,
    ) -> Result<Option<bool>, ErrKind> {
        let sym = self.sym(name);
        // next(instance.igetattr(method_name, context), None) — single pull
        let meth = match self.igetattr_first(instance, sym, Some(ctx)) {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !self.has_infer_call_result(&meth) {
            return Ok(None);
        }
        if !self.value_callable(&meth, ctx) {
            return Ok(None);
        }
        let cc = Rc::new(CallCtx {
            id: self.next_callctx_id(),
            args: std::cell::RefCell::new(Vec::new()),
            keywords: std::cell::RefCell::new(Vec::new()),
            callee: std::cell::RefCell::new(Some(meth.clone())),
        });
        *ctx.callcontext.borrow_mut() = Some(cc);
        // first call-result value only (the `return` abandons the generator)
        match self.infer_call_result_first(&meth, None, Some(ctx)) {
            Ok(None) => Ok(None),
            Ok(Some(Value::Uninferable)) => Ok(None),
            Ok(Some(value)) => Ok(self.bool_value(&value, ctx)),
            Err(e) if e.is_inference() => Ok(None),
            Err(_) => Ok(None),
        }
    }

    pub fn has_infer_call_result(&self, v: &Value) -> bool {
        match v {
            Value::Node(g) => {
                let md = self.md(g.m);
                matches!(
                    md.tree.nodes[g.n.idx()].kind,
                    NodeKind::FunctionDef(_)
                        | NodeKind::AsyncFunctionDef(_)
                        | NodeKind::Lambda(_)
                        | NodeKind::ClassDef(_)
                        // Const/containers are Instance subclasses
                        | NodeKind::Const(_)
                        | NodeKind::List { .. }
                        | NodeKind::Tuple { .. }
                        | NodeKind::Set { .. }
                        | NodeKind::Dict { .. }
                )
            }
            Value::Inst { .. }
            | Value::ExcInst { .. }
            | Value::BoundMethod { .. }
            | Value::UnboundMethod { .. }
            | Value::Property { .. }
            | Value::Partial { .. }
            | Value::Generator { .. }
            | Value::SynthConst(_)
            | Value::SynthSeq { .. }
            | Value::SynthDict { .. }
            | Value::FrozenSet { .. }
            | Value::UnionType => true,
            _ => false,
        }
    }

    /// `.callable()` semantics (§11.8)
    pub fn value_callable(&self, v: &Value, _ctx: &Rc<Ctx>) -> bool {
        match v {
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::Lambda(_)
                    | NodeKind::ClassDef(_) => true,
                    NodeKind::Const(_)
                    | NodeKind::List { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::Set { .. }
                    | NodeKind::Dict { .. }
                    | NodeKind::Slice { .. } => self.instance_callable(v),
                    _ => false,
                }
            }
            Value::Inst { .. } | Value::ExcInst { .. } => self.instance_callable(v),
            Value::BoundMethod { .. }
            | Value::UnboundMethod { .. }
            | Value::Property { .. }
            | Value::Partial { .. } => true,
            Value::Generator { .. } => false,
            Value::SynthConst(_) | Value::SynthSeq { .. } | Value::SynthDict { .. }
            | Value::FrozenSet { .. } => self.instance_callable(v),
            _ => false,
        }
    }

    fn instance_callable(&self, v: &Value) -> bool {
        // Instance.callable: class getattr("__call__", class_context=False)
        match self.proxied_class(v) {
            Some(cls) => {
                let sym = self.sym("__call__");
                self.class_getattr(cls, sym, None, false).is_ok()
            }
            None => false,
        }
    }
}

// ---------- const helpers ----------

pub fn const_truth(c: &ConstValue) -> bool {
    match c {
        ConstValue::None => false,
        ConstValue::Bool(b) => *b,
        ConstValue::Int(IntValue::Small(i)) => *i != 0,
        ConstValue::Int(IntValue::Big(_)) => true,
        ConstValue::Float(f) => *f != 0.0,
        ConstValue::Complex { real, imag } => *real != 0.0 || *imag != 0.0,
        ConstValue::Str(s) => !s.is_empty(),
        ConstValue::StrSurrogate(p) => !p.is_empty(),
        ConstValue::Bytes(b) => !b.is_empty(),
        ConstValue::Ellipsis => true,
        // NotImplemented is truthy on 3.12 (node_classes.py:2165-2175)
        ConstValue::NotImplemented => true,
    }
}

/// numeric/str comparison for Compare folding. Mirrors Python's comparison
/// for the literal types ast.literal_eval can produce. None => Uninferable.
fn compare_consts(l: &ConstValue, op: &str, r: &ConstValue) -> Option<bool> {
    use std::cmp::Ordering;
    let ord: Option<Ordering> = match (const_num(l), const_num(r)) {
        (Some(a), Some(b)) => a.partial_cmp(&b),
        _ => match (l, r) {
            (ConstValue::Str(a), ConstValue::Str(b)) => Some(a.cmp(b)),
            (ConstValue::Bytes(a), ConstValue::Bytes(b)) => Some(a.cmp(b)),
            _ => None,
        },
    };
    let eq: Option<bool> = match (l, r) {
        (ConstValue::Str(a), ConstValue::Str(b)) => Some(a == b),
        (ConstValue::Bytes(a), ConstValue::Bytes(b)) => Some(a == b),
        (ConstValue::None, ConstValue::None) => Some(true),
        (ConstValue::None, _) | (_, ConstValue::None) => Some(false),
        _ => match (const_num(l), const_num(r)) {
            (Some(a), Some(b)) => Some(a == b),
            _ => None,
        },
    };
    match op {
        "==" => eq,
        "!=" => eq.map(|b| !b),
        "<" => ord.map(|o| o == Ordering::Less),
        "<=" => ord.map(|o| o != Ordering::Greater),
        ">" => ord.map(|o| o == Ordering::Greater),
        ">=" => ord.map(|o| o != Ordering::Less),
        // in / not in over literals: rarely fold-worthy; Uninferable
        _ => None,
    }
}

fn const_num(c: &ConstValue) -> Option<f64> {
    match c {
        ConstValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        ConstValue::Int(IntValue::Small(i)) => Some(*i as f64),
        ConstValue::Float(f) => Some(*f),
        _ => None,
    }
}

/// format(value, spec) folding for f-strings; only the empty/simple specs
/// matter in practice — bail (None => Uninferable) otherwise.
/// str(value) for JoinedStr concatenation (node_classes.py:4840
/// `result += str(node.value)`)
fn const_str_value(c: &ConstValue) -> String {
    format_const(c, "").unwrap_or_else(|| match c {
        ConstValue::Bytes(b) => format!("b{:?}", String::from_utf8_lossy(b)),
        ConstValue::Ellipsis => "Ellipsis".to_string(),
        _ => String::new(),
    })
}

fn format_const(c: &ConstValue, spec: &str) -> Option<String> {
    if !spec.is_empty() {
        return None;
    }
    match c {
        ConstValue::Str(s) => Some(s.to_string()),
        ConstValue::Int(IntValue::Small(i)) => Some(i.to_string()),
        ConstValue::Int(IntValue::Big(s)) => Some(s.to_string()),
        ConstValue::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        ConstValue::Float(f) => Some(pyast::pyrepr::repr_float(*f)),
        ConstValue::None => Some("None".to_string()),
        _ => None,
    }
}
