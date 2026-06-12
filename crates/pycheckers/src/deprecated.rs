//! Port of pylint/checkers/deprecated.py — the DeprecatedMixin framework
//! (W4901-W4906), spec notes/09-misc-wc.md §2 + §3.1/§3.12.
//!
//! Implementors wired in this port:
//! - StdlibChecker: W4902/W4903 (check_deprecated_method from its visit_call
//!   override), W4904 (class-in-call + import paths), W4905 (decorators),
//!   W4906 (attributes). Its deprecated_modules() is empty.
//! - ImportsChecker: W4901 only (visit_import per name, visit_importfrom on
//!   the absolute name, and the mixin visit_call `__import__("x")` path).

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
fn is_classdef(eng: &Engine, g: GNode) -> bool {
    matches!(&eng.md(g.m).tree.nodes[g.n.idx()].kind, NodeKind::ClassDef(_))
}

/// check_deprecated_module (deprecated.py:237-243) — W4901, one message per
/// matching table entry (iteration order = the seed-0 set order baked into
/// depdata). Only ImportsChecker has module data.
pub fn check_deprecated_module(cx: &mut WalkCx, node: GNode, mod_path: &str) {
    for mod_name in depdata::DEPRECATED_MODULES {
        if mod_path == *mod_name
            || (!mod_path.is_empty() && mod_path.starts_with(&format!("{mod_name}.")))
        {
            cx.emit_node(
                "W4901",
                u::lineno(cx.eng, node),
                u::col_offset(cx.eng, node) as i64,
                u::format_template("Deprecated module %r", &[mod_path]),
            );
        }
    }
}

fn deprecated_classes_of(module: &str) -> &'static [&'static str] {
    depdata::DEPRECATED_CLASSES
        .iter()
        .find(|(m, _)| *m == module)
        .map(|(_, v)| *v)
        .unwrap_or(&[])
}

/// check_deprecated_class (deprecated.py:280-287) — W4904.
pub fn check_deprecated_class<'a>(
    cx: &mut WalkCx,
    node: GNode,
    mod_name: &str,
    class_names: impl Iterator<Item = &'a str>,
) {
    let table = deprecated_classes_of(mod_name);
    for class_name in class_names {
        if table.contains(&class_name) {
            cx.emit_node(
                "W4904",
                u::lineno(cx.eng, node),
                u::col_offset(cx.eng, node) as i64,
                u::format_template(
                    "Using deprecated class %s of module %s",
                    &[class_name, mod_name],
                ),
            );
        }
    }
}

/// check_deprecated_class_in_call (deprecated.py:289-294): purely syntactic
/// `Name.attr(...)` form.
pub fn check_deprecated_class_in_call(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let (mod_name, class_name): (String, String) = {
        let md = eng.md(node.m);
        let NodeKind::Call { func, .. } = &md.tree.nodes[node.n.idx()].kind else { return };
        let NodeKind::Attribute { expr, attrname, .. } = &md.tree.nodes[func.idx()].kind else {
            return;
        };
        let NodeKind::Name { name } = &md.tree.nodes[expr.idx()].kind else { return };
        (md.tree.s(*name).to_string(), md.tree.s(*attrname).to_string())
    };
    check_deprecated_class(cx, node, &mod_name, std::iter::once(class_name.as_str()));
}

/// ACCEPTABLE_NODES (deprecated.py:22-28): BoundMethod, UnboundMethod,
/// FunctionDef, ClassDef, Attribute. Property/PartialFunction are
/// FunctionDef subclasses in astroid -> acceptable.
fn is_acceptable(eng: &Engine, v: &Value) -> bool {
    match v {
        Value::BoundMethod { .. } | Value::DescBM { .. } | Value::UnboundMethod { .. } => true,
        Value::Property { .. } | Value::Partial { .. } => true,
        Value::Node(g) => {
            is_funcdef(eng, *g)
                || is_classdef(eng, *g)
                || matches!(&eng.md(g.m).tree.nodes[g.n.idx()].kind, NodeKind::Attribute { .. })
        }
        _ => false,
    }
}

fn deprecated_args_of(method: &str) -> &'static [(i64, &'static str)] {
    depdata::DEPRECATED_ARGUMENTS
        .iter()
        .find(|(m, _)| *m == method)
        .map(|(_, v)| *v)
        .unwrap_or(&[])
}

/// check_deprecated_method (deprecated.py:245-278) — W4902/W4903. Called per
/// inferred value of node.func (StdlibChecker only; ImportsChecker has no
/// method data).
pub fn check_deprecated_method(cx: &mut WalkCx, node: GNode, inferred: &Value) {
    let eng = cx.eng;
    if !is_acceptable(eng, inferred) {
        return;
    }
    let (func_name, args_len, kwargs): (String, usize, Vec<String>) = {
        let md = eng.md(node.m);
        let NodeKind::Call { func, args, keywords } = &md.tree.nodes[node.n.idx()].kind else {
            return;
        };
        let fname = match &md.tree.nodes[func.idx()].kind {
            NodeKind::Attribute { attrname, .. } => md.tree.s(*attrname).to_string(),
            NodeKind::Name { name } => md.tree.s(*name).to_string(),
            _ => return,
        };
        let kwargs = keywords
            .iter()
            .filter_map(|&k| match &md.tree.nodes[k.idx()].kind {
                NodeKind::Keyword { arg: Some(a), .. } => Some(md.tree.s(*a).to_string()),
                _ => None,
            })
            .collect();
        (fname, args.len(), kwargs)
    };
    // qnames = {inferred.qname(), func_name} — a 2-element SET; iteration
    // order is CPython seed-0 set order (matters for multi-W4903 emission).
    let qname = u::value_qname(eng, inferred);
    let mut qnames: Vec<&str> = Vec::new();
    match &qname {
        Some(q) if q != &func_name => {
            let refs = [q.as_str(), func_name.as_str()];
            for i in crate::pyset::cpython_set_order(&refs) {
                qnames.push(refs[i]);
            }
        }
        _ => qnames.push(func_name.as_str()),
    }
    if qnames.iter().any(|q| depdata::DEPRECATED_METHODS.contains(q)) {
        cx.emit_node(
            "W4902",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            u::format_template("Using deprecated method %s()", &[&func_name]),
        );
        return;
    }
    for q in &qnames {
        for (position, arg_name) in deprecated_args_of(q) {
            let hit = kwargs.iter().any(|k| k == arg_name)
                || (*position >= 0 && (*position as usize) < args_len);
            if hit {
                cx.emit_node(
                    "W4903",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    u::format_template(
                        "Using deprecated argument %s of method %s()",
                        &[arg_name, &func_name],
                    ),
                );
            }
        }
    }
}

/// mixin visit_attribute (deprecated.py:91-94, 222-235) — W4906
/// (StdlibChecker data only).
pub fn visit_attribute(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let (expr, attrname): (GNode, String) = {
        let md = eng.md(node.m);
        let NodeKind::Attribute { expr, attrname, .. } = &md.tree.nodes[node.n.idx()].kind else {
            return;
        };
        (GNode { m: node.m, n: *expr }, md.tree.s(*attrname).to_string())
    };
    let Some(v) = u::safe_infer(eng, cx.caches, expr) else { return };
    let ok = match &v {
        Value::Inst { .. } | Value::ExcInst { .. } => true,
        Value::Node(g) => {
            is_classdef(eng, *g)
                || matches!(&eng.md(g.m).tree.nodes[g.n.idx()].kind, NodeKind::Module(_))
        }
        // Const/containers are Instance subclasses in astroid
        Value::SynthConst(_)
        | Value::SynthSeq { .. }
        | Value::SynthDict { .. }
        | Value::FrozenSet { .. } => true,
        _ => false,
    };
    // Const NODES are Instances too
    let ok = ok
        || matches!(&v, Value::Node(g) if matches!(
            &eng.md(g.m).tree.nodes[g.n.idx()].kind,
            NodeKind::Const(_) | NodeKind::List { .. } | NodeKind::Tuple { .. }
                | NodeKind::Set { .. } | NodeKind::Dict { .. }
        ));
    if !ok {
        return;
    }
    let Some(q) = u::value_qname(eng, &v) else { return };
    let attribute_qname = format!("{q}.{attrname}");
    for dep in depdata::DEPRECATED_ATTRIBUTES {
        if attribute_qname == *dep {
            cx.emit_node(
                "W4906",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                u::format_template("Using deprecated attribute %r", &[&attribute_qname]),
            );
        }
    }
}

/// mixin visit_decorators (deprecated.py:139-153) — W4905. Only the FIRST
/// decorator is checked. `node` is the Decorators node.
pub fn visit_decorators(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let first: GNode = {
        let md = eng.md(node.m);
        let NodeKind::Decorators { nodes } = &md.tree.nodes[node.n.idx()].kind else { return };
        match nodes.first() {
            Some(&d) => GNode { m: node.m, n: d },
            None => return,
        }
    };
    let target = {
        let md = eng.md(first.m);
        match &md.tree.nodes[first.n.idx()].kind {
            NodeKind::Call { func, .. } => GNode { m: first.m, n: *func },
            _ => first,
        }
    };
    let Some(v) = u::safe_infer(eng, cx.caches, target) else { return };
    let is_def = matches!(&v, Value::Node(g) if is_funcdef(eng, *g) || is_classdef(eng, *g));
    if !is_def {
        return;
    }
    let Some(q) = u::value_qname(eng, &v) else { return };
    if depdata::DEPRECATED_DECORATORS.contains(&q.as_str()) {
        cx.emit_node(
            "W4905",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            u::format_template("Using deprecated decorator %s()", &[&q]),
        );
    }
}

/// mixin visit_import (deprecated.py:118-129) for STDLIB (no module data):
/// only the dotted-name W4904 class check is live.
pub fn stdlib_visit_import(cx: &mut WalkCx, node: GNode) {
    let names: Vec<String> = {
        let md = cx.eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Import { names } => {
                names.iter().map(|(n, _)| md.tree.s(*n).to_string()).collect()
            }
            _ => return,
        }
    };
    for name in names {
        // (check_deprecated_module: stdlib deprecated_modules() == () -> no-op)
        if let Some((mod_name, class_name)) = name.split_once('.') {
            check_deprecated_class(cx, node, mod_name, std::iter::once(class_name));
        }
    }
}

/// mixin visit_importfrom (deprecated.py:155-162) for STDLIB: W4904 on the
/// imported names against the (relative-resolved) module.
pub fn stdlib_visit_importfrom(cx: &mut WalkCx, node: GNode, basename: &str) {
    let names: Vec<String> = {
        let md = cx.eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ImportFrom { names, .. } => {
                names.iter().map(|(n, _)| md.tree.s(*n).to_string()).collect()
            }
            _ => return,
        }
    };
    check_deprecated_class(cx, node, basename, names.iter().map(|s| s.as_str()));
}

/// mixin visit_call (deprecated.py:96-116) for IMPORTS: only the
/// `__import__("name")` -> W4901 path is live (its deprecated_methods() is
/// empty). Runs for every inferred value of node.func.
pub fn imports_visit_call(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let (func, args): (GNode, Vec<GNode>) = {
        let md = eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, args, .. } => (
                GNode { m: node.m, n: *func },
                args.iter().map(|&a| GNode { m: node.m, n: a }).collect(),
            ),
            _ => return,
        }
    };
    let vals = u::infer_all(eng, cx.caches, func);
    for v in vals.iter() {
        // (check_deprecated_method: no method data for imports -> no-op)
        let is_import_builtin = matches!(v, Value::Node(g) if is_funcdef(eng, *g))
            && u::value_qname(eng, v).as_deref() == Some("builtins.__import__");
        if is_import_builtin && args.len() == 1 {
            if let Some(Value::Node(c)) = u::safe_infer(eng, cx.caches, args[0]) {
                let md = eng.md(c.m);
                if let NodeKind::Const(pyast::tree::ConstValue::Str(s)) =
                    &md.tree.nodes[c.n.idx()].kind
                {
                    let path = s.to_string();
                    check_deprecated_module(cx, node, &path);
                }
            }
        }
    }
}
