//! BasicErrorChecker (checkers/base/basic_error_checker.py) — E0100/E0101/
//! E0102/E0103/E0104/E0105/E0107/E0108/E0112/E0113/E0114/E0115/E0117/E0118 —
//! and BasicChecker (checkers/base/basic_checker.py) — E0111/E0119 (+ the
//! W0101/W0134-family syntactic computations that feed I0021 bookkeeping).
//!
//! E0106 (maxversion 3.3) and E0110 (disabled by flags) are skipped; the
//! visit_call abstract-class path is NOT registered under the target flags
//! (walk_order.rs has no BasicErrorChecker.visit_call).

use pyast::tree::NodeKind;
use pyast::NodeId;
use pyinfer::ctx::Ctx;
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, Value};

use crate::ckutils as u;
use crate::classes::is_method;
use crate::typecheck::{decorated_with, is_overload_stub};
use crate::walker::WalkCx;

fn is_funcdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)))
}
fn is_classdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_)))
}
fn is_lambda(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::Lambda(_)))
}

/// astroid nodes_of_class(klass, skip_klass): root yields itself if matching;
/// children matching skip are pruned (subtree excluded).
pub fn nodes_of_class<P, S>(eng: &Engine, root: GNode, pred: P, skip: S) -> Vec<GNode>
where
    P: Fn(&NodeKind) -> bool,
    S: Fn(&NodeKind) -> bool,
{
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut first = true;
    while let Some(g) = stack.pop() {
        let md = eng.md(g.m);
        let k = &md.tree.nodes[g.n.idx()].kind;
        if !first && skip(k) {
            continue;
        }
        if pred(k) {
            out.push(g);
        }
        first = false;
        let children = md.tree.children(g.n);
        // preserve preorder: push children reversed
        for &c in children.iter().rev() {
            stack.push(GNode { m: g.m, n: c });
        }
    }
    out
}

/// FunctionDef.is_generator(): yields not nested under inner functions or
/// lambdas (intersection of skip-lambdas and skip-functions walks).
fn is_generator(eng: &Engine, func: GNode) -> bool {
    !nodes_of_class(
        eng,
        func,
        |k| matches!(k, NodeKind::Yield { .. } | NodeKind::YieldFrom { .. }),
        |k| {
            matches!(
                k,
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::Lambda(_)
            )
        },
    )
    .is_empty()
}

/// utils.is_none: python None (here: absent) or Const(None)
fn is_none_node(eng: &Engine, g: Option<GNode>) -> bool {
    match g {
        None => true,
        Some(g) => eng.kind_is(g, |k| {
            matches!(k, NodeKind::Const(pyast::tree::ConstValue::None))
        }),
    }
}

/// `Arguments.arguments` property: posonlyargs + args + [vararg node] +
/// kwonlyargs + [kwarg node] (AssignName nodes).
fn all_argument_nodes(eng: &Engine, func: GNode) -> Vec<GNode> {
    let md = eng.md(func.m);
    let args_id = match &md.tree.nodes[func.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
        NodeKind::Lambda(d) => d.args,
        _ => return Vec::new(),
    };
    let NodeKind::Arguments(a) = &md.tree.nodes[args_id.idx()].kind else {
        return Vec::new();
    };
    let mut out: Vec<NodeId> = Vec::new();
    out.extend(a.posonlyargs.iter().copied());
    out.extend(a.args.iter().copied());
    if let Some(v) = a.vararg_node {
        out.push(v);
    }
    out.extend(a.kwonlyargs.iter().copied());
    if let Some(kw) = a.kwarg_node {
        out.push(kw);
    }
    out.into_iter().map(|n| GNode { m: func.m, n }).collect()
}

fn arg_name(eng: &Engine, g: GNode) -> Option<String> {
    eng.node_name(g)
}

/// redefined_by_decorator (basic_error_checker.py:79-95)
fn redefined_by_decorator(eng: &Engine, node: GNode) -> bool {
    let name = eng.node_name(node);
    for dn in crate::typecheck::decorator_nodes_pub(eng, node) {
        let md = eng.md(dn.m);
        if let NodeKind::Attribute { expr, .. } = &md.tree.nodes[dn.n.idx()].kind {
            // getattr(decorator.expr, "name", None) == node.name
            let expr = GNode { m: dn.m, n: *expr };
            drop(md);
            if let NodeKind::Name { name: n } | NodeKind::AssignName { name: n } =
                &eng.md(expr.m).tree.nodes[expr.n.idx()].kind
            {
                let nm = {
                    let md2 = eng.md(expr.m);
                    md2.tree.s(*n).to_string()
                };
                if Some(nm) == name {
                    return true;
                }
            }
        }
    }
    false
}

/// decorator shape `@f.register` / `@f.register(...)` -> the `f` expr node
fn extract_register_target(eng: &Engine, dec: GNode) -> Option<GNode> {
    let md = eng.md(dec.m);
    let func_part = match &md.tree.nodes[dec.n.idx()].kind {
        NodeKind::Call { func, .. } => GNode { m: dec.m, n: *func },
        _ => dec,
    };
    if let NodeKind::Attribute { expr, attrname, .. } = &md.tree.nodes[func_part.n.idx()].kind {
        if md.tree.s(*attrname) == "register" {
            return Some(GNode { m: dec.m, n: *expr });
        }
    }
    None
}

/// utils.is_registered_in_singledispatch_function (utils.py:1515-1546)
pub fn is_registered_in_singledispatch_function(eng: &Engine, cx: &WalkCx, node: GNode) -> bool {
    if !is_funcdef(eng, node) {
        return false;
    }
    let _ = cx;
    for dec in crate::typecheck::decorator_nodes_pub(eng, node) {
        let Some(target) = extract_register_target(eng, dec) else {
            continue;
        };
        // next(func.expr.infer()) — single pull; InferenceError -> continue
        let mut first: Option<Value> = None;
        let _end = eng.infer_to(target, &Ctx::new(), &mut |v| {
            first = Some(v);
            pyinfer::value::Drive::Stop
        });
        match first {
            Some(Value::Node(g)) if is_funcdef(eng, g) => {
                return decorated_with(
                    eng,
                    g,
                    &["functools.singledispatch", "singledispatch.singledispatch"],
                );
            }
            _ => continue,
        }
    }
    false
}

/// _is_singledispatchmethod_registration (basic_error_checker.py:138-160)
fn is_singledispatchmethod_registration(eng: &Engine, cx: &WalkCx, node: GNode) -> bool {
    for dec in crate::typecheck::decorator_nodes_pub(eng, node) {
        let Some(target) = extract_register_target(eng, dec) else {
            continue;
        };
        // _inferred_has_singledispatchmethod: safe_infer(target)
        let inferred = u::safe_infer(eng, cx.caches, target);
        let Some(Value::Node(g)) = inferred else { continue };
        if !is_funcdef(eng, g) {
            continue;
        }
        for d in crate::typecheck::decorator_nodes_pub(eng, g) {
            if let Some(v) = u::safe_infer(eng, cx.caches, d) {
                if eng.value_qname(&v).as_deref() == Some("functools.singledispatchmethod") {
                    return true;
                }
            }
        }
    }
    false
}

#[derive(Default)]
pub struct BasicErrCk;

impl BasicErrCk {
    /// visit_classdef — E0102 only
    pub fn visit_classdef(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_redefinition(cx, "class", node);
    }

    /// visit_functiondef / visit_asyncfunctiondef
    pub fn visit_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        self.check_nonlocal_and_global(cx, node);
        self.check_name_used_prior_global(cx, node);
        if !redefined_by_decorator(eng, node)
            && !is_registered_in_singledispatch_function(eng, cx, node)
        {
            let redeftype = if is_method(eng, node) { "method" } else { "function" };
            self.check_redefinition(cx, redeftype, node);
        }
        // E0100 / E0101
        if is_method(eng, node) && eng.node_name(node).as_deref() == Some("__init__") {
            if is_generator(eng, node) {
                cx.emit_node(
                    "E0100",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    "__init__ method is a generator".into(),
                );
            } else {
                let returns = nodes_of_class(
                    eng,
                    node,
                    |k| matches!(k, NodeKind::Return { .. }),
                    |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::ClassDef(_)),
                );
                let any_non_none = returns.iter().any(|&r| {
                    let md = eng.md(r.m);
                    let NodeKind::Return { value } = &md.tree.nodes[r.n.idx()].kind else {
                        return false;
                    };
                    let v = value.map(|n| GNode { m: r.m, n });
                    drop(md);
                    // `any(v for v in values if not is_none(v))` — None
                    // (bare return) is falsy and never counted anyway
                    match v {
                        None => false,
                        Some(g) => !is_none_node(eng, Some(g)),
                    }
                });
                if any_non_none {
                    cx.emit_node(
                        "E0101",
                        u::msg_line(eng, node),
                        u::msg_col(eng, node),
                        "Explicit return in __init__".into(),
                    );
                }
            }
        }
        // E0108 duplicate-argument-name
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for arg in all_argument_nodes(eng, node) {
            let Some(name) = arg_name(eng, arg) else { continue };
            if seen.contains(&name) {
                cx.emit_node(
                    "E0108",
                    u::lineno(eng, arg),
                    u::col_offset(eng, arg) as i64,
                    u::format_template(
                        "Duplicate argument name %r in function definition",
                        &[&name],
                    ),
                );
            } else {
                seen.insert(name);
            }
        }
    }

    /// _check_name_used_prior_global (E0118)
    fn check_name_used_prior_global(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // {name: global_stmt} — later Global stmts overwrite earlier
        let mut scope_globals: std::collections::HashMap<String, GNode> =
            std::collections::HashMap::new();
        for g in nodes_of_class(eng, node, |k| matches!(k, NodeKind::Global { .. }), |_| false) {
            if eng.scope(g) != node {
                continue;
            }
            let md = eng.md(g.m);
            if let NodeKind::Global { names } = &md.tree.nodes[g.n.idx()].kind {
                for &s in names {
                    scope_globals.insert(md.tree.s(s).to_string(), g);
                }
            }
        }
        if scope_globals.is_empty() {
            return;
        }
        for name_node in
            nodes_of_class(eng, node, |k| matches!(k, NodeKind::Name { .. }), |_| false)
        {
            if eng.scope(name_node) != node {
                continue;
            }
            let Some(nm) = eng.node_name(name_node) else { continue };
            let Some(&gstmt) = scope_globals.get(&nm) else { continue };
            let glineno = u::lineno(eng, gstmt);
            if glineno > 0 && glineno > u::lineno(eng, name_node) {
                cx.emit_node(
                    "E0118",
                    u::lineno(eng, name_node),
                    u::col_offset(eng, name_node) as i64,
                    u::format_template("Name %r is used prior to global declaration", &[&nm]),
                );
            }
        }
    }

    /// _check_nonlocal_and_global (E0115)
    fn check_nonlocal_and_global(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let collect = |want_nonlocal: bool| -> indexmap::IndexSet<String> {
            let mut out = indexmap::IndexSet::new();
            for g in nodes_of_class(
                eng,
                node,
                |k| {
                    if want_nonlocal {
                        matches!(k, NodeKind::Nonlocal { .. })
                    } else {
                        matches!(k, NodeKind::Global { .. })
                    }
                },
                |_| false,
            ) {
                if eng.scope(g) != node {
                    continue;
                }
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Nonlocal { names } | NodeKind::Global { names } => {
                        for &s in names {
                            out.insert(md.tree.s(s).to_string());
                        }
                    }
                    _ => {}
                }
            }
            out
        };
        let nonlocals = collect(true);
        if nonlocals.is_empty() {
            return;
        }
        let globals_ = collect(false);
        for name in nonlocals.iter() {
            if globals_.contains(name) {
                cx.emit_node(
                    "E0115",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    u::format_template("Name %r is nonlocal and global", &[name]),
                );
            }
        }
    }

    /// _check_redefinition (E0102)
    fn check_redefinition(&mut self, cx: &mut WalkCx, redeftype: &str, node: GNode) {
        let eng = cx.eng;
        let Some(parent) = eng.parent(node) else { return };
        let parent_frame = eng.frame(parent);
        let Some(name) = eng.node_name(node) else { return };
        let name_sym = eng.sym(&name);
        let locs: Vec<GNode> = {
            let md = eng.md(parent_frame.m);
            let l = md.locals.borrow();
            l.get(&parent_frame.n)
                .and_then(|m| m.get(&name_sym).cloned())
                .unwrap_or_default()
        };
        // filter AnnAssign-simple stubs
        let redefinitions: Vec<GNode> = locs
            .into_iter()
            .filter(|&i| {
                let Some(p) = eng.parent(i) else { return true };
                let md = eng.md(p.m);
                !matches!(&md.tree.nodes[p.n.idx()].kind,
                    NodeKind::AnnAssign { simple, .. } if *simple)
            })
            .collect();
        // astroid ClassDef construction injects implicit locals FIRST
        // (__module__/__qualname__ Const + __annotations__ Unknown,
        // scoped_nodes.py:1911-1933 + objectmodel ClassModel): for those
        // names defined_self is the implicit node — never `node`, never an
        // overload stub, and never exclusive with it (the implicit Const is
        // a direct child of the ClassDef, so are_exclusive's common
        // ancestor is the ClassDef — not an If/Try branch).
        let implicit_first = is_classdef(eng, parent_frame)
            && matches!(name.as_str(), "__module__" | "__qualname__" | "__annotations__");
        let defined_self: Option<GNode> = if implicit_first {
            None // the implicit class local
        } else {
            let d = redefinitions
                .iter()
                .copied()
                .find(|&l| !is_overload_stub(cx.caches, eng, l))
                .unwrap_or(node);
            if d == node || u::are_exclusive(eng, node, d) {
                return;
            }
            Some(d)
        };
        if is_classdef(eng, parent_frame) && name == "__module__" {
            return; // REDEFINABLE_METHODS
        }
        if is_singledispatchmethod_registration(eng, cx, node) {
            return;
        }
        if is_overload_stub(cx.caches, eng, node) {
            return;
        }
        // "if not <func>:" / "if <func> is None:" guards
        if let Some(p) = eng.parent(node) {
            let md = eng.md(p.m);
            if let NodeKind::If { test, .. } = &md.tree.nodes[p.n.idx()].kind {
                let test = *test;
                match &md.tree.nodes[test.idx()].kind {
                    NodeKind::UnaryOp { op, operand } if &**op == "not" => {
                        if let NodeKind::Name { name: n } = &md.tree.nodes[operand.idx()].kind {
                            if md.tree.s(*n) == name {
                                return;
                            }
                        }
                    }
                    NodeKind::Compare { left, ops } if ops.len() == 1 && &*ops[0].0 == "is" => {
                        let is_none = matches!(
                            &md.tree.nodes[ops[0].1.idx()].kind,
                            NodeKind::Const(pyast::tree::ConstValue::None)
                        );
                        if is_none {
                            if let NodeKind::Name { name: n } = &md.tree.nodes[left.idx()].kind {
                                if md.tree.s(*n) == name {
                                    return;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // forward-reference exemption
        if let Some(idx) = redefinitions.iter().position(|&r| r == node) {
            for &redefinition in &redefinitions[..idx] {
                let inferred = u::safe_infer(eng, cx.caches, redefinition);
                if let Some(v) = &inferred {
                    if matches!(v, Value::Inst { .. } | Value::ExcInst { .. }) {
                        if let Some(q) = eng.value_qname(v) {
                            if q == "typing.ForwardRef" || q == "annotationlib.ForwardRef" {
                                return;
                            }
                        }
                    }
                }
            }
        }
        // message arg: defined_self.fromlineno. For the implicit class
        // local: Const has no lineno -> _fixed_source_line falls back to
        // the parent ClassDef's RAW lineno (the class keyword line, no
        // decorator-quirk adjustment).
        let defined_line = match defined_self {
            Some(d) => u::lineno(eng, d),
            None => {
                let md = eng.md(parent_frame.m);
                match md.tree.positions.get(&parent_frame.n) {
                    Some(&(l, _)) => l,
                    None => {
                        drop(md);
                        eng.fromlineno(parent_frame)
                    }
                }
            }
        };
        cx.emit_node(
            "E0102",
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            u::format_template(
                "%s already defined line %s",
                &[redeftype, &defined_line.to_string()],
            ),
        );
    }

    /// visit_assign — E0112 / E0113
    pub fn visit_assign(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let NodeKind::Assign { targets, .. } = &md.tree.nodes[node.n.idx()].kind else {
            return;
        };
        let Some(&t0) = targets.first() else { return };
        let target = GNode { m: node.m, n: t0 };
        match &md.tree.nodes[t0.idx()].kind {
            NodeKind::Starred { .. } => {
                drop(md);
                cx.emit_node(
                    "E0113",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    "Starred assignment target must be in a list or tuple".into(),
                );
            }
            NodeKind::Tuple { .. } => {
                drop(md);
                if too_many_starred_for_tuple(eng, target) {
                    cx.emit_node(
                        "E0112",
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        "More than one starred expression in assignment".into(),
                    );
                }
            }
            _ => {}
        }
    }

    /// visit_starred — E0114
    pub fn visit_starred(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some(parent) = eng.parent(node) else { return };
        if eng.kind_is(parent, |k| {
            matches!(
                k,
                NodeKind::Call { .. }
                    | NodeKind::List { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::Set { .. }
                    | NodeKind::Dict { .. }
            )
        }) {
            return;
        }
        let Some(stmt) = eng.statement(node) else { return };
        let md = eng.md(stmt.m);
        let NodeKind::Assign { value, .. } = &md.tree.nodes[stmt.n.idx()].kind else {
            return;
        };
        let value = GNode { m: stmt.m, n: *value };
        drop(md);
        // stmt.value is node or stmt.value.parent_of(node)
        let mut cur = Some(node);
        let mut on_rhs = false;
        while let Some(c) = cur {
            if c == value {
                on_rhs = true;
                break;
            }
            if c == stmt {
                break;
            }
            cur = eng.parent(c);
        }
        if on_rhs {
            cx.emit_node(
                "E0114",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "Can use starred expression only in assignment target".into(),
            );
        }
    }

    /// visit_return — E0104
    pub fn visit_return(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if !is_funcdef(eng, eng.frame(node)) {
            cx.emit_node(
                "E0104",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "Return outside function".into(),
            );
        }
    }

    /// visit_yield / visit_yieldfrom — E0105
    pub fn visit_yield(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let frame = eng.frame(node);
        if !is_funcdef(eng, frame) && !is_lambda(eng, frame) {
            cx.emit_node(
                "E0105",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "Yield outside function".into(),
            );
        }
    }

    /// visit_continue / visit_break — E0103 (+ W0136/W0137 in-finally)
    pub fn visit_loopkw(&mut self, cx: &mut WalkCx, node: GNode, node_name: &str) {
        let eng = cx.eng;
        let mut cur = node;
        while let Some(parent) = eng.parent(cur) {
            let md = eng.md(parent.m);
            match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::For(d) | NodeKind::AsyncFor(d) => {
                    // `node not in parent.orelse` — DIRECT membership of the
                    // ORIGINAL node only
                    if !d.orelse.iter().any(|&o| o == node.n && parent.m == node.m) {
                        return;
                    }
                }
                NodeKind::While { orelse, .. } => {
                    if !orelse.iter().any(|&o| o == node.n && parent.m == node.m) {
                        return;
                    }
                }
                NodeKind::ClassDef(_) | NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => {
                    break;
                }
                NodeKind::Try(d) | NodeKind::TryStar(d) => {
                    // W0136/W0137 (continue/break-in-finally): direct membership
                    if d.finalbody.iter().any(|&f| f == node.n && parent.m == node.m) {
                        let w = if node_name == "continue" { "W0136" } else { "W0137" };
                        let text = if node_name == "continue" {
                            "'continue' discouraged inside 'finally' clause"
                        } else {
                            "'break' discouraged inside 'finally' clause"
                        };
                        drop(md);
                        cx.emit_node(
                            w,
                            u::lineno(eng, node),
                            u::col_offset(eng, node) as i64,
                            text.into(),
                        );
                        cur = parent;
                        continue;
                    }
                }
                _ => {}
            }
            cur = parent;
        }
        cx.emit_node(
            "E0103",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            u::format_template("%r not properly in loop", &[node_name]),
        );
    }

    /// visit_nonlocal — E0117
    pub fn visit_nonlocal(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let names: Vec<String> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Nonlocal { names } => names
                    .iter()
                    .map(|&s| md.tree.s(s).to_string())
                    .collect(),
                _ => return,
            }
        };
        for name in names {
            self.check_nonlocal_without_binding(cx, node, &name);
        }
    }

    fn check_nonlocal_without_binding(&mut self, cx: &mut WalkCx, node: GNode, name: &str) {
        let eng = cx.eng;
        let node_scope = eng.scope(node);
        let mut current_scope = node_scope;
        let name_sym = eng.sym(name);
        while let Some(parent) = eng.parent(current_scope) {
            if !is_classdef(eng, current_scope) && !is_funcdef(eng, current_scope) {
                cx.emit_node(
                    "E0117",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    u::format_template("nonlocal name %s found without binding", &[name]),
                );
                return;
            }
            let has_local = {
                let md = eng.md(current_scope.m);
                let l = md.locals.borrow();
                l.get(&current_scope.n)
                    .map(|m| m.contains_key(&name_sym))
                    .unwrap_or(false)
            };
            if current_scope == node_scope || !has_local {
                current_scope = eng.scope(parent);
                continue;
            }
            return; // found a binding in an enclosing function/class scope
        }
        if !is_funcdef(eng, current_scope) {
            cx.emit_node(
                "E0117",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                u::format_template("nonlocal name %s found without binding", &[name]),
            );
        }
    }

    /// visit_unaryop — E0107
    pub fn visit_unaryop(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let NodeKind::UnaryOp { op, operand } = &md.tree.nodes[node.n.idx()].kind else {
            return;
        };
        let op = op.to_string();
        let operand = GNode { m: node.m, n: *operand };
        if !(op == "+" || op == "-") {
            return;
        }
        let inner_op = match &md.tree.nodes[operand.n.idx()].kind {
            NodeKind::UnaryOp { op: o, .. } => o.to_string(),
            _ => return,
        };
        let node_col = md.tree.nodes[node.n.idx()].col_offset;
        let operand_col = md.tree.nodes[operand.n.idx()].col_offset;
        drop(md);
        if inner_op == op && node_col + 1 == operand_col {
            let dbl = format!("{op}{op}");
            cx.emit_node(
                "E0107",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                u::format_template("Use of the non-existent %s operator", &[&dbl]),
            );
        }
    }
}

fn too_many_starred_for_tuple(eng: &Engine, assign_tuple: GNode) -> bool {
    let md = eng.md(assign_tuple.m);
    let elts: Vec<NodeId> = match &md.tree.nodes[assign_tuple.n.idx()].kind {
        NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => elts.clone(),
        _ => return false,
    };
    drop(md);
    let mut starred_count = 0;
    for e in elts {
        let g = GNode { m: assign_tuple.m, n: e };
        let md = eng.md(g.m);
        match &md.tree.nodes[e.idx()].kind {
            NodeKind::Tuple { .. } => {
                drop(md);
                // bug-for-bug: recurse into the FIRST nested tuple and RETURN
                return too_many_starred_for_tuple(eng, g);
            }
            NodeKind::Starred { .. } => starred_count += 1,
            _ => {}
        }
    }
    starred_count > 1
}

// ---------------------------------------------------------------------------
// BasicChecker — E0111 bad-reversed-sequence + E0119 misplaced-format-function
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct BasicCk {
    /// BasicChecker._trys stack (visit_try/leave_try, undecorated) — the
    /// W0150 lost-exception gate
    pub trys: Vec<GNode>,
}

impl BasicCk {
    /// visit_call (basic_checker.py:689-712)
    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // is_terminating_func burn + W0101 unreachable (syntactic, skipped:
        // the unreachable add_message is W and needs next_sibling only)
        if u::is_terminating_func(eng, cx.caches, node) {
            self.check_unreachable(cx, node);
        }
        self.check_misplaced_format_function(cx, node);
        let md = eng.md(node.m);
        let NodeKind::Call { func, .. } = &md.tree.nodes[node.n.idx()].kind else {
            return;
        };
        let func = GNode { m: node.m, n: *func };
        let name = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::Name { name } => md.tree.s(*name).to_string(),
            _ => return,
        };
        drop(md);
        // `name in node.frame() or name in node.root()` — locals membership
        let frame = eng.frame(node);
        let name_sym = eng.sym(&name);
        let in_frame = {
            let md = eng.md(frame.m);
            let l = md.locals.borrow();
            l.get(&frame.n).map(|m| m.contains_key(&name_sym)).unwrap_or(false)
        };
        let root = GNode { m: node.m, n: NodeId::MODULE };
        let in_root = {
            let md = eng.md(root.m);
            let l = md.locals.borrow();
            l.get(&root.n).map(|m| m.contains_key(&name_sym)).unwrap_or(false)
        };
        if in_frame || in_root {
            return;
        }
        match name.as_str() {
            "exec" => {
                cx.emit_node(
                    "W0122",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    "Use of exec".into(),
                );
            }
            "reversed" => self.check_reversed(cx, node),
            "eval" => {
                cx.emit_node(
                    "W0123",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    "Use of eval".into(),
                );
            }
            _ => {}
        }
    }

    /// _check_unreachable (basic_checker.py): W0101 at the next sibling
    fn check_unreachable(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node here is a Call used as terminating func; pylint passes the
        // CALL node; unreachable_statement = node.next_sibling()
        if let Some(sib) = u::next_sibling(eng, node) {
            cx.emit_node(
                "W0101",
                u::lineno(eng, sib),
                u::col_offset(eng, sib) as i64,
                "Unreachable code".into(),
            );
        }
    }

    /// _check_misplaced_format_function — E0119
    fn check_misplaced_format_function(&mut self, cx: &mut WalkCx, call_node: GNode) {
        let eng = cx.eng;
        let md = eng.md(call_node.m);
        let NodeKind::Call { func, .. } = &md.tree.nodes[call_node.n.idx()].kind else {
            return;
        };
        let func = GNode { m: call_node.m, n: *func };
        let NodeKind::Attribute { expr, attrname, .. } = &md.tree.nodes[func.n.idx()].kind else {
            return;
        };
        if md.tree.s(*attrname) != "format" {
            return;
        }
        let expr = GNode { m: call_node.m, n: *expr };
        drop(md);
        let inferred = u::safe_infer(eng, cx.caches, expr);
        match inferred {
            Some(v) if v.is_uninferable() => return,
            Some(_) => return, // truthy inferred value -> no message
            None => {}
        }
        // pattern: print(...).format(...)
        let md = eng.md(expr.m);
        if let NodeKind::Call { func: pf, .. } = &md.tree.nodes[expr.n.idx()].kind {
            if let NodeKind::Name { name } = &md.tree.nodes[pf.idx()].kind {
                if md.tree.s(*name) == "print" {
                    drop(md);
                    cx.emit_node(
                        "E0119",
                        u::lineno(eng, call_node),
                        u::col_offset(eng, call_node) as i64,
                        "format function is not called on str".into(),
                    );
                }
            }
        }
    }

    /// _check_reversed — E0111
    fn check_reversed(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // get_argument_from_call(node, position=0)
        let md = eng.md(node.m);
        let NodeKind::Call { args, .. } = &md.tree.nodes[node.n.idx()].kind else {
            return;
        };
        let args: Vec<NodeId> = args.clone();
        drop(md);
        // position=0, no keyword: NoSuchArgumentError when args empty or
        // args[0] is a Starred? get_argument_from_call returns args[position]
        // if len > position; Starred counts as an arg node
        let Some(&a0) = args.first() else { return };
        let arg0 = GNode { m: node.m, n: a0 };
        let argument = u::safe_infer(eng, cx.caches, arg0);
        let emit = |cx: &mut WalkCx| {
            cx.emit_node(
                "E0111",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "The first reversed() argument is not a sequence".into(),
            );
        };
        let argument = match argument {
            Some(v) if v.is_uninferable() => return,
            None => {
                // maybe reversed(iter(...))
                if eng.kind_is(arg0, |k| matches!(k, NodeKind::Call { .. })) {
                    let md = eng.md(arg0.m);
                    let NodeKind::Call { func, .. } = &md.tree.nodes[arg0.n.idx()].kind else {
                        return;
                    };
                    let f = GNode { m: arg0.m, n: *func };
                    drop(md);
                    // next(node.args[0].func.infer())
                    let mut first: Option<Value> = None;
                    let end = eng.infer_to(f, &Ctx::new(), &mut |v| {
                        first = Some(v);
                        pyinfer::value::Drive::Stop
                    });
                    if first.is_none() && matches!(end, pyinfer::value::End::Raised(_)) {
                        return;
                    }
                    if let Some(v) = first {
                        let nm = crate::typecheck::value_name(eng, &v);
                        let is_builtin = match &v {
                            Value::Node(g) => eng.md(g.m).name == "builtins",
                            _ => eng
                                .value_qname(&v)
                                .map(|q| q.starts_with("builtins."))
                                .unwrap_or(false),
                        };
                        if nm.as_deref() == Some("iter") && is_builtin {
                            emit(cx);
                        }
                    }
                }
                return;
            }
            Some(v) => v,
        };
        // List/Tuple ok
        if let Value::Node(g) = &argument {
            if eng.kind_is(*g, |k| matches!(k, NodeKind::List { .. } | NodeKind::Tuple { .. })) {
                return;
            }
        }
        if matches!(argument, Value::SynthSeq { kind: pyinfer::value::SeqKind::List, .. })
            || matches!(argument, Value::SynthSeq { kind: pyinfer::value::SeqKind::Tuple, .. })
        {
            return;
        }
        // hasattr(argument, "getattr"): everything except Uninferable-likes;
        // values without getattr surface (e.g. SynthSlice? it proxies slice).
        // Replicate via value_getattr probing of the protocol groups.
        let groups: [&[&str]; 2] = [&["__getitem__", "__len__"], &["__reversed__"]];
        let mut ok = false;
        'outer: for group in groups {
            for meth in group {
                let sym = eng.sym(meth);
                if crate::typecheck::value_getattr(eng, &argument, sym).is_err() {
                    continue 'outer; // this group fails -> try next
                }
            }
            ok = true;
            break;
        }
        if !ok {
            emit(cx);
        }
    }

    /// visit_try — _trys push + W0134 return-in-finally (undecorated:
    /// registered whenever the BasicChecker is kept)
    pub fn visit_try(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let finalbody: Vec<NodeId> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Try(d) => d.finalbody.clone(),
            _ => return,
        };
        drop(md);
        self.trys.push(node);
        for f in finalbody {
            let fg = GNode { m: node.m, n: f };
            for r in nodes_of_class(eng, fg, |k| matches!(k, NodeKind::Return { .. }), |_| false) {
                cx.emit_node(
                    "W0134",
                    u::lineno(eng, r),
                    u::col_offset(eng, r) as i64,
                    "'return' shadowed by the 'finally' clause.".into(),
                );
            }
        }
    }
}


