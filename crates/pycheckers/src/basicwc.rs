//! checkers/base/ remaining W/C/R messages (full-pylint mode), spec:
//! reference/notes/09-basic-wc.md. Covers BasicChecker's W-codes,
//! BasicErrorChecker W0120, PassChecker W0107, ComparisonChecker
//! C0121/C0123/R0123/R0124/R0133/W0143/W0177, NameChecker C0103-C0105/
//! C0131/C0132, DocStringChecker C0112/C0114-C0116, FunctionChecker W0135.
//!
//! All callbacks are gated by `Prepared` flags computed from config-level
//! message states (prepare_checkers drop + only_required_for_messages);
//! under `-E` every flag is false except the pre-existing visit_call /
//! visit_try paths.

use pyast::tree::{ConstValue, NodeKind};
use pyast::NodeId;
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, NV};
use pyinfer::value::Value;

use crate::basicerr::{nodes_of_class, BasicCk};
use crate::ckutils as u;
use crate::walker::WalkCx;

fn as_string(eng: &Engine, g: GNode) -> String {
    pyinfer::asstr::as_string(eng, g)
}

/// `str(value)` of a python const (R0124 suggestion rendering, C0132 args).
fn const_str(c: &ConstValue) -> String {
    match c {
        ConstValue::Str(s) => s.to_string(),
        _ => pyinfer::asstr::const_repr(c),
    }
}

/// emit helper: position-aware anchor (node.position for Class/FunctionDef,
/// else fromlineno/col_offset)
fn emit(cx: &mut WalkCx, msgid: &'static str, g: GNode, text: String) {
    let line = u::msg_line(cx.eng, g);
    let col = u::msg_col(cx.eng, g);
    // node messages are attributed to node.root() (pylinter.py:1257-1263):
    // anchors can live in a FOREIGN module's tree (e.g. instance_attrs
    // collected from another module's delayed assattr)
    cx.emit_node_rooted(msgid, g, line, col, text);
}

fn is_const_kind(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::Const(_)))
}

fn const_of(eng: &Engine, g: GNode) -> Option<ConstValue> {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Const(c) => Some(c.clone()),
        _ => None,
    }
}

fn name_of(eng: &Engine, g: GNode) -> Option<String> {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Name { name } | NodeKind::AssignName { name } => {
            Some(md.tree.s(*name).to_string())
        }
        _ => None,
    }
}

// ===========================================================================
// BasicChecker (basic_checker.py) — W-codes
// ===========================================================================

impl BasicCk {
    /// visit_assert — W0129 assert-on-string-literal / W0199 assert-on-tuple
    pub fn visit_assert(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let test = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Assert { test, .. } => *test,
                _ => return,
            }
        };
        let md = eng.md(node.m);
        match &md.tree.nodes[test.idx()].kind {
            NodeKind::Tuple { elts, .. } if !elts.is_empty() => {
                drop(md);
                emit(
                    cx,
                    "W0199",
                    node,
                    "Assert called on a populated tuple. Did you mean 'assert x,y'?".into(),
                );
            }
            NodeKind::Const(ConstValue::Str(s)) => {
                let when = if s.is_empty() { "always" } else { "never" };
                drop(md);
                emit(
                    cx,
                    "W0129",
                    node,
                    format!(
                        "Assert statement has a string literal as its first argument. The assert will {when} fail."
                    ),
                );
            }
            _ => {}
        }
    }

    /// visit_assign — W0127 self-assigning-variable + W0128 redeclared
    pub fn visit_assign(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (targets, value) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Assign { targets, value } => (targets.clone(), *value),
                _ => return,
            }
        };
        self.check_self_assigning_variable(cx, node, &targets, value);
        let tg: Vec<GNode> = targets.iter().map(|&n| GNode { m: node.m, n }).collect();
        self.check_redeclared_assign_name(cx, &tg);
    }

    /// visit_for (BasicChecker) — W0128 on the loop target
    pub fn visit_for_basic(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let target = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::For(d) => d.target,
                _ => return,
            }
        };
        self.check_redeclared_assign_name(cx, &[GNode { m: node.m, n: target }]);
    }

    fn check_self_assigning_variable(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        targets: &[NodeId],
        value: NodeId,
    ) {
        let eng = cx.eng;
        let scope = eng.scope(node);
        let scope_is_class = u::is_classdef(eng, scope);
        let mut targets: Vec<NodeId> = targets.to_vec();
        // unpack single tuple target
        let first_is_tuple = {
            let md = eng.md(node.m);
            matches!(md.tree.nodes[targets[0].idx()].kind, NodeKind::Tuple { .. })
        };
        if first_is_tuple {
            if targets.len() != 1 {
                return;
            }
            let md = eng.md(node.m);
            let NodeKind::Tuple { elts, .. } = &md.tree.nodes[targets[0].idx()].kind else {
                return;
            };
            targets = elts.clone();
            drop(md);
            if targets.len() == 1 {
                return;
            }
        }
        let rhs_names: Vec<NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[value.idx()].kind {
                NodeKind::Name { .. } => {
                    if targets.len() != 1 {
                        return;
                    }
                    vec![value]
                }
                NodeKind::Tuple { elts, .. } => {
                    let rhs_count = elts.len();
                    if targets.len() != rhs_count || rhs_count == 1 {
                        return;
                    }
                    elts.clone()
                }
                _ => return,
            }
        };
        for (&t, &r) in targets.iter().zip(rhs_names.iter()) {
            let md = eng.md(node.m);
            let NodeKind::Name { name: rn } = &md.tree.nodes[r.idx()].kind else {
                continue;
            };
            let NodeKind::AssignName { name: tn } = &md.tree.nodes[t.idx()].kind else {
                continue;
            };
            let tname = md.tree.s(*tn).to_string();
            let rname = md.tree.s(*rn).to_string();
            drop(md);
            if scope_is_class {
                // target.name in scope.locals -> exempt
                let sym = eng.sym(&tname);
                let in_locals = {
                    let smd = eng.md(scope.m);
                    let l = smd.locals.borrow();
                    l.get(&scope.n).map(|m| m.contains_key(&sym)).unwrap_or(false)
                };
                if in_locals {
                    continue;
                }
            }
            if tname == rname {
                let tg = GNode { m: node.m, n: t };
                emit(
                    cx,
                    "W0127",
                    tg,
                    format!("Assigning the same variable {} to itself", u::py_repr_str(&tname)),
                );
            }
        }
    }

    /// _check_redeclared_assign_name (basic_checker.py:930-951). Returns
    /// false when a dummy-rgx name aborted the WHOLE check.
    fn check_redeclared_assign_name(&mut self, cx: &mut WalkCx, targets: &[GNode]) -> bool {
        let eng = cx.eng;
        for &target in targets {
            let elts: Vec<NodeId> = {
                let md = eng.md(target.m);
                match &md.tree.nodes[target.n.idx()].kind {
                    NodeKind::Tuple { elts, .. } => elts.clone(),
                    _ => continue,
                }
            };
            let mut found_names: Vec<String> = Vec::new();
            for e in elts {
                let eg = GNode { m: target.m, n: e };
                let is_tuple = eng.kind_is(eg, |k| matches!(k, NodeKind::Tuple { .. }));
                if is_tuple {
                    if !self.check_redeclared_assign_name(cx, &[eg]) {
                        // nested call hit a dummy name: it returned from the
                        // nested invocation only (python `return` in recursion)
                    }
                    continue;
                }
                let Some(name) = name_of(eng, eg) else { continue };
                if !eng.kind_is(eg, |k| matches!(k, NodeKind::AssignName { .. })) {
                    continue;
                }
                if name == "_" {
                    continue;
                }
                if dummy_rgx_match(&name) {
                    return false; // aborts remaining elements AND targets
                }
                found_names.push(name);
            }
            // collections.Counter.most_common(): count desc, insertion order ties
            let mut counts: Vec<(String, usize)> = Vec::new();
            for n in &found_names {
                if let Some(e) = counts.iter_mut().find(|(k, _)| k == n) {
                    e.1 += 1;
                } else {
                    counts.push((n.clone(), 1));
                }
            }
            counts.sort_by(|a, b| b.1.cmp(&a.1)); // stable: ties keep insertion order
            for (name, count) in counts {
                if count > 1 {
                    emit(
                        cx,
                        "W0128",
                        target,
                        format!("Redeclared variable {} in assignment", u::py_repr_str(&name)),
                    );
                }
            }
        }
        true
    }

    /// visit_expr — W0104/W0105/W0106/W0131/W0133 (basic_checker.py:422-494)
    pub fn visit_expr(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let expr = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Expr { value } => *value,
                _ => return,
            }
        };
        let eg = GNode { m: node.m, n: expr };
        // 1. string statement
        if let Some(ConstValue::Str(_)) = const_of(eng, eg) {
            let scope = eng.scope(eg);
            let exempt_scope = {
                let md = eng.md(scope.m);
                match &md.tree.nodes[scope.n.idx()].kind {
                    NodeKind::ClassDef(_) | NodeKind::Module(_) => true,
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                        md.tree.s(d.name) == "__init__"
                    }
                    _ => false,
                }
            };
            if exempt_scope {
                // expr.previous_sibling() -> the Expr stmt's previous sibling
                if let Some(sib) = u::previous_sibling(eng, node) {
                    let sib_scope = eng.scope(sib);
                    let is_assignish = eng.kind_is(sib, |k| {
                        matches!(
                            k,
                            NodeKind::Assign { .. }
                                | NodeKind::AnnAssign { .. }
                                | NodeKind::TypeAlias { .. }
                        )
                    });
                    if sib_scope == scope && is_assignish {
                        return;
                    }
                }
            }
            emit(cx, "W0105", node, "String statement has no effect".into());
            return;
        }
        // 2. bare call: W0133 only
        if eng.kind_is(eg, |k| matches!(k, NodeKind::Call { .. })) {
            let name: String = {
                let md = eng.md(eg.m);
                let NodeKind::Call { func, .. } = &md.tree.nodes[eg.n.idx()].kind else {
                    return;
                };
                match &md.tree.nodes[func.idx()].kind {
                    NodeKind::Name { name } => md.tree.s(*name).to_string(),
                    NodeKind::Attribute { attrname, .. } => md.tree.s(*attrname).to_string(),
                    _ => String::new(),
                }
            };
            let upper = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
            if upper {
                if let Some(Value::ExcInst { .. }) = u::safe_infer(eng, cx.caches, eg) {
                    emit(cx, "W0133", node, "Exception statement has no effect".into());
                }
            }
            return;
        }
        // 3. skip list (YieldFrom subclasses Yield in astroid)
        if eng.kind_is(eg, |k| {
            matches!(k, NodeKind::Yield { .. } | NodeKind::YieldFrom { .. } | NodeKind::Await { .. })
        }) {
            return;
        }
        if let Some(parent) = eng.parent(node) {
            let md = eng.md(parent.m);
            let solo_try_body = match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::Try(d) | NodeKind::TryStar(d) => {
                    d.body.len() == 1 && d.body[0] == node.n
                }
                _ => false,
            };
            if solo_try_body {
                return;
            }
        }
        if let Some(ConstValue::Ellipsis) = const_of(eng, eg) {
            return;
        }
        // 4. NamedExpr
        if eng.kind_is(eg, |k| matches!(k, NodeKind::NamedExpr { .. })) {
            emit(cx, "W0131", node, "Named expression used without context".into());
            return;
        }
        // 5. W0106 vs W0104
        let any_call = !nodes_of_class(eng, eg, |k| matches!(k, NodeKind::Call { .. }), |_| false)
            .is_empty();
        if any_call {
            emit(
                cx,
                "W0106",
                node,
                format!("Expression \"{}\" is assigned to nothing", as_string(eng, eg)),
            );
        } else {
            emit(cx, "W0104", node, "Statement seems to have no effect".into());
        }
    }

    /// visit_lambda — W0108 unnecessary-lambda (basic_checker.py:522-577)
    pub fn visit_lambda(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (args_n, body_n) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Lambda(d) => (d.args, d.body),
                _ => return,
            }
        };
        let (defaults_empty, vararg, kwarg, ordinary): (bool, Option<String>, Option<String>, Vec<String>) = {
            let md = eng.md(node.m);
            let NodeKind::Arguments(a) = &md.tree.nodes[args_n.idx()].kind else {
                return;
            };
            let ordinary: Vec<String> = a
                .args
                .iter()
                .filter_map(|&n| match &md.tree.nodes[n.idx()].kind {
                    NodeKind::AssignName { name } => Some(md.tree.s(*name).to_string()),
                    _ => None,
                })
                .collect();
            // posonly args present -> count mismatch below (they are NOT in
            // args.args); replicate by treating ordinary as args.args only.
            (
                a.defaults.is_empty(),
                a.vararg.map(|s| md.tree.s(s).to_string()),
                a.kwarg.map(|s| md.tree.s(s).to_string()),
                ordinary,
            )
        };
        if !defaults_empty {
            return;
        }
        let (call_func, call_args, call_keywords): (NodeId, Vec<NodeId>, Vec<NodeId>) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[body_n.idx()].kind {
                NodeKind::Call { func, args, keywords } => (*func, args.clone(), keywords.clone()),
                _ => return,
            }
        };
        // chained call `lambda x: foo().method(x)`
        {
            let md = eng.md(node.m);
            if let NodeKind::Attribute { expr, .. } = &md.tree.nodes[call_func.idx()].kind {
                if matches!(md.tree.nodes[expr.idx()].kind, NodeKind::Call { .. }) {
                    return;
                }
            }
        }
        // _has_variadic_argument(args, variadic): not args or any(value not
        // Name(variadic))
        let has_variadic = |values: &[NodeId], variadic: &str| -> bool {
            if values.is_empty() {
                return true;
            }
            let md = eng.md(node.m);
            values.iter().any(|&v| match &md.tree.nodes[v.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name) != variadic,
                _ => true,
            })
        };
        // kwarg / keywords correspondence (keyword VALUES inspected)
        let kw_values: Vec<NodeId> = {
            let md = eng.md(node.m);
            call_keywords
                .iter()
                .map(|&k| match &md.tree.nodes[k.idx()].kind {
                    NodeKind::Keyword { value, .. } => *value,
                    _ => k,
                })
                .collect()
        };
        match &kwarg {
            Some(kw) => {
                if has_variadic(&kw_values, kw) {
                    return;
                }
            }
            None => {
                if !call_keywords.is_empty() {
                    return;
                }
            }
        }
        // vararg / starargs correspondence
        let star_values: Vec<NodeId> = {
            let md = eng.md(node.m);
            call_args
                .iter()
                .filter_map(|&a| match &md.tree.nodes[a.idx()].kind {
                    NodeKind::Starred { value, .. } => Some(*value),
                    _ => None,
                })
                .collect()
        };
        match &vararg {
            Some(va) => {
                if has_variadic(&star_values, va) {
                    return;
                }
            }
            None => {
                if !star_values.is_empty() {
                    return;
                }
            }
        }
        // ordinary args
        let new_call_args: Vec<NodeId> = {
            let md = eng.md(node.m);
            call_args
                .iter()
                .filter_map(|&a| match &md.tree.nodes[a.idx()].kind {
                    NodeKind::Starred { value, .. } => match &md.tree.nodes[value.idx()].kind {
                        NodeKind::Name { name } => {
                            if Some(md.tree.s(*name).to_string()) == vararg {
                                None // dropped
                            } else {
                                Some(a) // the Starred node itself (not a Name
                                        // -> zip below returns)
                            }
                        }
                        _ => None, // non-Name star dropped
                    },
                    _ => Some(a),
                })
                .collect()
        };
        if ordinary.len() != new_call_args.len() {
            return;
        }
        {
            let md = eng.md(node.m);
            for (an, &pn) in ordinary.iter().zip(new_call_args.iter()) {
                match &md.tree.nodes[pn.idx()].kind {
                    NodeKind::Name { name } => {
                        if md.tree.s(*name) != an.as_str() {
                            return;
                        }
                    }
                    _ => return,
                }
            }
        }
        // func-uses-param: any Name inside call.func resolving to the lambda
        let fg = GNode { m: node.m, n: call_func };
        for nm in nodes_of_class(eng, fg, |k| matches!(k, NodeKind::Name { .. }), |_| false) {
            if let Some(nstr) = name_of(eng, nm) {
                let sym = eng.sym(&nstr);
                let res = eng.lookup(nm, sym);
                if res.0 == node {
                    return;
                }
            }
        }
        cx.emit_node(
            "W0108",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            "Lambda may not be necessary".into(),
        );
    }

    /// visit_dict — W0109 duplicate-key
    pub fn visit_dict(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let items: Vec<(NodeId, NodeId)> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Dict { items } => items.clone(),
                _ => return,
            }
        };
        let mut keys: Vec<u::PyKey> = Vec::new();
        for (k, _) in items {
            let kg = GNode { m: node.m, n: k };
            let (key, repr): (u::PyKey, String) = {
                let md = eng.md(node.m);
                match &md.tree.nodes[k.idx()].kind {
                    NodeKind::Const(c) => (u::py_key(c), pyinfer::asstr::const_repr(c)),
                    NodeKind::Attribute { .. } => {
                        drop(md);
                        let s = as_string(eng, kg);
                        (u::py_key(&ConstValue::Str(s.clone().into())), u::py_repr_str(&s))
                    }
                    _ => continue,
                }
            };
            if keys.contains(&key) {
                emit(cx, "W0109", node, format!("Duplicate key {repr} in dictionary"));
            }
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }

    /// visit_set — W0130 duplicate-value
    pub fn visit_set(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let elts: Vec<NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Set { elts } => elts.clone(),
                _ => return,
            }
        };
        let mut values: Vec<u::PyKey> = Vec::new();
        for v in elts {
            let (key, repr) = {
                let md = eng.md(node.m);
                match &md.tree.nodes[v.idx()].kind {
                    NodeKind::Const(c) => (u::py_key(c), pyinfer::asstr::const_repr(c)),
                    _ => continue,
                }
            };
            if values.contains(&key) {
                emit(cx, "W0130", node, format!("Duplicate value {repr} in set"));
            }
            if !values.contains(&key) {
                values.push(key);
            }
        }
    }

    /// visit_with — W0124 confusing-with-statement
    pub fn visit_with(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let items: Vec<(NodeId, Option<NodeId>)> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::With(d) => d.items.clone(),
                _ => return,
            }
        };
        for w in items.windows(2) {
            let (prev, cur) = (&w[0], &w[1]);
            let prev_is_assignname = prev
                .1
                .map(|n| {
                    let md = eng.md(node.m);
                    matches!(md.tree.nodes[n.idx()].kind, NodeKind::AssignName { .. })
                })
                .unwrap_or(false);
            let cur_no_binding = cur.1.is_none();
            let cur_not_call = {
                let md = eng.md(node.m);
                !matches!(md.tree.nodes[cur.0.idx()].kind, NodeKind::Call { .. })
            };
            if prev_is_assignname && cur_no_binding && cur_not_call {
                emit(
                    cx,
                    "W0124",
                    node,
                    "Following \"as\" with another context manager looks like a tuple.".into(),
                );
            }
        }
    }

    /// visit_if / visit_ifexp — W0125/W0126
    pub fn visit_if_test(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let test = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::If { test, .. } | NodeKind::IfExp { test, .. } => *test,
                _ => return,
            }
        };
        self.check_using_constant_test(cx, node, GNode { m: node.m, n: test });
    }

    /// visit_comprehension — W0125/W0126 per if-clause
    pub fn visit_comprehension(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let ifs: Vec<NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Comprehension { ifs, .. } => ifs.clone(),
                _ => return,
            }
        };
        for t in ifs {
            self.check_using_constant_test(cx, node, GNode { m: node.m, n: t });
        }
    }

    fn check_using_constant_test(&mut self, cx: &mut WalkCx, node: GNode, test: GNode) {
        let eng = cx.eng;
        let is_struct_or_const = eng.kind_is(test, |k| {
            matches!(
                k,
                NodeKind::Const(_)
                    | NodeKind::Dict { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::Set { .. }
                    | NodeKind::List { .. }
                    | NodeKind::Module(_)
                    | NodeKind::GeneratorExp(_)
                    | NodeKind::Lambda(_)
                    | NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::ClassDef(_)
            )
        });
        let is_except = eng.kind_is(test, |k| {
            matches!(
                k,
                NodeKind::Call { .. }
                    | NodeKind::BinOp { .. }
                    | NodeKind::BoolOp { .. }
                    | NodeKind::UnaryOp { .. }
                    | NodeKind::Subscript { .. }
            )
        });
        let mut emit_flag = is_struct_or_const;
        let mut inferred: Option<Value> = None;
        let mut maybe_generator_call: Option<GNode> = None;
        if !is_except {
            inferred = u::safe_infer(eng, cx.caches, test);
            if matches!(inferred, Some(Value::Uninferable))
                && eng.kind_is(test, |k| matches!(k, NodeKind::Name { .. }))
            {
                let (e, c) = name_holds_generator(eng, test);
                emit_flag = e;
                maybe_generator_call = c;
            }
        } else if eng.kind_is(test, |k| matches!(k, NodeKind::Call { .. })) {
            maybe_generator_call = Some(test);
        }
        if let Some(call) = maybe_generator_call {
            let func = {
                let md = eng.md(call.m);
                match &md.tree.nodes[call.n.idx()].kind {
                    NodeKind::Call { func, .. } => Some(GNode { m: call.m, n: *func }),
                    _ => None,
                }
            };
            if let Some(func) = func {
                let inferred_call = u::safe_infer(eng, cx.caches, func);
                if let Some(Value::Node(f)) = inferred_call {
                    if u::is_functiondef(eng, f) {
                        let mut all_gen: Option<bool> = None;
                        for ret in eng.return_nodes_skip_functions(f) {
                            let v = {
                                let md = eng.md(ret.m);
                                match &md.tree.nodes[ret.n.idx()].kind {
                                    NodeKind::Return { value } => *value,
                                    _ => None,
                                }
                            };
                            let is_gen = v
                                .map(|vn| {
                                    let md = eng.md(ret.m);
                                    matches!(
                                        md.tree.nodes[vn.idx()].kind,
                                        NodeKind::GeneratorExp(_)
                                    )
                                })
                                .unwrap_or(false);
                            if !is_gen {
                                all_gen = Some(false);
                                break;
                            }
                            all_gen = Some(true);
                        }
                        if all_gen == Some(true) {
                            emit(
                                cx,
                                "W0125",
                                node,
                                "Using a conditional statement with a constant value".into(),
                            );
                            return;
                        }
                    }
                }
            }
        }
        if emit_flag {
            emit(cx, "W0125", test, "Using a conditional statement with a constant value".into());
            return;
        }
        // inferred const_nodes: Module/GeneratorExp/Lambda/FunctionDef/
        // ClassDef nodes, Generator, UnboundMethod, BoundMethod values
        let Some(inf) = inferred else { return };
        let (is_const_value, callable_fn): (bool, Option<Value>) = match &inf {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Module(_) | NodeKind::GeneratorExp(_) | NodeKind::ClassDef(_) => {
                        (true, None)
                    }
                    NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::Lambda(_) => (true, Some(inf.clone())),
                    _ => (false, None),
                }
            }
            Value::Generator { .. } | Value::UnboundMethod { .. } | Value::BoundMethod { .. }
            | Value::DescBM { .. } => (true, None),
            // objects.Property subclasses FunctionDef; its
            // infer_call_result raises InferenceError (not callable) ->
            // W0125 only
            Value::Property { .. } => (true, None),
            _ => (false, None),
        };
        if !is_const_value {
            return;
        }
        let mut call_inferred = false;
        if let Some(fv) = callable_fn {
            // astroid with_metaclass hack: caller (the If node) has no
            // `.args` -> AttributeError -> module crash (F0002). Replicate.
            if let Value::Node(f) = &fv {
                if u::is_functiondef(eng, *f) && !eng.is_generator(*f) {
                    let is_metaclass_shape = {
                        let md = eng.md(f.m);
                        match &md.tree.nodes[f.n.idx()].kind {
                            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                                if md.tree.s(d.name) == "with_metaclass" {
                                    match &md.tree.nodes[d.args.idx()].kind {
                                        NodeKind::Arguments(a) => {
                                            a.args.len() == 1 && a.vararg.is_some()
                                        }
                                        _ => false,
                                    }
                                } else {
                                    false
                                }
                            }
                            _ => false,
                        }
                    };
                    if is_metaclass_shape {
                        cx.crashed.set(true);
                        return;
                    }
                }
            }
            let flow = eng.infer_call_result(&fv, None, None);
            call_inferred = flow.err.is_none() && !flow.vals.is_empty();
        }
        if call_inferred {
            emit(
                cx,
                "W0126",
                test,
                "Using a conditional statement with potentially wrong function or method call due to missing parentheses".into(),
            );
        }
        emit(cx, "W0125", test, "Using a conditional statement with a constant value".into());
    }

    /// visit_functiondef (BasicChecker) — W0102 dangerous-default-value
    pub fn visit_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let args_n = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
                _ => return,
            }
        };
        let defaults: Vec<NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[args_n.idx()].kind {
                NodeKind::Arguments(a) => {
                    let mut v = a.defaults.clone();
                    v.extend(a.kw_defaults.iter().filter_map(|d| *d));
                    v
                }
                _ => return,
            }
        };
        const TABLE: &[(&str, &str)] = &[
            ("builtins.set", "set()"),
            ("builtins.dict", "{}"),
            ("builtins.list", "[]"),
            ("collections.deque", "collections.deque()"),
            ("collections.ChainMap", "collections.ChainMap()"),
            ("collections.Counter", "collections.Counter()"),
            ("collections.OrderedDict", "collections.OrderedDict()"),
            ("collections.defaultdict", "collections.defaultdict()"),
            ("collections.UserDict", "collections.UserDict()"),
            ("collections.UserList", "collections.UserList()"),
        ];
        for d in defaults {
            let dg = GNode { m: node.m, n: d };
            // value = next(default.infer()) — single pull; InferenceError -> skip
            let mut first: Option<Value> = None;
            let end = eng.infer_to(dg, &u::fresh_ctx(), &mut |v| {
                first = Some(v);
                pyinfer::value::Drive::Stop
            });
            let Some(value) = first else {
                let _ = end;
                continue;
            };
            // isinstance(value, astroid.Instance) and qname in table
            let Some(qname) = u::value_qname(eng, &value) else { continue };
            let Some(&(_, symbol)) = TABLE.iter().find(|(q, _)| *q == qname) else {
                continue;
            };
            // Instance-ness: Const/List/Set/Dict nodes, Inst, fresh containers
            let is_instance = match &value {
                Value::Node(g) => eng.kind_is(*g, |k| {
                    matches!(
                        k,
                        NodeKind::Const(_)
                            | NodeKind::List { .. }
                            | NodeKind::Tuple { .. }
                            | NodeKind::Set { .. }
                            | NodeKind::Dict { .. }
                    )
                }),
                Value::Inst { .. }
                | Value::ExcInst { .. }
                | Value::SynthSeq { .. }
                | Value::SynthDict { .. }
                | Value::SynthConst(_)
                | Value::FrozenSet { .. } => true,
                _ => false,
            };
            if !is_instance {
                continue;
            }
            let value_is_default = matches!(&value, Value::Node(g) if *g == dg);
            let default_is_iterable = eng.kind_is(dg, |k| {
                matches!(k, NodeKind::List { .. } | NodeKind::Set { .. } | NodeKind::Dict { .. })
            });
            let default_is_call = eng.kind_is(dg, |k| matches!(k, NodeKind::Call { .. }));
            let msg = if value_is_default {
                symbol.to_string()
            } else if default_is_iterable {
                u::value_pytype(eng, &value).unwrap_or_default()
            } else if default_is_call {
                let cls_name = qname.rsplit('.').next().unwrap_or("").to_string();
                format!("{cls_name}() ({qname})")
            } else {
                format!("{} ({qname})", as_string(eng, dg))
            };
            emit(cx, "W0102", node, format!("Dangerous default value {msg} as argument"));
        }
    }

    /// _check_unreachable for Return/Continue/Break/Raise statements
    pub fn check_unreachable_stmt(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some(mut sib) = u::next_sibling(eng, node) else { return };
        let node_is_return = eng.kind_is(node, |k| matches!(k, NodeKind::Return { .. }));
        if node_is_return {
            let is_yield_expr = {
                let md = eng.md(sib.m);
                match &md.tree.nodes[sib.n.idx()].kind {
                    NodeKind::Expr { value } => matches!(
                        md.tree.nodes[value.idx()].kind,
                        NodeKind::Yield { .. } | NodeKind::YieldFrom { .. }
                    ),
                    _ => false,
                }
            };
            if is_yield_expr {
                match u::next_sibling(eng, sib) {
                    Some(s2) => sib = s2,
                    None => return,
                }
            }
        }
        emit(cx, "W0101", sib, "Unreachable code".into());
    }

    /// _check_not_in_finally — W0150 lost-exception
    pub fn check_not_in_finally(&mut self, cx: &mut WalkCx, node: GNode, node_name: &str, break_on_loop: bool) {
        let eng = cx.eng;
        if self.trys.is_empty() {
            return;
        }
        let mut cur = node;
        let mut parent = eng.parent(node);
        while let Some(p) = parent {
            let is_breaker = if break_on_loop {
                eng.kind_is(p, |k| {
                    matches!(k, NodeKind::For(_) | NodeKind::AsyncFor(_) | NodeKind::While { .. })
                })
            } else {
                eng.kind_is(p, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                })
            };
            if is_breaker {
                return;
            }
            let in_finalbody = {
                let md = eng.md(p.m);
                match &md.tree.nodes[p.n.idx()].kind {
                    NodeKind::Try(d) | NodeKind::TryStar(d) => d.finalbody.contains(&cur.n),
                    _ => false,
                }
            };
            if in_finalbody {
                emit(
                    cx,
                    "W0150",
                    node,
                    format!("{node_name} statement in finally block may swallow exception"),
                );
                return;
            }
            cur = p;
            parent = eng.parent(p);
        }
    }
}

/// dummy-variables-rgx default:
/// `_+$|(_[a-zA-Z0-9_]*[a-zA-Z0-9]+?$)|dummy|^ignored_|^unused_` with
/// re.match (prefix-anchored) semantics.
fn dummy_rgx_match(name: &str) -> bool {
    if !name.is_empty() && name.chars().all(|c| c == '_') {
        return true;
    }
    if name.starts_with('_')
        && name.len() >= 2
        && name[1..].chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.ends_with(|c: char| c.is_ascii_alphanumeric())
    {
        return true;
    }
    name.starts_with("dummy") || name.starts_with("ignored_") || name.starts_with("unused_")
}

/// _name_holds_generator (basic_checker.py:382-410)
fn name_holds_generator(eng: &Engine, test: GNode) -> (bool, Option<GNode>) {
    let Some(name) = name_of(eng, test) else { return (false, None) };
    let sym = eng.sym(&name);
    let frame = eng.frame(test);
    let res = eng.lookup(frame, sym);
    let stmts: &Vec<NV> = &res.1;
    // maybe_generator_assigned: for Assign-parented AssignNames, whether the
    // Assign's value is a GeneratorExp
    let mut gen_flags: Vec<bool> = Vec::new();
    for nv in stmts {
        let NV::N(an) = nv else { continue };
        let Some(parent) = eng.parent(*an) else { continue };
        let md = eng.md(parent.m);
        if let NodeKind::Assign { value, .. } = &md.tree.nodes[parent.n.idx()].kind {
            let is_gen = matches!(md.tree.nodes[value.idx()].kind, NodeKind::GeneratorExp(_));
            gen_flags.push(is_gen);
        }
    }
    if let Some(&first) = gen_flags.first() {
        if first && gen_flags.iter().all(|&b| b) {
            return (true, None);
        }
        // single assignment whose value is a Call
        if stmts.len() == 1 {
            if let NV::N(an) = &stmts[0] {
                if let Some(parent) = eng.parent(*an) {
                    let md = eng.md(parent.m);
                    if let NodeKind::Assign { value, .. } = &md.tree.nodes[parent.n.idx()].kind {
                        if matches!(md.tree.nodes[value.idx()].kind, NodeKind::Call { .. }) {
                            return (false, Some(GNode { m: parent.m, n: *value }));
                        }
                    }
                }
            }
        }
    }
    (false, None)
}

// ===========================================================================
// BasicErrorChecker — W0120 useless-else-on-loop
// ===========================================================================

pub fn check_else_on_loop(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let orelse_first: Option<NodeId> = {
        let md = eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::For(d) | NodeKind::AsyncFor(d) => d.orelse.first().copied(),
            NodeKind::While { orelse, .. } => orelse.first().copied(),
            _ => None,
        }
    };
    let Some(first) = orelse_first else { return };
    if loop_exits_early(eng, node) {
        return;
    }
    let line = u::lineno(eng, GNode { m: node.m, n: first }).saturating_sub(1);
    let col = u::col_offset(eng, node) as i64;
    cx.emit_node(
        "W0120",
        line,
        col,
        "Else clause on loop without a break statement, remove the else and de-indent all the code inside it".into(),
    );
}

fn is_loop_kind(k: &NodeKind) -> bool {
    matches!(k, NodeKind::For(_) | NodeKind::AsyncFor(_) | NodeKind::While { .. })
}

fn loop_exits_early(eng: &Engine, loop_g: GNode) -> bool {
    let skip = |k: &NodeKind| {
        matches!(
            k,
            NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::ClassDef(_)
        )
    };
    let inner_loops: Vec<GNode> = nodes_of_class(eng, loop_g, is_loop_kind, skip)
        .into_iter()
        .filter(|&n| n != loop_g)
        .collect();
    nodes_of_class(eng, loop_g, |k| matches!(k, NodeKind::Break), skip)
        .into_iter()
        .any(|b| match get_break_loop_node(eng, b) {
            Some(l) => !inner_loops.contains(&l),
            None => true,
        })
}

fn get_break_loop_node(eng: &Engine, break_node: GNode) -> Option<GNode> {
    let mut cur = break_node;
    let mut parent = eng.parent(cur);
    loop {
        match parent {
            None => return None,
            Some(p) => {
                let is_loop = eng.kind_is(p, is_loop_kind);
                let in_orelse = {
                    let md = eng.md(p.m);
                    match &md.tree.nodes[p.n.idx()].kind {
                        NodeKind::For(d) | NodeKind::AsyncFor(d) => d.orelse.contains(&cur.n),
                        NodeKind::While { orelse, .. } => orelse.contains(&cur.n),
                        _ => false,
                    }
                };
                if is_loop && !in_orelse {
                    return Some(p);
                }
                cur = p;
                parent = eng.parent(p);
            }
        }
    }
}

// ===========================================================================
// PassChecker — W0107 unnecessary-pass
// ===========================================================================

pub fn visit_pass(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let Some(parent) = eng.parent(node) else { return };
    let md = eng.md(parent.m);
    let pk = &md.tree.nodes[parent.n.idx()].kind;
    let seq_len = crate::ckutils::child_sequence_len(pk, node.n);
    let parent_doc = match pk {
        NodeKind::ClassDef(d) => d.doc_node.is_some(),
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.doc_node.is_some(),
        _ => false,
    };
    drop(md);
    if seq_len > 1 || parent_doc {
        emit(cx, "W0107", node, "Unnecessary pass statement".into());
    }
}

// ===========================================================================
// ComparisonChecker (comparison_checker.py)
// ===========================================================================

pub fn visit_compare(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let (left, ops): (NodeId, Vec<(Box<str>, NodeId)>) = {
        let md = eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Compare { left, ops } => (*left, ops.clone()),
            _ => return,
        }
    };
    if ops.is_empty() {
        return;
    }
    let lg = GNode { m: node.m, n: left };
    let rg = GNode { m: node.m, n: ops[0].1 };
    let op: &str = &ops[0].0;

    check_callable_comparison(cx, node, lg, rg, op);
    check_logical_tautology(cx, node, lg, rg, op);
    check_unidiomatic_typecheck(cx, node, lg, rg, op);
    check_constants_comparison(cx, node, lg, rg, op);
    if ops.len() != 1 {
        return;
    }
    if op == "==" || op == "!=" {
        check_singleton_comparison(cx, node, lg, rg, op == "!=");
    }
    if matches!(op, "==" | "!=" | "is" | "is not") {
        check_nan_comparison(cx, node, lg, rg, matches!(op, "!=" | "is not"));
    }
    if op == "is" || op == "is not" {
        check_literal_comparison(cx, node, rg);
    }
}

fn check_callable_comparison(cx: &mut WalkCx, node: GNode, left: GNode, right: GNode, op: &str) {
    if !matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=") {
        return;
    }
    let eng = cx.eng;
    let mut count = 0;
    for operand in [left, right] {
        let inferred = u::safe_infer(eng, cx.caches, operand);
        // bare_callables = (nodes.FunctionDef, astroid.BoundMethod);
        // objects.Property SUBCLASSES FunctionDef (synthetic '<property>'
        // with empty body and no decorators -> always counts)
        let func: Option<GNode> = match &inferred {
            Some(Value::Node(g)) if u::is_functiondef(eng, *g) => Some(*g),
            Some(Value::BoundMethod { func, .. }) | Some(Value::DescBM { func, .. }) => {
                Some(*func)
            }
            Some(Value::Property { .. }) => {
                count += 1;
                continue;
            }
            _ => None,
        };
        let Some(f) = func else { continue };
        if crate::typecheck::decorated_with(eng, f, &["typing._SpecialForm"]) {
            continue;
        }
        let has_raise = {
            let md = eng.md(f.m);
            match &md.tree.nodes[f.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d
                    .body
                    .iter()
                    .any(|&b| matches!(md.tree.nodes[b.idx()].kind, NodeKind::Raise { .. })),
                _ => false,
            }
        };
        if has_raise {
            continue;
        }
        count += 1;
    }
    if count == 1 {
        emit(
            cx,
            "W0143",
            node,
            "Comparing against a callable, did you omit the parenthesis?".into(),
        );
    }
}

fn check_logical_tautology(cx: &mut WalkCx, node: GNode, left: GNode, right: GNode, op: &str) {
    let eng = cx.eng;
    let suggestion: Option<String> = match (const_of(eng, left), const_of(eng, right)) {
        (Some(lc), Some(rc)) => {
            if u::py_key(&lc) == u::py_key(&rc) {
                Some(format!("{} {} {}", const_str(&lc), op, const_str(&rc)))
            } else {
                None
            }
        }
        _ => match (name_of(eng, left), name_of(eng, right)) {
            (Some(ln), Some(rn))
                if eng.kind_is(left, |k| matches!(k, NodeKind::Name { .. }))
                    && eng.kind_is(right, |k| matches!(k, NodeKind::Name { .. }))
                    && ln == rn =>
            {
                Some(format!("{ln} {op} {rn}"))
            }
            _ => None,
        },
    };
    if let Some(s) = suggestion {
        emit(cx, "R0124", node, format!("Redundant comparison - {s}"));
    }
}

fn is_one_arg_pos_call(eng: &Engine, g: GNode) -> bool {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Call { args, keywords, .. } => args.len() == 1 && keywords.is_empty(),
        _ => false,
    }
}

fn check_unidiomatic_typecheck(cx: &mut WalkCx, node: GNode, left: GNode, right: GNode, op: &str) {
    if !matches!(op, "is" | "is not" | "==" | "!=") {
        return;
    }
    let eng = cx.eng;
    if is_one_arg_pos_call(eng, left) {
        check_type_x_is_y(cx, node, left, right);
    } else if eng.kind_is(left, |k| matches!(k, NodeKind::Name { .. }))
        && is_one_arg_pos_call(eng, right)
    {
        check_type_x_is_y(cx, node, right, left);
    }
}

fn infers_to_builtin_type(cx: &mut WalkCx, call: GNode) -> bool {
    let eng = cx.eng;
    let func = {
        let md = eng.md(call.m);
        match &md.tree.nodes[call.n.idx()].kind {
            NodeKind::Call { func, .. } => GNode { m: call.m, n: *func },
            _ => return false,
        }
    };
    match u::safe_infer(eng, cx.caches, func) {
        Some(Value::Node(g)) if u::is_classdef(eng, g) => eng.qname(g) == "builtins.type",
        _ => false,
    }
}

fn check_type_x_is_y(cx: &mut WalkCx, node: GNode, left: GNode, right: GNode) {
    let eng = cx.eng;
    if !infers_to_builtin_type(cx, left) {
        return;
    }
    if is_one_arg_pos_call(eng, right) && infers_to_builtin_type(cx, right) {
        let arg0 = {
            let md = eng.md(right.m);
            match &md.tree.nodes[right.n.idx()].kind {
                NodeKind::Call { args, .. } => GNode { m: right.m, n: args[0] },
                _ => return,
            }
        };
        let right_arg = u::safe_infer(eng, cx.caches, arg0);
        let is_literal = match &right_arg {
            Some(Value::Node(g)) => eng.kind_is(*g, |k| {
                matches!(
                    k,
                    NodeKind::Const(_) | NodeKind::Dict { .. } | NodeKind::List { .. } | NodeKind::Set { .. }
                )
            }),
            Some(Value::SynthConst(_)) | Some(Value::SynthDict { .. }) => true,
            Some(Value::SynthSeq { kind, .. }) => !matches!(kind, pyinfer::value::SeqKind::Tuple),
            _ => false,
        };
        if !is_literal {
            return;
        }
    }
    emit(cx, "C0123", node, "Use isinstance() rather than type() for a typecheck.".into());
}

fn check_constants_comparison(cx: &mut WalkCx, node: GNode, left: GNode, right: GNode, op: &str) {
    let eng = cx.eng;
    if is_const_kind(eng, left) && is_const_kind(eng, right) {
        emit(
            cx,
            "R0133",
            node,
            format!(
                "Comparison between constants: \"{} {} {}\" has a constant value",
                as_string(eng, left),
                op,
                as_string(eng, right)
            ),
        );
    }
}

/// utils.is_singleton_const: Const value IS True/False/None
fn singleton_const(eng: &Engine, g: GNode) -> Option<&'static str> {
    match const_of(eng, g) {
        Some(ConstValue::Bool(true)) => Some("True"),
        Some(ConstValue::Bool(false)) => Some("False"),
        Some(ConstValue::None) => Some("None"),
        _ => None,
    }
}

fn check_singleton_comparison(
    cx: &mut WalkCx,
    root: GNode,
    left: GNode,
    right: GNode,
    checking_for_absence: bool,
) {
    let eng = cx.eng;
    let (singleton, other) = if let Some(s) = singleton_const(eng, left) {
        (s, right)
    } else if let Some(s) = singleton_const(eng, right) {
        (s, left)
    } else {
        return;
    };
    let example = if checking_for_absence {
        format!("'{} is not {}'", as_string(eng, left), as_string(eng, right))
    } else {
        format!("'{} is {}'", as_string(eng, left), as_string(eng, right))
    };
    let suggestion = if singleton == "True" || singleton == "False" {
        let singleton_bool = singleton == "True";
        let checking_truthiness = singleton_bool != checking_for_absence;
        let other_str = as_string(eng, other);
        let truthiness_example = if checking_truthiness {
            other_str.clone()
        } else {
            format!("not {other_str}")
        };
        let wrapped = if !is_test_condition(eng, root) && checking_truthiness {
            format!("'bool({truthiness_example})'")
        } else {
            format!("'{truthiness_example}'")
        };
        let phrase = if checking_truthiness { "truthiness" } else { "falsiness" };
        format!(
            "{example} if checking for the singleton value {singleton}, or {wrapped} if testing for {phrase}"
        )
    } else {
        example
    };
    emit(
        cx,
        "C0121",
        root,
        format!("Comparison '{}' should be {}", as_string(eng, root), suggestion),
    );
}

/// utils.is_test_condition(node) with parent = node.parent
fn is_test_condition(eng: &Engine, node: GNode) -> bool {
    let Some(parent) = eng.parent(node) else { return false };
    let md = eng.md(parent.m);
    match &md.tree.nodes[parent.n.idx()].kind {
        NodeKind::While { test, .. } | NodeKind::If { test, .. } | NodeKind::IfExp { test, .. } => {
            let t = *test;
            drop(md);
            let tg = GNode { m: parent.m, n: t };
            tg == node || eng.parent_of(tg, node)
        }
        NodeKind::Assert { test, .. } => {
            let t = *test;
            drop(md);
            let tg = GNode { m: parent.m, n: t };
            tg == node || eng.parent_of(tg, node)
        }
        NodeKind::Comprehension { ifs, .. } => ifs.contains(&node.n),
        NodeKind::Call { func, .. } => {
            let is_bool = matches!(
                &md.tree.nodes[func.idx()].kind,
                NodeKind::Name { name } if md.tree.s(*name) == "bool"
            );
            is_bool
        }
        _ => false,
    }
}

fn check_nan_comparison(
    cx: &mut WalkCx,
    root: GNode,
    left: GNode,
    right: GNode,
    checking_for_absence: bool,
) {
    let eng = cx.eng;
    let mut crashed = false;
    let mut is_nan = |g: GNode| -> bool {
        // _is_float_nan: Call with exactly one Const-str arg "nan"
        let is_float_call_shape = {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Call { args, .. } if args.len() == 1 => {
                    matches!(
                        &md.tree.nodes[args[0].idx()].kind,
                        NodeKind::Const(ConstValue::Str(s)) if s.to_lowercase() == "nan"
                    )
                }
                _ => false,
            }
        };
        if is_float_call_shape {
            // node.inferred()[0].pytype() — full drain; errors crash (F0002)
            let flow = eng.infer(g, &u::fresh_ctx());
            if flow.err.is_some() || flow.vals.is_empty() {
                crashed = true;
                return false;
            }
            return u::value_pytype(eng, &flow.vals[0]).as_deref() == Some("builtins.float");
        }
        // _is_numpy_nan: numpy.NaN / np.NaN (attrname case-sensitive)
        let md = eng.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Attribute { expr, attrname, .. } if md.tree.s(*attrname) == "NaN" => {
                matches!(
                    &md.tree.nodes[expr.idx()].kind,
                    NodeKind::Name { name } if matches!(md.tree.s(*name), "numpy" | "np")
                )
            }
            _ => false,
        }
    };
    let nan_left = is_nan(left);
    let nan_right = if nan_left { false } else { is_nan(right) };
    if crashed {
        cx.crashed.set(true);
        return;
    }
    if !nan_left && !nan_right {
        return;
    }
    let absence_text = if checking_for_absence { "not " } else { "" };
    let suggestion = if nan_left {
        format!("'{absence_text}math.isnan({})'", as_string(eng, right))
    } else {
        format!("'{absence_text}math.isnan({})'", as_string(eng, left))
    };
    emit(
        cx,
        "W0177",
        root,
        format!("Comparison '{}' should be {}", as_string(eng, root), suggestion),
    );
}

fn check_literal_comparison(cx: &mut WalkCx, node: GNode, literal: GNode) {
    let eng = cx.eng;
    let flagged = {
        let md = eng.md(literal.m);
        match &md.tree.nodes[literal.n.idx()].kind {
            NodeKind::Const(c) => match c {
                ConstValue::Bool(_) | ConstValue::None => false,
                ConstValue::Bytes(_) | ConstValue::Str(_) | ConstValue::StrSurrogate(_)
                | ConstValue::Int(_) | ConstValue::Float(_) => true,
                _ => false,
            },
            NodeKind::List { .. } | NodeKind::Dict { .. } | NodeKind::Set { .. } => true,
            _ => false,
        }
    };
    if !flagged {
        return;
    }
    let incorrect = as_string(eng, node);
    let (eq, isop) = if incorrect.contains("is not") {
        ("!=", "is not")
    } else {
        ("==", "is")
    };
    let fixed = incorrect.replace(isop, eq);
    emit(
        cx,
        "R0123",
        node,
        format!("In '{incorrect}', use '{eq}' when comparing constant literals not '{isop}' ('{fixed}')"),
    );
}

// ===========================================================================
// NameChecker (name_checker/checker.py) — C0103/C0104/C0105/C0131/C0132
// ===========================================================================

const GOOD_NAMES: &[&str] = &["i", "j", "k", "ex", "Run", "_"];
const BAD_NAMES: &[&str] = &["foo", "bar", "baz", "toto", "tutu", "tata"];

fn human_label(node_type: &str) -> &'static str {
    match node_type {
        "module" => "module",
        "const" => "constant",
        "class" => "class",
        "function" => "function",
        "method" => "method",
        "attr" => "attribute",
        "argument" => "argument",
        "variable" => "variable",
        "class_attribute" => "class attribute",
        "class_const" => "class constant",
        "inlinevar" => "inline iteration",
        "typevar" => "type variable",
        "paramspec" => "parameter specification variable",
        "typevartuple" => "type variable tuple",
        "typealias" => "type alias",
        _ => "file",
    }
}

fn name_hint(node_type: &str) -> &'static str {
    match node_type {
        "module" | "function" | "method" | "attr" | "argument" | "variable" => {
            "snake_case naming style"
        }
        "const" | "class_const" => "UPPER_CASE naming style",
        "class" => "PascalCase naming style",
        "class_attribute" | "inlinevar" => "any naming style",
        _ => "predefined naming style",
    }
}

// --- default naming regex matchers (python `re` unicode semantics) --------

/// python \w = str.isalnum() or '_': general category L* or N*
/// (isalpha: Lu/Ll/Lt/Lm/Lo; isdecimal/isdigit/isnumeric: Nd/Nl/No).
/// NOT Rust's is_alphanumeric (Alphabetic property includes Other_Alphabetic
/// combining marks like Khmer vowel signs, which python isalnum rejects).
fn is_word(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    c == '_'
        || matches!(
            get_general_category(c),
            G::UppercaseLetter
                | G::LowercaseLetter
                | G::TitlecaseLetter
                | G::ModifierLetter
                | G::OtherLetter
                | G::DecimalNumber
                | G::LetterNumber
                | G::OtherNumber
        )
}
/// python \d = general category Nd
fn is_unicode_digit(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    matches!(get_general_category(c), G::DecimalNumber)
}

/// snake DEFAULT/MOD: `[^\W\dA-Z][^\WA-Z]*$`
fn match_snake(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else { return false };
    if !is_word(first) || is_unicode_digit(first) || first.is_ascii_uppercase() {
        return false;
    }
    chars.all(|c| is_word(c) && !c.is_ascii_uppercase())
}

/// UPPER CONST: `([^\W\da-z][^\Wa-z]*|__.*__)$`
fn match_const(name: &str) -> bool {
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        if is_word(first) && !is_unicode_digit(first) && !first.is_ascii_lowercase() {
            let rest_ok = name
                .chars()
                .skip(1)
                .all(|c| is_word(c) && !c.is_ascii_lowercase());
            if rest_ok {
                return true;
            }
        }
    }
    // __.*__ (no newlines in identifiers)
    name.len() >= 4 && name.starts_with("__") && name.ends_with("__")
}

/// Pascal CLASS: `[^\W\da-z][^\W_]*$`
fn match_pascal(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else { return false };
    if !is_word(first) || is_unicode_digit(first) || first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| is_word(c) && c != '_')
}

/// strip up to two leading underscores; None when 3+ remain after stripping
fn strip_dunder_prefix(name: &str) -> Option<&str> {
    let stripped = name.strip_prefix('_').unwrap_or(name);
    let stripped = stripped.strip_prefix('_').unwrap_or(stripped);
    if stripped.starts_with('_') {
        return None;
    }
    Some(stripped)
}

/// (?:[A-Z]+[a-z]+)+ — alternating runs, starts upper, ends lower, ASCII
fn match_camel_units(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars().peekable();
    loop {
        // [A-Z]+
        let mut n_up = 0;
        while let Some(&c) = chars.peek() {
            if c.is_ascii_uppercase() {
                chars.next();
                n_up += 1;
            } else {
                break;
            }
        }
        if n_up == 0 {
            return false;
        }
        // [a-z]+
        let mut n_low = 0;
        while let Some(&c) = chars.peek() {
            if c.is_ascii_lowercase() {
                chars.next();
                n_low += 1;
            } else {
                break;
            }
        }
        if n_low == 0 {
            return false;
        }
        if chars.peek().is_none() {
            return true;
        }
    }
}

/// core alternative: `[A-Z]+|(?:[A-Z]+[a-z]+)+(?:SUFFIX)?(?<!Type)`
fn match_typevar_core(core: &str, opt_suffix: &str) -> bool {
    if !core.is_empty() && core.chars().all(|c| c.is_ascii_uppercase()) {
        return true;
    }
    // with the optional suffix consumed
    if !opt_suffix.is_empty() {
        if let Some(units) = core.strip_suffix(opt_suffix) {
            if match_camel_units(units) && !core.ends_with("Type") {
                return true;
            }
        }
    }
    // without the suffix
    match_camel_units(core) && !core.ends_with("Type")
}

fn match_typevar_name(name: &str) -> bool {
    let Some(rest) = strip_dunder_prefix(name) else { return false };
    // (?!T[A-Z])
    let mut it = rest.chars();
    if it.next() == Some('T') {
        if let Some(c2) = it.next() {
            if c2.is_ascii_uppercase() {
                return false;
            }
        }
    }
    for suffix in ["_contra", "_co", ""] {
        if let Some(core) = rest.strip_suffix(suffix) {
            if match_typevar_core(core, "T") {
                return true;
            }
        }
    }
    false
}

fn match_paramspec_name(name: &str) -> bool {
    let Some(rest) = strip_dunder_prefix(name) else { return false };
    match_typevar_core(rest, "P")
}

fn match_typevartuple_name(name: &str) -> bool {
    let Some(rest) = strip_dunder_prefix(name) else { return false };
    match_typevar_core(rest, "Ts")
}

/// `^_{0,2}(?!T[A-Z]|Type)[A-Z]+[a-z0-9]+(?:[A-Z][a-z0-9]+)*$`
fn match_typealias_name(name: &str) -> bool {
    let Some(rest) = strip_dunder_prefix(name) else { return false };
    if rest.starts_with("Type") {
        return false;
    }
    {
        let mut it = rest.chars();
        if it.next() == Some('T') {
            if let Some(c2) = it.next() {
                if c2.is_ascii_uppercase() {
                    return false;
                }
            }
        }
    }
    let b: Vec<char> = rest.chars().collect();
    let n = b.len();
    let mut i = 0;
    while i < n && b[i].is_ascii_uppercase() {
        i += 1;
    }
    if i == 0 {
        return false;
    }
    let mut j = i;
    while j < n && (b[j].is_ascii_lowercase() || b[j].is_ascii_digit()) {
        j += 1;
    }
    if j == i {
        return false;
    }
    // (?:[A-Z][a-z0-9]+)*
    let mut k = j;
    while k < n {
        if !b[k].is_ascii_uppercase() {
            return false;
        }
        k += 1;
        let start = k;
        while k < n && (b[k].is_ascii_lowercase() || b[k].is_ascii_digit()) {
            k += 1;
        }
        if k == start {
            return false;
        }
    }
    true
}

fn name_regex_match(node_type: &str, name: &str) -> bool {
    match node_type {
        "module" | "function" | "method" | "attr" | "argument" | "variable" => match_snake(name),
        "const" | "class_const" => match_const(name),
        "class" => match_pascal(name),
        "class_attribute" | "inlinevar" => true, // AnyStyle `.*`
        "typevar" => match_typevar_name(name),
        "paramspec" => match_paramspec_name(name),
        "typevartuple" => match_typevartuple_name(name),
        "typealias" => match_typealias_name(name),
        _ => true,
    }
}

fn capitalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    if let Some(c) = chars.next() {
        out.extend(c.to_uppercase());
    }
    for c in chars {
        out.extend(c.to_lowercase());
    }
    out
}

/// _check_name funnel. `node` is the message anchor.
fn check_name(cx: &mut WalkCx, node_type: &str, name: &str, node: GNode, disallowed_check_only: bool) {
    if GOOD_NAMES.contains(&name) {
        return;
    }
    if BAD_NAMES.contains(&name) {
        emit(cx, "C0104", node, format!("Disallowed name \"{name}\""));
        return;
    }
    let matched = name_regex_match(node_type, name);
    if !matched && !disallowed_check_only && !should_exempt_from_invalid_name(cx, node_type, node) {
        let label = capitalize(human_label(node_type));
        let hint = name_hint(node_type);
        emit(
            cx,
            "C0103",
            node,
            format!("{label} name \"{name}\" doesn't conform to {hint}"),
        );
    }
    if node_type == "typevar" {
        check_typevar(cx, name, node);
    }
}

fn should_exempt_from_invalid_name(cx: &mut WalkCx, node_type: &str, node: GNode) -> bool {
    if node_type != "variable" {
        return false;
    }
    matches!(
        u::safe_infer(cx.eng, cx.caches, node),
        Some(Value::Node(g)) if u::is_classdef(cx.eng, g)
    )
}

pub fn name_visit_module(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let modname = eng.md(node.m).name.clone();
    let last = modname.rsplit('.').next().unwrap_or(&modname).to_string();
    check_name(cx, "module", &last, node, false);
}

pub fn name_visit_classdef(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let name = eng.node_name(node).unwrap_or_default();
    check_name(cx, "class", &name, node, false);
    let iattrs = eng.instance_attrs_of(node);
    if iattrs.is_empty() {
        return;
    }
    let ancestors = eng.ancestors(node, true, None);
    for (sym, anodes) in iattrs.iter() {
        let Some(&first) = anodes.first() else { continue };
        let inherited = ancestors
            .iter()
            .any(|&anc| eng.instance_attrs_of(anc).contains_key(sym));
        if inherited {
            continue;
        }
        // brain-synthesized Unknown placeholders (dataclass/attrs) carry the
        // class-body stmt's position and parent in astroid; resolve them
        let mapped: Option<GNode> = eng
            .dataclass_attrs
            .borrow()
            .get(&first)
            .copied()
            .or_else(|| eng.reparents.borrow().get(&first).copied());
        let anchor = mapped.unwrap_or(first);
        // is_assign_name_annotated_with(anodes[0], "Final"): parent is the
        // mapped stmt for placeholders, the syntactic parent otherwise
        let final_parent = mapped.or_else(|| eng.parent(first));
        let is_final = final_parent
            .map(|p| annassign_annotated_with(eng, p, "Final"))
            .unwrap_or(false);
        if is_final {
            continue;
        }
        let attr_name = eng.sname(*sym);
        check_name(cx, "attr", &attr_name, anchor, false);
    }
}

/// `parent` is an AnnAssign whose annotation (or annotation.value for a
/// Subscript) is Name/Attribute == typing_name
fn annassign_annotated_with(eng: &Engine, parent: GNode, typing_name: &str) -> bool {
    let md = eng.md(parent.m);
    let NodeKind::AnnAssign { annotation, .. } = &md.tree.nodes[parent.n.idx()].kind else {
        return false;
    };
    let mut ann = *annotation;
    if let NodeKind::Subscript { value, .. } = &md.tree.nodes[ann.idx()].kind {
        ann = *value;
    }
    match &md.tree.nodes[ann.idx()].kind {
        NodeKind::Name { name } => md.tree.s(*name) == typing_name,
        NodeKind::Attribute { attrname, .. } => md.tree.s(*attrname) == typing_name,
        _ => false,
    }
}

/// utils.is_assign_name_annotated_with(node, typing_name)
fn is_assign_name_annotated_with(eng: &Engine, node: GNode, typing_name: &str) -> bool {
    let Some(parent) = eng.parent(node) else { return false };
    let md = eng.md(parent.m);
    let NodeKind::AnnAssign { annotation, .. } = &md.tree.nodes[parent.n.idx()].kind else {
        return false;
    };
    let mut ann = *annotation;
    if let NodeKind::Subscript { value, .. } = &md.tree.nodes[ann.idx()].kind {
        ann = *value;
    }
    match &md.tree.nodes[ann.idx()].kind {
        NodeKind::Name { name } => md.tree.s(*name) == typing_name,
        NodeKind::Attribute { attrname, .. } => md.tree.s(*attrname) == typing_name,
        _ => false,
    }
}

/// utils.is_assign_name_annotated_with_class_var_typing_name(node, "Final")
fn is_annotated_classvar_final(eng: &Engine, node: GNode) -> bool {
    if !is_assign_name_annotated_with(eng, node, "ClassVar") {
        return false;
    }
    let Some(parent) = eng.parent(node) else { return false };
    let md = eng.md(parent.m);
    let NodeKind::AnnAssign { annotation, .. } = &md.tree.nodes[parent.n.idx()].kind else {
        return false;
    };
    let mut ann = *annotation;
    if let NodeKind::Subscript { slice, .. } = &md.tree.nodes[ann.idx()].kind {
        ann = *slice;
        if let NodeKind::Subscript { value, .. } = &md.tree.nodes[ann.idx()].kind {
            ann = *value;
        }
    }
    match &md.tree.nodes[ann.idx()].kind {
        NodeKind::Name { name } => md.tree.s(*name) == "Final",
        NodeKind::Attribute { attrname, .. } => md.tree.s(*attrname) == "Final",
        _ => false,
    }
}

/// utils.overrides_a_method
fn overrides_a_method(eng: &Engine, class_node: GNode, name: &str) -> bool {
    let sym = eng.sym(name);
    for anc in eng.ancestors(class_node, true, None) {
        if eng.node_name(anc).as_deref() == Some("object") {
            continue;
        }
        let locals = eng.class_locals_get(anc, sym);
        if let Some(&first) = locals.first() {
            if u::is_functiondef(eng, first) {
                return true;
            }
        }
    }
    false
}

/// FunctionDef.is_method(): type != "function" and parent frame is ClassDef
fn is_method(eng: &Engine, func: GNode) -> bool {
    if eng.func_type(func) == pyinfer::graph::FType::Function {
        return false;
    }
    let Some(parent) = eng.parent(func) else { return false };
    u::is_classdef(eng, eng.frame(parent))
}

/// is_property_setter / is_property_deleter (syntactic)
pub fn is_property_setter_or_deleter(eng: &Engine, func: GNode) -> bool {
    let md = eng.md(func.m);
    let dec = match &md.tree.nodes[func.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.decorators,
        _ => None,
    };
    let Some(dec) = dec else { return false };
    let NodeKind::Decorators { nodes } = &md.tree.nodes[dec.idx()].kind else {
        return false;
    };
    nodes.iter().any(|&n| {
        matches!(
            &md.tree.nodes[n.idx()].kind,
            NodeKind::Attribute { attrname, .. }
                if matches!(md.tree.s(*attrname), "setter" | "deleter")
        )
    })
}

fn determine_function_name_type(cx: &mut WalkCx, node: GNode) -> &'static str {
    let eng = cx.eng;
    if !is_method(eng, node) {
        return "function";
    }
    if is_property_setter_or_deleter(eng, node) {
        return "attr";
    }
    let decorators: Vec<NodeId> = {
        let md = eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => match d.decorators {
                Some(dn) => match &md.tree.nodes[dn.idx()].kind {
                    NodeKind::Decorators { nodes } => nodes.clone(),
                    _ => Vec::new(),
                },
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    };
    for dec in decorators {
        let dg = GNode { m: node.m, n: dec };
        let eligible = {
            let md = eng.md(node.m);
            match &md.tree.nodes[dec.idx()].kind {
                NodeKind::Name { .. } => true,
                NodeKind::Attribute { attrname, .. } => {
                    md.tree.s(*attrname) == "abstractproperty"
                }
                _ => false,
            }
        };
        if !eligible {
            continue;
        }
        let inferred = u::safe_infer(eng, cx.caches, dg);
        if let Some(v) = inferred {
            if let Some(q) = u::value_qname(eng, &v) {
                if q == "builtins.property" || q == "abc.abstractproperty" {
                    return "attr";
                }
            }
        }
    }
    "method"
}

pub fn name_visit_functiondef(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let name = eng.node_name(node).unwrap_or_default();
    if is_method(eng, node) {
        let parent_frame = eng.frame(eng.parent(node).unwrap_or(node));
        if overrides_a_method(eng, parent_frame, &name) {
            return;
        }
        // confidence: has_known_bases — affects nothing in default output,
        // but the call burns inference identically to pylint
        let _ = crate::typecheck::has_known_bases(eng, cx.caches, parent_frame);
    }
    let ftype = determine_function_name_type(cx, node);
    check_name(cx, ftype, &name, node, false);
    let args: Vec<NodeId> = {
        let md = eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                match &md.tree.nodes[d.args.idx()].kind {
                    NodeKind::Arguments(a) => a.args.clone(),
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    };
    for a in args {
        let ag = GNode { m: node.m, n: a };
        if let Some(an) = name_of(eng, ag) {
            check_name(cx, "argument", &an, ag, false);
        }
    }
}

/// AssignName.assign_type(): nearest self-returning ancestor
fn assign_type_of(eng: &Engine, node: GNode) -> GNode {
    let mut cur = node;
    loop {
        let Some(p) = eng.parent(cur) else { return cur };
        let is_self_returning = eng.kind_is(p, |k| {
            matches!(
                k,
                NodeKind::Assign { .. }
                    | NodeKind::AnnAssign { .. }
                    | NodeKind::AugAssign { .. }
                    | NodeKind::Delete { .. }
                    | NodeKind::ExceptHandler { .. }
                    | NodeKind::For(_)
                    | NodeKind::AsyncFor(_)
                    | NodeKind::With(_)
                    | NodeKind::AsyncWith(_)
                    | NodeKind::TypeAlias { .. }
                    | NodeKind::TypeVar { .. }
                    | NodeKind::ParamSpec { .. }
                    | NodeKind::TypeVarTuple { .. }
                    | NodeKind::NamedExpr { .. }
                    | NodeKind::MatchMapping { .. }
                    | NodeKind::MatchStar { .. }
                    | NodeKind::MatchAs { .. }
                    | NodeKind::Comprehension { .. }
                    | NodeKind::Arguments(_)
            )
        });
        if is_self_returning {
            return p;
        }
        cur = p;
    }
}

/// _assigns_typevar(value): Call whose func infers to a TypeVar-family class
fn assigns_typevar(cx: &mut WalkCx, value: Option<GNode>) -> Option<&'static str> {
    let eng = cx.eng;
    let v = value?;
    if !eng.kind_is(v, |k| matches!(k, NodeKind::Call { .. })) {
        return None;
    }
    let func = {
        let md = eng.md(v.m);
        match &md.tree.nodes[v.n.idx()].kind {
            NodeKind::Call { func, .. } => GNode { m: v.m, n: *func },
            _ => return None,
        }
    };
    match u::safe_infer(eng, cx.caches, func) {
        Some(Value::Node(g)) if u::is_classdef(eng, g) => {
            let q = eng.qname(g);
            match q.as_str() {
                "typing.TypeVar" | "typing_extensions.TypeVar" => Some("typevar"),
                "typing.ParamSpec" | "typing_extensions.ParamSpec" => Some("paramspec"),
                "typing.TypeVarTuple" | "typing_extensions.TypeVarTuple" => Some("typevartuple"),
                _ => None,
            }
        }
        _ => None,
    }
}

/// _assigns_typealias(node)
fn assigns_typealias(cx: &mut WalkCx, node: Option<GNode>) -> bool {
    let eng = cx.eng;
    let Some(g) = node else { return false };
    let inferred = u::safe_infer(eng, cx.caches, g);
    match &inferred {
        Some(Value::Node(c)) if u::is_classdef(eng, *c) => {
            let q = eng.qname(*c);
            if q == "typing.TypeAlias" {
                return true;
            }
            if matches!(q.as_str(), ".Union" | "builtins.Union" | "builtins.UnionType") {
                // unless the parent is an AnnAssign (annotation usage)
                let parent_is_annassign = eng
                    .parent(g)
                    .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::AnnAssign { .. })))
                    .unwrap_or(false);
                return !parent_is_annassign;
            }
            false
        }
        Some(Value::UnionType) => {
            let parent_is_annassign = eng
                .parent(g)
                .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::AnnAssign { .. })))
                .unwrap_or(false);
            !parent_is_annassign
        }
        Some(Value::Node(f)) if u::is_functiondef(eng, *f) => eng.qname(*f) == "typing.TypeAlias",
        _ => false,
    }
}

/// _should_check_class_regex(inferred)
fn should_check_class_regex(cx: &mut WalkCx, inferred: &Value) -> bool {
    let eng = cx.eng;
    match inferred {
        Value::Node(g) if u::is_classdef(eng, *g) => true,
        Value::Node(g) if u::is_functiondef(eng, *g) => eng.qname(*g) == "typing.Annotated",
        _ => {
            // bases.Instance whose mro() names intersect {EnumMeta, TypedDict}
            let cls = match inferred {
                Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(*cls),
                Value::Node(g) => {
                    if eng.kind_is(*g, |k| {
                        matches!(
                            k,
                            NodeKind::Const(_)
                                | NodeKind::List { .. }
                                | NodeKind::Tuple { .. }
                                | NodeKind::Set { .. }
                                | NodeKind::Dict { .. }
                        )
                    }) {
                        eng.proxied_class(inferred)
                    } else {
                        None
                    }
                }
                Value::SynthConst(_) | Value::SynthSeq { .. } | Value::SynthDict { .. } => {
                    eng.proxied_class(inferred)
                }
                _ => None,
            };
            let Some(cls) = cls else { return false };
            let mro = match eng.mro(cls, None) {
                Ok(m) => m,
                Err(_) => return false,
            };
            mro.iter().any(|&c| {
                matches!(eng.node_name(c).as_deref(), Some("EnumMeta") | Some("TypedDict"))
            })
        }
    }
}

/// name_checker._redefines_import(node)
fn redefines_import(eng: &Engine, node: GNode) -> bool {
    let name = match name_of(eng, node) {
        Some(n) => n,
        None => return false,
    };
    let mut current = node;
    loop {
        let Some(p) = eng.parent(current) else { return false };
        if eng.kind_is(p, |k| matches!(k, NodeKind::ExceptHandler { .. })) {
            break;
        }
        current = p;
    }
    let handler = eng.parent(current).unwrap();
    if !u::handler_catch(eng, handler, &["ImportError"]) {
        return false;
    }
    let Some(try_block) = eng.parent(handler) else { return false };
    for imp in nodes_of_class(
        eng,
        try_block,
        |k| matches!(k, NodeKind::Import { .. } | NodeKind::ImportFrom { .. }),
        |_| false,
    ) {
        let md = eng.md(imp.m);
        let names: Vec<(String, Option<String>)> = match &md.tree.nodes[imp.n.idx()].kind {
            NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } => names
                .iter()
                .map(|(n, a)| {
                    (md.tree.s(*n).to_string(), a.map(|s| md.tree.s(s).to_string()))
                })
                .collect(),
            _ => continue,
        };
        drop(md);
        for (n, alias) in names {
            match alias {
                Some(a) => {
                    if a == name {
                        return true;
                    }
                }
                None => {
                    if n == name {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// utils._is_reassigned_relative_to_current
fn is_reassigned_relative(eng: &Engine, node: GNode, varname: &str, before: bool) -> bool {
    let node_scope = eng.scope(node);
    let node_lineno = u::lineno(eng, node);
    let candidates = nodes_of_class(
        eng,
        node_scope,
        |k| {
            matches!(
                k,
                NodeKind::AssignName { .. } | NodeKind::ClassDef(_) | NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
            )
        },
        |_| false,
    );
    for a in candidates {
        let aname = {
            let md = eng.md(a.m);
            match &md.tree.nodes[a.n.idx()].kind {
                NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                NodeKind::ClassDef(d) => md.tree.s(d.name).to_string(),
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    md.tree.s(d.name).to_string()
                }
                _ => continue,
            }
        };
        if aname != varname {
            continue;
        }
        let alineno = u::lineno(eng, a);
        let hit = if before { alineno < node_lineno } else { alineno > node_lineno };
        if !hit {
            continue;
        }
        // _is_node_in_same_scope
        let same_scope = if eng.kind_is(a, |k| {
            matches!(k, NodeKind::ClassDef(_) | NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
        }) {
            eng.parent(a).map(|p| eng.scope(p) == node_scope).unwrap_or(false)
        } else {
            eng.scope(a) == node_scope
        };
        if same_scope {
            return true;
        }
    }
    false
}

/// Arguments.argnames(): all arg names incl posonly/vararg/kwonly/kwarg
fn argnames(eng: &Engine, func: GNode) -> Vec<String> {
    let md = eng.md(func.m);
    let args_n = match &md.tree.nodes[func.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
        NodeKind::Lambda(d) => d.args,
        _ => return Vec::new(),
    };
    let NodeKind::Arguments(a) = &md.tree.nodes[args_n.idx()].kind else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, ids) in [&a.posonlyargs, &a.args, &a.kwonlyargs].into_iter().enumerate() {
        if i == 2 {
            if let Some(v) = a.vararg {
                out.push(md.tree.s(v).to_string());
            }
        }
        for &n in ids {
            if let NodeKind::AssignName { name } = &md.tree.nodes[n.idx()].kind {
                out.push(md.tree.s(*name).to_string());
            }
        }
    }
    if let Some(kw) = a.kwarg {
        out.push(md.tree.s(kw).to_string());
    }
    out
}

/// utils.is_enum_member(node)
fn is_enum_member(eng: &Engine, node: GNode, frame: GNode) -> bool {
    if !u::is_classdef(eng, frame) {
        return false;
    }
    if !eng.is_subtype_of(frame, "enum.Enum", None) {
        return false;
    }
    if eng.md(frame.m).name == "enum" {
        return false;
    }
    // members = frame.locals.get("__members__"); name in [name_obj.name
    // for _, name_obj in members[0].items] — the value-Name names (the
    // LAST target of each member stmt; enum_member_names side table)
    let sym = eng.sym("__members__");
    let members = eng.class_locals_get(frame, sym);
    if members.is_empty() {
        return false;
    }
    let table = eng.enum_member_names.borrow();
    let Some(names) = table.get(&frame) else { return false };
    let Some(name) = name_of(eng, node) else { return false };
    names.contains(&name)
}

pub fn name_visit_assignname(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let Some(name) = name_of(eng, node) else { return };
    let frame = eng.frame(node);
    let assign_type = assign_type_of(eng, node);
    let at_kind = {
        let md = eng.md(assign_type.m);
        match &md.tree.nodes[assign_type.n.idx()].kind {
            NodeKind::Comprehension { .. } => 1,
            NodeKind::TypeVar { .. } => 2,
            NodeKind::ParamSpec { .. } => 3,
            NodeKind::TypeVarTuple { .. } => 4,
            NodeKind::TypeAlias { .. } => 5,
            NodeKind::Assign { .. } => 6,
            NodeKind::AnnAssign { .. } => 7,
            _ => 0,
        }
    };
    match at_kind {
        1 => return check_name(cx, "inlinevar", &name, node, false),
        2 => return check_name(cx, "typevar", &name, node, false),
        3 => return check_name(cx, "paramspec", &name, node, false),
        4 => return check_name(cx, "typevartuple", &name, node, false),
        5 => return check_name(cx, "typealias", &name, node, false),
        _ => {}
    }
    if u::is_module(eng, frame) {
        if at_kind == 7 {
            // AnnAssign: typealias annotation check first
            let annotation = {
                let md = eng.md(assign_type.m);
                match &md.tree.nodes[assign_type.n.idx()].kind {
                    NodeKind::AnnAssign { annotation, .. } => {
                        Some(GNode { m: assign_type.m, n: *annotation })
                    }
                    _ => None,
                }
            };
            if assigns_typealias(cx, annotation) {
                return check_name(cx, "typealias", &name, node, false);
            }
        }
        if at_kind == 6 || at_kind == 7 {
            let value: Option<GNode> = {
                let md = eng.md(assign_type.m);
                match &md.tree.nodes[assign_type.n.idx()].kind {
                    NodeKind::Assign { value, .. } => Some(GNode { m: assign_type.m, n: *value }),
                    NodeKind::AnnAssign { value, .. } => {
                        value.map(|v| GNode { m: assign_type.m, n: v })
                    }
                    _ => None,
                }
            };
            let inferred_assign_type: Option<Value> = match value {
                Some(v) => u::safe_infer(eng, cx.caches, v),
                None => None,
            };
            // single-name target TypeVar / TypeAlias
            let parent = eng.parent(node);
            let parent_is_assign = parent
                .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::Assign { .. })))
                .unwrap_or(false);
            if parent_is_assign {
                if let Some(tv) = assigns_typevar(cx, value) {
                    let target_name = first_target_name(eng, assign_type).unwrap_or_else(|| name.clone());
                    return check_name(cx, tv, &target_name, node, false);
                }
                if assigns_typealias(cx, value) {
                    let target_name = first_target_name(eng, assign_type).unwrap_or_else(|| name.clone());
                    return check_name(cx, "typealias", &target_name, node, false);
                }
            }
            // tuple unpacking with a literal-tuple RHS
            let parent_is_tuple = parent
                .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::Tuple { .. })))
                .unwrap_or(false);
            let value_is_tuple = value
                .map(|v| eng.kind_is(v, |k| matches!(k, NodeKind::Tuple { .. })))
                .unwrap_or(false);
            let mut tuple_branch_taken = false;
            if parent_is_tuple && value_is_tuple {
                let p = parent.unwrap();
                let v = value.unwrap();
                let idx = {
                    let md = eng.md(p.m);
                    match &md.tree.nodes[p.n.idx()].kind {
                        NodeKind::Tuple { elts, .. } => elts.iter().position(|&e| e == node.n),
                        _ => None,
                    }
                };
                let v_len = {
                    let md = eng.md(v.m);
                    match &md.tree.nodes[v.n.idx()].kind {
                        NodeKind::Tuple { elts, .. } => elts.len(),
                        _ => 0,
                    }
                };
                if let Some(idx) = idx {
                    if idx < v_len {
                        tuple_branch_taken = true;
                        let assigner = {
                            let md = eng.md(v.m);
                            match &md.tree.nodes[v.n.idx()].kind {
                                NodeKind::Tuple { elts, .. } => GNode { m: v.m, n: elts[idx] },
                                _ => return,
                            }
                        };
                        if let Some(tv) = assigns_typevar(cx, Some(assigner)) {
                            let tn = tuple_target_name(eng, assign_type, idx)
                                .unwrap_or_else(|| name.clone());
                            return check_name(cx, tv, &tn, node, false);
                        }
                        if assigns_typealias(cx, Some(assigner)) {
                            let tn = tuple_target_name(eng, assign_type, idx)
                                .unwrap_or_else(|| name.clone());
                            return check_name(cx, "typealias", &tn, node, false);
                        }
                        // neither matched -> fall out entirely (no check)
                        return;
                    }
                }
            }
            let _ = tuple_branch_taken;
            // elif chain continues only when the tuple branch was NOT taken
            let is_uninferable = matches!(inferred_assign_type, Some(Value::Uninferable));
            if inferred_assign_type.is_none() || is_uninferable {
                return;
            }
            let inferred = inferred_assign_type.unwrap();
            if should_check_class_regex(cx, &inferred) {
                return check_name(cx, "class", &name, node, false);
            }
            let redefines = redefines_import(eng, node);
            let is_func_alias = matches!(
                &inferred,
                Value::Node(g) if u::is_functiondef(eng, *g) || u::is_lambda(eng, *g)
            );
            let const_path = !redefines
                && !is_func_alias
                && !is_reassigned_relative(eng, node, &name, true)
                && !is_reassigned_relative(eng, node, &name, false)
                && u::first_ancestor(eng, node, |k| {
                    matches!(k, NodeKind::For(_) | NodeKind::AsyncFor(_) | NodeKind::While { .. })
                })
                .is_none();
            let inferred_is_const = matches!(&inferred, Value::SynthConst(_))
                || matches!(&inferred, Value::Node(g) if is_const_kind(eng, *g));
            let meets_exception = !inferred_is_const && match_snake(&name);
            if const_path {
                if !meets_exception {
                    check_name(cx, "const", &name, node, false);
                }
            } else {
                let mut node_type = "variable";
                // iattrs = tuple(frame.igetattr(name)) — full drain
                let sym = eng.sym(&name);
                let owner = Value::Node(frame);
                let mut vals: Vec<Value> = Vec::new();
                let _ = eng.igetattr_value_to(&owner, sym, None, &mut |v| {
                    vals.push(v);
                    pyinfer::value::Drive::Go
                });
                let has_uninferable = vals.iter().any(|v| v.is_uninferable());
                if has_uninferable && match_const(&name) {
                    return;
                }
                let attrs: Vec<GNode> = {
                    let md = eng.md(frame.m);
                    let l = md.locals.borrow();
                    l.get(&frame.n)
                        .and_then(|m| m.get(&sym))
                        .cloned()
                        .unwrap_or_default()
                };
                if attrs.len() > 1 {
                    let mut all_excl = true;
                    'outer: for i in 0..attrs.len() {
                        for j in (i + 1)..attrs.len() {
                            if !u::are_exclusive(eng, attrs[i], attrs[j]) {
                                all_excl = false;
                                break 'outer;
                            }
                        }
                    }
                    if all_excl {
                        node_type = "const";
                    }
                }
                if !meets_exception {
                    check_name(cx, node_type, &name, node, redefines);
                }
            }
        }
        // other assign types at module scope: unchecked
    } else if u::is_functiondef(eng, frame) {
        let sym = eng.sym(&name);
        let in_locals = {
            let md = eng.md(frame.m);
            let l = md.locals.borrow();
            l.get(&frame.n).map(|m| m.contains_key(&sym)).unwrap_or(false)
        };
        if in_locals && !argnames(eng, frame).contains(&name) {
            if !redefines_import(eng, node) {
                let annotation_typealias = if at_kind == 7 {
                    let annotation = {
                        let md = eng.md(assign_type.m);
                        match &md.tree.nodes[assign_type.n.idx()].kind {
                            NodeKind::AnnAssign { annotation, .. } => {
                                Some(GNode { m: assign_type.m, n: *annotation })
                            }
                            _ => None,
                        }
                    };
                    assigns_typealias(cx, annotation)
                } else {
                    false
                };
                if annotation_typealias {
                    check_name(cx, "typealias", &name, node, false);
                } else {
                    check_name(cx, "variable", &name, node, false);
                }
            }
        }
    } else if u::is_classdef(eng, frame) {
        let sym = eng.sym(&name);
        // `any(frame.local_attr_ancestors(node.name))` — ClassDef.
        // local_attr_ancestors (scoped_nodes.py) tries mro(context)[1:]
        // FIRST (recomputed per call; its base inference writes the global
        // cache — these repeated class-scope walks progressively warm
        // clamp-prone bases like sqlalchemy's `Mapped[_T_co]` so the later
        // C0116 ancestors() walk resolves them), falling back to a LAZY
        // ancestors() walk on MroError, where any() abandons the generator
        // at the first defining ancestor.
        let inherited = match eng.mro(frame, None) {
            Ok(m) => m
                .get(1..)
                .unwrap_or(&[])
                .iter()
                .any(|&anc| !eng.class_locals_get(anc, sym).is_empty()),
            Err(_) => {
                let mut found = false;
                let _ = eng.ancestors_to(frame, true, None, &mut |anc| {
                    if !eng.class_locals_get(anc, sym).is_empty() {
                        found = true;
                        return pyinfer::value::Drive::Stop;
                    }
                    pyinfer::value::Drive::Go
                });
                found
            }
        };
        if inherited {
            return;
        }
        if is_annotated_classvar_final(eng, node) {
            check_name(cx, "class_const", &name, node, false);
        } else if is_assign_name_annotated_with(eng, node, "Final") {
            if eng.is_dataclass_flag.borrow().contains(&frame) {
                check_name(cx, "class_attribute", &name, node, false);
            } else {
                check_name(cx, "class_const", &name, node, false);
            }
        } else if is_enum_member(eng, node, frame) {
            check_name(cx, "class_const", &name, node, false);
        } else {
            check_name(cx, "class_attribute", &name, node, false);
        }
    }
}

fn first_target_name(eng: &Engine, assign: GNode) -> Option<String> {
    let md = eng.md(assign.m);
    let t0 = match &md.tree.nodes[assign.n.idx()].kind {
        NodeKind::Assign { targets, .. } => *targets.first()?,
        _ => return None,
    };
    match &md.tree.nodes[t0.idx()].kind {
        NodeKind::AssignName { name } => Some(md.tree.s(*name).to_string()),
        _ => None,
    }
}

fn tuple_target_name(eng: &Engine, assign: GNode, idx: usize) -> Option<String> {
    let md = eng.md(assign.m);
    let t0 = match &md.tree.nodes[assign.n.idx()].kind {
        NodeKind::Assign { targets, .. } => *targets.first()?,
        _ => return None,
    };
    let elt = match &md.tree.nodes[t0.idx()].kind {
        NodeKind::Tuple { elts, .. } => *elts.get(idx)?,
        _ => return None,
    };
    match &md.tree.nodes[elt.idx()].kind {
        NodeKind::AssignName { name } => Some(md.tree.s(*name).to_string()),
        _ => None,
    }
}

/// _check_typevar — C0105/C0131/C0132
fn check_typevar(cx: &mut WalkCx, name: &str, node: GNode) {
    let eng = cx.eng;
    #[derive(PartialEq, Clone, Copy)]
    enum Variance {
        Invariant,
        Covariant,
        Contravariant,
        DoubleVariant,
        Inferred,
    }
    let parent = eng.parent(node);
    let parent_kind = parent.map(|p| {
        let md = eng.md(p.m);
        match &md.tree.nodes[p.n.idx()].kind {
            NodeKind::Assign { .. } => 1,
            NodeKind::Tuple { .. } => 2,
            _ => 0,
        }
    });
    let mut variance = Variance::Invariant;
    let (keywords, args): (Vec<NodeId>, Vec<NodeId>) = match parent_kind {
        Some(1) => {
            let at = assign_type_of(eng, node);
            let md = eng.md(at.m);
            let value = match &md.tree.nodes[at.n.idx()].kind {
                NodeKind::Assign { value, .. } => *value,
                _ => {
                    variance = Variance::Inferred;
                    NodeId::MODULE
                }
            };
            if variance == Variance::Inferred {
                (Vec::new(), Vec::new())
            } else {
                match &md.tree.nodes[value.idx()].kind {
                    NodeKind::Call { args, keywords, .. } => (keywords.clone(), args.clone()),
                    _ => {
                        // .keywords on a non-Call -> AttributeError -> crash
                        drop(md);
                        cx.crashed.set(true);
                        return;
                    }
                }
            }
        }
        Some(2) => {
            let at = assign_type_of(eng, node);
            let p = parent.unwrap();
            let idx = {
                let md = eng.md(p.m);
                match &md.tree.nodes[p.n.idx()].kind {
                    NodeKind::Tuple { elts, .. } => {
                        elts.iter().position(|&e| e == node.n).unwrap_or(0)
                    }
                    _ => 0,
                }
            };
            let md = eng.md(at.m);
            let value = match &md.tree.nodes[at.n.idx()].kind {
                NodeKind::Assign { value, .. } => *value,
                _ => {
                    drop(md);
                    cx.crashed.set(true);
                    return;
                }
            };
            let elt = match &md.tree.nodes[value.idx()].kind {
                NodeKind::Tuple { elts, .. } => match elts.get(idx) {
                    Some(&e) => e,
                    None => {
                        drop(md);
                        cx.crashed.set(true);
                        return;
                    }
                },
                _ => {
                    drop(md);
                    cx.crashed.set(true);
                    return;
                }
            };
            match &md.tree.nodes[elt.idx()].kind {
                NodeKind::Call { args, keywords, .. } => (keywords.clone(), args.clone()),
                _ => {
                    drop(md);
                    cx.crashed.set(true);
                    return;
                }
            }
        }
        _ => {
            variance = Variance::Inferred;
            (Vec::new(), Vec::new())
        }
    };
    let mut name_arg: Option<ConstValue> = None;
    for kw in &keywords {
        let (arg, val): (Option<String>, NodeId) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[kw.idx()].kind {
                NodeKind::Keyword { arg, value } => {
                    (arg.map(|s| md.tree.s(s).to_string()), *value)
                }
                _ => continue,
            }
        };
        let truthy = || -> Option<bool> {
            let md = eng.md(node.m);
            match &md.tree.nodes[val.idx()].kind {
                NodeKind::Const(c) => Some(u::const_truthy(c)),
                _ => None,
            }
        };
        if variance == Variance::DoubleVariant {
            // pass
        } else if arg.as_deref() == Some("covariant") {
            match truthy() {
                Some(true) => {
                    variance = if variance != Variance::Contravariant {
                        Variance::Covariant
                    } else {
                        Variance::DoubleVariant
                    };
                }
                Some(false) => {}
                None => {
                    // kw.value.value AttributeError -> crash
                    cx.crashed.set(true);
                    return;
                }
            }
        } else if arg.as_deref() == Some("contravariant") {
            match truthy() {
                Some(true) => {
                    variance = if variance != Variance::Covariant {
                        Variance::Contravariant
                    } else {
                        Variance::DoubleVariant
                    };
                }
                Some(false) => {}
                None => {
                    cx.crashed.set(true);
                    return;
                }
            }
        }
        if arg.as_deref() == Some("name") {
            let md = eng.md(node.m);
            if let NodeKind::Const(c) = &md.tree.nodes[val.idx()].kind {
                name_arg = Some(c.clone());
            }
        }
    }
    if name_arg.is_none() {
        if let Some(&a0) = args.first() {
            let md = eng.md(node.m);
            if let NodeKind::Const(c) = &md.tree.nodes[a0.idx()].kind {
                name_arg = Some(c.clone());
            }
        }
    }
    match variance {
        Variance::Inferred => {}
        Variance::DoubleVariant => {
            emit(
                cx,
                "C0131",
                node,
                "TypeVar cannot be both covariant and contravariant".into(),
            );
            emit(cx, "C0105", node, "Type variable name does not reflect variance".into());
        }
        Variance::Covariant => {
            if !name.ends_with("_co") {
                let base = name.strip_suffix("_contra").unwrap_or(name);
                let suggest = format!("{base}_co");
                emit(
                    cx,
                    "C0105",
                    node,
                    format!(
                        "Type variable name does not reflect variance. \"{name}\" is covariant, use \"{suggest}\" instead"
                    ),
                );
            }
        }
        Variance::Contravariant => {
            if !name.ends_with("_contra") {
                let base = name.strip_suffix("_co").unwrap_or(name);
                let suggest = format!("{base}_contra");
                emit(
                    cx,
                    "C0105",
                    node,
                    format!(
                        "Type variable name does not reflect variance. \"{name}\" is contravariant, use \"{suggest}\" instead"
                    ),
                );
            }
        }
        Variance::Invariant => {
            if name.ends_with("_co") || name.ends_with("_contra") {
                let suggest = name
                    .strip_suffix("_contra")
                    .or_else(|| name.strip_suffix("_co"))
                    .unwrap_or(name);
                emit(
                    cx,
                    "C0105",
                    node,
                    format!(
                        "Type variable name does not reflect variance. \"{name}\" is invariant, use \"{suggest}\" instead"
                    ),
                );
            }
        }
    }
    if let Some(na) = name_arg {
        let na_str = const_str(&na);
        let differs = match &na {
            ConstValue::Str(s) => s.as_ref() != name,
            _ => true,
        };
        if differs {
            emit(
                cx,
                "C0132",
                node,
                format!("TypeVar name \"{na_str}\" does not match assigned variable name \"{name}\""),
            );
        }
    }
}

// ===========================================================================
// DocStringChecker — C0112/C0114/C0115/C0116
// ===========================================================================

pub fn doc_visit_module(cx: &mut WalkCx, node: GNode) {
    check_docstring(cx, "module", node, true);
}

pub fn doc_visit_classdef(cx: &mut WalkCx, node: GNode) {
    let name = cx.eng.node_name(node).unwrap_or_default();
    if !name.starts_with('_') {
        check_docstring(cx, "class", node, true);
    }
}

pub fn doc_visit_functiondef(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let name = eng.node_name(node).unwrap_or_default();
    if name.starts_with('_') {
        return;
    }
    let ftype = if is_method(eng, node) { "method" } else { "function" };
    if is_property_setter_or_deleter(eng, node) {
        return;
    }
    // is_overload_stub: decorated with typing.overload
    if crate::typecheck::decorated_with(eng, node, &["typing.overload", "overload"]) {
        return;
    }
    let parent_frame = eng.frame(eng.parent(node).unwrap_or(node));
    if u::is_classdef(eng, parent_frame) {
        // burn parity: has_known_bases for the confidence value
        let _ = crate::typecheck::has_known_bases(eng, cx.caches, parent_frame);
        let mut overridden = false;
        let sym = eng.sym(&name);
        for anc in eng.ancestors(parent_frame, true, None) {
            if eng.qname(anc) == "builtins.object" {
                continue;
            }
            let locals = eng.class_locals_get(anc, sym);
            if let Some(&first) = locals.first() {
                if u::is_functiondef(eng, first) {
                    overridden = true;
                    break;
                }
            }
        }
        check_docstring(cx, ftype, node, !overridden);
    } else if u::is_module(eng, parent_frame) {
        check_docstring(cx, ftype, node, true);
    }
}

/// utils.get_node_last_lineno
fn get_node_last_lineno(eng: &Engine, node: GNode) -> u32 {
    let last: Option<NodeId> = {
        let md = eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Try(d) | NodeKind::TryStar(d) => d
                .finalbody
                .last()
                .or(d.orelse.last())
                .or(d.handlers.last())
                .or(d.body.last())
                .copied(),
            NodeKind::For(d) | NodeKind::AsyncFor(d) => {
                d.orelse.last().or(d.body.last()).copied()
            }
            NodeKind::While { body, orelse, .. } | NodeKind::If { body, orelse, .. } => {
                orelse.last().or(body.last()).copied()
            }
            NodeKind::Module(d) => d.body.last().copied(),
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.last().copied(),
            NodeKind::ClassDef(d) => d.body.last().copied(),
            NodeKind::With(d) | NodeKind::AsyncWith(d) => d.body.last().copied(),
            NodeKind::ExceptHandler { body, .. } => body.last().copied(),
            NodeKind::Match { cases, .. } => cases.last().copied(),
            NodeKind::MatchCase { body, .. } => body.last().copied(),
            _ => None,
        }
    };
    match last {
        Some(l) => get_node_last_lineno(eng, GNode { m: node.m, n: l }),
        None => u::lineno(eng, node),
    }
}

fn check_docstring(cx: &mut WalkCx, node_type: &str, node: GNode, report_missing: bool) {
    let eng = cx.eng;
    // doc_node string
    let doc: Option<String> = {
        let md = eng.md(node.m);
        let doc_n = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Module(d) => d.doc_node,
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.doc_node,
            NodeKind::ClassDef(d) => d.doc_node,
            _ => None,
        };
        doc_n.and_then(|dn| match &md.tree.nodes[dn.idx()].kind {
            NodeKind::Const(ConstValue::Str(s)) => Some(s.to_string()),
            _ => None,
        })
    };
    let doc: Option<String> = match doc {
        Some(d) => Some(d),
        None => infer_dunder_doc_attribute(cx, node),
    };
    match doc {
        None => {
            if !report_missing {
                return;
            }
            let node_lineno = if u::is_module(eng, node) { 0 } else { u::lineno(eng, node) };
            let lines = get_node_last_lineno(eng, node).saturating_sub(node_lineno);
            if node_type == "module" && lines == 0 {
                return;
            }
            // docstring-min-length default -1: no gate
            // str.format() heuristic on the first body statement
            let first_call: Option<GNode> = {
                let md = eng.md(node.m);
                let body: &Vec<NodeId> = match &md.tree.nodes[node.n.idx()].kind {
                    NodeKind::Module(d) => &d.body,
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => &d.body,
                    NodeKind::ClassDef(d) => &d.body,
                    _ => return,
                };
                body.first().and_then(|&b| match &md.tree.nodes[b.idx()].kind {
                    NodeKind::Expr { value } => {
                        match &md.tree.nodes[value.idx()].kind {
                            NodeKind::Call { func, .. } => Some(GNode { m: node.m, n: *func }),
                            _ => None,
                        }
                    }
                    _ => None,
                })
            };
            if let Some(func) = first_call {
                if let Some(Value::BoundMethod { bound, .. }) = u::safe_infer(eng, cx.caches, func)
                {
                    if let Some(cls) = eng.proxied_class(&bound) {
                        if matches!(
                            eng.node_name(cls).as_deref(),
                            Some("str") | Some("unicode") | Some("bytes")
                        ) {
                            return;
                        }
                    }
                }
            }
            let (msgid, text): (&'static str, &str) = match node_type {
                "module" => ("C0114", "Missing module docstring"),
                "class" => ("C0115", "Missing class docstring"),
                _ => ("C0116", "Missing function or method docstring"),
            };
            emit(cx, msgid, node, text.to_string());
        }
        Some(d) => {
            if d.trim().is_empty() {
                emit(cx, "C0112", node, format!("Empty {node_type} docstring"));
            }
        }
    }
}

/// _infer_dunder_doc_attribute: node["__doc__"] -> safe_infer -> Const ->
/// str(value)
fn infer_dunder_doc_attribute(cx: &mut WalkCx, node: GNode) -> Option<String> {
    let eng = cx.eng;
    let sym = eng.sym("__doc__");
    let first: GNode = {
        let md = eng.md(node.m);
        let l = md.locals.borrow();
        *l.get(&node.n)?.get(&sym)?.first()?
    };
    let v = u::safe_infer(eng, cx.caches, first)?;
    match v {
        Value::Node(g) => const_of(eng, g).map(|c| const_str(&c)),
        Value::SynthConst(c) => Some(const_str(&c)),
        _ => None,
    }
}

// ===========================================================================
// FunctionChecker — W0135 contextmanager-generator-missing-cleanup
// ===========================================================================

pub fn function_visit_functiondef(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    // With includes AsyncWith (astroid subclass)
    let with_nodes = nodes_of_class(
        eng,
        node,
        |k| matches!(k, NodeKind::With(_) | NodeKind::AsyncWith(_)),
        |_| false,
    );
    if with_nodes.is_empty() {
        return;
    }
    let mut yield_nodes: Vec<GNode> = Vec::new();
    for &wn in &with_nodes {
        yield_nodes.extend(nodes_of_class(
            eng,
            wn,
            |k| matches!(k, NodeKind::Yield { .. } | NodeKind::YieldFrom { .. }),
            |_| false,
        ));
    }
    if yield_nodes.is_empty() {
        return;
    }
    let func_name = eng.node_name(node).unwrap_or_default();
    for &with_node in &with_nodes {
        let items: Vec<(NodeId, Option<NodeId>)> = {
            let md = eng.md(with_node.m);
            match &md.tree.nodes[with_node.n.idx()].kind {
                NodeKind::With(d) | NodeKind::AsyncWith(d) => d.items.clone(),
                _ => continue,
            }
        };
        for (call, held) in items {
            if held.is_none() {
                continue;
            }
            let cg = GNode { m: with_node.m, n: call };
            let inferred = u::safe_infer(eng, cx.caches, cg);
            // getattr(inferred, "parent", None)
            let inferred_node: Option<GNode> = match &inferred {
                Some(Value::Generator { func, .. }) => Some(*func),
                Some(Value::Node(g)) => eng.parent(*g),
                Some(Value::Inst { cls, .. }) | Some(Value::ExcInst { cls, .. }) => {
                    eng.parent(*cls)
                }
                Some(Value::BoundMethod { func, .. })
                | Some(Value::DescBM { func, .. })
                | Some(Value::UnboundMethod { func })
                | Some(Value::Property { func, .. }) => eng.parent(*func),
                Some(Value::Partial { parent, .. }) => *parent,
                _ => None,
            };
            let Some(cm_func) = inferred_node else { continue };
            if !u::is_functiondef(eng, cm_func) {
                continue;
            }
            if node_fails_contextmanager_cleanup(cx, cm_func, &yield_nodes) {
                emit(
                    cx,
                    "W0135",
                    with_node,
                    format!(
                        "The context used in function {} will not be exited.",
                        u::py_repr_str(&func_name)
                    ),
                );
            }
        }
    }
}

fn node_fails_contextmanager_cleanup(
    cx: &mut WalkCx,
    cm_func: GNode,
    caller_yields: &[GNode],
) -> bool {
    let eng = cx.eng;
    // 1. any caller-yield bare or yielding a Const -> no message
    for &y in caller_yields {
        let md = eng.md(y.m);
        match &md.tree.nodes[y.n.idx()].kind {
            NodeKind::Yield { value } => match value {
                None => return false,
                Some(v) => {
                    if matches!(md.tree.nodes[v.idx()].kind, NodeKind::Const(_)) {
                        return false;
                    }
                }
            },
            NodeKind::YieldFrom { value } => {
                if matches!(md.tree.nodes[value.idx()].kind, NodeKind::Const(_)) {
                    return false;
                }
            }
            _ => {}
        }
    }
    // 2. single yield that is the last statement of the CM function
    let cm_yields = nodes_of_class(
        eng,
        cm_func,
        |k| matches!(k, NodeKind::Yield { .. } | NodeKind::YieldFrom { .. }),
        |_| false,
    );
    if cm_yields.len() == 1 {
        let mut n = eng.parent(cm_yields[0]).unwrap_or(cm_yields[0]);
        let mut reached_top = true;
        while n != cm_func {
            if u::next_sibling(eng, n).is_some() {
                reached_top = false;
                break;
            }
            match eng.parent(n) {
                Some(p) => n = p,
                None => break,
            }
        }
        if reached_top {
            return false;
        }
    }
    // 3. try blocks containing a yield
    let trys: Vec<GNode> = nodes_of_class(eng, cm_func, |k| matches!(k, NodeKind::Try(_)), |_| false)
        .into_iter()
        .filter(|&t| {
            !nodes_of_class(
                eng,
                t,
                |k| matches!(k, NodeKind::Yield { .. } | NodeKind::YieldFrom { .. }),
                |_| false,
            )
            .is_empty()
        })
        .collect();
    if trys.is_empty() {
        return true;
    }
    let all_final = trys.iter().all(|&t| {
        let md = eng.md(t.m);
        match &md.tree.nodes[t.n.idx()].kind {
            NodeKind::Try(d) => !d.finalbody.is_empty(),
            _ => false,
        }
    });
    if all_final {
        return false;
    }
    let all_handled = trys.iter().all(|&t| {
        let handlers: Vec<NodeId> = {
            let md = eng.md(t.m);
            match &md.tree.nodes[t.n.idx()].kind {
                NodeKind::Try(d) => d.handlers.clone(),
                _ => Vec::new(),
            }
        };
        handlers.iter().any(|&h| {
            let type_: Option<NodeId> = {
                let md = eng.md(t.m);
                match &md.tree.nodes[h.idx()].kind {
                    NodeKind::ExceptHandler { type_, .. } => *type_,
                    _ => None,
                }
            };
            match type_ {
                None => true, // bare except
                Some(tn) => {
                    let tg = GNode { m: t.m, n: tn };
                    match u::safe_infer(eng, cx.caches, tg) {
                        Some(v) => matches!(
                            u::value_qname(eng, &v).as_deref(),
                            Some("builtins.GeneratorExit") | Some("builtins.Exception")
                        ),
                        None => false,
                    }
                }
            }
        })
    });
    if all_handled {
        return false;
    }
    true
}
