//! VariablesChecker port — E0601/E0602/E0603/E0604/E0605/E0606 with the FULL
//! NamesConsumer machinery.
//!
//! Truth: pylint 4.0.5 pylint/checkers/variables.py (cited variables.py:NNN),
//! spec reference/notes/05-variables.md. Disabled sibling messages
//! (W0611/W0621/W0631/W0640/E0611/...) are NOT emitted but state they share
//! with the in-scope logic is preserved. Config defaults assumed:
//! additional-builtins=(), init-import=False, py-version=3.12.

use indexmap::IndexMap;
use pyast::tree::{ConstValue, Ctx as ExprCtx, NodeKind};
use pyast::NodeId;
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, GSym, Value, NV};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::ckutils as u;
use crate::walker::WalkCx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeType {
    Module,
    Class,
    Function,
    Lambda,
    Comprehension,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Continue,
    Return,
}

/// NamesConsumer (variables.py:504-559)
pub struct Consumer {
    node: GNode,
    scope_type: ScopeType,
    to_consume: IndexMap<GSym, Vec<GNode>>,
    consumed: IndexMap<GSym, Vec<GNode>>,
    /// defaultdict(list): key EXISTENCE matters (notes/05 §3, §10)
    consumed_uncertain: IndexMap<GSym, Vec<GNode>>,
    names_under_always_false_test: FxHashSet<GSym>,
    names_defined_under_one_branch_only: FxHashSet<GSym>,
}

impl Consumer {
    fn new(eng: &Engine, node: GNode, scope_type: ScopeType) -> Consumer {
        let md = eng.md(node.m);
        let mut to_consume: IndexMap<GSym, Vec<GNode>> = IndexMap::new();
        if scope_type == ScopeType::Class {
            // astroid ClassDef.__init__ add_local_node's the implicit locals
            // (__module__/__qualname__/__annotations__, scoped_nodes.py:
            // 1910-1912, 1921-1933) BEFORE any body name; their parent is
            // the class (add_local_node -> _append_node sets child.parent).
            for (w, nm) in ["__module__", "__qualname__", "__annotations__"]
                .iter()
                .enumerate()
            {
                let sym = eng.sym(nm);
                to_consume.insert(sym, vec![eng.implicit_class_local(node, w as u8)]);
            }
        }
        if let Some(locals) = md.locals.borrow().get(&node.n) {
            for (k, v) in locals {
                to_consume
                    .entry(*k)
                    .or_default()
                    .extend(v.iter().copied());
            }
        }
        Consumer {
            node,
            scope_type,
            to_consume,
            consumed: IndexMap::new(),
            consumed_uncertain: IndexMap::new(),
            names_under_always_false_test: FxHashSet::default(),
            names_defined_under_one_branch_only: FxHashSet::default(),
        }
    }

    /// mark_as_consumed (variables.py:547-559)
    fn mark_as_consumed(&mut self, name: GSym, consumed_nodes: Vec<GNode>) {
        let set: FxHashSet<GNode> = consumed_nodes.iter().copied().collect();
        let unconsumed: Vec<GNode> = self
            .to_consume
            .get(&name)
            .map(|v| v.iter().copied().filter(|n| !set.contains(n)).collect())
            .unwrap_or_default();
        self.consumed.insert(name, consumed_nodes);
        if !unconsumed.is_empty() {
            self.to_consume.insert(name, unconsumed);
        } else {
            self.to_consume.shift_remove(&name);
        }
    }
}

pub struct VarsChecker {
    to_consume: Vec<Consumer>,
    postponed_evaluation_enabled: bool,
    /// _reported_type_checking_usage_scopes — instance state, persists
    /// ACROSS modules (variables.py:1334-1336)
    reported_type_checking_usage_scopes: FxHashMap<String, Vec<GNode>>,
    /// _type_annotation_names (string-literal annotations recorded by
    /// visit_const). NOT reset when leave_module returns early for a
    /// package __init__ — the LEAK into the next module is replicated
    /// (variables.py:1438-1444, notes/09 §1.3.1).
    type_annotation_names: Vec<String>,
    /// _except_handler_names_queue: (outer handler, its AssignName)
    except_handler_names_queue: Vec<(GNode, GNode)>,
}

impl Default for VarsChecker {
    fn default() -> Self {
        VarsChecker {
            to_consume: Vec::new(),
            postponed_evaluation_enabled: false,
            reported_type_checking_usage_scopes: FxHashMap::default(),
            type_annotation_names: Vec::new(),
            except_handler_names_queue: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// small node helpers
// ---------------------------------------------------------------------------

fn name_of(eng: &Engine, g: GNode) -> Option<GSym> {
    u::name_gsym(eng, g)
}

fn stmt_of(eng: &Engine, g: GNode) -> GNode {
    eng.statement(g).unwrap_or(g)
}

/// type_params of a ClassDef/FunctionDef: any param AssignName == name?
fn type_param_matches(eng: &Engine, scope: GNode, name: GSym) -> bool {
    let md = eng.md(scope.m);
    let tps: Vec<NodeId> = match &md.tree.nodes[scope.n.idx()].kind {
        NodeKind::ClassDef(d) => d.type_params.clone(),
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.type_params.clone(),
        _ => return false,
    };
    drop(md);
    for tp in tps {
        let g = GNode { m: scope.m, n: tp };
        let md = eng.md(g.m);
        let nm = match &md.tree.nodes[tp.idx()].kind {
            NodeKind::TypeVar { name, .. }
            | NodeKind::ParamSpec { name }
            | NodeKind::TypeVarTuple { name } => *name,
            _ => continue,
        };
        if let NodeKind::AssignName { name: n } = &md.tree.nodes[nm.idx()].kind {
            if eng.g(&md, *n) == name {
                return true;
            }
        }
    }
    false
}

/// node is (by identity) inside defframe.type_params subtrees
fn defnode_in_type_params(eng: &Engine, defframe: GNode, defnode: GNode) -> bool {
    let md = eng.md(defframe.m);
    let tps: Vec<NodeId> = match &md.tree.nodes[defframe.n.idx()].kind {
        NodeKind::ClassDef(d) => d.type_params.clone(),
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.type_params.clone(),
        _ => return false,
    };
    drop(md);
    tps.iter().any(|&tp| defnode.n == tp && defnode.m == defframe.m)
}

fn locals_contains(eng: &Engine, scope: GNode, name: GSym) -> bool {
    eng.md(scope.m)
        .locals
        .borrow()
        .get(&scope.n)
        .map(|l| l.contains_key(&name))
        .unwrap_or(false)
}

fn locals_get(eng: &Engine, scope: GNode, name: GSym) -> Vec<GNode> {
    eng.md(scope.m)
        .locals
        .borrow()
        .get(&scope.n)
        .and_then(|l| l.get(&name).cloned())
        .unwrap_or_default()
}

fn module_node(g: GNode) -> GNode {
    GNode { m: g.m, n: NodeId::MODULE }
}

/// _flattened_scope_names over Global nodes of a frame (top-down preorder)
fn global_names_in(eng: &Engine, frame: GNode) -> FxHashSet<GSym> {
    let mut out = FxHashSet::default();
    for n in u::preorder(eng, frame) {
        let md = eng.md(n.m);
        if let NodeKind::Global { names } = &md.tree.nodes[n.n.idx()].kind {
            for &s in names {
                out.insert(eng.g(&md, s));
            }
        }
    }
    out
}

/// _flattened_scope_names over Nonlocal nodes of a frame
fn nonlocal_names_in(eng: &Engine, frame: GNode) -> FxHashSet<GSym> {
    let mut out = FxHashSet::default();
    for n in u::preorder(eng, frame) {
        let md = eng.md(n.m);
        if let NodeKind::Nonlocal { names } = &md.tree.nodes[n.n.idx()].kind {
            for &s in names {
                out.insert(eng.g(&md, s));
            }
        }
    }
    out
}

/// utils.find_assigned_names_recursive (utils.py:2051-2061)
fn find_assigned_names_recursive(eng: &Engine, target: GNode, out: &mut FxHashSet<GSym>) {
    let md = eng.md(target.m);
    match &md.tree.nodes[target.n.idx()].kind {
        NodeKind::AssignName { name } => {
            out.insert(eng.g(&md, *name));
        }
        NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } | NodeKind::Set { elts } => {
            let elts = elts.clone();
            drop(md);
            for e in elts {
                find_assigned_names_recursive(eng, GNode { m: target.m, n: e }, out);
            }
        }
        _ => {}
    }
}

/// _import_name_is_global (variables.py:277-289)
fn import_name_is_global(eng: &Engine, stmt: GNode, global_names: &FxHashSet<GSym>) -> bool {
    let md = eng.md(stmt.m);
    let pairs: Vec<(GSym, Option<GSym>)> = match &md.tree.nodes[stmt.n.idx()].kind {
        NodeKind::Global { names } => names.iter().map(|&s| (eng.g(&md, s), None)).collect(),
        NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } => names
            .iter()
            .map(|&(n, a)| (eng.g(&md, n), a.map(|x| eng.g(&md, x))))
            .collect(),
        _ => return false,
    };
    drop(md);
    for (import_name, alias) in pairs {
        if let Some(a) = alias {
            if global_names.contains(&a) {
                return true;
            }
        } else if global_names.contains(&import_name) {
            return true;
        }
    }
    false
}

/// FunctionDef.argnames(): posonly + args + vararg + kwonly + kwarg
fn func_argnames(eng: &Engine, func: GNode) -> Vec<GSym> {
    let Some(spec) = eng.arg_spec(func) else { return Vec::new() };
    let mut out: Vec<GSym> = Vec::new();
    for g in spec.posonlyargs.iter().chain(spec.args.iter()) {
        if let Some(s) = eng.assign_name_of(*g) {
            out.push(s);
        }
    }
    if let Some(v) = spec.vararg {
        out.push(v);
    }
    for g in &spec.kwonlyargs {
        if let Some(s) = eng.assign_name_of(*g) {
            out.push(s);
        }
    }
    if let Some(k) = spec.kwarg {
        out.push(k);
    }
    out
}

/// _has_locals_call_after_node (variables.py:333-348)
fn has_locals_call_after_node(cx: &mut WalkCx, stmt: GNode, scope: GNode) -> bool {
    let eng = cx.eng;
    let calls = crate::basicerr::nodes_of_class(
        eng,
        scope,
        |k| matches!(k, NodeKind::Call { .. }),
        |k| {
            matches!(
                k,
                NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::ClassDef(_)
                    | NodeKind::Import { .. }
                    | NodeKind::ImportFrom { .. }
            )
        },
    );
    for call in calls {
        let func = {
            let md = eng.md(call.m);
            match &md.tree.nodes[call.n.idx()].kind {
                NodeKind::Call { func, .. } => GNode { m: call.m, n: *func },
                _ => continue,
            }
        };
        let inferred = u::safe_infer(eng, cx.caches, func);
        if let Some(Value::Node(g)) = inferred {
            if eng.md(g.m).name == "builtins"
                && eng.node_name(g).as_deref() == Some("locals")
                && raw_lineno(eng, stmt) < eng.fromlineno(call)
            {
                return true;
            }
        }
    }
    false
}

/// utils.overridden_method (utils.py:2323-2340)
fn overridden_method(eng: &Engine, klass: GNode, name: &str) -> Option<GNode> {
    if !u::is_classdef(eng, klass) {
        return None;
    }
    let sym = eng.sym(name);
    // ClassDef.local_attr_ancestors: mro()[1:] first, lazy ancestors()
    // fallback on MroError; next() abandons after the FIRST hit.
    let parent: Option<GNode> = match eng.mro(klass, None) {
        Ok(m) => m
            .get(1..)
            .unwrap_or(&[])
            .iter()
            .copied()
            .find(|&anc| !eng.class_locals_get(anc, sym).is_empty()),
        Err(_) => {
            let mut found = None;
            let _ = eng.ancestors_to(klass, true, None, &mut |anc| {
                if !eng.class_locals_get(anc, sym).is_empty() {
                    found = Some(anc);
                    return pyinfer::value::Drive::Stop;
                }
                pyinfer::value::Drive::Go
            });
            found
        }
    };
    let parent = parent?;
    let meth_node = *eng.class_locals_get(parent, sym).first()?;
    if u::is_functiondef(eng, meth_node) {
        Some(meth_node)
    } else {
        None
    }
}

/// astroid FunctionDef.is_abstract(pass_is_abstract=True)
fn func_is_abstract(cx: &mut WalkCx, func: GNode) -> bool {
    let eng = cx.eng;
    for dec in crate::typecheck::decorator_nodes_pub(eng, func) {
        let inferred = match eng.first_value(dec, &u::fresh_ctx()) {
            Ok(Some(v)) => v,
            _ => continue,
        };
        if let Some(q) = eng.value_qname(&inferred) {
            if q == "abc.abstractproperty" || q == "abc.abstractmethod" {
                return true;
            }
        }
    }
    let body: Vec<NodeId> = {
        let md = eng.md(func.m);
        match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
            _ => return false,
        }
    };
    for &child in &body {
        let g = GNode { m: func.m, n: child };
        let exc: Option<NodeId> = {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Raise { exc, .. } => Some(exc.unwrap_or(pyast::NodeId::MODULE)),
                _ => None,
            }
        };
        if let Some(e) = exc {
            // raises_not_implemented: any Name node in exc subtree named
            // NotImplementedError (textual, node_classes.py:3470-3479)
            if e != pyast::NodeId::MODULE {
                let eg = GNode { m: func.m, n: e };
                let raises_ni = u::preorder(eng, eg).iter().any(|&n| {
                    let md = eng.md(n.m);
                    matches!(&md.tree.nodes[n.n.idx()].kind,
                        NodeKind::Name { name } if md.tree.s(*name) == "NotImplementedError")
                });
                if raises_ni {
                    return true;
                }
            }
        }
        // first child decides (the loop returns on its first iteration)
        return eng.kind_is(g, |k| matches!(k, NodeKind::Pass));
    }
    // empty function body == single pass
    true
}

/// _is_exception_binding_used_in_handler (variables.py:2943-2950)
fn is_exception_binding_used_in_handler(eng: &Engine, stmt: GNode, name: GSym) -> bool {
    let Some(parent) = eng.parent(stmt) else { return false };
    if !u::is_excepthandler(eng, parent) {
        return false;
    }
    let is_handler_name = {
        let md = eng.md(parent.m);
        matches!(&md.tree.nodes[parent.n.idx()].kind,
            NodeKind::ExceptHandler { name: Some(n), .. } if *n == stmt.n)
    };
    if !is_handler_name {
        return false;
    }
    u::preorder(eng, parent).iter().any(|&n| {
        eng.kind_is(n, |k| matches!(k, NodeKind::Name { .. })) && name_of(eng, n) == Some(name)
    })
}

/// _is_nonlocal_name (variables.py:320-330)
fn is_nonlocal_name(eng: &Engine, node: GNode, frame: GNode) -> bool {
    if !u::is_functiondef(eng, frame) {
        return false;
    }
    let Some(name) = name_of(eng, node) else { return false };
    let md = eng.md(frame.m);
    let body: Vec<NodeId> = match &md.tree.nodes[frame.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
        _ => return false,
    };
    drop(md);
    for b in body {
        let g = GNode { m: frame.m, n: b };
        let md = eng.md(g.m);
        if let NodeKind::Nonlocal { names } = &md.tree.nodes[b.idx()].kind {
            if names.iter().any(|&s| eng.g(&md, s) == name) {
                drop(md);
                if u::is_before(eng, g, node) {
                    return true;
                }
            }
        }
    }
    false
}

/// _find_frame_imports (variables.py:255-274)
fn find_frame_imports(eng: &Engine, name: GSym, frame: GNode) -> bool {
    if global_names_in(eng, frame).contains(&name) {
        return false;
    }
    for n in u::preorder(eng, frame) {
        let md = eng.md(n.m);
        if let NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } =
            &md.tree.nodes[n.n.idx()].kind
        {
            for (imp, alias) in names {
                if let Some(a) = alias {
                    if eng.g(&md, *a) == name {
                        return true;
                    }
                } else if eng.g(&md, *imp) == name {
                    return true;
                }
            }
        }
    }
    false
}

/// _assigned_locally (variables.py:299-305)
fn assigned_locally(eng: &Engine, node: GNode) -> bool {
    let Some(name) = name_of(eng, node) else { return false };
    let scope = eng.scope(node);
    for n in u::preorder(eng, scope) {
        if eng.kind_is(n, |k| matches!(k, NodeKind::AssignName { .. }))
            && name_of(eng, n) == Some(name)
        {
            return true;
        }
    }
    find_frame_imports(eng, name, scope)
}

// ---------------------------------------------------------------------------
// NamesConsumer machinery
// ---------------------------------------------------------------------------

impl Consumer {
    /// get_next_to_consume (variables.py:561-654).
    /// None => CONTINUE to outer; Some(empty) => unfound path; Some(v) => defs.
    fn get_next_to_consume(&mut self, cx: &mut WalkCx, node: GNode) -> Option<Vec<GNode>> {
        let eng = cx.eng;
        let name = name_of(eng, node)?;
        let parent_node = eng.parent(node);
        let mut found_nodes: Option<Vec<GNode>> = self.to_consume.get(&name).cloned();
        let node_statement = stmt_of(eng, node);

        let truthy = |f: &Option<Vec<GNode>>| f.as_ref().map(|v| !v.is_empty()).unwrap_or(false);

        // (a) `x = x` self-definition
        if truthy(&found_nodes) {
            if let Some(pn) = parent_node {
                if eng.kind_is(pn, |k| matches!(k, NodeKind::Assign { .. })) {
                    let first = found_nodes.as_ref().unwrap()[0];
                    if eng.parent(first) == Some(pn) {
                        let md = eng.md(pn.m);
                        if let NodeKind::Assign { targets, .. } = &md.tree.nodes[pn.n.idx()].kind {
                            if let Some(&t0) = targets.first() {
                                if let NodeKind::AssignName { name: lhs } =
                                    &md.tree.nodes[t0.idx()].kind
                                {
                                    if eng.g(&md, *lhs) == name {
                                        found_nodes = None;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // (b) `for x in x:`
        if truthy(&found_nodes) {
            if let Some(pn) = parent_node {
                let md = eng.md(pn.m);
                if let NodeKind::For(d) | NodeKind::AsyncFor(d) = &md.tree.nodes[pn.n.idx()].kind {
                    let (target, iter) = (d.target, d.iter);
                    drop(md);
                    let tg = GNode { m: pn.m, n: target };
                    if iter == node.n
                        && found_nodes.as_ref().unwrap().contains(&tg)
                    {
                        let others: Vec<GNode> = found_nodes
                            .as_ref()
                            .unwrap()
                            .iter()
                            .copied()
                            .filter(|&fnode| fnode != tg)
                            .collect();
                        found_nodes = if others.is_empty() { None } else { Some(others) };
                    }
                }
            }
        }

        // (c) nonlocal in node.frame() -> unfiltered
        if is_nonlocal_name(eng, node, eng.frame(node)) {
            return found_nodes;
        }
        // (d) comprehension between frame and node -> unfiltered
        if comprehension_between_frame_and_node(eng, node) {
            return found_nodes;
        }

        // (e) except-binding filter (silent)
        if truthy(&found_nodes) {
            let filtered: Vec<GNode> = found_nodes
                .as_ref()
                .unwrap()
                .iter()
                .copied()
                .filter(|&n| {
                    let st = stmt_of(eng, n);
                    !u::is_excepthandler(eng, st) || eng.parent_of(st, node)
                })
                .collect();
            found_nodes = Some(filtered);
        }

        // (f) if-test filter
        if truthy(&found_nodes) {
            let uncertain = self.uncertain_nodes_if_tests(cx, found_nodes.as_ref().unwrap(), node);
            let entry = self.consumed_uncertain.entry(name).or_default();
            entry.extend(uncertain.iter().copied());
            let set: FxHashSet<GNode> = uncertain.into_iter().collect();
            found_nodes = Some(
                found_nodes
                    .unwrap()
                    .into_iter()
                    .filter(|n| !set.contains(n))
                    .collect(),
            );
        }
        // (g) except-block filter
        if truthy(&found_nodes) {
            let uncertain =
                uncertain_nodes_in_except_blocks(cx, found_nodes.as_ref().unwrap(), node, node_statement);
            let entry = self.consumed_uncertain.entry(name).or_default();
            entry.extend(uncertain.iter().copied());
            let set: FxHashSet<GNode> = uncertain.into_iter().collect();
            found_nodes = Some(
                found_nodes
                    .unwrap()
                    .into_iter()
                    .filter(|n| !set.contains(n))
                    .collect(),
            );
        }
        // (h) try-vs-finally filter
        if truthy(&found_nodes) {
            let uncertain = uncertain_nodes_in_try_finally(
                cx,
                found_nodes.as_ref().unwrap(),
                node_statement,
                name,
            );
            let entry = self.consumed_uncertain.entry(name).or_default();
            entry.extend(uncertain.iter().copied());
            let set: FxHashSet<GNode> = uncertain.into_iter().collect();
            found_nodes = Some(
                found_nodes
                    .unwrap()
                    .into_iter()
                    .filter(|n| !set.contains(n))
                    .collect(),
            );
        }
        // (i) try-vs-except filter
        if truthy(&found_nodes) {
            let uncertain = uncertain_nodes_in_try_except(
                cx.eng,
                found_nodes.as_ref().unwrap(),
                node_statement,
            );
            let entry = self.consumed_uncertain.entry(name).or_default();
            entry.extend(uncertain.iter().copied());
            let set: FxHashSet<GNode> = uncertain.into_iter().collect();
            found_nodes = Some(
                found_nodes
                    .unwrap()
                    .into_iter()
                    .filter(|n| !set.contains(n))
                    .collect(),
            );
        }
        found_nodes
    }

    /// _uncertain_nodes_if_tests (variables.py:759-809)
    fn uncertain_nodes_if_tests(
        &mut self,
        cx: &mut WalkCx,
        found_nodes: &[GNode],
        node: GNode,
    ) -> Vec<GNode> {
        let eng = cx.eng;
        let mut uncertain = Vec::new();
        for &other_node in found_nodes {
            let name: GSym = {
                let md = eng.md(other_node.m);
                match &md.tree.nodes[other_node.n.idx()].kind {
                    NodeKind::AssignName { name } => eng.g(&md, *name),
                    NodeKind::Import { .. } | NodeKind::ImportFrom { .. } => {
                        drop(md);
                        match name_of(eng, node) {
                            Some(s) => s,
                            None => continue,
                        }
                    }
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => eng.g(&md, d.name),
                    NodeKind::ClassDef(d) => eng.g(&md, d.name),
                    _ => continue,
                }
            };
            let all_if: Vec<GNode> = u::ancestors(eng, other_node)
                .into_iter()
                .filter(|&a| u::is_if(eng, a) && !eng.parent_of(a, node))
                .collect();
            if all_if.is_empty() {
                continue;
            }
            let closest_if = all_if[0];
            let node_is_assignname =
                eng.kind_is(node, |k| matches!(k, NodeKind::AssignName { .. }));
            if node_is_assignname && eng.frame(node) != eng.frame(closest_if) {
                continue;
            }
            if eng.parent_of(closest_if, node) {
                continue;
            }
            let outer_if = *all_if.last().unwrap();
            if node_guarded_by_same_test(cx, node, outer_if) {
                continue;
            }
            if self.inferred_to_define_name_raise_or_return(cx, name, outer_if) {
                continue;
            }
            uncertain.push(other_node);
        }
        uncertain
    }

    /// _inferred_to_define_name_raise_or_return (variables.py:656-700)
    fn inferred_to_define_name_raise_or_return(
        &mut self,
        cx: &mut WalkCx,
        name: GSym,
        node: GNode,
    ) -> bool {
        let eng = cx.eng;
        let md = eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Try(d) => {
                // try_except_node = node (next over self-inclusive preorder)
                let handlers = d.handlers.clone();
                drop(md);
                if defines_name_raises_or_returns_recursive(cx, name, node) {
                    return true;
                }
                handlers.iter().all(|&h| {
                    defines_name_raises_or_returns_recursive(cx, name, GNode { m: node.m, n: h })
                })
            }
            NodeKind::With(_)
            | NodeKind::AsyncWith(_)
            | NodeKind::For(_)
            | NodeKind::AsyncFor(_)
            | NodeKind::While { .. } => {
                drop(md);
                defines_name_raises_or_returns_recursive(cx, name, node)
            }
            NodeKind::Match { cases, .. } => {
                let cases = cases.clone();
                drop(md);
                cases.iter().all(|&c| {
                    defines_name_raises_or_returns_recursive(cx, name, GNode { m: node.m, n: c })
                })
            }
            NodeKind::If { .. } => {
                drop(md);
                self.inferred_to_define_name_raise_or_return_for_if_node(cx, name, node)
            }
            _ => false, // unreachable per pylint AssertionError
        }
    }

    /// _inferred_to_define_name_raise_or_return_for_if_node
    /// (variables.py:702-737)
    fn inferred_to_define_name_raise_or_return_for_if_node(
        &mut self,
        cx: &mut WalkCx,
        name: GSym,
        node: GNode,
    ) -> bool {
        let eng = cx.eng;
        // "Be permissive if there is a break or a continue" — but
        // `node.nodes_of_class(nodes.Break, nodes.Continue)` passes Continue
        // as SKIP_KLASS (node_ng.py:497-528): only Break nodes match!
        if u::preorder(eng, node)
            .iter()
            .any(|&n| eng.kind_is(n, |k| matches!(k, NodeKind::Break)))
        {
            return true;
        }
        if defines_name_raises_or_returns(cx, name, node) {
            return true;
        }
        let test = {
            let md = eng.md(node.m);
            let NodeKind::If { test, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return false;
            };
            let t = GNode { m: node.m, n: *test };
            match &md.tree.nodes[t.n.idx()].kind {
                NodeKind::NamedExpr { value, .. } => GNode { m: node.m, n: *value },
                _ => t,
            }
        };
        let all_inferred = u::infer_all(eng, cx.caches, test);
        let mut only_search_else = true;
        for inferred in all_inferred.iter() {
            match eng.value_const(inferred) {
                None => {
                    only_search_else = false;
                    continue;
                }
                Some(c) => {
                    // only_search_if is computed but never used (dead code)
                    only_search_else = only_search_else && !u::const_truthy(&c);
                }
            }
        }
        let (body, orelse) = {
            let md = eng.md(node.m);
            let NodeKind::If { body, orelse, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return false;
            };
            (body.clone(), orelse.clone())
        };
        if !all_inferred.is_empty() && only_search_else {
            self.names_under_always_false_test.insert(name);
            return self.branch_handles_name(cx, name, node.m, &orelse);
        }
        let if_branch_handles = self.branch_handles_name(cx, name, node.m, &body);
        let else_branch_handles = self.branch_handles_name(cx, name, node.m, &orelse);
        if if_branch_handles ^ else_branch_handles {
            self.names_defined_under_one_branch_only.insert(name);
        } else if self.names_defined_under_one_branch_only.contains(&name) {
            self.names_defined_under_one_branch_only.remove(&name);
        }
        if_branch_handles && else_branch_handles
    }

    /// _branch_handles_name (variables.py:739-757)
    fn branch_handles_name(
        &mut self,
        cx: &mut WalkCx,
        name: GSym,
        m: pyinfer::value::ModId,
        body: &[NodeId],
    ) -> bool {
        for &stmt in body {
            let g = GNode { m, n: stmt };
            if defines_name_raises_or_returns(cx, name, g) {
                return true;
            }
            let is_compound = cx.eng.kind_is(g, |k| {
                matches!(
                    k,
                    NodeKind::If { .. }
                        | NodeKind::Try(_)
                        | NodeKind::With(_)
                        | NodeKind::AsyncWith(_)
                        | NodeKind::For(_)
                        | NodeKind::AsyncFor(_)
                        | NodeKind::While { .. }
                        | NodeKind::Match { .. }
                )
            });
            if is_compound && self.inferred_to_define_name_raise_or_return(cx, name, g) {
                return true;
            }
        }
        false
    }
}

/// _comprehension_between_frame_and_node (variables.py:3010-3020)
fn comprehension_between_frame_and_node(eng: &Engine, node: GNode) -> bool {
    let closest = u::first_ancestor(eng, node, |k| {
        matches!(
            k,
            NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_) | NodeKind::GeneratorExp(_)
        )
    });
    match closest {
        Some(c) => eng.parent_of(eng.frame(node), c),
        None => false,
    }
}

/// _node_guarded_by_same_test (variables.py:811-845)
fn node_guarded_by_same_test(cx: &mut WalkCx, node: GNode, other_if: GNode) -> bool {
    let eng = cx.eng;
    let other_if_test = {
        let md = eng.md(other_if.m);
        let NodeKind::If { test, .. } = &md.tree.nodes[other_if.n.idx()].kind else {
            return false;
        };
        let t = GNode { m: other_if.m, n: *test };
        match &md.tree.nodes[t.n.idx()].kind {
            NodeKind::NamedExpr { target, .. } => GNode { m: other_if.m, n: *target },
            _ => t,
        }
    };
    let other_str = u::as_string(eng, other_if_test);
    let other_inferred = u::infer_all(eng, cx.caches, other_if_test);
    for ancestor in u::ancestors(eng, node) {
        let md = eng.md(ancestor.m);
        let test = match &md.tree.nodes[ancestor.n.idx()].kind {
            NodeKind::If { test, .. } | NodeKind::IfExp { test, .. } => {
                GNode { m: ancestor.m, n: *test }
            }
            _ => continue,
        };
        drop(md);
        if u::as_string(eng, test) == other_str {
            return true;
        }
        if eng.kind_is(test, |k| matches!(k, NodeKind::Name { .. })) {
            continue;
        }
        let all_inferred = u::infer_all(eng, cx.caches, test);
        if all_inferred.len() == other_inferred.len() {
            let mut all_const = true;
            let mut set_a: FxHashSet<u::PyKey> = FxHashSet::default();
            let mut set_b: FxHashSet<u::PyKey> = FxHashSet::default();
            for v in all_inferred.iter() {
                match eng.value_const(v) {
                    Some(c) => {
                        set_a.insert(u::py_key(&c));
                    }
                    None => {
                        all_const = false;
                        break;
                    }
                }
            }
            if all_const {
                for v in other_inferred.iter() {
                    match eng.value_const(v) {
                        Some(c) => {
                            set_b.insert(u::py_key(&c));
                        }
                        None => {
                            all_const = false;
                            break;
                        }
                    }
                }
            }
            if !all_const {
                continue;
            }
            if set_a != set_b {
                continue;
            }
            return true;
        }
    }
    false
}

/// _defines_name_raises_or_returns (variables.py:928-980)
fn defines_name_raises_or_returns(cx: &mut WalkCx, name: GSym, node: GNode) -> bool {
    let eng = cx.eng;
    let md = eng.md(node.m);
    match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::Raise { .. } | NodeKind::Assert { .. } | NodeKind::Return { .. }
        | NodeKind::Continue => return true,
        NodeKind::Expr { value } => {
            let v = GNode { m: node.m, n: *value };
            if let NodeKind::Call { func, .. } = &md.tree.nodes[v.n.idx()].kind {
                let func = GNode { m: node.m, n: *func };
                drop(md);
                if u::is_terminating_func(eng, cx.caches, v) {
                    return true;
                }
                let md = eng.md(func.m);
                if let NodeKind::Name { name: fname } = &md.tree.nodes[func.n.idx()].kind {
                    if md.tree.s(*fname) == "assert_never" {
                        return true;
                    }
                }
            }
            return false;
        }
        NodeKind::AnnAssign { target, value: Some(_), .. } => {
            if let NodeKind::AssignName { name: t } = &md.tree.nodes[target.idx()].kind {
                if eng.g(&md, *t) == name {
                    return true;
                }
            }
        }
        _ => {}
    }
    let md = eng.md(node.m);
    if let NodeKind::Assign { targets, .. } = &md.tree.nodes[node.n.idx()].kind {
        let targets = targets.clone();
        drop(md);
        for t in targets {
            for elt in u::get_all_elements(eng, GNode { m: node.m, n: t }) {
                let elt = {
                    let md = eng.md(elt.m);
                    match &md.tree.nodes[elt.n.idx()].kind {
                        NodeKind::Starred { value, .. } => GNode { m: elt.m, n: *value },
                        _ => elt,
                    }
                };
                if eng.kind_is(elt, |k| matches!(k, NodeKind::AssignName { .. }))
                    && name_of(eng, elt) == Some(name)
                {
                    return true;
                }
            }
        }
    }
    let md = eng.md(node.m);
    if matches!(md.tree.nodes[node.n.idx()].kind, NodeKind::If { .. }) {
        drop(md);
        for sub in u::preorder(eng, node) {
            let md = eng.md(sub.m);
            if let NodeKind::NamedExpr { target, .. } = &md.tree.nodes[sub.n.idx()].kind {
                if let NodeKind::AssignName { name: t } = &md.tree.nodes[target.idx()].kind {
                    if eng.g(&md, *t) == name {
                        return true;
                    }
                }
            }
        }
    }
    let md = eng.md(node.m);
    if let NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } =
        &md.tree.nodes[node.n.idx()].kind
    {
        let name_str = eng.sname(name);
        for (n, asn) in names {
            let n_str = md.tree.s(*n);
            if let Some(a) = asn {
                if md.tree.s(*a) == name_str {
                    return true;
                }
            }
            if n_str == name_str || n_str.starts_with(&format!("{name_str}.")) {
                return true;
            }
        }
    }
    let md = eng.md(node.m);
    if let NodeKind::With(d) | NodeKind::AsyncWith(d) = &md.tree.nodes[node.n.idx()].kind {
        for (_, var) in &d.items {
            if let Some(v) = var {
                if let NodeKind::AssignName { name: t } = &md.tree.nodes[v.idx()].kind {
                    if eng.g(&md, *t) == name {
                        return true;
                    }
                }
            }
        }
    }
    let md = eng.md(node.m);
    match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::ClassDef(d) => {
            if eng.g(&md, d.name) == name {
                return true;
            }
        }
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
            if eng.g(&md, d.name) == name {
                return true;
            }
        }
        NodeKind::ExceptHandler { name: Some(n), .. } => {
            if let NodeKind::AssignName { name: t } = &md.tree.nodes[n.idx()].kind {
                if eng.g(&md, *t) == name {
                    return true;
                }
            }
        }
        _ => {}
    }
    false
}

/// _defines_name_raises_or_returns_recursive (variables.py:982-1014)
fn defines_name_raises_or_returns_recursive(cx: &mut WalkCx, name: GSym, node: GNode) -> bool {
    let eng = cx.eng;
    let children: Vec<NodeId> = eng.md(node.m).tree.children(node.n);
    for c in children {
        let stmt = GNode { m: node.m, n: c };
        if defines_name_raises_or_returns(cx, name, stmt) {
            return true;
        }
        let md = eng.md(stmt.m);
        match &md.tree.nodes[stmt.n.idx()].kind {
            NodeKind::If { .. } | NodeKind::With(_) | NodeKind::AsyncWith(_) => {
                drop(md);
                let kids = eng.md(stmt.m).tree.children(stmt.n);
                if kids
                    .iter()
                    .any(|&k| defines_name_raises_or_returns(cx, name, GNode { m: stmt.m, n: k }))
                {
                    return true;
                }
            }
            NodeKind::Try(d) => {
                let has_final = !d.finalbody.is_empty();
                drop(md);
                if !has_final && defines_name_raises_or_returns_recursive(cx, name, stmt) {
                    return true;
                }
            }
            NodeKind::Match { cases, .. } => {
                let cases = cases.clone();
                drop(md);
                // beware: returns IMMEDIATELY (even when False)
                return cases.iter().all(|&case| {
                    defines_name_raises_or_returns_recursive(cx, name, GNode { m: stmt.m, n: case })
                });
            }
            _ => {}
        }
    }
    false
}

/// _uncertain_nodes_in_except_blocks (variables.py:847-926)
fn uncertain_nodes_in_except_blocks(
    cx: &mut WalkCx,
    found_nodes: &[GNode],
    node: GNode,
    node_statement: GNode,
) -> Vec<GNode> {
    let eng = cx.eng;
    let mut uncertain = Vec::new();
    for &other_node in found_nodes {
        let other_statement = stmt_of(eng, other_node);
        let Some(closest_handler) = u::first_ancestor(eng, other_statement, |k| {
            matches!(k, NodeKind::ExceptHandler { .. })
        }) else {
            continue;
        };
        if eng.parent_of(closest_handler, node) {
            continue;
        }
        let closest_try_except = eng.parent(closest_handler).unwrap_or(closest_handler);
        let md = eng.md(closest_try_except.m);
        let NodeKind::Try(d) = &md.tree.nodes[closest_try_except.n.idx()].kind else {
            continue;
        };
        let (body, orelse, handlers) = (d.body.clone(), d.orelse.clone(), d.handlers.clone());
        drop(md);
        let is_return = |n: &NodeId| {
            eng.kind_is(GNode { m: closest_try_except.m, n: *n }, |k| {
                matches!(k, NodeKind::Return { .. })
            })
        };
        let try_block_returns = body.iter().any(is_return);
        let else_block_returns = orelse.iter().any(is_return);
        let else_block_exits = orelse.iter().any(|&n| {
            let g = GNode { m: closest_try_except.m, n };
            let md = eng.md(g.m);
            if let NodeKind::Expr { value } = &md.tree.nodes[g.n.idx()].kind {
                let v = GNode { m: g.m, n: *value };
                if matches!(md.tree.nodes[v.n.idx()].kind, NodeKind::Call { .. }) {
                    drop(md);
                    return u::is_terminating_func(eng, cx.caches, v);
                }
            }
            false
        });
        let else_block_continues = orelse.iter().any(|&n| {
            eng.kind_is(GNode { m: closest_try_except.m, n }, |k| matches!(k, NodeKind::Continue))
        });
        let stmt_parent = eng.parent(node_statement);
        if else_block_continues {
            if let Some(p) = stmt_parent {
                let in_loop = eng.kind_is(p, |k| {
                    matches!(
                        k,
                        NodeKind::For(_) | NodeKind::AsyncFor(_) | NodeKind::While { .. }
                    )
                });
                let tep = eng.parent(closest_try_except);
                if in_loop
                    && tep.map(|t| eng.parent_of(t, node_statement)).unwrap_or(false)
                {
                    continue;
                }
            }
        }

        if try_block_returns || else_block_returns || else_block_exits {
            let mut appended = false;
            if let Some(p) = stmt_parent {
                if u::is_try(eng, p) {
                    let md = eng.md(p.m);
                    if let NodeKind::Try(pd) = &md.tree.nodes[p.n.idx()].kind {
                        let in_final = pd.finalbody.contains(&node_statement.n);
                        let in_orelse = pd.orelse.contains(&node_statement.n);
                        drop(md);
                        let tep = eng.parent(closest_try_except);
                        let guard =
                            tep.map(|t| eng.parent_of(t, node_statement)).unwrap_or(false);
                        if (in_final || in_orelse) && guard {
                            uncertain.push(other_node);
                            appended = true;
                        }
                    }
                }
            }
            if !appended
                && handlers.iter().all(|&h| {
                    defines_name_raises_or_returns_recursive(
                        cx,
                        name_of(cx.eng, node).unwrap_or(0),
                        GNode { m: closest_try_except.m, n: h },
                    )
                })
            {
                continue;
            }
            // NOTE bug-for-bug: when the first two sub-branches appended,
            // control FALLS THROUGH to the checks below (possible duplicate
            // append of other_node).
        }

        if check_loop_finishes_via_except(cx, node, closest_try_except) {
            continue;
        }
        uncertain.push(other_node);
    }
    uncertain
}

/// _check_loop_finishes_via_except (variables.py:1016-1089)
fn check_loop_finishes_via_except(cx: &mut WalkCx, node: GNode, try_except: GNode) -> bool {
    let eng = cx.eng;
    let md = eng.md(try_except.m);
    let NodeKind::Try(d) = &md.tree.nodes[try_except.n.idx()].kind else {
        return false;
    };
    let orelse = d.orelse.clone();
    drop(md);
    if orelse.is_empty() {
        return false;
    }
    let Some(closest_loop) = u::first_ancestor(eng, node, |k| {
        matches!(k, NodeKind::For(_) | NodeKind::AsyncFor(_) | NodeKind::While { .. })
    }) else {
        return false;
    };
    let loop_orelse: Vec<NodeId> = {
        let md = eng.md(closest_loop.m);
        match &md.tree.nodes[closest_loop.n.idx()].kind {
            NodeKind::For(d) | NodeKind::AsyncFor(d) => d.orelse.clone(),
            NodeKind::While { orelse, .. } => orelse.clone(),
            _ => return false,
        }
    };
    if !loop_orelse.iter().any(|&s| {
        let g = GNode { m: closest_loop.m, n: s };
        g == node || eng.parent_of(g, node)
    }) {
        return false;
    }
    let mut break_stmt: Option<GNode> = None;
    for &s in &orelse {
        let g = GNode { m: try_except.m, n: s };
        if eng.kind_is(g, |k| matches!(k, NodeKind::Break)) {
            break_stmt = Some(g);
            break;
        }
    }
    let Some(break_stmt) = break_stmt else { return false };

    let try_in_loop_body = |loop_node: GNode| -> bool {
        let md = eng.md(loop_node.m);
        let body: Vec<NodeId> = match &md.tree.nodes[loop_node.n.idx()].kind {
            NodeKind::For(d) | NodeKind::AsyncFor(d) => d.body.clone(),
            NodeKind::While { body, .. } => body.clone(),
            _ => return false,
        };
        drop(md);
        body.iter().any(|&s| {
            let g = GNode { m: loop_node.m, n: s };
            g == try_except || eng.parent_of(g, try_except)
        })
    };

    if !try_in_loop_body(closest_loop) {
        let mut found = false;
        for anc in u::ancestors(eng, closest_loop) {
            if eng.kind_is(anc, |k| {
                matches!(k, NodeKind::For(_) | NodeKind::AsyncFor(_) | NodeKind::While { .. })
            }) && try_in_loop_body(anc)
            {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }

    let loop_body: Vec<NodeId> = {
        let md = eng.md(closest_loop.m);
        match &md.tree.nodes[closest_loop.n.idx()].kind {
            NodeKind::For(d) | NodeKind::AsyncFor(d) => d.body.clone(),
            NodeKind::While { body, .. } => body.clone(),
            _ => return false,
        }
    };
    for &s in &loop_body {
        if recursive_search_for_continue_before_break(
            eng,
            GNode { m: closest_loop.m, n: s },
            break_stmt,
        ) {
            return false;
        }
    }
    true
}

/// _recursive_search_for_continue_before_break (variables.py:1091-1110)
fn recursive_search_for_continue_before_break(eng: &Engine, stmt: GNode, break_stmt: GNode) -> bool {
    if stmt == break_stmt {
        return false;
    }
    if eng.kind_is(stmt, |k| matches!(k, NodeKind::Continue)) {
        return true;
    }
    let stmt_is_loop = eng.kind_is(stmt, |k| {
        matches!(k, NodeKind::For(_) | NodeKind::AsyncFor(_) | NodeKind::While { .. })
    });
    let children = eng.md(stmt.m).tree.children(stmt.n);
    for c in children {
        // NOTE: pylint checks `stmt` (not child!) — loops skip ALL children
        if stmt_is_loop {
            continue;
        }
        if recursive_search_for_continue_before_break(eng, GNode { m: stmt.m, n: c }, break_stmt) {
            return true;
        }
    }
    false
}

/// _uncertain_nodes_in_try_blocks_when_evaluating_except_blocks
/// (variables.py:1112-1159)
fn uncertain_nodes_in_try_except(
    eng: &Engine,
    found_nodes: &[GNode],
    node_statement: GNode,
) -> Vec<GNode> {
    let mut uncertain = Vec::new();
    let Some(closest_handler) = u::first_ancestor(eng, node_statement, |k| {
        matches!(k, NodeKind::ExceptHandler { .. })
    }) else {
        return uncertain;
    };
    for &other_node in found_nodes {
        let other_statement = stmt_of(eng, other_node);
        if other_statement == closest_handler {
            continue;
        }
        let Some((try_anc, try_child)) =
            u::first_ancestor_and_child(eng, other_statement, |k| matches!(k, NodeKind::Try(_)))
        else {
            continue;
        };
        let md = eng.md(try_anc.m);
        let NodeKind::Try(d) = &md.tree.nodes[try_anc.n.idx()].kind else { continue };
        let (body, handlers) = (d.body.clone(), d.handlers.clone());
        drop(md);
        if !body.contains(&try_child.n) {
            continue;
        }
        let closest_in_handlers = handlers.contains(&closest_handler.n)
            && closest_handler.m == try_anc.m;
        let handler_ancestors: FxHashSet<GNode> =
            u::ancestors(eng, closest_handler).into_iter().collect();
        let related = handlers.iter().any(|&h| {
            closest_in_handlers || handler_ancestors.contains(&GNode { m: try_anc.m, n: h })
        });
        if !related {
            continue;
        }
        uncertain.push(other_node);
    }
    uncertain
}

/// _uncertain_nodes_in_try_blocks_when_evaluating_finally_blocks
/// (variables.py:1161-1220)
fn uncertain_nodes_in_try_finally(
    cx: &mut WalkCx,
    found_nodes: &[GNode],
    node_statement: GNode,
    name: GSym,
) -> Vec<GNode> {
    let eng = cx.eng;
    let mut uncertain = Vec::new();
    let Some((closest_try, child_of_closest)) =
        u::first_ancestor_and_child(eng, node_statement, |k| matches!(k, NodeKind::Try(_)))
    else {
        return uncertain;
    };
    {
        let md = eng.md(closest_try.m);
        let NodeKind::Try(d) = &md.tree.nodes[closest_try.n.idx()].kind else {
            return uncertain;
        };
        if !d.finalbody.contains(&child_of_closest.n) {
            return uncertain;
        }
    }
    for &other_node in found_nodes {
        let other_statement = stmt_of(eng, other_node);
        let Some((other_try, other_child)) =
            u::first_ancestor_and_child(eng, other_statement, |k| matches!(k, NodeKind::Try(_)))
        else {
            continue;
        };
        let (body, finalbody, handlers) = {
            let md = eng.md(other_try.m);
            let NodeKind::Try(d) = &md.tree.nodes[other_try.n.idx()].kind else { continue };
            (d.body.clone(), d.finalbody.clone(), d.handlers.clone())
        };
        if !body.contains(&other_child.n) {
            continue;
        }
        if other_try != closest_try {
            let covers = finalbody.iter().any(|&s| {
                let g = GNode { m: other_try.m, n: s };
                g == closest_try || eng.parent_of(g, closest_try)
            });
            if !covers {
                continue;
            }
        }
        // Is the name defined in all exception clauses?
        if !handlers.is_empty()
            && handlers.iter().all(|&h| {
                defines_name_raises_or_returns_recursive(
                    cx,
                    name,
                    GNode { m: other_try.m, n: h },
                )
            })
        {
            continue;
        }
        uncertain.push(other_node);
    }
    uncertain
}

// ---------------------------------------------------------------------------
// VariablesChecker visit/leave + core
// ---------------------------------------------------------------------------

const METACLASS_NAME_TRANSFORMS: &[(&str, &str)] = &[("_py_abc", "abc")];

impl VarsChecker {
    pub fn visit_module(&mut self, cx: &mut WalkCx, node: GNode) {
        self.to_consume = vec![Consumer::new(cx.eng, node, ScopeType::Module)];
        self.postponed_evaluation_enabled =
            u::is_postponed_evaluation_enabled(cx.eng, node.m);
        // redefined-builtin (a): module locals loop (variables.py:1401-1414;
        // undecorated — runs in every mode, emission filtered downstream)
        let eng = cx.eng;
        let locals: Vec<(GSym, GNode)> = {
            let md = eng.md(node.m);
            let l = md.locals.borrow();
            match l.get(&node.n) {
                Some(map) => map
                    .iter()
                    .filter_map(|(k, v)| v.first().map(|&g| (*k, g)))
                    .collect(),
                None => Vec::new(),
            }
        };
        for (name, stmt0) in locals {
            if cx.caches.is_builtin(eng, name) {
                let name_str = eng.sname(name);
                if should_ignore_redefined_builtin(eng, stmt0) || name_str == "__doc__" {
                    continue;
                }
                cx.emit_node_rooted(
                    "W0622",
                    stmt0,
                    u::msg_line(eng, stmt0),
                    u::msg_col(eng, stmt0),
                    u::format_template("Redefining built-in %r", &[&name_str]),
                );
            }
        }
    }

    pub fn leave_module(&mut self, cx: &mut WalkCx, node: GNode) {
        // only_required_for_messages gate (variables.py:1416-1425): skipped
        // when ALL eight are config-disabled (visit_module reassigns
        // _to_consume per module, so the unpopped consumer doesn't leak)
        if !["W0611", "W0614", "W0622", "E0603", "E0604", "E0605", "W0612", "E0602"]
            .iter()
            .any(|m| (cx.cfg_enabled)(m))
        {
            return;
        }
        debug_assert_eq!(self.to_consume.len(), 1);
        self.check_metaclasses(cx, node);
        let mut not_consumed =
            self.to_consume.pop().map(|c| c.to_consume).unwrap_or_default();
        let all_sym = cx.eng.sym("__all__");
        if locals_contains(cx.eng, node, all_sym) {
            // _check_all also DELETES __all__-exported names from
            // not_consumed (variables.py:3253-3255) -> no W0611/W0612
            self.check_all(cx, node, &mut not_consumed);
        }
        // _check_globals: allow-global-unused-variables default True -> no-op
        // `if not init_import and node.package: return` (variables.py:1438)
        // NOTE this early return also skips the _type_annotation_names
        // reset — the __init__.py LEAK (notes/09 §1.3.1) is replicated.
        let is_package = {
            let md = cx.eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Module(d) => d.package,
                _ => false,
            }
        };
        if is_package {
            return;
        }
        self.check_imports(cx, not_consumed);
        self.type_annotation_names = Vec::new();
    }

    /// _check_imports (variables.py:3298-3385) — W0611 unused-import +
    /// W0614 unused-wildcard-import (disabled; resurrection + I0021)
    fn check_imports(&mut self, cx: &mut WalkCx, not_consumed: IndexMap<GSym, Vec<GNode>>) {
        let eng = cx.eng;
        // _fix_dot_imports (variables.py:209-253)
        let mut names: indexmap::IndexMap<String, GNode> = indexmap::IndexMap::new();
        for (name_sym, stmts) in &not_consumed {
            let name = eng.sname(*name_sym);
            // AugAssign-assigned AssignName -> skip whole name
            let has_aug = stmts.iter().any(|&s| {
                eng.kind_is(s, |k| matches!(k, NodeKind::AssignName { .. }))
                    && eng
                        .parent(s)
                        .map(|p| {
                            // assign_type(): walk to the assignment stmt
                            let st = u::assign_parent(eng, s);
                            let _ = p;
                            eng.kind_is(st, |k| matches!(k, NodeKind::AugAssign { .. }))
                        })
                        .unwrap_or(false)
            });
            if has_aug {
                continue;
            }
            for &stmt in stmts {
                let md = eng.md(stmt.m);
                let import_names: Vec<(String, Option<String>)> =
                    match &md.tree.nodes[stmt.n.idx()].kind {
                        NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } => names
                            .iter()
                            .map(|&(n, a)| {
                                (
                                    md.tree.s(n).to_string(),
                                    a.map(|x| md.tree.s(x).to_string()),
                                )
                            })
                            .collect(),
                        _ => continue,
                    };
                drop(md);
                for (import_module_name, alias) in &import_names {
                    let second_name: Option<String> = if import_module_name == "*" {
                        Some(name.clone())
                    } else {
                        let dotted = import_module_name.starts_with(name.as_str())
                            && import_module_name.contains('.');
                        // `name in imports`: the CURRENT (qname, alias) tuple
                        let in_imports =
                            *import_module_name == name || alias.as_deref() == Some(name.as_str());
                        if dotted || in_imports {
                            Some(import_module_name.clone())
                        } else {
                            None
                        }
                    };
                    if let Some(sn) = second_name {
                        names.entry(sn).or_insert(stmt);
                    }
                }
            }
        }
        let mut local_names: Vec<(String, GNode)> = names.into_iter().collect();
        local_names.sort_by_key(|(_, stmt)| u::lineno(eng, *stmt));
        // the main loop
        let mut checked: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut unused_wildcard: indexmap::IndexMap<(String, GNode), Vec<String>> =
            indexmap::IndexMap::new();
        for (name, stmt) in local_names {
            let md = eng.md(stmt.m);
            let (is_import, modname, import_names): (bool, String, Vec<(String, Option<String>)>) =
                match &md.tree.nodes[stmt.n.idx()].kind {
                    NodeKind::Import { names } => (
                        true,
                        String::new(),
                        names
                            .iter()
                            .map(|&(n, a)| {
                                (md.tree.s(n).to_string(), a.map(|x| md.tree.s(x).to_string()))
                            })
                            .collect(),
                    ),
                    NodeKind::ImportFrom { modname, names, .. } => (
                        false,
                        md.tree.s(*modname).to_string(),
                        names
                            .iter()
                            .map(|&(n, a)| {
                                (md.tree.s(n).to_string(), a.map(|x| md.tree.s(x).to_string()))
                            })
                            .collect(),
                    ),
                    _ => continue,
                };
            drop(md);
            for (imported_name, as_name) in &import_names {
                let real_name = if imported_name == "*" { name.clone() } else { imported_name.clone() };
                if checked.contains(&real_name) {
                    continue;
                }
                if name != real_name && Some(name.as_str()) != as_name.as_deref() {
                    continue;
                }
                checked.insert(real_name.clone());
                let is_type_annotation_import = self
                    .type_annotation_names
                    .iter()
                    .any(|t| t == imported_name || Some(t.as_str()) == as_name.as_deref());
                let is_dummy_import = as_name
                    .as_deref()
                    .map(dummy_rgx_match)
                    .unwrap_or(false);
                if is_import || (!is_import && modname.is_empty()) {
                    if !is_import && special_obj_match(imported_name) {
                        continue;
                    }
                    if is_type_annotation_import || is_dummy_import {
                        continue;
                    }
                    let msg = match as_name {
                        None => format!("import {imported_name}"),
                        Some(a) => format!("{imported_name} imported as {a}"),
                    };
                    if !u::in_type_checking_block(eng, cx.caches, stmt) {
                        cx.emit_node_rooted(
                            "W0611",
                            stmt,
                            u::lineno(eng, stmt),
                            u::col_offset(eng, stmt).max(0) as i64,
                            u::format_template("Unused %s", &[&msg]),
                        );
                    }
                } else if !is_import && modname != "__future__" {
                    if special_obj_match(imported_name) {
                        continue;
                    }
                    // _is_from_future_import: name loaded from a __future__
                    // import in the imported module
                    if is_from_future_import(eng, stmt, &name) {
                        continue;
                    }
                    if is_type_annotation_import || is_dummy_import {
                        continue;
                    }
                    if imported_name == "*" {
                        unused_wildcard
                            .entry((modname.clone(), stmt))
                            .or_default()
                            .push(name.clone());
                    } else {
                        let msg = match as_name {
                            None => format!("{imported_name} imported from {modname}"),
                            Some(a) => {
                                format!("{imported_name} imported from {modname} as {a}")
                            }
                        };
                        if !u::in_type_checking_block(eng, cx.caches, stmt) {
                            cx.emit_node(
                                "W0611",
                                u::lineno(eng, stmt),
                                u::col_offset(eng, stmt) as i64,
                                u::format_template("Unused %s", &[&msg]),
                            );
                        }
                    }
                }
            }
        }
        for ((module, stmt), unused_list) in unused_wildcard {
            let arg_string = if unused_list.len() == 1 {
                unused_list[0].clone()
            } else {
                format!(
                    "{} and {}",
                    unused_list[..unused_list.len() - 1].join(", "),
                    unused_list[unused_list.len() - 1]
                )
            };
            cx.emit_node_rooted(
                "W0614",
                stmt,
                u::lineno(eng, stmt),
                u::col_offset(eng, stmt).max(0) as i64,
                u::format_template(
                    "Unused import(s) %s from wildcard import of %s",
                    &[&arg_string, &module],
                ),
            );
        }
    }

    /// visit_importfrom (variables.py:2119-2143) — no-name-in-module (E0611).
    /// Scoped to the relative-import-resolves-to-empty-module case: `from .
    /// import X` in a non-package module loaded by path resolves to astroid's
    /// bootstrap empty module (name='', file='<?>'), so pylint's
    /// visit_import (variables.py:2096-2117) — E0611 no-name-in-module for
    /// `import a.b.c`. For each dotted name: infer parts[0] to a Module, then
    /// _check_module_attrs(node, module, parts[1:]).
    pub fn visit_import(&mut self, cx: &mut WalkCx, node: GNode) {
        if !cx.full {
            return;
        }
        let eng = cx.eng;
        let md = eng.md(node.m);
        let names: Vec<String> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Import { names } => {
                names.iter().map(|&(n, _)| md.tree.s(n).to_string()).collect()
            }
            _ => return,
        };
        drop(md);
        if u::is_from_fallback_block(eng, node) {
            return;
        }
        if u::in_type_checking_block(eng, cx.caches, node) {
            return;
        }
        if let Some(p) = eng.parent(node) {
            if eng.kind_is(p, |k| matches!(k, NodeKind::If { .. })) && u::is_sys_guard(eng, p) {
                return;
            }
        }
        let line = u::lineno(eng, node);
        let col = u::col_offset(eng, node).max(0) as i64;
        for name in &names {
            let parts: Vec<&str> = name.split('.').collect();
            // module = next(_infer_name_module(node, parts[0])); ResolveError
            // -> continue; not a Module -> continue.
            let root = match eng.do_import_module(node, Some(parts[0])) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let rest: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
            self.check_module_attrs(cx, node, root, &rest, line, col);
        }
    }

    /// visit_importfrom (variables.py:2119-2144) — E0611 no-name-in-module.
    /// Full _check_module_attrs port: resolve the module path, then verify
    /// each imported name exists via Module.getattr (NotFoundError -> E0611).
    pub fn visit_importfrom(&mut self, cx: &mut WalkCx, node: GNode) {
        if !cx.full {
            return;
        }
        let eng = cx.eng;
        let md = eng.md(node.m);
        let (modname, _level, names): (String, Option<u32>, Vec<String>) =
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::ImportFrom { modname, level, names } => (
                    md.tree.s(*modname).to_string(),
                    *level,
                    names.iter().map(|&(n, _)| md.tree.s(n).to_string()).collect(),
                ),
                _ => return,
            };
        drop(md);
        // analyse_fallback_blocks default False: skip fallback ImportError blocks.
        if u::is_from_fallback_block(eng, node) {
            return;
        }
        // Skip TYPE_CHECKING / sys.version_info guards (variables.py:2126-2131).
        if u::in_type_checking_block(eng, cx.caches, node) {
            return;
        }
        if let Some(p) = eng.parent(node) {
            if eng.kind_is(p, |k| matches!(k, NodeKind::If { .. })) && u::is_sys_guard(eng, p) {
                return;
            }
        }
        let line = u::lineno(eng, node);
        let col = u::col_offset(eng, node).max(0) as i64;
        // name_parts = node.modname.split("."); module = do_import_module(parts[0]).
        // AstroidBuildingError -> return. (do_import_module resolves the full
        // relative/absolute modname; we then walk parts[1:] via getattr.)
        let name_parts: Vec<&str> = modname.split('.').collect();
        let mid = match eng.do_import_module(node, Some(name_parts[0])) {
            Ok(m) => m,
            Err(_) => return,
        };
        // _check_module_attrs(node, module, name_parts[1:]): traverse the
        // remaining dotted module path. NotFoundError on a part -> E0611 at
        // that part, then stop.
        let parts_rest: Vec<String> = name_parts[1..].iter().map(|s| s.to_string()).collect();
        let module = match self.check_module_attrs(cx, node, mid, &parts_rest, line, col) {
            Some(m) => m,
            None => return,
        };
        let eng = cx.eng;
        let _ = eng;
        // for name, _ in node.names: skip '*'; _check_module_attrs(node, module, name.split('.'))
        for name in &names {
            if name == "*" {
                continue;
            }
            let parts: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
            self.check_module_attrs(cx, node, module, &parts, line, col);
        }
    }

    /// _check_module_attrs (variables.py:3179-3217): walk `module_names`
    /// through `module` via getattr; on NotFoundError emit no-name-in-module
    /// and return None. Returns the final Module if the chain resolves to one.
    /// (ignored-modules default () -> is_module_ignored always False.)
    fn check_module_attrs(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        start: pyinfer::value::ModId,
        module_names: &[String],
        line: u32,
        col: i64,
    ) -> Option<pyinfer::value::ModId> {
        let mut module = Some(start);
        for name in module_names {
            if name == "__dict__" {
                module = None;
                break;
            }
            let cur = module?;
            let sym = cx.eng.sym(name);
            match cx.eng.module_getattr(cur, sym, false) {
                Ok(vals) => {
                    // module = module.getattr(name)[0]; if not Module -> next(infer)
                    let first = vals.into_iter().next();
                    module = self.nv_to_module(cx, first);
                    if module.is_none() {
                        // not isinstance(module, Module) after infer -> return None
                        return None;
                    }
                }
                Err(_) => {
                    // NotFoundError (AttributeInferenceError) -> no-name-in-module.
                    let modname = cx.eng.md(cur).name.clone();
                    cx.emit_node_rooted(
                        "E0611",
                        node,
                        line,
                        col,
                        u::format_template("No name %r in module %r", &[name, &modname]),
                    );
                    return None;
                }
            }
        }
        // InferenceError during infer -> handled as None in nv_to_module path.
        module
    }

    /// Resolve a getattr result NV to a Module: if it is already a Module node
    /// use it; else `next(module.infer())` and require the result be a Module.
    /// Any non-Module / inference failure -> None (mirrors the isinstance gate).
    fn nv_to_module(
        &self,
        cx: &mut WalkCx,
        nv: Option<pyinfer::value::NV>,
    ) -> Option<pyinfer::value::ModId> {
        let eng = cx.eng;
        let is_module_node = |g: GNode| -> bool {
            g.n == pyast::NodeId::MODULE
                && eng.kind_is(g, |k| matches!(k, NodeKind::Module(_)))
        };
        match nv? {
            NV::N(g) if is_module_node(g) => Some(g.m),
            NV::N(g) => {
                // next(node.infer()): require a Module result.
                match eng.infer_first(g, Some(&pyinfer::ctx::Ctx::new())) {
                    Ok(Value::Node(m)) if is_module_node(m) => Some(m.m),
                    _ => None,
                }
            }
            NV::V(Value::Node(m)) if is_module_node(m) => Some(m.m),
            NV::V(_) => None,
        }
    }

    pub fn visit_classdef(&mut self, cx: &mut WalkCx, node: GNode) {
        self.to_consume.push(Consumer::new(cx.eng, node, ScopeType::Class));
    }

    /// leave_classdef (variables.py:1450-1461)
    pub fn leave_classdef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        for name_node in u::preorder(eng, node) {
            if !eng.kind_is(name_node, |k| matches!(k, NodeKind::Name { .. })) {
                continue;
            }
            let Some(parent) = eng.parent(name_node) else { continue };
            let md = eng.md(parent.m);
            let NodeKind::Call { func, .. } = &md.tree.nodes[parent.n.idx()].kind else {
                continue;
            };
            let func = *func;
            let NodeKind::Attribute { expr, .. } = &md.tree.nodes[func.idx()].kind else {
                continue;
            };
            let expr = *expr;
            let NodeKind::Name { name } = &md.tree.nodes[expr.idx()].kind else {
                continue;
            };
            let name = eng.g(&md, *name);
            drop(md);
            // consumer scan OUTER -> INNER (list order; module first)
            for consumer in self.to_consume.iter_mut() {
                if let Some(nodes) = consumer.to_consume.get(&name).cloned() {
                    consumer.mark_as_consumed(name, nodes);
                    break;
                }
            }
        }
        self.to_consume.pop();
    }

    pub fn visit_lambda(&mut self, cx: &mut WalkCx, node: GNode) {
        self.to_consume.push(Consumer::new(cx.eng, node, ScopeType::Lambda));
    }
    pub fn leave_lambda(&mut self, _cx: &mut WalkCx, _node: GNode) {
        self.to_consume.pop();
    }
    pub fn visit_comprehension_scope(&mut self, cx: &mut WalkCx, node: GNode) {
        self.to_consume.push(Consumer::new(cx.eng, node, ScopeType::Comprehension));
    }
    pub fn leave_comprehension_scope(&mut self, _cx: &mut WalkCx, _node: GNode) {
        self.to_consume.pop();
    }
    pub fn visit_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        self.to_consume.push(Consumer::new(cx.eng, node, ScopeType::Function));
        // redefined-outer-name / redefined-builtin (variables.py:1502-1544)
        if !((cx.cfg_enabled)("W0621") || (cx.cfg_enabled)("W0622")) {
            return;
        }
        let eng = cx.eng;
        let module = module_node(node);
        // node.items(): (name, locals[name][0]) in locals insertion order
        let items: Vec<(GSym, GNode)> = {
            let md = eng.md(node.m);
            let l = md.locals.borrow();
            match l.get(&node.n) {
                Some(map) => map
                    .iter()
                    .filter_map(|(k, v)| v.first().map(|&g| (*k, g)))
                    .collect(),
                None => Vec::new(),
            }
        };
        for (name, stmt) in items {
            let globs = locals_get(eng, module, name);
            let stmt_is_global = eng.kind_is(stmt, |k| matches!(k, NodeKind::Global { .. }));
            if !globs.is_empty() && !stmt_is_global {
                let definition = globs[0];
                // __future__ directive, not a symbol
                let is_future = {
                    let md = eng.md(definition.m);
                    matches!(&md.tree.nodes[definition.n.idx()].kind,
                        NodeKind::ImportFrom { modname, .. }
                            if md.tree.s(*modname) == "__future__")
                };
                if is_future {
                    continue;
                }
                if globs
                    .iter()
                    .any(|&d| u::in_type_checking_block(eng, cx.caches, d))
                {
                    continue;
                }
                // outer `except ... as e` binding
                let is_except_binding = eng.kind_is(globs[0], |k| {
                    matches!(k, NodeKind::AssignName { .. })
                }) && eng
                    .parent(globs[0])
                    .map(|p| u::is_excepthandler(eng, p))
                    .unwrap_or(false);
                if is_except_binding {
                    continue;
                }
                let line = eng.fromlineno(definition);
                if !is_name_ignored(eng, stmt, name) {
                    let name_str = eng.sname(name);
                    cx.emit_node_rooted(
                        "W0621",
                        stmt,
                        u::msg_line(eng, stmt),
                        u::msg_col(eng, stmt),
                        u::format_template(
                            "Redefining name %r from outer scope (line %s)",
                            &[&name_str, &line.to_string()],
                        ),
                    );
                }
            } else if cx.caches.is_builtin(eng, name)
                && !should_ignore_redefined_builtin(eng, stmt)
            {
                // allowed-redefined-builtins default () — no exemption
                let name_str = eng.sname(name);
                cx.emit_node_rooted(
                    "W0622",
                    stmt,
                    u::msg_line(eng, stmt),
                    u::msg_col(eng, stmt),
                    u::format_template("Redefining built-in %r", &[&name_str]),
                );
            }
        }
    }

    /// visit_global (variables.py:1595-1665) — W0601/W0602/W0603/W0604 +
    /// the special-attribute W0622 arm
    pub fn visit_global(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let frame = eng.frame(node);
        let line = u::lineno(eng, node);
        let col = u::col_offset(eng, node).max(0) as i64;
        if u::is_module(eng, frame) {
            cx.emit_node(
                "W0604",
                line,
                col,
                "Using the global statement at the module level".to_string(),
            );
            return;
        }
        let module = module_node(node);
        let names: Vec<GSym> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Global { names } => names.iter().map(|&s| eng.g(&md, s)).collect(),
                _ => return,
            }
        };
        let mut default_message = true;
        for name in names {
            let assign_nodes = locals_get(eng, module, name);
            let not_defined_locally_by_import = !locals_get(eng, module, name).iter().any(|&l| {
                eng.kind_is(l, |k| {
                    matches!(k, NodeKind::Import { .. } | NodeKind::ImportFrom { .. })
                })
            });
            if !is_reassigned_after_current(eng, node, name)
                && !is_deleted_after_current(eng, node, name)
                && not_defined_locally_by_import
            {
                let name_str = eng.sname(name);
                cx.emit_node(
                    "W0602",
                    line,
                    col,
                    u::format_template(
                        "Using global for %r but no assignment is done",
                        &[&name_str],
                    ),
                );
                default_message = false;
                continue;
            }
            let mut broke = false;
            for anode in &assign_nodes {
                let is_special_assignname = eng
                    .kind_is(*anode, |k| matches!(k, NodeKind::AssignName { .. }))
                    && MODULE_SPECIAL_ATTRIBUTES
                        .contains(&eng.sname(name_of(eng, *anode).unwrap_or(0)).as_str());
                if is_special_assignname {
                    let name_str = eng.sname(name);
                    cx.emit_node(
                        "W0622",
                        line,
                        col,
                        u::format_template("Redefining built-in %r", &[&name_str]),
                    );
                    broke = true;
                    break;
                }
                if eng.frame(*anode) == module {
                    broke = true;
                    break;
                }
                let is_def = eng.kind_is(*anode, |k| {
                    matches!(
                        k,
                        NodeKind::ClassDef(_)
                            | NodeKind::FunctionDef(_)
                            | NodeKind::AsyncFunctionDef(_)
                    )
                });
                if is_def && eng.parent(*anode) == Some(module) {
                    broke = true;
                    break;
                }
            }
            if !broke && not_defined_locally_by_import {
                let name_str = eng.sname(name);
                cx.emit_node(
                    "W0601",
                    line,
                    col,
                    u::format_template(
                        "Global variable %r undefined at the module level",
                        &[&name_str],
                    ),
                );
                default_message = false;
            }
        }
        if default_message {
            cx.emit_node("W0603", line, col, "Using the global statement".to_string());
        }
    }

    /// visit_excepthandler (variables.py:1689-1704) — W0621 except queue
    pub fn visit_excepthandler(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let name_node: Option<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::ExceptHandler { name: Some(n), .. } => Some(GNode { m: node.m, n: *n }),
                _ => None,
            }
        };
        let Some(name_node) = name_node else { return };
        if !eng.kind_is(name_node, |k| matches!(k, NodeKind::AssignName { .. })) {
            return;
        }
        let nm = name_of(eng, name_node);
        for (outer_except, outer_assign) in self.except_handler_names_queue.clone() {
            if name_of(eng, outer_assign) == nm {
                let name_str = eng.sname(nm.unwrap_or(0));
                let outer_line = eng.fromlineno(outer_except);
                cx.emit_node(
                    "W0621",
                    u::lineno(eng, node),
                    u::col_offset(eng, node).max(0) as i64,
                    u::format_template(
                        "Redefining name %r from outer scope (line %s)",
                        &[&name_str, &outer_line.to_string()],
                    ),
                );
                break;
            }
        }
        self.except_handler_names_queue.push((node, name_node));
    }

    /// leave_excepthandler (variables.py:1706-1709)
    pub fn leave_excepthandler(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let has_assignname = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::ExceptHandler { name: Some(n), .. } => matches!(
                    md.tree.nodes[n.idx()].kind,
                    NodeKind::AssignName { .. }
                ),
                _ => false,
            }
        };
        if !has_assignname {
            return;
        }
        self.except_handler_names_queue.pop();
    }
    pub fn leave_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_metaclasses(cx, node);
        // type_comment_returns/_args (`# type: (...) -> T` signature form)
        {
            let payload: Option<String> = {
                let md = cx.eng.md(node.m);
                md.tree
                    .type_comments
                    .iter()
                    .find(|(n, is_func, _)| *n == node.n && *is_func)
                    .map(|(_, _, p)| p.to_string())
            };
            if let Some(p) = payload {
                self.store_func_type_comment(&p);
            }
        }
        let not_consumed = self.to_consume.pop().map(|c| c.to_consume).unwrap_or_default();
        if !((cx.cfg_enabled)("W0612")
            || (cx.cfg_enabled)("W0641")
            || (cx.cfg_enabled)("W0613"))
        {
            return;
        }
        let eng = cx.eng;
        // utils.is_error: the body only raises
        let body: Vec<NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
                _ => return,
            }
        };
        if body.len() == 1
            && eng.kind_is(GNode { m: node.m, n: body[0] }, |k| {
                matches!(k, NodeKind::Raise { .. })
            })
        {
            return;
        }
        let is_meth = is_method(eng, node);
        if is_meth && func_is_abstract(cx, node) {
            return;
        }
        let global_names = global_names_in(eng, node);
        let nonlocal_names = nonlocal_names_in(eng, node);
        let mut comprehension_target_names: FxHashSet<GSym> = FxHashSet::default();
        for comp in u::preorder(eng, node) {
            let md = eng.md(comp.m);
            let generators: Vec<NodeId> = match &md.tree.nodes[comp.n.idx()].kind {
                NodeKind::ListComp(d) | NodeKind::SetComp(d) | NodeKind::GeneratorExp(d) => {
                    d.generators.clone()
                }
                NodeKind::DictComp(d) => d.generators.clone(),
                _ => continue,
            };
            drop(md);
            for g in generators {
                let target: Option<NodeId> = {
                    let md = eng.md(comp.m);
                    match &md.tree.nodes[g.idx()].kind {
                        NodeKind::Comprehension { target, .. } => Some(*target),
                        _ => None,
                    }
                };
                if let Some(t) = target {
                    find_assigned_names_recursive(
                        eng,
                        GNode { m: comp.m, n: t },
                        &mut comprehension_target_names,
                    );
                }
            }
        }
        let entries: Vec<(GSym, GNode)> = not_consumed
            .iter()
            .filter_map(|(k, v)| v.first().map(|&g| (*k, g)))
            .collect();
        for (name, stmt0) in entries {
            self.check_is_unused(
                cx,
                name,
                node,
                stmt0,
                &global_names,
                &nonlocal_names,
                &comprehension_target_names,
            );
        }
    }

    /// _check_is_unused (variables.py:2774-2872)
    #[allow(clippy::too_many_arguments)]
    fn check_is_unused(
        &mut self,
        cx: &mut WalkCx,
        name: GSym,
        node: GNode,
        stmt: GNode,
        global_names: &FxHashSet<GSym>,
        nonlocal_names: &FxHashSet<GSym>,
        comprehension_target_names: &FxHashSet<GSym>,
    ) {
        let eng = cx.eng;
        if is_name_ignored(eng, stmt, name) {
            return;
        }
        // `__class__` dynamic-locals guard: astroid does not inject
        // __class__ into method locals (probed) — pattern can't match.
        let stmt_is_global_or_import = eng.kind_is(stmt, |k| {
            matches!(
                k,
                NodeKind::Global { .. } | NodeKind::Import { .. } | NodeKind::ImportFrom { .. }
            )
        });
        if stmt_is_global_or_import
            && !global_names.is_empty()
            && import_name_is_global(eng, stmt, global_names)
        {
            return;
        }
        if comprehension_target_names.contains(&name) {
            return;
        }
        let mut name_str = eng.sname(name);
        if self.type_annotation_names.iter().any(|t| *t == name_str) {
            return;
        }
        let argnames = func_argnames(eng, node);
        if argnames.contains(&name) {
            // __new__ + __init__-defined special case
            if eng.node_name(node).as_deref() == Some("__new__") {
                if let Some(parent) = eng.parent(node) {
                    let children = eng.md(parent.m).tree.children(parent.n);
                    let has_init = children.iter().any(|&c| {
                        let g = GNode { m: parent.m, n: c };
                        eng.node_name(g).as_deref() == Some("__init__")
                            || name_of(eng, g).map(|s| eng.sname(s)).as_deref()
                                == Some("__init__")
                    });
                    if has_init {
                        return;
                    }
                }
            }
            self.check_unused_arguments(cx, name, node, stmt, &argnames, nonlocal_names);
        } else {
            let parent_kind_ok = eng
                .parent(stmt)
                .map(|p| {
                    eng.kind_is(p, |k| {
                        matches!(
                            k,
                            NodeKind::Assign { .. }
                                | NodeKind::AnnAssign { .. }
                                | NodeKind::Tuple { .. }
                                | NodeKind::For(_)
                        )
                    })
                })
                .unwrap_or(false);
            if parent_kind_ok && nonlocal_names.contains(&name) {
                return;
            }
            let mut qname: Option<String> = None;
            let mut asname: Option<String> = None;
            let stmt_import_kind: u8 = {
                let md = eng.md(stmt.m);
                match &md.tree.nodes[stmt.n.idx()].kind {
                    NodeKind::Import { .. } => 1,
                    NodeKind::ImportFrom { .. } => 2,
                    _ => 0,
                }
            };
            if stmt_import_kind != 0 {
                let md = eng.md(stmt.m);
                let names: Vec<(String, Option<String>)> = match &md.tree.nodes[stmt.n.idx()].kind
                {
                    NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } => names
                        .iter()
                        .map(|&(n, a)| {
                            (md.tree.s(n).to_string(), a.map(|x| md.tree.s(x).to_string()))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                drop(md);
                let import_names: Option<(String, Option<String>)> = if names.len() > 1 {
                    names
                        .iter()
                        .find(|(q, a)| *q == name_str || a.as_deref() == Some(name_str.as_str()))
                        .cloned()
                } else {
                    names.first().cloned()
                };
                if let Some((q, a)) = import_names {
                    qname = Some(q.clone());
                    asname = a.clone();
                    name_str = a.unwrap_or(q);
                }
            }
            let message_is_possibly = has_locals_call_after_node(cx, stmt, eng.scope(node));
            if !message_is_possibly {
                if stmt_import_kind == 1 {
                    let msg = match &asname {
                        Some(a) => format!("{} imported as {}", qname.as_deref().unwrap_or(""), a),
                        None => format!("import {name_str}"),
                    };
                    cx.emit_node_rooted(
                        "W0611",
                        stmt,
                        u::lineno(eng, stmt),
                        u::col_offset(eng, stmt).max(0) as i64,
                        u::format_template("Unused %s", &[&msg]),
                    );
                    return;
                }
                if stmt_import_kind == 2 {
                    let modname: String = {
                        let md = eng.md(stmt.m);
                        match &md.tree.nodes[stmt.n.idx()].kind {
                            NodeKind::ImportFrom { modname, .. } => {
                                md.tree.s(*modname).to_string()
                            }
                            _ => String::new(),
                        }
                    };
                    let msg = match &asname {
                        Some(a) => format!(
                            "{} imported from {} as {}",
                            qname.as_deref().unwrap_or(""),
                            modname,
                            a
                        ),
                        None => format!("{name_str} imported from {modname}"),
                    };
                    cx.emit_node_rooted(
                        "W0611",
                        stmt,
                        u::lineno(eng, stmt),
                        u::col_offset(eng, stmt).max(0) as i64,
                        u::format_template("Unused %s", &[&msg]),
                    );
                    return;
                }
            }
            // decorated nested function counts as used
            let stmt_is_decorated_func = {
                let md = eng.md(stmt.m);
                match &md.tree.nodes[stmt.n.idx()].kind {
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                        d.decorators.is_some()
                    }
                    _ => false,
                }
            };
            if stmt_is_decorated_func {
                return;
            }
            if crate::typecheck::is_overload_stub(cx.caches, eng, node) {
                return;
            }
            if is_exception_binding_used_in_handler(eng, stmt, name) {
                return;
            }
            let (msgid, template) = if message_is_possibly {
                ("W0641", "Possibly unused variable %r")
            } else {
                ("W0612", "Unused variable %r")
            };
            cx.emit_node_rooted(
                msgid,
                stmt,
                u::msg_line(eng, stmt),
                u::msg_col(eng, stmt),
                u::format_template(template, &[&name_str]),
            );
        }
    }

    /// _check_unused_arguments (variables.py:2890-2941) — W0613
    fn check_unused_arguments(
        &mut self,
        cx: &mut WalkCx,
        name: GSym,
        node: GNode,
        stmt: GNode,
        argnames: &[GSym],
        nonlocal_names: &FxHashSet<GSym>,
    ) {
        let eng = cx.eng;
        let is_meth = is_method(eng, node);
        let klass = eng.frame(eng.parent(node).unwrap_or(node));
        if is_meth && u::is_classdef(eng, klass) {
            // confidence selection burn (INFERENCE vs INFERENCE_FAILURE)
            let _ = crate::typecheck::has_known_bases(eng, cx.caches, klass);
        }
        let func_name = eng.node_name(node).unwrap_or_default();
        if is_meth {
            if eng.func_type(node) != pyinfer::graph::FType::StaticMethod
                && Some(&name) == argnames.first()
            {
                return;
            }
            if let Some(overridden) = overridden_method(eng, klass, &func_name) {
                if func_argnames(eng, overridden).contains(&name) {
                    return;
                }
            }
            if u::PYMETHODS.contains(&func_name.as_str())
                && func_name != "__init__"
                && func_name != "__new__"
            {
                return;
            }
        }
        // callbacks default ("cb_", "_cb") — against the FUNCTION name
        if func_name.starts_with("cb_")
            || func_name.ends_with("cb_")
            || func_name.starts_with("_cb")
            || func_name.ends_with("_cb")
        {
            return;
        }
        if crate::basicerr::is_registered_in_singledispatch_function(eng, cx, node) {
            return;
        }
        if crate::typecheck::is_overload_stub(cx.caches, eng, node) {
            return;
        }
        if u::is_classdef(eng, klass) && crate::typecheck::is_protocol_class(eng, klass) {
            return;
        }
        if nonlocal_names.contains(&name) {
            return;
        }
        let name_str = eng.sname(name);
        cx.emit_node_rooted(
            "W0613",
            stmt,
            u::msg_line(eng, stmt),
            u::msg_col(eng, stmt),
            u::format_template("Unused argument %r", &[&name_str]),
        );
    }

    /// visit_assignname (variables.py:1667-1669)
    pub fn visit_assignname(&mut self, cx: &mut WalkCx, node: GNode) {
        let at = cx.eng.assign_type(node);
        if cx.eng.kind_is(at, |k| matches!(k, NodeKind::AugAssign { .. })) {
            self.visit_name(cx, node);
        }
    }
    /// visit_delname (variables.py:1671-1672)
    pub fn visit_delname(&mut self, cx: &mut WalkCx, node: GNode) {
        self.visit_name(cx, node);
    }

    /// visit_name (variables.py:1674-1687)
    pub fn visit_name(&mut self, cx: &mut WalkCx, node: GNode) {
        let Some(stmt) = cx.eng.statement(node) else { return };
        self.undefined_and_used_before_checker(cx, node, stmt);
        // _loopvar_name (W0631). pylint runs it unconditionally; we gate on
        // the message's config state so the -E pipeline stays byte-frozen
        // (its inference burns are output-inert there: 27-corpus -E parity
        // was achieved without them).
        if (cx.cfg_enabled)("W0631") {
            self.loopvar_name(cx, node);
        }
    }

    /// _loopvar_name (variables.py:2625-2771) — W0631
    fn loopvar_name(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some(name) = name_of(eng, node) else { return };
        let name_str = eng.sname(name);
        let emit = |cx: &mut WalkCx| {
            cx.emit_node(
                "W0631",
                u::lineno(cx.eng, node),
                u::col_offset(cx.eng, node).max(0) as i64,
                u::format_template("Using possibly undefined loop variable %r", &[&name_str]),
            );
        };
        let lk = eng.lookup(node, name);
        let astmts: Vec<GNode> = lk
            .1
            .iter()
            .filter_map(|nv| match nv {
                NV::N(g) => Some(*g),
                _ => None,
            })
            .collect();
        let scope = eng.scope(node);
        if eng.kind_is(scope, |k| {
            matches!(k, NodeKind::Lambda(_) | NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
        }) && astmts.iter().any(|&a| eng.parent_of(eng.scope(a), scope))
        {
            return;
        }
        let mut filtered: Vec<GNode> = Vec::new();
        let drop_all = astmts.is_empty() || {
            let a0 = astmts[0];
            let a0_parent = eng.parent(a0);
            let a0_root = module_node(a0);
            let cond1 = a0_parent == Some(a0_root)
                && a0_parent.map(|p| eng.parent_of(p, node)).unwrap_or(false);
            let cond2 = {
                let is_stmt = eng.statement(a0) == Some(a0);
                is_stmt
                    || (!a0_parent
                        .map(|p| u::is_module(eng, p))
                        .unwrap_or(false)
                        && eng
                            .statement(a0)
                            .map(|st| eng.parent_of(st, node))
                            .unwrap_or(false))
            };
            cond1 || cond2
        };
        if !drop_all {
            filtered.push(astmts[0]);
        }
        for (i, &stmt) in astmts.iter().enumerate().skip(1) {
            // NOTE pylint indexes astmts[i] == the PREVIOUS element
            let prev = astmts[i - 1];
            let Some(prev_statement) = eng.statement(prev) else { continue };
            if eng.parent_of(prev_statement, stmt)
                && !in_for_else_branch(eng, prev_statement, stmt)
            {
                continue;
            }
            filtered.push(stmt);
        }
        if filtered.len() != 1 {
            return;
        }
        let assign = eng.assign_type(filtered[0]);
        let assign_is_loopish = eng.kind_is(assign, |k| {
            matches!(
                k,
                NodeKind::For(_) | NodeKind::Comprehension { .. } | NodeKind::GeneratorExp(_)
            )
        });
        if !(assign_is_loopish && eng.statement(assign) != eng.statement(node)) {
            return;
        }
        if !eng.kind_is(assign, |k| matches!(k, NodeKind::For(_))) {
            emit(cx);
            return;
        }
        let (orelse, iter_n): (Vec<NodeId>, NodeId) = {
            let md = eng.md(assign.m);
            match &md.tree.nodes[assign.n.idx()].kind {
                NodeKind::For(d) => (d.orelse.clone(), d.iter),
                _ => return,
            }
        };
        for &es in &orelse {
            let eg = GNode { m: assign.m, n: es };
            if eng.kind_is(eg, |k| {
                matches!(
                    k,
                    NodeKind::Return { .. }
                        | NodeKind::Raise { .. }
                        | NodeKind::Break
                        | NodeKind::Continue
                )
            }) {
                return;
            }
            let call_func: Option<GNode> = {
                let md = eng.md(eg.m);
                match &md.tree.nodes[eg.n.idx()].kind {
                    NodeKind::Expr { value } => match &md.tree.nodes[value.idx()].kind {
                        NodeKind::Call { func, .. } => Some(GNode { m: eg.m, n: *func }),
                        _ => None,
                    },
                    _ => None,
                }
            };
            if let Some(f) = call_func {
                if let Some(Value::Node(fd)) = u::safe_infer(eng, cx.caches, f) {
                    let returns: Option<GNode> = {
                        let md = eng.md(fd.m);
                        match &md.tree.nodes[fd.n.idx()].kind {
                            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                                d.returns.map(|r| GNode { m: fd.m, n: r })
                            }
                            _ => None,
                        }
                    };
                    if let Some(r) = returns {
                        let inferred_return = u::safe_infer(eng, cx.caches, r);
                        match &inferred_return {
                            Some(Value::Node(g)) if u::is_functiondef(eng, *g) => {
                                let q = eng.qname(*g);
                                if [
                                    "typing.NoReturn",
                                    "typing_extensions.NoReturn",
                                    "typing.Never",
                                    "typing_extensions.Never",
                                    "typing._SpecialForm",
                                ]
                                .contains(&q.as_str())
                                {
                                    return;
                                }
                            }
                            Some(v @ (Value::Inst { .. } | Value::ExcInst { .. })) => {
                                if eng.value_qname(v).as_deref() == Some("typing._SpecialForm") {
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        // walrus-in-comprehension exemption (variables.py:2716-2732)
        let maybe_walrus =
            u::first_ancestor(eng, node, |k| matches!(k, NodeKind::NamedExpr { .. }));
        if let Some(w) = maybe_walrus {
            if let Some(comp) =
                u::first_ancestor(eng, w, |k| matches!(k, NodeKind::Comprehension { .. }))
            {
                let comp_scope = u::first_ancestor(eng, comp, |k| {
                    matches!(
                        k,
                        NodeKind::ListComp(_)
                            | NodeKind::SetComp(_)
                            | NodeKind::DictComp(_)
                            | NodeKind::GeneratorExp(_)
                    )
                });
                if let Some(cs) = comp_scope {
                    let parent_scope_ok = eng
                        .parent(cs)
                        .map(|p| eng.scope(p) == scope)
                        .unwrap_or(false);
                    if parent_scope_ok && locals_contains(eng, cs, name) {
                        return;
                    }
                }
            }
        }
        // iterable length heuristic
        let iter_g = GNode { m: assign.m, n: iter_n };
        let mut inferred = match eng.first_value(iter_g, &u::fresh_ctx()) {
            Ok(Some(v)) => v,
            _ => {
                emit(cx);
                return;
            }
        };
        if matches!(&inferred, Value::Inst { .. } | Value::ExcInst { .. })
            && eng.value_qname(&inferred).as_deref() == Some("builtins.enumerate")
        {
            let likely_call: GNode = {
                let md = eng.md(iter_g.m);
                match &md.tree.nodes[iter_g.n.idx()].kind {
                    NodeKind::IfExp { body, .. } => GNode { m: iter_g.m, n: *body },
                    _ => iter_g,
                }
            };
            let first_arg: Option<GNode> = {
                let md = eng.md(likely_call.m);
                match &md.tree.nodes[likely_call.n.idx()].kind {
                    NodeKind::Call { args, .. } => {
                        args.first().map(|&a| GNode { m: likely_call.m, n: a })
                    }
                    _ => None,
                }
            };
            if let Some(a0) = first_arg {
                match eng.first_value(a0, &u::fresh_ctx()) {
                    Ok(Some(v)) => inferred = v,
                    _ => {
                        emit(cx);
                        return;
                    }
                }
            }
        }
        if matches!(&inferred, Value::Inst { .. } | Value::ExcInst { .. })
            && eng.value_qname(&inferred).as_deref() == Some("builtins.range")
        {
            return;
        }
        // sequences: List/Tuple/Dict/Set nodes, synth seqs/dicts, FrozenSet
        let elements: Option<usize> = match &inferred {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Tuple { elts, .. }
                    | NodeKind::List { elts, .. }
                    | NodeKind::Set { elts } => Some(elts.len()),
                    NodeKind::Dict { items } => Some(items.len()),
                    _ => None,
                }
            }
            Value::SynthSeq { elems, .. } => Some(elems.len()),
            Value::SynthDict { items } => Some(items.len()),
            Value::FrozenSet { elems } => Some(elems.len()),
            _ => None,
        };
        match elements {
            None => emit(cx),
            Some(0) => emit(cx),
            Some(_) => {}
        }
    }

    /// visit_assign (variables.py:2152-2170) — E0633 unpacking-non-sequence
    /// (+ W0632/W0644 unbalanced unpacking, W0642 self-cls-assignment)
    pub fn visit_assign(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        self.check_self_cls_assign(cx, node);
        let (targets, value): (Vec<NodeId>, GNode) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Assign { targets, value } => {
                    (targets.clone(), GNode { m: node.m, n: *value })
                }
                _ => return,
            }
        };
        let Some(&t0) = targets.first() else { return };
        let target_elts: Vec<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[t0.idx()].kind {
                NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => {
                    elts.iter().map(|&e| GNode { m: node.m, n: e }).collect()
                }
                _ => return,
            }
        };
        if target_elts
            .iter()
            .any(|&t| eng.kind_is(t, |k| matches!(k, NodeKind::Starred { .. })))
        {
            return;
        }
        // node.value.inferred(): InferenceError -> return
        let inferred = u::infer_all(eng, cx.caches, value);
        if inferred.len() == 1 {
            self.check_unpacking(cx, &inferred[0], node, value, &target_elts);
        }
    }

    /// _check_unpacking (variables.py:3087-3122)
    fn check_unpacking(
        &mut self,
        cx: &mut WalkCx,
        inferred: &Value,
        node: GNode,
        value: GNode,
        targets: &[GNode],
    ) {
        let eng = cx.eng;
        if crate::typecheck::is_inside_abstract_class(cx.caches, eng, node) {
            return;
        }
        // is_comprehension(node): an Assign is never a comprehension
        if inferred.is_uninferable() {
            return;
        }
        // vararg exemption: inferred.parent is Arguments and value is the
        // Name of that vararg
        if let Value::Node(g) = inferred {
            if let Some(p) = eng.parent(*g) {
                let md = eng.md(p.m);
                if let NodeKind::Arguments(a) = &md.tree.nodes[p.n.idx()].kind {
                    let vararg = a.vararg.map(|s| md.tree.s(s).to_string());
                    drop(md);
                    let vmd = eng.md(value.m);
                    if let NodeKind::Name { name } = &vmd.tree.nodes[value.n.idx()].kind {
                        let vn = vmd.tree.s(*name).to_string();
                        if Some(vn) == vararg {
                            return;
                        }
                    }
                }
            }
        }
        // astroid infers a context-free vararg name to a fresh Tuple
        // PARENTED TO the Arguments node (protocols.py); our engine yields
        // a synthetic tuple — recover the parent link via the binding
        if matches!(
            inferred,
            Value::SynthSeq { kind: pyinfer::value::SeqKind::Tuple, .. }
        ) {
            if let Some(vn) = name_of(eng, value) {
                let lk = eng.lookup(value, vn);
                if let Some(NV::N(def0)) = lk.1.first() {
                    // the binding resolves to the Arguments node (or the
                    // vararg AssignName) — match the vararg name
                    let md = eng.md(def0.m);
                    match &md.tree.nodes[def0.n.idx()].kind {
                        NodeKind::Arguments(a) => {
                            if a.vararg.map(|sy| eng.g(&md, sy)) == Some(vn) {
                                return;
                            }
                        }
                        NodeKind::AssignName { .. } => {
                            drop(md);
                            if let Some(p) = eng.parent(*def0) {
                                let md = eng.md(p.m);
                                if let NodeKind::Arguments(a) = &md.tree.nodes[p.n.idx()].kind {
                                    if a.vararg_node == Some(def0.n) {
                                        return;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        let values = nodes_to_unpack_len(eng, inferred);
        let details = unpacking_extra_info(eng, node, value, inferred);
        match values {
            Some(count) => {
                if targets.len() != count {
                    // W0632 / W0644 (disabled; resurrection)
                    let is_dict = value_is_dict_type(eng, inferred);
                    let (msgid, template) = if is_dict {
                        ("W0644", "Possible unbalanced dict unpacking with %s: left side has %d label%s, right side has %d value%s")
                    } else {
                        ("W0632", "Possible unbalanced tuple unpacking with sequence %s: left side has %d label%s, right side has %d value%s")
                    };
                    let lp = if targets.len() == 1 { "" } else { "s" };
                    let vp = if count == 1 { "" } else { "s" };
                    cx.emit_node(
                        msgid,
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        u::format_template(
                            template,
                            &[&details, &targets.len().to_string(), lp, &count.to_string(), vp],
                        ),
                    );
                }
            }
            None => {
                if !crate::typecheck::is_iterable(eng, cx.caches, inferred, false) {
                    let details = if !details.is_empty() && !details.starts_with(' ') {
                        format!(" {details}")
                    } else {
                        details
                    };
                    cx.emit_node(
                        "E0633",
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        format!("Attempting to unpack a non-sequence{details}"),
                    );
                }
            }
        }
    }

    /// _check_self_cls_assign (variables.py:3054-3085) — W0642
    fn check_self_cls_assign(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let mut assign_names: Vec<String> = Vec::new();
        {
            let md = eng.md(node.m);
            let NodeKind::Assign { targets, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            for &t in targets {
                match &md.tree.nodes[t.idx()].kind {
                    NodeKind::AssignName { name } => {
                        assign_names.push(md.tree.s(*name).to_string());
                    }
                    NodeKind::Tuple { elts, .. } => {
                        for &e in elts {
                            if let NodeKind::AssignName { name } = &md.tree.nodes[e.idx()].kind {
                                assign_names.push(md.tree.s(*name).to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut scope = eng.scope(node);
        // nonlocals_with_same_name: any Nonlocal in scope.body
        let has_nonlocal = {
            let md = eng.md(scope.m);
            let body: Vec<NodeId> = match &md.tree.nodes[scope.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
                NodeKind::ClassDef(d) => d.body.clone(),
                NodeKind::Module(d) => d.body.clone(),
                _ => Vec::new(),
            };
            body.iter().any(|&b| {
                matches!(md.tree.nodes[b.idx()].kind, NodeKind::Nonlocal { .. })
            }) && eng.parent(scope).is_some()
        };
        if has_nonlocal {
            if let Some(p) = eng.parent(scope) {
                scope = eng.scope(p);
            }
        }
        let is_fn = eng.kind_is(scope, |k| {
            matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
        });
        if !is_fn || !is_method(eng, scope) {
            return;
        }
        if eng
            .decoratornames(scope, None)
            .iter()
            .any(|q| q.as_deref() == Some("builtins.staticmethod"))
        {
            return;
        }
        // argnames()[0]
        let first_arg: Option<String> = {
            let md = eng.md(scope.m);
            let args_id = match &md.tree.nodes[scope.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
                _ => return,
            };
            let NodeKind::Arguments(a) = &md.tree.nodes[args_id.idx()].kind else { return };
            let first = a
                .posonlyargs
                .first()
                .or(a.args.first())
                .copied();
            match first {
                Some(f) => match &md.tree.nodes[f.idx()].kind {
                    NodeKind::AssignName { name } => {
                        Some(md.tree.s(*name).to_string())
                    }
                    _ => None,
                },
                None => {
                    if a.vararg.is_some() {
                        Some(md.tree.s(a.vararg.unwrap()).to_string())
                    } else {
                        None
                    }
                }
            }
        };
        let Some(self_cls_name) = first_arg else { return };
        if assign_names.iter().any(|n| *n == self_cls_name) {
            cx.emit_node(
                "W0642",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                u::format_template("Invalid assignment to %s in method", &[&self_cls_name]),
            );
        }
    }

    /// _check_late_binding_closure (variables.py:2952-3000) — W0640
    fn check_late_binding_closure(&mut self, cx: &mut WalkCx, node: GNode) {
        if !(cx.cfg_enabled)("W0640") {
            return;
        }
        let eng = cx.eng;
        let Some(name) = name_of(eng, node) else { return };
        let mut node_scope = eng.frame(node);
        if u::is_default_argument(eng, node, Some(node_scope)) {
            if let Some(p) = eng.parent(node_scope) {
                node_scope = eng.frame(p);
            }
        }
        let scope_is_fn = eng.kind_is(node_scope, |k| {
            matches!(
                k,
                NodeKind::Lambda(_) | NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
            )
        });
        if !scope_is_fn || locals_contains(eng, node_scope, name) {
            return;
        }
        let lk = eng.lookup(node, name);
        let assign_scope = lk.0;
        let stmts: Vec<GNode> = lk
            .1
            .iter()
            .filter_map(|nv| match nv {
                NV::N(g) => Some(*g),
                _ => None,
            })
            .collect();
        if stmts.is_empty() || !eng.parent_of(assign_scope, node_scope) {
            return;
        }
        let name_str = eng.sname(name);
        let is_comprehension = eng.kind_is(assign_scope, |k| {
            matches!(
                k,
                NodeKind::ListComp(_)
                    | NodeKind::SetComp(_)
                    | NodeKind::DictComp(_)
                    | NodeKind::GeneratorExp(_)
            )
        });
        if is_comprehension {
            cx.emit_node(
                "W0640",
                u::lineno(eng, node),
                u::col_offset(eng, node).max(0) as i64,
                u::format_template("Cell variable %s defined in loop", &[&name_str]),
            );
            return;
        }
        let assignment_node = stmts[0];
        // while maybe_for and not For: break at assign_scope / climb
        let mut maybe_for: Option<GNode> = Some(assignment_node);
        let mut broke = false;
        while let Some(mf) = maybe_for {
            if eng.kind_is(mf, |k| matches!(k, NodeKind::For(_))) {
                break;
            }
            if mf == assign_scope {
                broke = true;
                break;
            }
            maybe_for = eng.parent(mf);
        }
        if broke {
            return;
        }
        let Some(mf) = maybe_for else {
            // walked off the tree without finding a For: while-condition
            // exit with maybe_for falsy -> the `if maybe_for and ...` guard
            // fails -> no message
            return;
        };
        // is_being_called: node_scope.parent is Call with func == node_scope
        let is_being_called = eng
            .parent(node_scope)
            .map(|p| {
                let md = eng.md(p.m);
                matches!(&md.tree.nodes[p.n.idx()].kind,
                    NodeKind::Call { func, .. } if *func == node_scope.n)
            })
            .unwrap_or(false);
        let stmt_is_return = eng
            .statement(node_scope)
            .map(|st| eng.kind_is(st, |k| matches!(k, NodeKind::Return { .. })))
            .unwrap_or(false);
        if eng.parent_of(mf, node_scope)
            && !is_being_called
            && eng.parent(node_scope).is_some()
            && !stmt_is_return
        {
            cx.emit_node(
                "W0640",
                u::lineno(eng, node),
                u::col_offset(eng, node).max(0) as i64,
                u::format_template("Cell variable %s defined in loop", &[&name_str]),
            );
        }
    }

    /// visit_for (variables.py:1346-1396) — W0644 (dict-iter path)
    pub fn visit_for(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (target, iter_n): (NodeId, NodeId) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::For(d) => (d.target, d.iter),
                _ => return,
            }
        };
        let targets: Vec<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[target.idx()].kind {
                NodeKind::Tuple { elts, .. } => {
                    elts.iter().map(|&e| GNode { m: node.m, n: e }).collect()
                }
                _ => return,
            }
        };
        let iter_g = GNode { m: node.m, n: iter_n };
        let Some(inferred) = u::safe_infer(eng, cx.caches, iter_g) else { return };
        if !value_is_dict_type(eng, &inferred) {
            return;
        }
        let values = dict_unpack_values(eng, &inferred);
        let Some(values) = values else { return };
        if values.is_empty() {
            return;
        }
        let any_starred = targets
            .iter()
            .any(|&t| eng.kind_is(t, |k| matches!(k, NodeKind::Starred { .. })));
        if matches!(&inferred, Value::DictItems(_)) {
            if targets.len() == 2 && values.iter().all(|v| v.tuple_len() == Some(2)) {
                return;
            }
            if any_starred {
                return;
            }
        }
        let is_raw_dict = matches!(&inferred, Value::Node(g) if eng.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })))
            || matches!(&inferred, Value::SynthDict { .. });
        if is_raw_dict {
            // CONTROL-FLOW QUIRK: the value loop lives in the `else` of
            // `isinstance(inferred, nodes.Dict)` — a raw Dict never reaches
            // it; only the Name-iter 2-target early return applies.
            if eng.kind_is(iter_g, |k| matches!(k, NodeKind::Name { .. })) && targets.len() == 2 {
            }
            return;
        }
        for value in &values {
            let value_length = value.length(eng);
            let is_valid_star_unpack = any_starred && value_length >= targets.len();
            if targets.len() != value_length && !is_valid_star_unpack {
                // details: For -> node.iter.as_string()
                let details = u::as_string(eng, iter_g);
                let lp = if targets.len() == 1 { "" } else { "s" };
                let vp = if value_length == 1 { "" } else { "s" };
                cx.emit_node(
                    "W0644",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    u::format_template(
                        "Possible unbalanced dict unpacking with %s: left side has %d label%s, right side has %d value%s",
                        &[&details, &targets.len().to_string(), lp, &value_length.to_string(), vp],
                    ),
                );
                break;
            }
        }
    }

    /// visit_const (variables.py:3502-3530): record string-literal type
    /// annotation names for W0611/W0612 suppression
    pub fn visit_const(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let value: String = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Const(ConstValue::Str(sv)) => sv.to_string(),
                _ => return,
            }
        };
        if !u::is_node_in_type_annotation_context(eng, node) {
            return;
        }
        // parent (through Tuple) Subscript origin typing.Literal/Annotated
        let mut parent = eng.parent(node);
        if let Some(p) = parent {
            if eng.kind_is(p, |k| matches!(k, NodeKind::Tuple { .. })) {
                parent = eng.parent(p);
            }
        }
        if let Some(p) = parent {
            let origin: Option<GNode> = {
                let md = eng.md(p.m);
                match &md.tree.nodes[p.n.idx()].kind {
                    NodeKind::Subscript { value, .. } => Some(GNode { m: p.m, n: *value }),
                    _ => None,
                }
            };
            if let Some(origin) = origin {
                if is_typing_member(cx, origin, &["Annotated", "Literal"]) {
                    return;
                }
            }
        }
        // extract_node(node.value) on the annotation string; parse errors
        // swallowed (ValueError / AstroidSyntaxError)
        self.store_type_annotation_string(&value);
    }

    /// visit_arguments (variables.py:2188-2190): per-argument `# type:`
    /// comments feed _type_annotation_names
    pub fn visit_arguments(&mut self, cx: &mut WalkCx, node: GNode) {
        let payloads: Vec<String> = {
            let md = cx.eng.md(node.m);
            md.tree
                .type_comments
                .iter()
                .filter(|(n, is_func, _)| *n == node.n && !*is_func)
                .map(|(_, _, p)| p.to_string())
                .collect()
        };
        for p in payloads {
            self.store_type_annotation_string(p.trim().trim_start_matches('*').trim());
        }
    }

    /// leave_assign / leave_with / leave_for — _store_type_annotation_names
    /// (`# type: T` statement comments)
    pub fn leave_stmt_type_comment(&mut self, cx: &mut WalkCx, node: GNode) {
        let payload: Option<String> = {
            let md = cx.eng.md(node.m);
            md.tree
                .type_comments
                .iter()
                .find(|(n, is_func, _)| *n == node.n && !*is_func)
                .map(|(_, _, p)| p.to_string())
        };
        if let Some(p) = payload {
            self.store_type_annotation_string(&p);
        }
    }

    /// `# type: (a, b) -> r` signature comment: store arg annotations then
    /// the return annotation (pylint leave_functiondef order: returns FIRST,
    /// then each arg — variables.py:1550-1555)
    fn store_func_type_comment(&mut self, payload: &str) {
        let Some(open) = payload.find('(') else { return };
        // matching close paren at depth 0
        let bytes = payload.as_bytes();
        let mut depth = 0i32;
        let mut close = None;
        for (i, &b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 && b == b')' {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { return };
        let after = payload[close + 1..].trim_start();
        let Some(ret) = after.strip_prefix("->") else { return };
        // returns first (type_comment_returns), then each arg
        self.store_type_annotation_string(ret.trim());
        let inner = &payload[open + 1..close];
        let mut depth = 0i32;
        let mut seg_start = 0usize;
        let ib = inner.as_bytes();
        let mut segs: Vec<&str> = Vec::new();
        for (i, &b) in ib.iter().enumerate() {
            match b {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => {
                    segs.push(&inner[seg_start..i]);
                    seg_start = i + 1;
                }
                _ => {}
            }
        }
        if seg_start < inner.len() {
            segs.push(&inner[seg_start..]);
        }
        for seg in segs {
            let seg = seg.trim().trim_start_matches('*').trim();
            if seg.is_empty() {
                continue;
            }
            self.store_type_annotation_string(seg);
        }
    }

    /// parse an annotation string (astroid extract_node) and run
    /// _store_type_annotation_node (variables.py:3022-3043) over the
    /// standalone tree; parse errors swallowed
    fn store_type_annotation_string(&mut self, src: &str) {
        let Ok(decoded) = pyast::decode_source(src.as_bytes(), "<annotation>") else {
            return;
        };
        let outcome = pyast::parse::parse_module(&decoded, "<annotation>", "<annotation>", false);
        let Some(tree) = outcome.tree else { return };
        // extract_node: the LAST top-level statement, Expr unwrapped;
        // empty body -> ValueError (swallowed)
        let body: Vec<pyast::NodeId> = match &tree.nodes[pyast::NodeId::MODULE.idx()].kind {
            NodeKind::Module(d) => d.body.clone(),
            _ => return,
        };
        let Some(&last) = body.last() else { return };
        let root = match &tree.nodes[last.idx()].kind {
            NodeKind::Expr { value } => *value,
            _ => last,
        };
        store_type_annotation_tree(&tree, root, &mut self.type_annotation_names);
    }

    /// visit_subscript (variables.py:3458-3496) — E0643 potential-index-error
    pub fn visit_subscript(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (value, slice) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Subscript { value, slice, .. } => {
                    (GNode { m: node.m, n: *value }, GNode { m: node.m, n: *slice })
                }
                _ => return,
            }
        };
        let inferred_slice = u::safe_infer(eng, cx.caches, slice);
        // _check_potential_index_error
        let idx: Option<i64> = match &inferred_slice {
            Some(Value::Node(g)) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(ConstValue::Int(pyast::tree::IntValue::Small(i))) => Some(*i),
                    NodeKind::Const(ConstValue::Int(pyast::tree::IntValue::Big(_))) => Some(i64::MAX),
                    NodeKind::Const(ConstValue::Bool(b)) => Some(*b as i64),
                    _ => None,
                }
            }
            Some(Value::SynthConst(c)) => match &**c {
                ConstValue::Int(pyast::tree::IntValue::Small(i)) => Some(*i),
                ConstValue::Int(pyast::tree::IntValue::Big(_)) => Some(i64::MAX),
                ConstValue::Bool(b) => Some(*b as i64),
                _ => None,
            },
            _ => None,
        };
        let Some(idx) = idx else { return };
        let elts: Vec<GNode> = {
            let md = eng.md(value.m);
            match &md.tree.nodes[value.n.idx()].kind {
                NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => {
                    elts.iter().map(|&e| GNode { m: value.m, n: e }).collect()
                }
                _ => return,
            }
        };
        // _inferred_iterable_length
        let mut length: i64 = 0;
        for elt in elts {
            let is_starred = eng.kind_is(elt, |k| matches!(k, NodeKind::Starred { .. }));
            if !is_starred {
                length += 1;
                continue;
            }
            let inner = {
                let md = eng.md(elt.m);
                match &md.tree.nodes[elt.n.idx()].kind {
                    NodeKind::Starred { value, .. } => GNode { m: elt.m, n: *value },
                    _ => continue,
                }
            };
            let unpacked = u::safe_infer(eng, cx.caches, inner);
            let n = match &unpacked {
                Some(Value::Node(g)) => {
                    let md = eng.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::Tuple { elts, .. }
                        | NodeKind::List { elts, .. }
                        | NodeKind::Set { elts, .. } => Some(elts.len() as i64),
                        _ => None,
                    }
                }
                Some(Value::SynthSeq { elems, .. }) => Some(elems.len() as i64),
                Some(Value::FrozenSet { elems }) => Some(elems.len() as i64),
                _ => None,
            };
            length += n.unwrap_or(1);
        }
        if length < idx + 1 {
            cx.emit_node(
                "E0643",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "Invalid index for iterable length".into(),
            );
        }
    }

    /// _undefined_and_used_before_checker (variables.py:1711-1759)
    fn undefined_and_used_before_checker(&mut self, cx: &mut WalkCx, node: GNode, stmt: GNode) {
        let eng = cx.eng;
        let Some(name) = name_of(eng, node) else { return };
        let frame = eng.scope(stmt);
        let start_index = self.to_consume.len().saturating_sub(1);
        let base_scope_type = self.to_consume[start_index].scope_type;

        let mut i = start_index as isize;
        while i >= 0 {
            let idx = i as usize;
            i -= 1;
            if self.should_node_be_skipped(cx, node, idx, idx == start_index) {
                continue;
            }
            let (action, nodes_to_consume) =
                self.check_consumer(cx, node, stmt, frame, idx, base_scope_type);
            if let Some(mut ntc) = nodes_to_consume {
                if !ntc.is_empty() {
                    // += consumed_uncertain[name] (defaultdict access creates key)
                    let extra = self.to_consume[idx]
                        .consumed_uncertain
                        .entry(name)
                        .or_default()
                        .clone();
                    ntc.extend(extra);
                    self.to_consume[idx].mark_as_consumed(name, ntc);
                }
            }
            match action {
                Action::Continue => continue,
                Action::Return => return,
            }
        }

        // final fallback: undefined-variable (variables.py:1742-1759)
        let name_str = eng.sname(name);
        let is_scope_attr = u::SCOPE_ATTRS.contains(&name_str.as_str());
        let is_class_in_method = name_str == "__class__"
            && u::ancestors(eng, node).iter().any(|&a| {
                u::is_functiondef(eng, a) && is_method(eng, a)
            });
        if !(is_scope_attr
            || cx.caches.is_builtin(eng, name)
            || is_class_in_method)
            && !u::node_ignores_exception(eng, cx.caches, node, "NameError")
        {
            cx.emit_name_msg("E0602", node, &name_str);
        }
    }

    /// _should_node_be_skipped (variables.py:1761-1808)
    fn should_node_be_skipped(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        idx: usize,
        is_start_index: bool,
    ) -> bool {
        let eng = cx.eng;
        let consumer_node = self.to_consume[idx].node;
        let scope_type = self.to_consume[idx].scope_type;
        let Some(name) = name_of(eng, node) else { return false };
        match scope_type {
            ScopeType::Class => {
                if u::is_ancestor_name(eng, consumer_node, node)
                    || (!is_start_index && self.ignore_class_scope(cx, node))
                {
                    if type_param_matches(eng, consumer_node, name) {
                        return false;
                    }
                    return true;
                }
                // Keyword whose parent is ClassDef (metaclass= lookup)
                if let Some(p) = eng.parent(node) {
                    if eng.kind_is(p, |k| matches!(k, NodeKind::Keyword { .. })) {
                        if let Some(pp) = eng.parent(p) {
                            if u::is_classdef(eng, pp) {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            ScopeType::Function => {
                if defined_in_function_definition(eng, node, consumer_node) {
                    if type_param_matches(eng, consumer_node, name) {
                        return false;
                    }
                    return true;
                }
                false
            }
            ScopeType::Lambda => u::is_default_argument(eng, node, Some(consumer_node)),
            _ => false,
        }
    }

    /// _ignore_class_scope (variables.py:2584-2622)
    fn ignore_class_scope(&self, cx: &mut WalkCx, node: GNode) -> bool {
        let eng = cx.eng;
        let Some(name) = name_of(eng, node) else { return true };
        let frame = eng.scope(stmt_of(eng, node));
        let in_ann = defined_in_function_definition(eng, node, frame);
        let in_ancestor_list = u::is_ancestor_name(eng, frame, node);
        let frame_locals_scope = if in_ann || in_ancestor_list {
            eng.parent(frame).map(|p| eng.scope(p)).unwrap_or(frame)
        } else {
            frame
        };
        let name_in_locals = locals_contains(eng, frame_locals_scope, name);
        !((u::is_classdef(eng, frame) || in_ann)
            && !in_lambda_or_comprehension_body(eng, node, frame)
            && name_in_locals)
    }

    /// _check_consumer (variables.py:1811-2019)
    #[allow(clippy::too_many_arguments)]
    fn check_consumer(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        stmt: GNode,
        frame: GNode,
        idx: usize,
        base_scope_type: ScopeType,
    ) -> (Action, Option<Vec<GNode>>) {
        let eng = cx.eng;
        let Some(name) = name_of(eng, node) else { return (Action::Return, None) };

        // consumed fast path (variables.py:1820-1829)
        if self.to_consume[idx].consumed.contains_key(&name) {
            // `not isinstance(node, ComprehensionScope)` is always True for
            // a Name -> _check_late_binding_closure always runs here
            self.check_late_binding_closure(cx, node);
            return (Action::Return, None);
        }

        let found_nodes = {
            let consumer = &mut self.to_consume[idx];
            consumer.get_next_to_consume(cx, node)
        };
        let Some(found_nodes) = found_nodes else {
            return (Action::Continue, None);
        };
        if found_nodes.is_empty() {
            let is_reported = self.report_unfound_name_definition(cx, node, idx);
            let nodes_to_consume = self.to_consume[idx]
                .consumed_uncertain
                .entry(name)
                .or_default()
                .clone();
            let nodes_to_consume = self.filter_type_checking_definitions_from_consumption(
                cx,
                node,
                nodes_to_consume,
                is_reported,
            );
            return (Action::Return, Some(nodes_to_consume));
        }

        self.check_late_binding_closure(cx, node);

        let defnode = u::assign_parent(eng, found_nodes[0]);
        let defstmt = stmt_of(eng, defnode);
        let defframe = eng.frame(defstmt);

        // recursive class reference (variables.py:1853-1886)
        let is_recursive_klass = frame == defframe
            && eng.parent_of(defframe, node)
            && u::is_classdef(eng, defframe)
            && eng.node_name(defframe).as_deref() == Some(eng.sname(name).as_str());

        if is_recursive_klass
            && u::first_ancestor(eng, node, |k| matches!(k, NodeKind::Lambda(_))).is_some()
            && !(u::is_default_argument(eng, node, None) && {
                let sc = eng.scope(node);
                eng.parent(sc).map(|p| eng.scope(p)) == Some(defframe)
            })
        {
            return (Action::Return, None);
        }

        let (maybe_before_assign, annotation_return, use_outer_definition) = self
            .is_variable_violation(
                cx,
                node,
                defnode,
                stmt,
                defstmt,
                frame,
                defframe,
                base_scope_type,
                is_recursive_klass,
            );

        if use_outer_definition {
            return (Action::Continue, None);
        }

        let name_str = cx.eng.sname(name);
        if maybe_before_assign
            && !u::is_defined_before(cx.eng, node)
            && !u::are_exclusive_exc(cx.eng, stmt, defstmt, &["NameError"])
        {
            let defined_by_stmt = defstmt == stmt
                && cx.eng.kind_is(node, |k| {
                    matches!(k, NodeKind::DelName { .. } | NodeKind::AssignName { .. })
                });
            let defstmt_is_delete =
                cx.eng.kind_is(defstmt, |k| matches!(k, NodeKind::Delete { .. }));

            if is_recursive_klass || defined_by_stmt || annotation_return || defstmt_is_delete {
                if !u::node_ignores_exception(cx.eng, cx.caches, node, "NameError") {
                    // postponed evaluation of annotations
                    let stmt_kind_ok = cx.eng.kind_is(stmt, |k| {
                        matches!(
                            k,
                            NodeKind::AnnAssign { .. }
                                | NodeKind::FunctionDef(_)
                                | NodeKind::AsyncFunctionDef(_)
                                | NodeKind::Arguments(_)
                        )
                    });
                    let in_root_locals = locals_contains(cx.eng, module_node(node), name);
                    if !(self.postponed_evaluation_enabled && stmt_kind_ok && in_root_locals) {
                        if defined_by_stmt {
                            return (Action::Continue, Some(vec![node]));
                        }
                        return (Action::Continue, None);
                    }
                }
                // fall through to final return
            } else if base_scope_type != ScopeType::Lambda {
                // operator precedence: see notes/05 §9.6
                let stmt_is_annassign =
                    cx.eng.kind_is(stmt, |k| matches!(k, NodeKind::AnnAssign { .. }));
                let stmt_is_functiondef = cx.eng.kind_is(stmt, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                });
                let stmt_is_typealias =
                    cx.eng.kind_is(stmt, |k| matches!(k, NodeKind::TypeAlias { .. }));
                let node_in_defaults = if stmt_is_functiondef {
                    let md = cx.eng.md(stmt.m);
                    let args_n = match &md.tree.nodes[stmt.n.idx()].kind {
                        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
                        _ => unreachable!(),
                    };
                    let NodeKind::Arguments(ad) = &md.tree.nodes[args_n.idx()].kind else {
                        unreachable!()
                    };
                    ad.defaults.iter().any(|&d| d == node.n)
                        || ad.kw_defaults.iter().any(|o| *o == Some(node.n))
                } else {
                    false
                };
                let exempt = (self.postponed_evaluation_enabled
                    && (stmt_is_annassign || (stmt_is_functiondef && !node_in_defaults)))
                    || (stmt_is_annassign
                        && u::first_ancestor(cx.eng, stmt, |k| {
                            matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                        })
                        .is_some())
                    || stmt_is_typealias;
                if !exempt {
                    cx.emit_name_msg("E0601", node, &name_str);
                    return (Action::Return, Some(found_nodes));
                }
                // else fall through to final return
            } else {
                // base_scope_type == "lambda" (variables.py:1968-1988)
                if u::is_classdef(cx.eng, frame)
                    && locals_contains(cx.eng, frame, name)
                    && u::lineno(cx.eng, stmt) <= u::lineno(cx.eng, defstmt)
                {
                    cx.emit_name_msg("E0601", node, &name_str);
                }
                // falls through to final return (consume found_nodes)
            }
        } else if !self.is_builtin(cx, name) && self.is_only_type_assignment(cx, node, defstmt) {
            if !locals_get(cx.eng, cx.eng.scope(node), name).is_empty() {
                cx.emit_name_msg("E0601", node, &name_str);
            } else {
                cx.emit_name_msg("E0602", node, &name_str);
            }
            return (Action::Return, Some(found_nodes));
        } else if cx.eng.kind_is(defstmt, |k| matches!(k, NodeKind::ClassDef(_)))
            && !defnode_in_type_params(cx.eng, defframe, defnode)
        {
            return self.is_first_level_self_reference(cx, node, defstmt, found_nodes);
        } else if cx.eng.kind_is(defnode, |k| matches!(k, NodeKind::NamedExpr { .. })) {
            if let Some(p) = cx.eng.parent(defnode) {
                if cx.eng.kind_is(p, |k| matches!(k, NodeKind::IfExp { .. }))
                    && is_never_evaluated(cx, defnode, p)
                {
                    cx.emit_name_msg("E0602", node, &name_str);
                    return (Action::Return, Some(found_nodes));
                }
            }
        }

        (Action::Return, Some(found_nodes))
    }

    /// _is_variable_violation (variables.py:2259-2413)
    #[allow(clippy::too_many_arguments)]
    fn is_variable_violation(
        &self,
        cx: &mut WalkCx,
        node: GNode,
        defnode: GNode,
        stmt: GNode,
        defstmt: GNode,
        frame: GNode,
        defframe: GNode,
        base_scope_type: ScopeType,
        is_recursive_klass: bool,
    ) -> (bool, bool, bool) {
        let eng = cx.eng;
        let name = name_of(eng, node).unwrap_or(0);
        let mut maybe_before_assign = true;
        let mut annotation_return = false;
        let mut use_outer_definition = false;

        if frame != defframe {
            maybe_before_assign = detect_global_scope(eng, node, frame, defframe);
        } else if eng.parent(defframe).is_none() {
            // module level
            let name_str = eng.sname(name);
            if u::SCOPE_ATTRS.contains(&name_str.as_str())
                || !eng.builtin_lookup(name).1.is_empty()
            {
                maybe_before_assign = false;
            }
        } else {
            // local scope
            let forbid_lookup = (u::is_functiondef(eng, frame)
                || u::is_lambda(eng, eng.frame(node)))
                && assigned_locally(eng, node);
            if !forbid_lookup && !eng.lookup(module_node(defframe), name).1.is_empty() {
                maybe_before_assign = false;
                use_outer_definition = stmt == defstmt
                    && !eng.kind_is(defnode, |k| matches!(k, NodeKind::Comprehension { .. }));
            } else if locals_contains(eng, defframe, name) {
                maybe_before_assign = !is_nonlocal_name(eng, node, defframe);
            }
        }

        if base_scope_type == ScopeType::Lambda
            && u::is_classdef(eng, frame)
            && locals_contains(eng, frame, name)
        {
            // bar = None; foo = lambda bar=bar: bar
            let in_defaults = {
                let md = eng.md(defnode.m);
                if let NodeKind::Arguments(ad) = &md.tree.nodes[defnode.n.idx()].kind {
                    ad.defaults.iter().any(|&d| d == node.n)
                } else {
                    false
                }
            };
            maybe_before_assign = !(in_defaults && {
                let fl = locals_get(eng, frame, name);
                !fl.is_empty()
                    && u::lineno(eng, fl[0]) < u::lineno(eng, defstmt)
            });
        } else if u::is_classdef(eng, defframe) && u::is_functiondef(eng, frame) {
            // function return annotations (variables.py:2310-2356)
            let frame_returns: Option<GNode> = {
                let md = eng.md(frame.m);
                match &md.tree.nodes[frame.n.idx()].kind {
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                        d.returns.map(|r| GNode { m: frame.m, n: r })
                    }
                    _ => None,
                }
            };
            if frame_returns == Some(node) {
                if eng.parent_of(defframe, node) {
                    annotation_return = true;
                    let dlocals = locals_get(eng, defframe, name);
                    if !dlocals.is_empty() {
                        let definition = dlocals[0];
                        maybe_before_assign =
                            u::lineno(eng, definition) >= u::lineno(eng, frame);
                    } else {
                        maybe_before_assign = true;
                    }
                } else {
                    let defframe_parent = eng.parent(defframe);
                    let is_module_parent = defframe_parent
                        .map(|p| u::is_module(eng, p))
                        .unwrap_or(false);
                    let frame_ancestors = u::ancestors(eng, frame);
                    let any_funcdef = frame_ancestors
                        .iter()
                        .any(|&a| u::is_functiondef(eng, a));
                    let last_is_defframe_parent = frame_ancestors
                        .last()
                        .map(|&l| Some(l) == defframe_parent)
                        .unwrap_or(false);
                    if is_module_parent
                        && !frame_ancestors.is_empty()
                        && any_funcdef
                        && last_is_defframe_parent
                    {
                        annotation_return = true;
                        maybe_before_assign = false;
                    }
                }
            }
            if eng
                .parent(node)
                .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::Arguments(_))))
                .unwrap_or(false)
            {
                maybe_before_assign = u::lineno(eng, stmt) <= u::lineno(eng, defstmt);
            }
        } else if is_recursive_klass {
            maybe_before_assign = true;
        } else {
            maybe_before_assign =
                maybe_before_assign && u::lineno(eng, stmt) <= u::lineno(eng, defstmt);
            if maybe_before_assign && u::lineno(eng, stmt) == u::lineno(eng, defstmt) {
                let defframe_is_func = u::is_functiondef(eng, defframe);
                if defframe_is_func
                    && frame == defframe
                    && eng.parent_of(defframe, node)
                    && (defnode_in_type_params(eng, defframe, defnode) || stmt != defstmt)
                {
                    maybe_before_assign = false;
                } else if eng.kind_is(defstmt, |k| {
                    matches!(
                        k,
                        NodeKind::Assign { .. }
                            | NodeKind::AnnAssign { .. }
                            | NodeKind::AugAssign { .. }
                            | NodeKind::Expr { .. }
                            | NodeKind::Return { .. }
                            | NodeKind::Match { .. }
                            | NodeKind::TypeAlias { .. }
                    )
                }) && maybe_used_and_assigned_at_once(eng, defstmt)
                    && frame == defframe
                    && eng.parent_of(defframe, node)
                    && stmt == defstmt
                {
                    maybe_before_assign = false;
                } else if eng.kind_is(defnode, |k| matches!(k, NodeKind::NamedExpr { .. }))
                    && frame == defframe
                    && eng.parent_of(defframe, stmt)
                    && stmt == defstmt
                    && u::is_before(eng, defnode, node)
                {
                    let defnode_value = {
                        let md = eng.md(defnode.m);
                        match &md.tree.nodes[defnode.n.idx()].kind {
                            NodeKind::NamedExpr { value, .. } => {
                                Some(GNode { m: defnode.m, n: *value })
                            }
                            _ => None,
                        }
                    };
                    maybe_before_assign = defnode_value == Some(node)
                        || u::ancestors(eng, node)
                            .iter()
                            .any(|&a| Some(a) == defnode_value);
                } else if u::is_classdef(eng, defframe)
                    && defnode_in_type_params(eng, defframe, defnode)
                {
                    maybe_before_assign = false;
                }
            }
        }
        (maybe_before_assign, annotation_return, use_outer_definition)
    }

    fn is_builtin(&self, cx: &mut WalkCx, name: GSym) -> bool {
        // additional_builtins default () + utils.is_builtin
        cx.caches.is_builtin(cx.eng, name)
    }

    /// _is_only_type_assignment (variables.py:2478-2533)
    fn is_only_type_assignment(&self, cx: &mut WalkCx, node: GNode, defstmt: GNode) -> bool {
        let eng = cx.eng;
        let is_bare_annassign = eng.kind_is(defstmt, |k| {
            matches!(k, NodeKind::AnnAssign { value: None, .. })
        });
        if !is_bare_annassign {
            return false;
        }
        let Some(name) = name_of(eng, node) else { return false };
        let defstmt_frame = eng.frame(defstmt);
        let node_frame = eng.frame(node);
        let mut parent: Option<GNode> = Some(node);
        let boundary = eng.parent(defstmt_frame);
        while parent.is_some() && parent != boundary {
            let parent_scope = eng.scope(parent.unwrap());
            // nonlocal assignment in inner functions?
            for inner in u::preorder(eng, parent_scope) {
                if !u::is_functiondef(eng, inner) || inner == parent_scope {
                    continue;
                }
                let mut has_nonlocal = false;
                let mut has_assign = false;
                for n in u::preorder(eng, inner) {
                    let md = eng.md(n.m);
                    match &md.tree.nodes[n.n.idx()].kind {
                        NodeKind::Nonlocal { names } => {
                            if names.iter().any(|&s| eng.g(&md, s) == name) {
                                has_nonlocal = true;
                            }
                        }
                        NodeKind::AssignName { name: an } => {
                            if eng.g(&md, *an) == name {
                                has_assign = true;
                            }
                        }
                        _ => {}
                    }
                }
                if has_nonlocal && has_assign {
                    return false;
                }
            }
            let local_refs = locals_get(eng, parent_scope, name);
            for ref_node in local_refs {
                if defstmt_frame == node_frame && u::lineno(eng, ref_node) > u::lineno(eng, node)
                {
                    break;
                }
                let ref_parent = eng.parent(ref_node);
                let parent_is_bare_annassign = ref_parent
                    .map(|p| {
                        eng.kind_is(p, |k| matches!(k, NodeKind::AnnAssign { value: None, .. }))
                    })
                    .unwrap_or(false);
                let walrus_ancestor = ref_parent
                    .map(|p| {
                        let md = eng.md(p.m);
                        if let NodeKind::NamedExpr { value, .. } = &md.tree.nodes[p.n.idx()].kind {
                            let vg = GNode { m: p.m, n: *value };
                            drop(md);
                            u::ancestors(eng, node).iter().any(|&a| a == vg)
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !parent_is_bare_annassign && !walrus_ancestor {
                    return false;
                }
            }
            parent = eng.parent(parent_scope);
        }
        true
    }

    /// _is_first_level_self_reference (variables.py:2535-2555)
    fn is_first_level_self_reference(
        &self,
        cx: &mut WalkCx,
        node: GNode,
        defstmt: GNode,
        found_nodes: Vec<GNode>,
    ) -> (Action, Option<Vec<GNode>>) {
        let eng = cx.eng;
        let nf = eng.frame(node);
        if eng.parent(nf) == Some(defstmt) && stmt_of(eng, node) == nf {
            if u::is_node_in_type_annotation_context(eng, node) {
                if !self.postponed_evaluation_enabled {
                    return (Action::Continue, None);
                }
                return (Action::Return, None);
            }
            if let Some(p) = eng.parent(node) {
                if eng.kind_is(p, |k| matches!(k, NodeKind::Call { .. })) {
                    if let Some(pp) = eng.parent(p) {
                        if eng.kind_is(pp, |k| matches!(k, NodeKind::Arguments(_))) {
                            return (Action::Continue, None);
                        }
                    }
                }
            }
        }
        (Action::Return, Some(found_nodes))
    }

    /// _report_unfound_name_definition (variables.py:2021-2068)
    fn report_unfound_name_definition(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        idx: usize,
    ) -> bool {
        let eng = cx.eng;
        let Some(name) = name_of(eng, node) else { return false };
        let name_str = eng.sname(name);
        if (self.postponed_evaluation_enabled && u::is_node_in_type_annotation_context(eng, node))
            || u::is_node_in_pep695_type_context(eng, node)
        {
            return false;
        }
        if self.is_builtin(cx, name) {
            return false;
        }
        if is_variable_annotation_in_function(eng, node) {
            return false;
        }
        let uncertain = self.to_consume[idx]
            .consumed_uncertain
            .get(&name)
            .cloned()
            .unwrap_or_default();
        if has_nonlocal_in_enclosing_frame(eng, node, &uncertain) {
            return false;
        }
        if let Some(scopes) = self.reported_type_checking_usage_scopes.get(&name_str) {
            if scopes.contains(&eng.scope(node)) {
                return false;
            }
        }
        let msg = if self.to_consume[idx]
            .names_defined_under_one_branch_only
            .contains(&name)
        {
            "E0606"
        } else {
            "E0601"
        };
        cx.emit_name_msg(msg, node, &name_str);
        true
    }

    /// _filter_type_checking_definitions_from_consumption (variables.py:2070-2094)
    fn filter_type_checking_definitions_from_consumption(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        nodes_to_consume: Vec<GNode>,
        is_reported: bool,
    ) -> Vec<GNode> {
        let eng = cx.eng;
        let mut type_checking: FxHashSet<GNode> = FxHashSet::default();
        for &n in &nodes_to_consume {
            let is_kind = eng.kind_is(n, |k| {
                matches!(
                    k,
                    NodeKind::Import { .. } | NodeKind::ImportFrom { .. } | NodeKind::ClassDef(_)
                )
            });
            if is_kind && u::in_type_checking_block(eng, cx.caches, n) {
                type_checking.insert(n);
            }
        }
        if !type_checking.is_empty() && is_reported {
            let name_str = eng.sname(name_of(eng, node).unwrap_or(0));
            self.reported_type_checking_usage_scopes
                .entry(name_str)
                .or_default()
                .push(eng.scope(node));
        }
        nodes_to_consume
            .into_iter()
            .filter(|n| !type_checking.contains(n))
            .collect()
    }

    // -----------------------------------------------------------------
    // __all__ checks: E0603/E0604/E0605 (variables.py:3220-3276)
    // -----------------------------------------------------------------
    fn check_all(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        not_consumed: &mut IndexMap<GSym, Vec<GNode>>,
    ) {
        let eng = cx.eng;
        let all_sym = eng.sym("__all__");
        let assigned = match eng.igetattr_first(&Value::Node(node), all_sym, None) {
            Ok(Some(v)) => v,
            _ => return, // InferenceError / StopIteration -> silent
        };
        if assigned.is_uninferable() {
            return;
        }
        let pytype = u::value_pytype(eng, &assigned);
        let ok_type = matches!(pytype.as_deref(), Some("builtins.list") | Some("builtins.tuple"));
        if !ok_type {
            // E0605 at line=assigned.tolineno, col=assigned.col_offset
            let (line, col) = match &assigned {
                Value::Node(g) => {
                    let md = eng.md(g.m);
                    let n = &md.tree.nodes[g.n.idx()];
                    (n.tolineno, n.col_offset.max(0) as i64)
                }
                _ => (0, 0),
            };
            cx.emit_node(
                "E0605",
                if line == 0 { 1 } else { line },
                col,
                "Invalid format for __all__, must be tuple or list".to_string(),
            );
            return;
        }
        // elements
        enum Elt {
            Node(GNode),
            Val(Value),
        }
        let elts: Vec<Elt> = match &assigned {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } => elts
                        .iter()
                        .map(|&e| Elt::Node(GNode { m: g.m, n: e }))
                        .collect(),
                    _ => Vec::new(), // getattr(assigned, "elts", ())
                }
            }
            Value::SynthSeq { elems, .. } => elems.iter().map(|v| Elt::Val(v.clone())).collect(),
            _ => Vec::new(),
        };
        for elt in elts {
            let (elt_node, inferred): (Option<GNode>, Option<Value>) = match elt {
                Elt::Node(g) => {
                    // next(elt.infer()) — single pull
                    match eng.first_value(g, &u::fresh_ctx()) {
                        Ok(Some(v)) => (Some(g), Some(v)),
                        _ => continue, // InferenceError -> continue
                    }
                }
                Elt::Val(v) => match v {
                    Value::Node(g) => match eng.first_value(g, &u::fresh_ctx()) {
                        Ok(Some(iv)) => (Some(g), Some(iv)),
                        _ => continue,
                    },
                    other => (None, Some(other)),
                },
            };
            let Some(inferred) = inferred else { continue };
            if inferred.is_uninferable() {
                continue;
            }
            // `if not elt_name.parent: continue`
            let inferred_node = match &inferred {
                Value::Node(g) => Some(*g),
                _ => None,
            };
            match inferred_node {
                Some(g) => {
                    if eng.parent(g).is_none() {
                        continue;
                    }
                }
                None => continue, // synthetic results have no parent
            }
            let const_val = eng.value_const(&inferred);
            let is_str = matches!(
                const_val,
                Some(ConstValue::Str(_)) | Some(ConstValue::StrSurrogate(_))
            );
            if !is_str {
                // E0604 — args = elt.as_string()
                let Some(en) = elt_node else { continue };
                let text = format!(
                    "Invalid object {} in __all__, must contain only strings",
                    u::py_repr_str(&u::as_string(eng, en))
                );
                cx.emit_node_rooted(
                    "E0604",
                    en,
                    u::lineno(eng, en),
                    u::col_offset(eng, en).max(0) as i64,
                    text,
                );
                continue;
            }
            let elt_name_str = match const_val {
                Some(ConstValue::Str(s)) => s.to_string(),
                _ => continue, // surrogate strings: skip (no corpus hit)
            };
            let elt_sym = eng.sym(&elt_name_str);
            if not_consumed.contains_key(&elt_sym) {
                not_consumed.shift_remove(&elt_sym);
                continue;
            }
            if locals_contains(eng, node, elt_sym) {
                continue;
            }
            let md = eng.md(node.m);
            let package = md.package;
            let file = md.file.clone();
            drop(md);
            let emit_e0603 = |cx: &mut WalkCx, en: GNode| {
                let text = format!(
                    "Undefined variable name {} in __all__",
                    u::py_repr_str(&elt_name_str)
                );
                cx.emit_node_rooted(
                    "E0603",
                    en,
                    u::lineno(cx.eng, en),
                    u::col_offset(cx.eng, en).max(0) as i64,
                    text,
                );
            };
            let Some(en) = elt_node else { continue };
            if !package {
                emit_e0603(cx, en);
            } else {
                // basename check: __init__ file
                let base = std::path::Path::new(&file)
                    .file_stem()
                    .map(|s| s == "__init__")
                    .unwrap_or(false);
                if base {
                    let mod_name = eng.md(node.m).name.clone();
                    let full = format!("{mod_name}.{elt_name_str}");
                    let parts: Vec<&str> = full.split('.').collect();
                    if !eng.modutils_can_resolve(&parts) {
                        emit_e0603(cx, en);
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // metaclass E0602 (variables.py:3388-3456)
    // -----------------------------------------------------------------
    fn check_metaclasses(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let children = eng.md(node.m).tree.children(node.n);
        let mut consumed: Vec<(usize, GSym)> = Vec::new();
        for c in children {
            let g = GNode { m: node.m, n: c };
            if u::is_classdef(eng, g) {
                consumed.extend(self.check_classdef_metaclasses(cx, g, node));
            }
        }
        for (idx, name) in consumed {
            self.to_consume[idx].to_consume.shift_remove(&name);
        }
    }

    fn check_classdef_metaclasses(
        &mut self,
        cx: &mut WalkCx,
        klass: GNode,
        parent_node: GNode,
    ) -> Vec<(usize, GSym)> {
        let eng = cx.eng;
        let metaclass_expr: Option<GNode> = {
            let md = eng.md(klass.m);
            match &md.tree.nodes[klass.n.idx()].kind {
                NodeKind::ClassDef(d) => d.metaclass.map(|n| GNode { m: klass.m, n }),
                _ => None,
            }
        };
        let Some(mexpr) = metaclass_expr else { return Vec::new() };
        let mut consumed: Vec<(usize, GSym)> = Vec::new();
        let metaclass = eng.metaclass(klass, None);
        let mut name_str = String::new();
        {
            let md = eng.md(mexpr.m);
            match &md.tree.nodes[mexpr.n.idx()].kind {
                NodeKind::Name { name } => name_str = md.tree.s(*name).to_string(),
                NodeKind::Attribute { expr, .. } => {
                    let mut attr = GNode { m: mexpr.m, n: *expr };
                    drop(md);
                    loop {
                        let md = eng.md(attr.m);
                        match &md.tree.nodes[attr.n.idx()].kind {
                            NodeKind::Name { name } => {
                                name_str = md.tree.s(*name).to_string();
                                break;
                            }
                            NodeKind::Attribute { expr, .. } => {
                                let e = *expr;
                                drop(md);
                                attr = GNode { m: attr.m, n: e };
                            }
                            _ => break, // would AttributeError in pylint; bail
                        }
                    }
                }
                NodeKind::Call { func, .. } => {
                    if let NodeKind::Name { name } = &md.tree.nodes[func.idx()].kind {
                        name_str = md.tree.s(*name).to_string();
                    } else if metaclass.is_some() {
                        drop(md);
                        name_str = metaclass_root_name(eng, metaclass.as_ref().unwrap());
                    }
                }
                _ => {
                    if metaclass.is_some() {
                        drop(md);
                        name_str = metaclass_root_name(eng, metaclass.as_ref().unwrap());
                    }
                }
            }
        }
        for (from, to) in METACLASS_NAME_TRANSFORMS {
            if name_str == *from {
                name_str = to.to_string();
            }
        }
        let mut found = false;
        if !name_str.is_empty() {
            let name = eng.sym(&name_str);
            let klass_line = u::lineno(eng, klass);
            // INNER -> OUTER ([::-1]); does NOT break across consumers
            for i in (0..self.to_consume.len()).rev() {
                let found_nodes = self.to_consume[i].to_consume.get(&name).cloned().unwrap_or_default();
                for fnode in found_nodes {
                    if u::lineno(eng, fnode) <= klass_line {
                        consumed.push((i, name));
                        found = true;
                        break;
                    }
                }
            }
            for fnode in locals_get(eng, parent_node, name) {
                if u::lineno(eng, fnode) <= klass_line {
                    found = true;
                    break;
                }
            }
        }
        if !found && metaclass.is_none() {
            let is_exempt = u::SCOPE_ATTRS.contains(&name_str.as_str())
                || (!name_str.is_empty() && cx.caches.is_builtin(eng, eng.sym(&name_str)))
                || (name_str.is_empty() && {
                    // "" in scope_attrs is False; is_builtin("") False
                    false
                });
            if !is_exempt {
                let text = format!("Undefined variable {}", u::py_repr_str(&name_str));
                cx.emit_node(
                    "E0602",
                    u::lineno(eng, klass),
                    u::col_offset(eng, klass).max(0) as i64,
                    text,
                );
            }
        }
        consumed
    }
}

fn metaclass_root_name(eng: &Engine, v: &Value) -> String {
    match v {
        Value::Node(g) => {
            let mut top = *g;
            while let Some(p) = eng.parent(top) {
                top = p;
            }
            eng.md(top.m).name.clone()
        }
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
            let mut top = *cls;
            while let Some(p) = eng.parent(top) {
                top = p;
            }
            eng.md(top.m).name.clone()
        }
        _ => String::new(),
    }
}

/// FunctionDef.is_method (astroid scoped_nodes.py)
fn is_method(eng: &Engine, func: GNode) -> bool {
    let t = eng.func_type(func);
    if t == pyinfer::graph::FType::Function {
        return false;
    }
    eng.parent(func)
        .map(|p| u::is_classdef(eng, eng.frame(p)))
        .unwrap_or(false)
}

/// _defined_in_function_definition (variables.py:2205-2227)
fn defined_in_function_definition(eng: &Engine, node: GNode, frame: GNode) -> bool {
    if !u::is_functiondef(eng, frame) {
        return false;
    }
    if eng.statement(node) != Some(frame) {
        return false;
    }
    let md = eng.md(frame.m);
    let (args_n, decorators, returns) = match &md.tree.nodes[frame.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
            (d.args, d.decorators, d.returns)
        }
        _ => return false,
    };
    let NodeKind::Arguments(ad) = &md.tree.nodes[args_n.idx()].kind else { return false };
    let in_annotations = ad
        .annotations
        .iter()
        .chain(ad.posonlyargs_annotations.iter())
        .chain(ad.kwonlyargs_annotations.iter())
        .any(|o| *o == Some(node.n))
        || ad.varargannotation == Some(node.n)
        || ad.kwargannotation == Some(node.n);
    drop(md);
    if in_annotations {
        return true;
    }
    let args_g = GNode { m: frame.m, n: args_n };
    if eng.parent_of(args_g, node) {
        return true;
    }
    if let Some(dec) = decorators {
        let dg = GNode { m: frame.m, n: dec };
        if eng.parent_of(dg, node) {
            return true;
        }
    }
    if let Some(r) = returns {
        let rg = GNode { m: frame.m, n: r };
        if rg == node || eng.parent_of(rg, node) {
            return true;
        }
    }
    false
}

/// _in_lambda_or_comprehension_body (variables.py:2229-2257)
fn in_lambda_or_comprehension_body(eng: &Engine, node: GNode, frame: GNode) -> bool {
    let mut child = node;
    let mut parent = eng.parent(node);
    while let Some(p) = parent {
        if p == frame {
            return false;
        }
        let md = eng.md(p.m);
        match &md.tree.nodes[p.n.idx()].kind {
            NodeKind::Lambda(d) => {
                if child.n != d.args {
                    return true;
                }
            }
            NodeKind::Comprehension { iter, .. } => {
                if child.n != *iter {
                    return true;
                }
            }
            NodeKind::ListComp(d) | NodeKind::SetComp(d) | NodeKind::GeneratorExp(d) => {
                if !(!d.generators.is_empty() && child.n == d.generators[0]) {
                    return true;
                }
            }
            NodeKind::DictComp(d) => {
                if !(!d.generators.is_empty() && child.n == d.generators[0]) {
                    return true;
                }
            }
            _ => {}
        }
        drop(md);
        child = p;
        parent = eng.parent(p);
    }
    false
}

/// _detect_global_scope (variables.py:125-200)
fn detect_global_scope(eng: &Engine, node: GNode, frame: GNode, defframe: GNode) -> bool {
    let scope = eng.parent(frame).map(|p| eng.scope(p));
    let def_scope = eng.parent(defframe).map(|p| eng.scope(p));
    if u::is_classdef(eng, frame) && scope != def_scope {
        let first_func_anc = u::first_ancestor(eng, node, |k| {
            matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
        });
        if scope == first_func_anc {
            return false;
        }
    }
    if u::is_functiondef(eng, frame) {
        if eng.parent_of(frame, defframe) {
            return u::lineno(eng, node) < u::lineno(eng, defframe);
        }
        let parent_ok = eng
            .parent(node)
            .map(|p| {
                eng.kind_is(p, |k| {
                    matches!(
                        k,
                        NodeKind::FunctionDef(_)
                            | NodeKind::AsyncFunctionDef(_)
                            | NodeKind::Arguments(_)
                    )
                })
            })
            .unwrap_or(false);
        if !parent_ok {
            return false;
        }
    }
    // for current_scope in (scope or frame, def_scope): while parent_scope:
    // a None def_scope contributes nothing (the while never runs).
    let mut break_scopes: Vec<GNode> = Vec::new();
    for current in [Some(scope.unwrap_or(frame)), def_scope] {
        let mut parent_scope = current;
        while let Some(ps) = parent_scope {
            let is_class_or_module = eng.kind_is(ps, |k| {
                matches!(k, NodeKind::ClassDef(_) | NodeKind::Module(_))
            });
            if !is_class_or_module {
                break_scopes.push(ps);
                break;
            }
            parent_scope = eng.parent(ps).map(|p| eng.scope(p));
        }
    }
    let set: FxHashSet<GNode> = break_scopes.into_iter().collect();
    if set.len() > 1 {
        return false;
    }
    u::lineno(eng, frame) < u::lineno(eng, defframe)
}

/// _maybe_used_and_assigned_at_once (variables.py:2415-2454)
fn maybe_used_and_assigned_at_once(eng: &Engine, defstmt: GNode) -> bool {
    let md = eng.md(defstmt.m);
    match &md.tree.nodes[defstmt.n.idx()].kind {
        NodeKind::Match { cases, .. } => {
            let cases = cases.clone();
            drop(md);
            return cases.iter().any(|&c| {
                eng.kind_is(GNode { m: defstmt.m, n: c }, |k| {
                    matches!(k, NodeKind::MatchCase { guard: Some(_), .. })
                })
            });
        }
        NodeKind::IfExp { .. } => return true,
        NodeKind::TypeAlias { .. } => return true,
        _ => {}
    }
    let value: Option<NodeId> = match &md.tree.nodes[defstmt.n.idx()].kind {
        NodeKind::Assign { value, .. } => Some(*value),
        NodeKind::AnnAssign { value, .. } => *value,
        NodeKind::AugAssign { value, .. } => Some(*value),
        NodeKind::Expr { value } => Some(*value),
        NodeKind::Return { value } => *value,
        _ => None,
    };
    drop(md);
    let Some(value) = value else { return false };
    let vg = GNode { m: defstmt.m, n: value };
    let md = eng.md(vg.m);
    match &md.tree.nodes[vg.n.idx()].kind {
        NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } | NodeKind::Set { elts } => {
            let elts = elts.clone();
            drop(md);
            return elts.iter().any(|&e| {
                let eg = GNode { m: vg.m, n: e };
                let matches_kind = eng.kind_is(eg, |k| {
                    matches!(
                        k,
                        NodeKind::Assign { .. }
                            | NodeKind::AnnAssign { .. }
                            | NodeKind::AugAssign { .. }
                            | NodeKind::Expr { .. }
                            | NodeKind::Return { .. }
                            | NodeKind::Match { .. }
                            | NodeKind::TypeAlias { .. }
                            | NodeKind::IfExp { .. }
                    )
                });
                matches_kind && maybe_used_and_assigned_at_once(eng, eg)
            });
        }
        NodeKind::IfExp { .. } => return true,
        NodeKind::Lambda(d) => {
            let body = d.body;
            drop(md);
            return eng.kind_is(GNode { m: vg.m, n: body }, |k| {
                matches!(k, NodeKind::IfExp { .. })
            });
        }
        NodeKind::Dict { items } => {
            let items = items.clone();
            drop(md);
            if items.iter().any(|&(k, v)| {
                eng.kind_is(GNode { m: vg.m, n: k }, |kk| matches!(kk, NodeKind::IfExp { .. }))
                    || eng.kind_is(GNode { m: vg.m, n: v }, |kk| {
                        matches!(kk, NodeKind::IfExp { .. })
                    })
            }) {
                return true;
            }
            return false;
        }
        NodeKind::Call { .. } => {}
        _ => return false,
    }
    drop(md);
    // any Call under value with IfExp args/kwargs/func.expr
    for call in u::preorder(eng, vg) {
        let md = eng.md(call.m);
        let NodeKind::Call { func, args, keywords } = &md.tree.nodes[call.n.idx()].kind else {
            continue;
        };
        let is_ifexp = |n: NodeId| {
            matches!(md.tree.nodes[n.idx()].kind, NodeKind::IfExp { .. })
        };
        if keywords.iter().any(|&kw| {
            if let NodeKind::Keyword { value, .. } = &md.tree.nodes[kw.idx()].kind {
                is_ifexp(*value)
            } else {
                false
            }
        }) {
            return true;
        }
        if args.iter().any(|&a| is_ifexp(a)) {
            return true;
        }
        if let NodeKind::Attribute { expr, .. } = &md.tree.nodes[func.idx()].kind {
            if is_ifexp(*expr) {
                return true;
            }
        }
    }
    false
}

/// _is_never_evaluated (variables.py:2557-2571)
fn is_never_evaluated(cx: &mut WalkCx, defnode: GNode, ifexp: GNode) -> bool {
    let eng = cx.eng;
    let md = eng.md(ifexp.m);
    let NodeKind::IfExp { test, body, orelse } = &md.tree.nodes[ifexp.n.idx()].kind else {
        return false;
    };
    let (test, body, orelse) = (*test, *body, *orelse);
    drop(md);
    let tv = u::safe_infer(eng, cx.caches, GNode { m: ifexp.m, n: test });
    if let Some(v) = tv {
        match eng.value_const(&v) {
            Some(ConstValue::Bool(true)) => return defnode.n == orelse,
            Some(ConstValue::Bool(false)) => return defnode.n == body,
            _ => {}
        }
    }
    false
}

/// _is_variable_annotation_in_function (variables.py:2573-2582)
fn is_variable_annotation_in_function(eng: &Engine, node: GNode) -> bool {
    let Some(ann_assign) = u::first_ancestor(eng, node, |k| matches!(k, NodeKind::AnnAssign { .. }))
    else {
        return false;
    };
    let md = eng.md(ann_assign.m);
    let NodeKind::AnnAssign { annotation, .. } = &md.tree.nodes[ann_assign.n.idx()].kind else {
        return false;
    };
    let ag = GNode { m: ann_assign.m, n: *annotation };
    drop(md);
    (ag == node || eng.parent_of(ag, node))
        && u::first_ancestor(eng, ann_assign, |k| {
            matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
        })
        .is_some()
}

/// _has_nonlocal_in_enclosing_frame (variables.py:2459-2476)
fn has_nonlocal_in_enclosing_frame(
    eng: &Engine,
    node: GNode,
    uncertain_definitions: &[GNode],
) -> bool {
    let defining_frames: FxHashSet<GNode> = uncertain_definitions
        .iter()
        .map(|&d| eng.frame(d))
        .collect();
    let mut frame: Option<GNode> = Some(eng.frame(node));
    let mut is_enclosing = false;
    while let Some(f) = frame {
        if is_enclosing {
            break;
        }
        is_enclosing = defining_frames
            .iter()
            .all(|&df| f == df || eng.parent_of(f, df));
        if is_enclosing && is_nonlocal_name(eng, node, f) {
            return true;
        }
        frame = eng.parent(f).map(|p| eng.frame(p));
    }
    false
}

// ---------------------------------------------------------------------------
// E0633 / unused-import helpers
// ---------------------------------------------------------------------------

/// _nodes_to_unpack length (variables.py:3140-3150)
fn nodes_to_unpack_len(eng: &Engine, v: &Value) -> Option<usize> {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Tuple { elts, .. }
                | NodeKind::List { elts, .. }
                | NodeKind::Set { elts, .. } => Some(elts.len()),
                NodeKind::Dict { items } => Some(items.len()),
                _ => None,
            }
        }
        Value::SynthSeq { elems, .. } => Some(elems.len()),
        Value::FrozenSet { elems } => Some(elems.len()),
        Value::SynthDict { items } => Some(items.len()),
        Value::DictKeys(dr) | Value::DictValues(dr) | Value::DictItems(dr) => match &**dr {
            pyinfer::value::DictRef::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => Some(items.len()),
                    _ => None,
                }
            }
            pyinfer::value::DictRef::Synth(items) => Some(items.len()),
        },
        Value::Inst { cls, .. } => {
            // typing.NamedTuple-derived instance: AssignName first-locals
            let is_nt = eng
                .ancestors(*cls, true, None)
                .iter()
                .any(|&a| eng.qname(a) == "typing.NamedTuple");
            if !is_nt {
                return None;
            }
            let md = eng.md(cls.m);
            let locals = md.locals.borrow();
            let count = locals
                .get(&cls.n)
                .map(|m| {
                    m.values()
                        .filter(|vs| {
                            vs.first()
                                .map(|&g| {
                                    eng.kind_is(g, |k| {
                                        matches!(k, NodeKind::AssignName { .. })
                                    })
                                })
                                .unwrap_or(false)
                        })
                        .count()
                })
                .unwrap_or(0);
            Some(count)
        }
        _ => None,
    }
}

fn value_is_dict_type(eng: &Engine, v: &Value) -> bool {
    match v {
        Value::Node(g) => eng.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })),
        Value::SynthDict { .. }
        | Value::DictKeys(_)
        | Value::DictValues(_)
        | Value::DictItems(_) => true,
        _ => false,
    }
}

/// _get_unpacking_extra_info (variables.py:101-122)
fn unpacking_extra_info(eng: &Engine, node: GNode, value: GNode, inferred: &Value) -> String {
    if value_is_dict_type(eng, inferred) {
        // node is an Assign -> more = node.value.as_string()
        return u::as_string(eng, value);
    }
    let (inferred_root, inferred_line, inferred_node): (String, u32, Option<GNode>) =
        match inferred {
            Value::Node(g) => (
                crate::tailmisc::root_name(eng, *g),
                raw_lineno(eng, *g),
                Some(*g),
            ),
            // container-brain results: register_builtin_transform parented
            // the fresh node to the builtin CALL and copied its lineno
            Value::SynthSeq { .. } | Value::FrozenSet { .. }
                if eng.container_prov(inferred).is_some() =>
            {
                let call = eng.container_prov(inferred).unwrap();
                (crate::tailmisc::root_name(eng, call), eng.fromlineno(call), None)
            }
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
                (crate::tailmisc::root_name(eng, *cls), raw_lineno(eng, *cls), None)
            }
            Value::BoundMethod { func, .. }
            | Value::DescBM { func, .. }
            | Value::UnboundMethod { func }
            | Value::Property { func, .. }
            | Value::Partial { func, .. }
            | Value::Generator { func, .. } => {
                (crate::tailmisc::root_name(eng, *func), raw_lineno(eng, *func), None)
            }
            _ => (String::new(), 0, None),
        };
    let node_root = eng.md(node.m).name.clone();
    if node_root == inferred_root {
        if eng.fromlineno(node) == inferred_line {
            if let Some(g) = inferred_node {
                return format!("'{}'", u::as_string(eng, g));
            }
            // container-brain value: astroid renders the from_elements
            // node's as_string (Tuple/List/Set of consts)
            if let Some(rendered) = synth_container_as_string(inferred) {
                return format!("'{rendered}'");
            }
            return String::new();
        } else if inferred_line != 0 {
            return format!("defined at line {inferred_line}");
        }
        String::new()
    } else if inferred_line != 0 {
        format!("defined at line {inferred_line} of {inferred_root}")
    } else {
        String::new()
    }
}

/// utils.in_for_else_branch (utils.py:2043-2048)
fn in_for_else_branch(eng: &Engine, parent: GNode, stmt: GNode) -> bool {
    let md = eng.md(parent.m);
    let NodeKind::For(d) = &md.tree.nodes[parent.n.idx()].kind else { return false };
    let orelse = d.orelse.clone();
    drop(md);
    orelse.iter().any(|&e| {
        let g = GNode { m: parent.m, n: e };
        g == stmt || eng.parent_of(g, stmt)
    })
}

/// utils.is_typing_member (utils.py:2020-2040)
fn is_typing_member(cx: &mut WalkCx, node: GNode, names_to_check: &[&str]) -> bool {
    let eng = cx.eng;
    let md = eng.md(node.m);
    match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::Name { name } => {
            let sym = eng.g(&md, *name);
            let local_name = md.tree.s(*name).to_string();
            drop(md);
            let lk = eng.lookup(node, sym);
            let Some(NV::N(first)) = lk.1.first() else { return false };
            let md = eng.md(first.m);
            if let NodeKind::ImportFrom { modname, names, .. } = &md.tree.nodes[first.n.idx()].kind
            {
                if md.tree.s(*modname) == "typing" {
                    // real_name(node.name)
                    for (q, a) in names {
                        let qs = md.tree.s(*q);
                        match a {
                            Some(al) if md.tree.s(*al) == local_name => {
                                return names_to_check.contains(&qs);
                            }
                            None if qs == local_name => {
                                return names_to_check.contains(&qs);
                            }
                            _ => {}
                        }
                    }
                    return false;
                }
            }
            false
        }
        NodeKind::Attribute { expr, attrname, .. } => {
            let attr = md.tree.s(*attrname).to_string();
            let e = GNode { m: node.m, n: *expr };
            drop(md);
            match u::safe_infer(eng, cx.caches, e) {
                Some(Value::Node(g))
                    if u::is_module(eng, g) && eng.md(g.m).name == "typing" =>
                {
                    names_to_check.contains(&attr.as_str())
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// element of a dict-view unpack (visit_for W0644)
enum UnpackVal {
    /// DictItems element: synthetic Tuple(key, value) — always length 2
    Pair,
    Node(GNode),
    Val(Value),
}

impl UnpackVal {
    fn tuple_len(&self) -> Option<usize> {
        match self {
            UnpackVal::Pair => Some(2),
            _ => None,
        }
    }
    /// _get_value_length (variables.py:3123-3138)
    fn length(&self, eng: &Engine) -> usize {
        match self {
            UnpackVal::Pair => 2,
            UnpackVal::Node(g) => {
                if let Some(n) = nodes_to_unpack_len(eng, &Value::Node(*g)) {
                    return n;
                }
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(ConstValue::Str(sv)) => sv.chars().count(),
                    NodeKind::Const(ConstValue::Bytes(b)) => b.len(),
                    NodeKind::Subscript { slice, .. } => {
                        // value_node.slice.{upper,lower,step} Const arithmetic
                        if let NodeKind::Slice { lower, upper, step } =
                            &md.tree.nodes[slice.idx()].kind
                        {
                            let cint = |o: &Option<NodeId>| -> Option<i64> {
                                o.and_then(|n| match &md.tree.nodes[n.idx()].kind {
                                    NodeKind::Const(ConstValue::Int(
                                        pyast::tree::IntValue::Small(i),
                                    )) => Some(*i),
                                    _ => None,
                                })
                            };
                            if let (Some(lo), Some(up)) = (cint(lower), cint(upper)) {
                                let st = cint(step).unwrap_or(1);
                                if st != 0 {
                                    let range = (up - lo) as f64;
                                    return (range / st as f64).ceil() as usize;
                                }
                            }
                        }
                        1
                    }
                    _ => 1,
                }
            }
            UnpackVal::Val(v) => {
                if let Some(n) = nodes_to_unpack_len(eng, v) {
                    return n;
                }
                if let Value::SynthConst(c) = v {
                    match &**c {
                        ConstValue::Str(sv) => return sv.chars().count(),
                        ConstValue::Bytes(b) => return b.len(),
                        _ => {}
                    }
                }
                1
            }
        }
    }
}

/// _nodes_to_unpack for the dict-view For path: per-element values
fn dict_unpack_values(eng: &Engine, v: &Value) -> Option<Vec<UnpackVal>> {
    use pyinfer::value::DictRef;
    match v {
        Value::DictItems(dr) => Some(match &**dr {
            DictRef::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => items.iter().map(|_| UnpackVal::Pair).collect(),
                    _ => Vec::new(),
                }
            }
            DictRef::Synth(items) => items.iter().map(|_| UnpackVal::Pair).collect(),
        }),
        Value::DictKeys(dr) => Some(match &**dr {
            DictRef::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => items
                        .iter()
                        .map(|&(k, _)| UnpackVal::Node(GNode { m: g.m, n: k }))
                        .collect(),
                    _ => Vec::new(),
                }
            }
            DictRef::Synth(items) => {
                items.iter().map(|(k, _)| UnpackVal::Val(k.clone())).collect()
            }
        }),
        Value::DictValues(dr) => Some(match &**dr {
            DictRef::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => items
                        .iter()
                        .map(|&(_, vn)| UnpackVal::Node(GNode { m: g.m, n: vn }))
                        .collect(),
                    _ => Vec::new(),
                }
            }
            DictRef::Synth(items) => {
                items.iter().map(|(_, vv)| UnpackVal::Val(vv.clone())).collect()
            }
        }),
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                // Dict.itered() == the KEYS
                NodeKind::Dict { items } => Some(
                    items
                        .iter()
                        .map(|&(k, _)| UnpackVal::Node(GNode { m: g.m, n: k }))
                        .collect(),
                ),
                _ => None,
            }
        }
        Value::SynthDict { items } => Some(
            items.iter().map(|(k, _)| UnpackVal::Val(k.clone())).collect(),
        ),
        _ => None,
    }
}

/// _store_type_annotation_node over a standalone annotation-string tree
fn store_type_annotation_tree(
    tree: &pyast::tree::Tree,
    node: pyast::NodeId,
    out: &mut Vec<String>,
) {
    match &tree.nodes[node.idx()].kind {
        NodeKind::Name { name } => out.push(tree.s(*name).to_string()),
        NodeKind::Attribute { expr, .. } => store_type_annotation_tree(tree, *expr, out),
        NodeKind::Subscript { value, .. } => {
            if let NodeKind::Attribute { expr, .. } = &tree.nodes[value.idx()].kind {
                if let NodeKind::Name { name } = &tree.nodes[expr.idx()].kind {
                    if tree.s(*name) == "typing" {
                        out.push("typing".to_string());
                        return;
                    }
                }
            }
            // all Name descendants (preorder)
            let mut stack = vec![node];
            let mut order: Vec<pyast::NodeId> = Vec::new();
            while let Some(n) = stack.pop() {
                order.push(n);
                for &c in tree.children(n).iter().rev() {
                    stack.push(c);
                }
            }
            for n in order {
                if let NodeKind::Name { name } = &tree.nodes[n.idx()].kind {
                    out.push(tree.s(*name).to_string());
                }
            }
        }
        _ => {}
    }
}

/// astroid as_string of a from_elements container holding only consts
/// (EvaluatedObject elements are unrenderable here -> None)
fn synth_container_as_string(v: &Value) -> Option<String> {
    let (open, close, elems): (&str, &str, &std::rc::Rc<Vec<Value>>) = match v {
        Value::SynthSeq { kind, elems } => match kind {
            pyinfer::value::SeqKind::Tuple => ("(", ")", elems),
            pyinfer::value::SeqKind::List => ("[", "]", elems),
            pyinfer::value::SeqKind::Set => ("{", "}", elems),
        },
        _ => return None,
    };
    let mut parts: Vec<String> = Vec::new();
    for e in elems.iter() {
        match e {
            Value::SynthConst(c) => parts.push(pyinfer::asstr::const_repr(c.as_ref())),
            _ => return None,
        }
    }
    let inner = parts.join(", ");
    if matches!(v, Value::SynthSeq { kind: pyinfer::value::SeqKind::Tuple, .. })
        && parts.len() == 1
    {
        return Some(format!("({inner}, )"));
    }
    Some(format!("{open}{inner}{close}"))
}

/// dummy-variables-rgx default:
/// `_+$|(_[a-zA-Z0-9_]*[a-zA-Z0-9]+?$)|dummy|^ignored_|^unused_` (re.match)
fn dummy_rgx_match(name: &str) -> bool {
    if !name.is_empty() && name.chars().all(|c| c == '_') {
        return true;
    }
    if name.starts_with('_')
        && name.len() >= 2
        && name
            .chars()
            .skip(1)
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.chars().last().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false)
    {
        return true;
    }
    name.starts_with("dummy") || name.starts_with("ignored_") || name.starts_with("unused_")
}

/// astroid ModuleModel attributes (interpreter/objectmodel.py) — probed on
/// the pinned venv: ObjectModel base + module attrs. NOTE `builtins` (no
/// dunder) and no `__qualname__`.
const MODULE_SPECIAL_ATTRIBUTES: &[&str] = &[
    "__cached__", "__dict__", "__doc__", "__file__", "__init__", "__loader__",
    "__name__", "__new__", "__package__", "__path__", "__spec__", "builtins",
];

/// _should_ignore_redefined_builtin (variables.py:3002-3005);
/// redefining-builtins-modules default config list.
fn should_ignore_redefined_builtin(eng: &Engine, stmt: GNode) -> bool {
    const MODS: &[&str] = &["six.moves", "past.builtins", "future.builtins", "builtins", "io"];
    let md = eng.md(stmt.m);
    matches!(&md.tree.nodes[stmt.n.idx()].kind,
        NodeKind::ImportFrom { modname, .. } if MODS.contains(&md.tree.s(*modname)))
}

/// ignored-argument-names default `_.*|^ignored_|^unused_` (re.match)
fn ignored_argument_match(name: &str) -> bool {
    name.starts_with('_') || name.starts_with("ignored_") || name.starts_with("unused_")
}

/// _is_name_ignored (variables.py:2874-2888)
fn is_name_ignored(eng: &Engine, stmt: GNode, name: GSym) -> bool {
    let name_str = eng.sname(name);
    let is_arg = eng.kind_is(stmt, |k| matches!(k, NodeKind::Arguments(_)))
        || (eng.kind_is(stmt, |k| matches!(k, NodeKind::AssignName { .. }))
            && eng
                .parent(stmt)
                .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::Arguments(_))))
                .unwrap_or(false));
    if is_arg {
        ignored_argument_match(&name_str)
    } else {
        dummy_rgx_match(&name_str)
    }
}

/// utils.is_reassigned_after_current (utils.py:1876-1912)
fn is_reassigned_after_current(eng: &Engine, node: GNode, name: GSym) -> bool {
    let node_scope = eng.scope(node);
    let node_lineno = eng.fromlineno(node);
    for a in u::preorder(eng, node_scope) {
        let md = eng.md(a.m);
        let (a_name, a_line, is_def) = match &md.tree.nodes[a.n.idx()].kind {
            NodeKind::AssignName { name: n } => (eng.g(&md, *n), eng.fromlineno(a), false),
            NodeKind::ClassDef(d) => (eng.g(&md, d.name), 0, true),
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                (eng.g(&md, d.name), 0, true)
            }
            _ => continue,
        };
        drop(md);
        // .lineno: raw attribute (first-decorator line for decorated defs)
        let a_line = if is_def { raw_lineno(eng, a) } else { a_line };
        if a_name == name && a_line > node_lineno {
            // _is_node_in_same_scope
            let same = if is_def {
                eng.parent(a).map(|p| eng.scope(p)) == Some(node_scope)
            } else {
                eng.scope(a) == node_scope
            };
            if same {
                return true;
            }
        }
    }
    false
}

/// utils.is_deleted_after_current (utils.py:1914-1922)
fn is_deleted_after_current(eng: &Engine, node: GNode, name: GSym) -> bool {
    let node_scope = eng.scope(node);
    let node_lineno = eng.fromlineno(node);
    for d in u::preorder(eng, node_scope) {
        let md = eng.md(d.m);
        let NodeKind::Delete { targets } = &md.tree.nodes[d.n.idx()].kind else {
            continue;
        };
        let targets = targets.clone();
        drop(md);
        for t in targets {
            let tg = GNode { m: d.m, n: t };
            if name_of(eng, tg) == Some(name) && eng.fromlineno(tg) > node_lineno {
                return true;
            }
        }
    }
    false
}

/// dummy_rgx_match for sibling checkers (W0221 parameter comparison)
pub fn dummy_rgx_match_pub(name: &str) -> bool {
    dummy_rgx_match(name)
}

/// FunctionDef.is_abstract(pass_is_abstract=True) for sibling checkers
pub fn func_is_abstract_pub(cx: &mut WalkCx, func: GNode) -> bool {
    func_is_abstract(cx, func)
}

/// SPECIAL_OBJ regex `^_{2}[a-z]+_{2}$` (re.search == fullmatch here)
fn special_obj_match(name: &str) -> bool {
    name.len() > 4
        && name.starts_with("__")
        && name.ends_with("__")
        && name[2..name.len() - 2]
            .chars()
            .all(|c| c.is_ascii_lowercase())
        && !name[2..name.len() - 2].is_empty()
}

/// _is_from_future_import (variables.py:88-98)
fn is_from_future_import(eng: &Engine, stmt: GNode, name: &str) -> bool {
    let modname: String = {
        let md = eng.md(stmt.m);
        match &md.tree.nodes[stmt.n.idx()].kind {
            NodeKind::ImportFrom { modname, .. } => md.tree.s(*modname).to_string(),
            _ => return false,
        }
    };
    let Ok(mid) = eng.do_import_module(stmt, Some(&modname)) else {
        return false;
    };
    let sym = eng.sym(name);
    let md = eng.md(mid);
    let locals = md.locals.borrow();
    let Some(vals) = locals.get(&pyast::NodeId::MODULE) else { return false };
    let Some(list) = vals.get(&sym) else { return false };
    list.iter().any(|&g| {
        let gm = eng.md(g.m);
        matches!(&gm.tree.nodes[g.n.idx()].kind,
            NodeKind::ImportFrom { modname, .. }
                if gm.tree.s(*modname) == "__future__")
    })
}

/// astroid raw `.lineno` attribute: decorated FunctionDef/ClassDef carry
/// the FIRST DECORATOR's line (rebuilder.py:1130-1139), unlike fromlineno
/// (position-based def/class line).
fn raw_lineno(eng: &Engine, g: GNode) -> u32 {
    let md = eng.md(g.m);
    let dec = match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.decorators,
        NodeKind::ClassDef(d) => d.decorators,
        _ => None,
    };
    if let Some(dn) = dec {
        if let NodeKind::Decorators { nodes } = &md.tree.nodes[dn.idx()].kind {
            if let Some(&first) = nodes.first() {
                return md.tree.nodes[first.idx()].fromlineno;
            }
        }
    }
    drop(md);
    eng.fromlineno(g)
}
