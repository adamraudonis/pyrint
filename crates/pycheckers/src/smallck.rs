//! Small full-mode AST checkers (specs: notes/09-format.md §5-8,
//! notes/09-misc-wc.md §9-§14):
//! - DunderCallChecker (name "unnecessary-dunder-call") — C2801
//! - ThreadingChecker (name "threading") — W2101
//! - LambdaExpressionChecker (name "lambda-expressions") — C3001/C3002
//! - NestedMinMaxChecker (name "nested_min_max") — W3301
//! - BadChainedComparisonChecker (name "bad-chained-comparison") — W3601

use pyast::tree::NodeKind;
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, Value};

use crate::ckutils as u;
use crate::depdata;
use crate::walker::WalkCx;

fn is_funcdef(eng: &Engine, g: GNode) -> bool {
    matches!(
        &eng.md(g.m).tree.nodes[g.n.idx()].kind,
        NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
    )
}

/// `isinstance(v, astroid.bases.Instance)`: Instance proper, plus the
/// Instance SUBCLASS nodes Const and BaseContainer (List/Tuple/Set/Dict)
/// and objects.FrozenSet. Generator/BoundMethod are BaseInstance/Proxy —
/// NOT Instance.
fn value_is_instance(eng: &Engine, v: &Value) -> bool {
    match v {
        Value::Inst { .. } | Value::ExcInst { .. } => true,
        Value::SynthConst(_)
        | Value::SynthSeq { .. }
        | Value::SynthDict { .. }
        | Value::FrozenSet { .. } => true,
        Value::Node(g) => matches!(
            &eng.md(g.m).tree.nodes[g.n.idx()].kind,
            NodeKind::Const(_)
                | NodeKind::List { .. }
                | NodeKind::Tuple { .. }
                | NodeKind::Set { .. }
                | NodeKind::Dict { .. }
        ),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// C2801 unnecessary-dunder-call (dunder_methods.py)
// ---------------------------------------------------------------------------

pub struct DunderCallCk;

impl DunderCallCk {
    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (expr, attrname): (GNode, String) = {
            let md = eng.md(node.m);
            let NodeKind::Call { func, .. } = &md.tree.nodes[node.n.idx()].kind else { return };
            match &md.tree.nodes[func.idx()].kind {
                NodeKind::Attribute { expr, attrname, .. } => {
                    (GNode { m: node.m, n: *expr }, md.tree.s(*attrname).to_string())
                }
                _ => return,
            }
        };
        let Some(hint) = depdata::DUNDER_METHODS
            .iter()
            .find(|(k, _)| *k == attrname)
            .map(|(_, v)| *v)
        else {
            return;
        };
        if self.within_dunder_or_lambda_def(eng, node, &attrname) {
            return;
        }
        // direct super() receiver (super(C, self) also matches: func is
        // still Name "super")
        {
            let md = eng.md(expr.m);
            if let NodeKind::Call { func: efunc, .. } = &md.tree.nodes[expr.n.idx()].kind {
                if let NodeKind::Name { name } = &md.tree.nodes[efunc.idx()].kind {
                    if md.tree.s(*name) == "super" {
                        return;
                    }
                }
            }
        }
        // inference gate: emit only for None / Uninferable / Instance
        if let Some(v) = u::safe_infer(eng, cx.caches, expr) {
            if !v.is_uninferable() && !value_is_instance(eng, &v) {
                return;
            }
        }
        cx.emit_node(
            "C2801",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            u::format_template("Unnecessarily calls dunder method %s. %s.", &[&attrname, hint]),
        );
    }

    /// `within_dunder_or_lambda_def` (dunder_methods.py:53-65).
    fn within_dunder_or_lambda_def(&self, eng: &Engine, node: GNode, attrname: &str) -> bool {
        let mut cur = node;
        while let Some(p) = eng.parent(cur) {
            let md = eng.md(p.m);
            match &md.tree.nodes[p.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    let name = md.tree.s(d.name);
                    if name.starts_with("__") && name.ends_with("__") {
                        return true;
                    }
                }
                NodeKind::Lambda(_) => {
                    if depdata::DUNDER_LAMBDA_EXCEPTIONS.contains(&attrname) {
                        return true;
                    }
                }
                _ => {}
            }
            cur = p;
        }
        false
    }
}

// ---------------------------------------------------------------------------
// W2101 useless-with-lock (threading_checker.py)
// ---------------------------------------------------------------------------

pub struct ThreadingCk;

const LOCKS: &[&str] = &[
    "threading.Lock",
    "threading.RLock",
    "threading.Condition",
    "threading.Semaphore",
    "threading.BoundedSemaphore",
];

impl ThreadingCk {
    /// visit_with only — AsyncWith is NOT visited.
    pub fn visit_with(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let items: Vec<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::With(d) => d.items.iter().map(|it| GNode { m: node.m, n: it.0 }).collect(),
                _ => return,
            }
        };
        for cm in items {
            let func = {
                let md = eng.md(cm.m);
                match &md.tree.nodes[cm.n.idx()].kind {
                    NodeKind::Call { func, .. } => GNode { m: cm.m, n: *func },
                    _ => continue,
                }
            };
            let Some(v) = u::safe_infer(eng, cx.caches, func) else { continue };
            let Some(q) = u::value_qname(eng, &v) else { continue };
            if LOCKS.contains(&q.as_str()) {
                cx.emit_node(
                    "W2101",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    u::format_template("'%s()' directly created in 'with' has no effect", &[&q]),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// C3001 / C3002 (lambda_expressions.py)
// ---------------------------------------------------------------------------

pub struct LambdaExprCk;

const T_C3001: &str = "Lambda expression assigned to a variable. Define a function using the \"def\" keyword instead.";
const T_C3002: &str = "Lambda expression called directly. Execute the expression inline instead.";

impl LambdaExprCk {
    fn is_lambda(eng: &Engine, g: GNode) -> bool {
        matches!(&eng.md(g.m).tree.nodes[g.n.idx()].kind, NodeKind::Lambda(_))
    }

    pub fn visit_assign(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (first_target, value) = {
            let md = eng.md(node.m);
            let NodeKind::Assign { targets, value } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            let Some(&t0) = targets.first() else { return };
            (GNode { m: node.m, n: t0 }, GNode { m: node.m, n: *value })
        };
        let md = eng.md(node.m);
        match &md.tree.nodes[first_target.n.idx()].kind {
            NodeKind::AssignName { .. } => {
                if Self::is_lambda(eng, value) {
                    cx.emit_node(
                        "C3001",
                        u::lineno(eng, value),
                        u::col_offset(eng, value) as i64,
                        T_C3001.to_string(),
                    );
                }
            }
            NodeKind::Tuple { elts: lhs, .. } => {
                let rhs: Vec<pyast::NodeId> = match &md.tree.nodes[value.n.idx()].kind {
                    NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => elts.clone(),
                    _ => return,
                };
                let lhs = lhs.clone();
                for i in 0.. {
                    // zip_longest with break at the first one-sided None
                    if i >= lhs.len() || i >= rhs.len() {
                        break;
                    }
                    let l = GNode { m: node.m, n: lhs[i] };
                    let r = GNode { m: node.m, n: rhs[i] };
                    if matches!(&md.tree.nodes[l.n.idx()].kind, NodeKind::AssignName { .. })
                        && Self::is_lambda(eng, r)
                    {
                        cx.emit_node(
                            "C3001",
                            u::lineno(eng, r),
                            u::col_offset(eng, r) as i64,
                            T_C3001.to_string(),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    pub fn visit_namedexpr(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (target, value) = {
            let md = eng.md(node.m);
            let NodeKind::NamedExpr { target, value } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            (GNode { m: node.m, n: *target }, GNode { m: node.m, n: *value })
        };
        let is_an = matches!(
            &eng.md(target.m).tree.nodes[target.n.idx()].kind,
            NodeKind::AssignName { .. }
        );
        if is_an && Self::is_lambda(eng, value) {
            cx.emit_node(
                "C3001",
                u::lineno(eng, value),
                u::col_offset(eng, value) as i64,
                T_C3001.to_string(),
            );
        }
    }

    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let func = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Call { func, .. } => GNode { m: node.m, n: *func },
                _ => return,
            }
        };
        if Self::is_lambda(eng, func) {
            cx.emit_node(
                "C3002",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                T_C3002.to_string(),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// W3301 nested-min-max (nested_min_max.py)
// ---------------------------------------------------------------------------

pub struct NestedMinMaxCk;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Elem {
    Plain(GNode),
    Splat(GNode),
}

impl NestedMinMaxCk {
    /// safe_infer(func) is a FunctionDef with qname builtins.min/max.
    fn inferred_min_max(cx: &mut WalkCx, func: GNode) -> Option<GNode> {
        let eng = cx.eng;
        match u::safe_infer(eng, cx.caches, func) {
            Some(Value::Node(g)) if is_funcdef(eng, g) => {
                let q = eng.qname(g);
                if q == "builtins.min" || q == "builtins.max" {
                    Some(g)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn call_args(eng: &Engine, g: GNode) -> Option<Vec<GNode>> {
        let md = eng.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Call { args, .. } => {
                Some(args.iter().map(|&a| GNode { m: g.m, n: a }).collect())
            }
            _ => None,
        }
    }

    /// get_redundant_calls (nested_min_max.py:60-76): qname comparison on
    /// bound methods == identity of the inferred FunctionDef node.
    fn redundant_calls(cx: &mut WalkCx, args: &[Elem], outer: GNode) -> Vec<GNode> {
        let eng = cx.eng;
        let mut out = Vec::new();
        for e in args {
            let Elem::Plain(a) = *e else { continue };
            let Some(func) = ({
                let md = eng.md(a.m);
                match &md.tree.nodes[a.n.idx()].kind {
                    NodeKind::Call { func, .. } => Some(GNode { m: a.m, n: *func }),
                    _ => None,
                }
            }) else {
                continue;
            };
            let Some(inf) = Self::inferred_min_max(cx, func) else { continue };
            if inf != outer {
                continue;
            }
            // len(arg.parent.args) > 1 — the REAL parent Call in the tree
            let Some(parent) = eng.parent(a) else { continue };
            let plen = match &eng.md(parent.m).tree.nodes[parent.n.idx()].kind {
                NodeKind::Call { args, .. } => args.len(),
                _ => continue,
            };
            if plen > 1 {
                out.push(a);
            }
        }
        out
    }

    fn is_const(eng: &Engine, g: GNode) -> bool {
        matches!(&eng.md(g.m).tree.nodes[g.n.idx()].kind, NodeKind::Const(_))
    }

    /// `_is_splattable_expression` (nested_min_max.py:141-172).
    fn is_splattable(cx: &mut WalkCx, arg: GNode) -> bool {
        let eng = cx.eng;
        {
            let md = eng.md(arg.m);
            if let NodeKind::BinOp { left, op, right } = &md.tree.nodes[arg.n.idx()].kind {
                if &**op == "+" || &**op == "|" {
                    let (l, r) = (GNode { m: arg.m, n: *left }, GNode { m: arg.m, n: *right });
                    return Self::is_splattable(cx, l) && Self::is_splattable(cx, r);
                }
            }
        }
        let inferred = u::safe_infer(eng, cx.caches, arg);
        if let Some(v) = &inferred {
            if !v.is_uninferable() {
                if let Some(pt) = u::value_pytype(eng, v) {
                    if pt == "builtins.list" || pt == "builtins.tuple" {
                        return true;
                    }
                }
            }
        }
        // isinstance(inferred or arg, (List, Tuple, Set, ListComp, DictComp,
        // DictValues, DictKeys, DictItems, Dict)) — note: NO SetComp/GenExp;
        // falsy `inferred` (None/Uninferable) falls back to the SYNTAX node.
        let node_matches = |g: GNode| {
            matches!(
                &eng.md(g.m).tree.nodes[g.n.idx()].kind,
                NodeKind::List { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::Set { .. }
                    | NodeKind::ListComp(_)
                    | NodeKind::DictComp(_)
                    | NodeKind::Dict { .. }
            )
        };
        match &inferred {
            Some(v) if !v.is_uninferable() => match v {
                Value::Node(g) => node_matches(*g),
                Value::SynthSeq { .. } | Value::SynthDict { .. } => true,
                Value::DictItems(_) | Value::DictKeys(_) | Value::DictValues(_) => true,
                _ => false,
            },
            _ => node_matches(arg),
        }
    }

    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (func, keywords): (GNode, Vec<GNode>) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Call { func, keywords, .. } => (
                    GNode { m: node.m, n: *func },
                    keywords.iter().map(|&k| GNode { m: node.m, n: k }).collect(),
                ),
                _ => return,
            }
        };
        let Some(inferred) = Self::inferred_min_max(cx, func) else { return };
        let Some(args0) = Self::call_args(eng, node) else { return };
        let mut fixed: Vec<Elem> = args0.into_iter().map(Elem::Plain).collect();
        let mut redundant = Self::redundant_calls(cx, &fixed, inferred);
        if redundant.is_empty() {
            return;
        }
        while !redundant.is_empty() {
            for i in 0..fixed.len() {
                let Elem::Plain(a) = fixed[i] else { continue };
                // genexp bail: ANY Call arg of fixed_node with a
                // GeneratorExp argument kills the whole message
                if let Some(inner) = Self::call_args(eng, a) {
                    let has_genexp = inner.iter().any(|&x| {
                        matches!(
                            &eng.md(x.m).tree.nodes[x.n.idx()].kind,
                            NodeKind::GeneratorExp(_)
                        )
                    });
                    if has_genexp {
                        return;
                    }
                    if redundant.contains(&a) {
                        let spliced: Vec<Elem> = inner.into_iter().map(Elem::Plain).collect();
                        let mut newargs = fixed[..i].to_vec();
                        newargs.extend(spliced);
                        newargs.extend_from_slice(&fixed[i + 1..]);
                        fixed = newargs;
                        break;
                    }
                }
            }
            redundant = Self::redundant_calls(cx, &fixed, inferred);
        }
        // splat pass: enumerate over the STALE list (snapshot), rebind
        // `fixed` with the empty-tail-slice bug ([idx+1:idx] == []).
        let snapshot = fixed.clone();
        for (idx, e) in snapshot.iter().enumerate() {
            let Elem::Plain(a) = *e else { continue };
            if !Self::is_const(eng, a) && Self::is_splattable(cx, a) {
                let keep = idx.min(fixed.len());
                let mut newargs = fixed[..keep].to_vec();
                newargs.push(Elem::Splat(a));
                fixed = newargs;
            }
        }
        let func_name: String = {
            let md = eng.md(func.m);
            match &md.tree.nodes[func.n.idx()].kind {
                NodeKind::Attribute { attrname, .. } => md.tree.s(*attrname).to_string(),
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return, // pylint would AttributeError-crash; unreachable
            }
        };
        // fixed_node.as_string(): Call rendering = func(args..., keywords...)
        let mut parts: Vec<String> = fixed
            .iter()
            .map(|e| match e {
                Elem::Plain(g) => u::as_string(eng, *g),
                Elem::Splat(g) => format!("*{}", u::as_string(eng, *g)),
            })
            .collect();
        for kw in &keywords {
            parts.push(u::as_string(eng, *kw));
        }
        let suggestion = format!("{}({})", u::as_string(eng, func), parts.join(", "));
        cx.emit_node(
            "W3301",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            u::format_template(
                "Do not use nested call of '%s'; it's possible to do '%s' instead",
                &[&func_name, &suggestion],
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// W3601 bad-chained-comparison (bad_chained_comparison.py)
// ---------------------------------------------------------------------------

pub struct ChainedCompCk;

const COMPARISON_OP: &[&str] = &["<", "<=", ">", ">=", "!=", "=="];
const IDENTITY_OP: &[&str] = &["is", "is not"];
const MEMBERSHIP_OP: &[&str] = &["in", "not in"];

impl ChainedCompCk {
    pub fn visit_compare(&mut self, cx: &mut WalkCx, node: GNode) {
        let ops: Vec<String> = {
            let md = cx.eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Compare { ops, .. } => ops.iter().map(|(o, _)| o.to_string()).collect(),
                _ => return,
            }
        };
        let num_parts = ops.len();
        // sorted unique operator strings
        let mut operators: Vec<&str> = ops.iter().map(|s| s.as_str()).collect();
        operators.sort_unstable();
        operators.dedup();
        if operators.is_empty() {
            return;
        }
        // group = the semantic group of operators[0]
        let mut group: &[&str] = COMPARISON_OP;
        for sg in [COMPARISON_OP, IDENTITY_OP, MEMBERSHIP_OP] {
            if sg.contains(&operators[0]) {
                group = sg;
            }
        }
        if operators.iter().all(|o| group.contains(o)) {
            return;
        }
        let incompatibles = if operators.len() == 1 {
            format!(" and '{}'", operators[0])
        } else {
            let head = operators[..operators.len() - 1]
                .iter()
                .map(|o| format!("'{o}'"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} and '{}'", head, operators[operators.len() - 1])
        };
        cx.emit_node(
            "W3601",
            u::lineno(cx.eng, node),
            u::col_offset(cx.eng, node) as i64,
            u::format_template(
                "Suspicious %s-part chained comparison using semantically incompatible operators (%s)",
                &[&num_parts.to_string(), &incompatibles],
            ),
        );
    }
}
