//! NodeNG.infer dispatch + per-node `_infer` implementations.
//! Ports: astroid/nodes/node_ng.py:121-176, decorators.py, bases.py
//! _infer_stmts, node_classes.py per-node inference (notes/07 §4,6,8,16,17).

use std::rc::Rc;

use pyast::tree::{ConstValue, Ctx as ExprCtx, IntValue, NodeKind};
use pyast::NodeId;

use crate::ctx::{bind_context_to_node, copy_context, CallCtx, Ctx, MAX_INFERABLE_VALUES, MAX_INFERRED};
use crate::graph::Engine;
use crate::snapshot::EInf;
use crate::value::{value_key, ErrKind, Flow, GNode, GSym, SeqKind, Value, NV};

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub enum DedupKey {
    Node(GNode),
    Uninferable,
}

/// path_wrapper dedup identity (decorators.py:25-54): exact-class Instance
/// unproxies to its ClassDef *node*; node-backed values use node identity;
/// Uninferable is a singleton; every other proxy is a fresh object.
pub fn dedup_key(v: &Value) -> Option<DedupKey> {
    match v {
        Value::Node(g) => Some(DedupKey::Node(*g)),
        Value::Inst { cls } => Some(DedupKey::Node(*cls)),
        Value::Uninferable => Some(DedupKey::Uninferable),
        _ => None,
    }
}

impl Engine {
    // ---------- entry point (NodeNG.infer) ----------

    pub fn infer(&self, node: GNode, ctx_in: &Rc<Ctx>) -> Flow {
        if self.depth.get() >= self.max_depth {
            return Flow::err(ErrKind::Recursion);
        }
        self.depth.set(self.depth.get() + 1);
        let r = self.infer_entry(node, ctx_in);
        self.depth.set(self.depth.get() - 1);
        r
    }

    fn infer_entry(&self, node: GNode, ctx_in: &Rc<Ctx>) -> Flow {
        // extra_context swap (node_ng.py:125-128)
        let ctx = {
            let extra = ctx_in.extra_context.borrow();
            match extra.get(&node) {
                Some(c) => Rc::clone(c),
                None => Rc::clone(ctx_in),
            }
        };
        // explicit inference (inference tips)
        if let Some(flow) = self.explicit_inference(node, &ctx) {
            for _ in &flow.vals {
                ctx.bump_inferred();
            }
            return flow;
        }
        let key = (
            node,
            ctx.lookupname.get(),
            ctx.callcontext.borrow().as_ref().map(|c| c.id),
            ctx.boundnode.borrow().as_ref().map(value_key),
        );
        if let Some(cached) = self.inf_cache.borrow().get(&key) {
            return Flow::ok(cached.to_vec());
        }
        let raw = self.infer_dispatch(node, &ctx);
        // limit loop (node_ng.py:160-176)
        let mut results: Vec<Value> = Vec::new();
        let mut truncated = false;
        for (i, v) in raw.vals.iter().enumerate() {
            if i >= MAX_INFERABLE_VALUES || ctx.nodes_inferred.get() > MAX_INFERRED {
                results.push(Value::Uninferable);
                truncated = true;
                break;
            }
            results.push(v.clone());
            ctx.bump_inferred();
        }
        if !truncated {
            if let Some(e) = raw.err {
                // error propagates; nothing cached
                return Flow {
                    vals: results,
                    err: Some(e),
                };
            }
        }
        self.inf_cache
            .borrow_mut()
            .insert(key, Rc::new(results.clone()));
        Flow::ok(results)
    }

    fn path_wrapped<F: FnOnce() -> Flow>(&self, node: GNode, ctx: &Rc<Ctx>, f: F) -> Flow {
        if ctx.push(node) {
            return Flow::empty();
        }
        let flow = f();
        let mut yielded: rustc_hash::FxHashSet<DedupKey> = Default::default();
        let vals = flow
            .vals
            .into_iter()
            .filter(|v| match dedup_key(v) {
                Some(k) => yielded.insert(k),
                None => true,
            })
            .collect();
        Flow {
            vals,
            err: flow.err,
        }
    }

    // ---------- per-kind dispatch with decorator table (notes/07 §4.1) ----------

    fn infer_dispatch(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let kind = &md.tree.nodes[node.n.idx()].kind;
        match kind {
            NodeKind::Name { .. } => self
                .path_wrapped(node, ctx, || self.infer_name(node, ctx))
                .raise_if_nothing(),
            NodeKind::AssignName { .. } => self
                .path_wrapped(node, ctx, || self.infer_assign_name(node, ctx))
                .raise_if_nothing(),
            NodeKind::AssignAttr { .. } => self
                .path_wrapped(node, ctx, || self.infer_assign_attr(node, ctx))
                .raise_if_nothing(),
            NodeKind::Attribute { .. } => self
                .path_wrapped(node, ctx, || self.infer_attribute_load(node, ctx))
                .raise_if_nothing(),
            NodeKind::Subscript { .. } => self
                .path_wrapped(node, ctx, || self.infer_subscript(node, ctx))
                .raise_if_nothing(),
            NodeKind::Call { .. } => self
                .path_wrapped(node, ctx, || self.infer_call(node, ctx))
                .raise_if_nothing(),
            NodeKind::AugAssign { .. } => self
                .path_wrapped(node, ctx, || self.infer_augassign_filtered(node, ctx))
                .raise_if_nothing(),
            NodeKind::BinOp { .. } => self
                .path_wrapped(node, ctx, || self.infer_binop_filtered(node, ctx))
                .yes_if_nothing(),
            NodeKind::BoolOp { .. } => self
                .path_wrapped(node, ctx, || self.infer_boolop(node, ctx))
                .raise_if_nothing(),
            NodeKind::UnaryOp { .. } => self
                .path_wrapped(node, ctx, || self.infer_unaryop_filtered(node, ctx))
                .raise_if_nothing(),
            NodeKind::Import { .. } => self
                .path_wrapped(node, ctx, || self.infer_import(node, ctx))
                .raise_if_nothing(),
            NodeKind::ImportFrom { .. } => self
                .path_wrapped(node, ctx, || self.infer_import_from(node, ctx))
                .raise_if_nothing(),
            NodeKind::Global { .. } => self
                .path_wrapped(node, ctx, || self.infer_global(node, ctx))
                .raise_if_nothing(),
            NodeKind::EmptyNode => self
                .path_wrapped(node, ctx, || self.infer_empty_node(node))
                .raise_if_nothing(),
            NodeKind::IfExp { .. } => self.infer_ifexp(node, ctx).raise_if_nothing(),
            NodeKind::List { .. } | NodeKind::Tuple { .. } | NodeKind::Set { .. } => {
                self.infer_container(node, ctx).raise_if_nothing()
            }
            NodeKind::Arguments(_) => self.infer_arguments_node(node, ctx).raise_if_nothing(),
            NodeKind::Const(_) | NodeKind::Slice { .. } | NodeKind::Module(_)
            | NodeKind::ClassDef(_) | NodeKind::Lambda(_) => Flow::one(Value::Node(node)),
            NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => {
                self.infer_functiondef(node, ctx)
            }
            NodeKind::Dict { .. } => self.infer_dict(node, ctx),
            NodeKind::Compare { .. } => self.infer_compare(node, ctx),
            NodeKind::JoinedStr { .. } => self.infer_joinedstr(node, ctx),
            NodeKind::FormattedValue { .. } => self.infer_formatted_value(node, ctx),
            NodeKind::Unknown => Flow::uninferable(),
            // NamedExpr is not directly inferable in astroid 4 (no _infer);
            // default: InferenceError
            _ => Flow::err(ErrKind::Inference),
        }
    }

    // ---------- _infer_stmts (bases.py:153-204) ----------

    pub fn infer_stmts(&self, stmts: &[NV], ctx_in: Option<&Rc<Ctx>>, frame: Option<GNode>) -> Flow {
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
        let mut out: Vec<Value> = Vec::new();
        for stmt in stmts {
            let stmt_node = match stmt {
                NV::V(Value::Uninferable) => {
                    out.push(Value::Uninferable);
                    inferred = true;
                    continue;
                }
                NV::V(v) => {
                    // proxies / object-model values infer to themselves
                    out.push(v.clone());
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
            let flow = self.infer(stmt_node, &ctx);
            for inf in &flow.vals {
                if stmt_constraints
                    .iter()
                    .all(|c| self.constraint_satisfied(c, inf, &ctx))
                {
                    out.push(inf.clone());
                    inferred = true;
                } else {
                    constraint_failed = true;
                }
            }
            if let Some(e) = flow.err {
                match e {
                    ErrKind::NameError => continue,
                    ErrKind::Inference => {
                        out.push(Value::Uninferable);
                        inferred = true;
                    }
                    other => {
                        return Flow {
                            vals: out,
                            err: Some(other),
                        }
                    }
                }
            }
        }
        if !inferred && constraint_failed {
            out.push(Value::Uninferable);
        } else if !inferred {
            return Flow::err(ErrKind::Inference);
        }
        Flow::ok(out)
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

    fn infer_name(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let name_sym = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Name { name } => self.g(&md, *name),
            _ => return Flow::err(ErrKind::Inference),
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
                return Flow::err(ErrKind::NameError);
            }
        }
        let ctx2 = copy_context(Some(ctx));
        ctx2.lookupname.set(Some(name_sym));
        let constraints = self.get_constraints(node, frame);
        ctx2.constraints
            .borrow_mut()
            .insert(name_sym, Rc::new(constraints));
        self.infer_stmts(&stmts, Some(&ctx2), Some(frame))
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

    fn infer_assign_name(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let parent = self.parent(node);
        if let Some(p) = parent {
            if self.kind_is(p, |k| matches!(k, NodeKind::AugAssign { .. })) {
                return self.infer(p, ctx);
            }
        }
        let stmts = match self.assigned_stmts(node, Some(ctx), None) {
            Ok(s) => s,
            Err(e) => return Flow::err(e),
        };
        self.infer_stmts(&stmts, Some(ctx), None)
    }

    fn infer_assign_attr(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let stmts = match self.assigned_stmts(node, Some(ctx), None) {
            Ok(s) => s,
            Err(e) => return Flow::err(e),
        };
        self.infer_stmts(&stmts, Some(ctx), None)
    }

    // ---------- Attribute(Load) (§12.1) ----------

    pub fn infer_attribute_load(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (expr, attrname) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Attribute { expr, attrname, .. } => {
                (GNode { m: node.m, n: *expr }, self.g(&md, *attrname))
            }
            NodeKind::AssignAttr { expr, attrname } => {
                (GNode { m: node.m, n: *expr }, self.g(&md, *attrname))
            }
            _ => return Flow::err(ErrKind::Inference),
        };
        let owners = self.infer(expr, ctx);
        let mut out: Vec<Value> = Vec::new();
        let mut cur_ctx = Rc::clone(ctx);
        for owner in &owners.vals {
            if owner.is_uninferable() {
                out.push(Value::Uninferable);
                continue;
            }
            cur_ctx = copy_context(Some(&cur_ctx));
            let old_bound = cur_ctx.boundnode.borrow().clone();
            *cur_ctx.boundnode.borrow_mut() = Some(owner.clone());
            // constraints when owner is ClassDef or Instance
            let frame_for_constraints: Option<GNode> = match owner {
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
                && matches!(owner, Value::Node(g)
                    if self.kind_is(*g, |k| matches!(k, NodeKind::Module(_)))
                        && self.md(g.m).name == "sys");
            if is_sys_argv {
                out.push(Value::Uninferable);
            } else {
                let flow = self.igetattr_value(owner, attrname, Some(&cur_ctx));
                match flow {
                    Ok(f) => {
                        out.extend(f.vals);
                        // errors mid-stream swallowed (try/except around
                        // the yield-from)
                    }
                    Err(_) => {}
                }
            }
            *cur_ctx.boundnode.borrow_mut() = old_bound;
        }
        if let Some(e) = owners.err {
            if !e.is_inference() {
                return Flow {
                    vals: out,
                    err: Some(e),
                };
            }
            // InferenceError from the owner generator propagates too
            return Flow {
                vals: out,
                err: Some(e),
            };
        }
        Flow::ok(out)
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

    fn infer_import_from(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return Flow::err(ErrKind::Inference),
        };
        let md = self.md(node.m);
        let names: Vec<(GSym, Option<GSym>)> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ImportFrom { names, .. } => names
                .iter()
                .map(|(a, b)| (self.g(&md, *a), b.map(|s| self.g(&md, s))))
                .collect(),
            _ => return Flow::err(ErrKind::Inference),
        };
        let real = match self.real_name(&names, name) {
            Some(r) => r,
            None => return Flow::err(ErrKind::Inference),
        };
        let module = match self.do_import_module(node, None) {
            Ok(m) => m,
            Err(_) => return Flow::err(ErrKind::Inference),
        };
        let ctx2 = copy_context(Some(ctx));
        let real_sym = self.sym(&real);
        ctx2.lookupname.set(Some(real_sym));
        let ignore_locals = module == node.m; // module is self.root()
        match self.module_getattr(module, real_sym, ignore_locals) {
            Ok(stmts) => self.infer_stmts(&stmts, Some(&ctx2), None),
            Err(_) => Flow::err(ErrKind::Inference),
        }
    }

    fn infer_global(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return Flow::err(ErrKind::Inference),
        };
        match self.module_getattr(node.m, name, false) {
            Ok(stmts) => self.infer_stmts(&stmts, Some(ctx), None),
            Err(_) => Flow::err(ErrKind::Inference),
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

    fn infer_ifexp(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (test, body, orelse) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::IfExp { test, body, orelse } => (*test, *body, *orelse),
            _ => return Flow::err(ErrKind::Inference),
        };
        let both_branches;
        // all inferred test values must agree (node_classes.py:3110-3135)
        let test_flow = self.infer(GNode { m: node.m, n: test }, &ctx.clone_ctx());
        let mut condition: Option<bool> = None;
        if test_flow.err.is_some() || test_flow.vals.is_empty() {
            both_branches = true;
        } else {
            let mut agreed: Option<bool> = None;
            let mut ok = true;
            for v in &test_flow.vals {
                match self.bool_value(v, ctx) {
                    None => {
                        ok = false;
                        break;
                    }
                    Some(b) => match agreed {
                        None => agreed = Some(b),
                        Some(prev) if prev == b => {}
                        _ => {
                            ok = false;
                            break;
                        }
                    },
                }
            }
            if ok {
                condition = agreed;
                both_branches = false;
            } else {
                both_branches = true;
            }
        }
        let mut out = Vec::new();
        let mut err = None;
        if both_branches || condition == Some(true) {
            let lhs = copy_context(Some(ctx));
            let f = self.infer(GNode { m: node.m, n: body }, &lhs);
            out.extend(f.vals);
            err = err.or(f.err);
        }
        if both_branches || condition == Some(false) {
            let rhs = copy_context(Some(ctx));
            let f = self.infer(GNode { m: node.m, n: orelse }, &rhs);
            out.extend(f.vals);
            err = err.or(f.err);
        }
        // InferenceError inside branches propagates only if nothing inferred
        if out.is_empty() {
            if let Some(e) = err {
                return Flow::err(e);
            }
        }
        Flow::ok(out)
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
        let mut out = Vec::new();
        let specs: Vec<Option<String>> = match format_spec {
            None => vec![Some(String::new())],
            Some(fs) => {
                let f = self.infer(GNode { m: node.m, n: fs }, ctx);
                f.vals
                    .iter()
                    .map(|v| match self.value_const(v) {
                        Some(ConstValue::Str(s)) => Some(s.to_string()),
                        _ => None,
                    })
                    .collect()
            }
        };
        for spec in specs {
            match spec {
                None => out.push(Value::Uninferable),
                Some(spec) => {
                    let vf = self.infer(GNode { m: node.m, n: value }, ctx);
                    for v in &vf.vals {
                        match self.value_const(v) {
                            Some(c) => match format_const(&c, &spec) {
                                Some(s) => out.push(Value::SynthConst(Rc::new(ConstValue::Str(
                                    s.into(),
                                )))),
                                None => out.push(Value::Uninferable),
                            },
                            None => out.push(Value::Uninferable),
                        }
                    }
                }
            }
        }
        Flow::ok(out)
    }

    fn infer_joinedstr(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let values: Vec<NodeId> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::JoinedStr { values } => values.clone(),
            _ => return Flow::err(ErrKind::Inference),
        };
        if values.is_empty() {
            return Flow::one(Value::SynthConst(Rc::new(ConstValue::Str("".into()))));
        }
        // cartesian combination; non-Const parts -> "{Uninferable}" marker
        let mut parts: Vec<Vec<Option<String>>> = Vec::new();
        for v in &values {
            let f = self.infer(GNode { m: node.m, n: *v }, ctx);
            if f.vals.is_empty() {
                return Flow::err(ErrKind::Inference);
            }
            parts.push(
                f.vals
                    .iter()
                    .map(|val| match self.value_const(val) {
                        Some(ConstValue::Str(s)) => Some(s.to_string()),
                        _ => None,
                    })
                    .collect(),
            );
        }
        let mut out: Vec<Value> = Vec::new();
        let mut uninferable_already = false;
        let mut idx = vec![0usize; parts.len()];
        'outer: loop {
            let mut s = String::new();
            let mut bad = false;
            for (i, &j) in idx.iter().enumerate() {
                match &parts[i][j] {
                    Some(p) => s.push_str(p),
                    None => {
                        bad = true;
                    }
                }
            }
            if bad {
                if !uninferable_already {
                    out.push(Value::Uninferable);
                    uninferable_already = true;
                }
            } else {
                out.push(Value::SynthConst(Rc::new(ConstValue::Str(s.into()))));
            }
            let mut i = idx.len();
            loop {
                if i == 0 {
                    break 'outer;
                }
                i -= 1;
                idx[i] += 1;
                if idx[i] < parts[i].len() {
                    break;
                }
                idx[i] = 0;
            }
        }
        Flow::ok(out)
    }

    // ---------- EmptyNode ----------

    fn infer_empty_node(&self, node: GNode) -> Flow {
        let md = self.md(node.m);
        match md.einf.get(&node.n) {
            None => Flow::uninferable(),
            Some(descs) => {
                let mut out = Vec::new();
                for d in descs {
                    out.push(self.resolve_einf(d));
                }
                if out.is_empty() {
                    Flow::uninferable()
                } else {
                    Flow::ok(out)
                }
            }
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

    // ---------- safe_infer (§5) ----------

    pub fn safe_infer(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Value> {
        let flow = self.infer(node, ctx);
        if flow.vals.is_empty() {
            return None;
        }
        if flow.vals.len() > 1 {
            return None; // ambiguity (incl. trailing error cases)
        }
        if flow.err.is_some() {
            // error while looking for the second value -> ambiguity
            return None;
        }
        Some(flow.vals[0].clone())
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
        let flow = self
            .igetattr_value(instance, sym, Some(ctx))
            .map_err(|e| e)?;
        let meth = match flow.vals.first() {
            Some(m) => m.clone(),
            None => return Ok(None),
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
        let res = self.infer_call_result(&meth, None, Some(ctx));
        match res.vals.first() {
            None => {
                if res.err.map(|e| e.is_inference()).unwrap_or(false) {
                    return Ok(None);
                }
                Ok(None)
            }
            Some(Value::Uninferable) => Ok(None),
            Some(value) => Ok(self.bool_value(value, ctx)),
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
