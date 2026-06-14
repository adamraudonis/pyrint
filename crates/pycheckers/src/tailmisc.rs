//! Tail checkers: AsyncChecker (E1700/E1701), MatchStatementChecker
//! (E1901-E1904 + R1906), MethodArgsChecker (E3102 + W3101 burn),
//! DataclassChecker (E3701), ModifiedIterationChecker (E4702/E4703 + W4701),
//! StdlibChecker (E1507/E1519/E1520 + W-burn paths).

use pyast::tree::{ConstValue, NodeKind};
use pyast::NodeId;
use pyinfer::ctx::Ctx;
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, Value};

use crate::basicerr::{is_registered_in_singledispatch_function, nodes_of_class};
use crate::ckutils as u;
use crate::classes::is_method;
use crate::typecheck::{call_parts, decorated_with, value_name};
use crate::walker::WalkCx;

fn is_classdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_)))
}
fn is_funcdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)))
}

// ---------------------------------------------------------------------------
// AsyncChecker — E1700 / E1701
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct AsyncCk;

impl AsyncCk {
    /// visit_asyncfunctiondef — E1700: only YieldFrom in own scope (3.12)
    pub fn visit_asyncfunctiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        for child in nodes_of_class(
            eng,
            node,
            |k| matches!(k, NodeKind::Yield { .. } | NodeKind::YieldFrom { .. }),
            |_| false,
        ) {
            if eng.scope(child) != node {
                continue;
            }
            if eng.kind_is(child, |k| matches!(k, NodeKind::YieldFrom { .. })) {
                cx.emit_node(
                    "E1700",
                    u::lineno(eng, child),
                    u::col_offset(eng, child) as i64,
                    "Yield inside async function".into(),
                );
            }
        }
    }

    /// visit_asyncwith — E1701 (async_checker.py:56-93)
    pub fn visit_asyncwith(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let items: Vec<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::AsyncWith(d) => {
                    d.items.iter().map(|&(e, _)| GNode { m: node.m, n: e }).collect()
                }
                _ => return,
            }
        };
        for ctx_mgr in items {
            let inferred = u::safe_infer(eng, cx.caches, ctx_mgr);
            let Some(inferred) = inferred else { continue };
            if inferred.is_uninferable() {
                continue;
            }
            let mut emit = false;
            match &inferred {
                Value::Node(g)
                    if eng.kind_is(*g, |k| matches!(k, NodeKind::AsyncFunctionDef(_))) =>
                {
                    if decorated_with(eng, *g, &["contextlib.asynccontextmanager"]) {
                        continue;
                    }
                    emit = true;
                }
                Value::Generator { func, is_async: true, .. } => {
                    if decorated_with(eng, *func, &["contextlib.asynccontextmanager"]) {
                        continue;
                    }
                    emit = true;
                }
                _ => {
                    let aenter =
                        crate::typecheck::value_getattr(eng, &inferred, eng.sym("__aenter__"));
                    let aexit =
                        crate::typecheck::value_getattr(eng, &inferred, eng.sym("__aexit__"));
                    if aenter.is_ok() && aexit.is_ok() {
                        continue;
                    }
                    if crate::typecheck::value_is_instance(eng, &inferred) {
                        if !crate::typecheck::value_has_known_bases(eng, cx.caches, &inferred) {
                            continue;
                        }
                        // ignored_checks_for_mixins includes
                        // not-async-context-manager by default; rgx .*[Mm]ixin
                        let name = value_name(eng, &inferred).unwrap_or_default();
                        if name.contains("Mixin") || name.contains("mixin") {
                            continue;
                        }
                    }
                    emit = true;
                }
            }
            if emit {
                let name = value_name(eng, &inferred).unwrap_or_default();
                cx.emit_node(
                    "E1701",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    u::format_template(
                        "Async context manager '%s' doesn't implement __aenter__ and __aexit__.",
                        &[&name],
                    ),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MatchStatementChecker — E1901/E1902/E1903/E1904 + R1906
// ---------------------------------------------------------------------------

const MATCH_CLASS_SELF_NAMES: &[&str] = &[
    "builtins.bool", "builtins.bytearray", "builtins.bytes", "builtins.dict",
    "builtins.float", "builtins.frozenset", "builtins.int", "builtins.list",
    "builtins.set", "builtins.str", "builtins.tuple",
];

#[derive(Default)]
pub struct MatchCk;

impl MatchCk {
    /// visit_match — E1901
    pub fn visit_match(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let cases: Vec<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Match { cases, .. } => {
                    cases.iter().map(|&c| GNode { m: node.m, n: c }).collect()
                }
                _ => return,
            }
        };
        let ncases = cases.len();
        for (idx, case) in cases.iter().enumerate() {
            if idx >= ncases - 1 {
                continue;
            }
            let md = eng.md(case.m);
            let NodeKind::MatchCase { pattern, guard, .. } = &md.tree.nodes[case.n.idx()].kind
            else {
                continue;
            };
            if guard.is_some() {
                continue;
            }
            let pattern = GNode { m: case.m, n: *pattern };
            let NodeKind::MatchAs { pattern: inner, name } =
                &md.tree.nodes[pattern.n.idx()].kind
            else {
                continue;
            };
            if inner.is_some() {
                continue;
            }
            let Some(name_id) = name else { continue };
            let nm = match &md.tree.nodes[name_id.idx()].kind {
                NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                _ => continue,
            };
            drop(md);
            cx.emit_node(
                "E1901",
                u::lineno(eng, pattern),
                u::col_offset(eng, pattern) as i64,
                u::format_template(
                    "The name capture `case %s` makes the remaining patterns unreachable. Use a dotted name (for example an enum) to fix this.",
                    &[&nm],
                ),
            );
        }
    }

    /// visit_assignname — E1902 (runs BEFORE VariablesChecker.visit_assignname)
    pub fn visit_assignname(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        {
            let md = eng.md(node.m);
            let NodeKind::AssignName { name } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            if md.tree.s(*name) != "__match_args__" {
                return;
            }
        }
        if !is_classdef(eng, eng.frame(node)) {
            return;
        }
        let Some(parent) = eng.parent(node) else { return };
        let value: GNode = {
            let md = eng.md(parent.m);
            match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::Assign { value, .. } => GNode { m: parent.m, n: *value },
                _ => return,
            }
        };
        let ok = {
            let md = eng.md(value.m);
            match &md.tree.nodes[value.n.idx()].kind {
                NodeKind::Tuple { elts, .. } => elts.iter().all(|&e| {
                    matches!(
                        md.tree.nodes[e.idx()].kind,
                        NodeKind::Const(ConstValue::Str(_))
                    )
                }),
                _ => false,
            }
        };
        if !ok {
            cx.emit_node(
                "E1902",
                u::lineno(eng, value),
                u::col_offset(eng, value) as i64,
                "`__match_args__` must be a tuple of strings.".into(),
            );
        }
    }

    /// visit_matchas — R1905 (match_statements_checker.py:124-142). Fires on a
    /// MatchAs(name=AssignName, pattern=None) whose parent is a
    /// MatchClass(cls=Name, patterns=[single]) where the cls infers to one of
    /// the self-binding builtin classes.
    pub fn visit_matchas(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node.name is AssignName and node.pattern is None
        let (name_str, parent) = {
            let md = eng.md(node.m);
            let NodeKind::MatchAs { pattern, name } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            if pattern.is_some() {
                return;
            }
            let Some(name_id) = name else { return };
            let nm = match &md.tree.nodes[name_id.idx()].kind {
                NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                _ => return,
            };
            (nm, eng.parent(node))
        };
        let Some(parent) = parent else { return };
        // parent is MatchClass(cls=Name, patterns=[exactly one])
        let cls_name_node = {
            let md = eng.md(parent.m);
            let NodeKind::MatchClass { cls, patterns, .. } =
                &md.tree.nodes[parent.n.idx()].kind
            else {
                return;
            };
            if patterns.len() != 1 {
                return;
            }
            let cls = *cls;
            if !matches!(md.tree.nodes[cls.idx()].kind, NodeKind::Name { .. }) {
                return;
            }
            GNode { m: parent.m, n: cls }
        };
        // safe_infer(cls_name) is a ClassDef whose qname is in the self set
        let inferred = u::safe_infer(eng, cx.caches, cls_name_node);
        let Some(Value::Node(cls)) = inferred else { return };
        if !is_classdef(eng, cls) {
            return;
        }
        let q = eng.value_qname(&Value::Node(cls)).unwrap_or_default();
        if !MATCH_CLASS_SELF_NAMES.contains(&q.as_str()) {
            return;
        }
        let cls_name = {
            let md = eng.md(cls_name_node.m);
            match &md.tree.nodes[cls_name_node.n.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => String::new(),
            }
        };
        cx.emit_node(
            "R1905",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            u::format_template("Use '%s() as %s' instead", &[&cls_name, &name_str]),
        );
    }

    // (helper for W1518 — see below)

    /// get_match_args_for_class (match_statements_checker.py:144-166)
    fn get_match_args_for_class(&self, cx: &mut WalkCx, node: GNode) -> Option<Vec<String>> {
        let eng = cx.eng;
        let inferred = u::safe_infer(eng, cx.caches, node);
        let Some(Value::Node(cls)) = inferred else { return None };
        if !is_classdef(eng, cls) {
            return None;
        }
        let sym = eng.sym("__match_args__");
        let attrs = match eng.class_getattr(cls, sym, None, true) {
            Ok(v) => v,
            Err(_) => {
                let q = eng.value_qname(&Value::Node(cls)).unwrap_or_default();
                if MATCH_CLASS_SELF_NAMES.contains(&q.as_str()) {
                    return Some(vec!["<self>".to_string()]);
                }
                return None;
            }
        };
        // first attr must be AssignName(parent=Assign(value=Tuple of str Consts))
        let first = attrs.first()?;
        let g = match first {
            pyinfer::value::NV::N(g) => *g,
            pyinfer::value::NV::V(_) => return None,
        };
        if !eng.kind_is(g, |k| matches!(k, NodeKind::AssignName { .. })) {
            return None;
        }
        let parent = eng.parent(g)?;
        let md = eng.md(parent.m);
        let NodeKind::Assign { value, .. } = &md.tree.nodes[parent.n.idx()].kind else {
            return None;
        };
        let value = *value;
        let NodeKind::Tuple { elts, .. } = &md.tree.nodes[value.idx()].kind else {
            return None;
        };
        let mut out = Vec::new();
        for &e in elts {
            match &md.tree.nodes[e.idx()].kind {
                NodeKind::Const(ConstValue::Str(s)) => out.push(s.to_string()),
                _ => return None,
            }
        }
        Some(out)
    }

    /// visit_matchclass — E1903/E1904 + R1906
    pub fn visit_matchclass(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (cls, patterns, kwd_attrs): (GNode, Vec<NodeId>, Vec<String>) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::MatchClass { cls, patterns, kwd_attrs, .. } => (
                    GNode { m: node.m, n: *cls },
                    patterns.clone(),
                    kwd_attrs
                        .iter()
                        .map(|&s| md.tree.s(s).to_string())
                        .collect(),
                ),
                _ => return,
            }
        };
        let mut attrs: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut dups: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut check_dup = |cx: &mut WalkCx,
                             name: &str,
                             attrs: &mut std::collections::HashSet<String>,
                             dups: &mut std::collections::HashSet<String>| {
            if attrs.contains(name) && !dups.contains(name) {
                dups.insert(name.to_string());
                cx.emit_node(
                    "E1904",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    u::format_template("Multiple sub-patterns for attribute %s", &[name]),
                );
            } else {
                attrs.insert(name.to_string());
            }
        };
        if !patterns.is_empty() {
            if let Some(match_args) = self.get_match_args_for_class(cx, cls) {
                if patterns.len() > match_args.len() {
                    cx.emit_node(
                        "E1903",
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        u::format_template(
                            "%s expects %d positional sub-patterns (given %d)",
                            &[
                                &u::as_string(eng, cls),
                                &match_args.len().to_string(),
                                &patterns.len().to_string(),
                            ],
                        ),
                    );
                    return;
                }
                // R1906 match-class-positional-attributes
                let inferred = u::safe_infer(eng, cx.caches, cls);
                let exempt = match &inferred {
                    Some(Value::Node(g)) if is_classdef(eng, *g) => {
                        let q = eng.value_qname(&Value::Node(*g)).unwrap_or_default();
                        MATCH_CLASS_SELF_NAMES.contains(&q.as_str())
                            || class_basenames_contains_tuple(eng, *g)
                    }
                    _ => false,
                };
                if !exempt {
                    let attributes: Vec<String> = match_args
                        .iter()
                        .take(patterns.len())
                        .map(|a| format!("'{a}'"))
                        .collect();
                    cx.emit_node(
                        "R1906",
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        u::format_template(
                            "Use keyword attributes instead of positional ones (%s)",
                            &[&attributes.join(", ")],
                        ),
                    );
                }
                for i in 0..patterns.len() {
                    let name = match_args[i].clone();
                    check_dup(cx, &name, &mut attrs, &mut dups);
                }
            }
        }
        for kw in &kwd_attrs {
            check_dup(cx, kw, &mut attrs, &mut dups);
        }
    }
}

/// "tuple" in inferred.basenames — textual base names
fn class_basenames_contains_tuple(eng: &Engine, cls: GNode) -> bool {
    let md = eng.md(cls.m);
    let NodeKind::ClassDef(d) = &md.tree.nodes[cls.n.idx()].kind else { return false };
    let bases = d.bases.clone();
    drop(md);
    bases.iter().any(|&b| {
        u::as_string(eng, GNode { m: cls.m, n: b }) == "tuple"
    })
}

// ---------------------------------------------------------------------------
// MethodArgsChecker — E3102 (+ W3101 missing-timeout burn)
// ---------------------------------------------------------------------------

const TIMEOUT_METHODS: &[&str] = &[
    "requests.api.delete", "requests.api.get", "requests.api.head",
    "requests.api.options", "requests.api.patch", "requests.api.post",
    "requests.api.put", "requests.api.request",
];

#[derive(Default)]
pub struct MethodArgsCk;

impl MethodArgsCk {
    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_missing_timeout(cx, node);
        self.check_positional_only_arguments_expected(cx, node);
    }

    /// _check_missing_timeout — W3101 (burn + resurrection)
    fn check_missing_timeout(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some((fnode, args, keywords)) = call_parts(eng, node) else { return };
        let inferred = u::safe_infer(eng, cx.caches, fnode);
        let Some(v) = &inferred else { return };
        // CallSite.from_call + has_invalid_keywords
        let cc = pyinfer::ctx::CallCtx {
            id: eng.next_callctx_id(),
            args: std::cell::RefCell::new(
                args.iter().map(|&a| pyinfer::value::NV::N(a)).collect(),
            ),
            keywords: std::cell::RefCell::new(
                keywords
                    .iter()
                    .map(|&k| {
                        let md = eng.md(k.m);
                        match &md.tree.nodes[k.n.idx()].kind {
                            NodeKind::Keyword { arg, value } => {
                                (arg.map(|s| eng.g(&md, s)), GNode { m: k.m, n: *value })
                            }
                            _ => (None, k),
                        }
                    })
                    .collect(),
            ),
            callee: std::cell::RefCell::new(None),
        };
        let site = eng.call_site_from(&cc, &Ctx::new());
        let qual = eng.value_qname(v);
        let is_kind = matches!(v, Value::Node(g) if is_funcdef(eng, *g) || is_classdef(eng, *g))
            || matches!(v, Value::UnboundMethod { .. });
        if site.has_invalid_keywords()
            || !is_kind
            || !qual
                .as_deref()
                .map(|q| TIMEOUT_METHODS.contains(&q))
                .unwrap_or(false)
        {
            return;
        }
        let mut kw_names: Vec<String> = keywords
            .iter()
            .filter_map(|&k| {
                let md = eng.md(k.m);
                match &md.tree.nodes[k.n.idx()].kind {
                    NodeKind::Keyword { arg: Some(s), .. } => {
                        Some(md.tree.s(*s).to_string())
                    }
                    _ => None,
                }
            })
            .collect();
        kw_names.extend(site.keyword_arguments().iter().map(|(k, _)| eng.sname(*k)));
        if !kw_names.iter().any(|k| k == "timeout") {
            cx.emit_node(
                "W3101",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                u::format_template(
                    "Missing timeout argument for method '%s' can cause your program to hang indefinitely",
                    &[&u::as_string(eng, fnode)],
                ),
            );
        }
    }

    /// _check_positional_only_arguments_expected — E3102
    fn check_positional_only_arguments_expected(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some((fnode, _, keywords)) = call_parts(eng, node) else { return };
        let mut inferred_func = u::safe_infer(eng, cx.caches, fnode);
        loop {
            match inferred_func {
                Some(Value::BoundMethod { func, .. })
                | Some(Value::DescBM { func, .. })
                | Some(Value::UnboundMethod { func }) => {
                    inferred_func = Some(Value::Node(func));
                }
                _ => break,
            }
        }
        let Some(Value::Node(f)) = inferred_func else { return };
        if !is_funcdef(eng, f) {
            return;
        }
        let (posonly, has_kwarg): (Vec<String>, bool) = {
            let md = eng.md(f.m);
            let args_id = match &md.tree.nodes[f.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
                _ => return,
            };
            let NodeKind::Arguments(a) = &md.tree.nodes[args_id.idx()].kind else { return };
            (
                a.posonlyargs
                    .iter()
                    .filter_map(|&p| match &md.tree.nodes[p.idx()].kind {
                        NodeKind::AssignName { name } => {
                            Some(md.tree.s(*name).to_string())
                        }
                        _ => None,
                    })
                    .collect(),
                a.kwarg.is_some(),
            )
        };
        if posonly.is_empty() || has_kwarg {
            return;
        }
        let kws: Vec<String> = keywords
            .iter()
            .filter_map(|&k| {
                let md = eng.md(k.m);
                match &md.tree.nodes[k.n.idx()].kind {
                    NodeKind::Keyword { arg: Some(s), .. } => {
                        let n = md.tree.s(*s).to_string();
                        if posonly.contains(&n) { Some(n) } else { None }
                    }
                    _ => None,
                }
            })
            .collect();
        if kws.is_empty() {
            return;
        }
        let joined = kws
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(", ");
        cx.emit_node(
            "E3102",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            u::format_template(
                "`%s()` got some positional-only arguments passed as keyword arguments: %s",
                &[&u::as_string(eng, fnode), &joined],
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// DataclassChecker — E3701
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct DataclassCk;

impl DataclassCk {
    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some((fnode, _, _)) = call_parts(eng, node) else { return };
        // func must be Name/Attribute named "field"
        {
            let md = eng.md(fnode.m);
            let name_ok = match &md.tree.nodes[fnode.n.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name) == "field",
                NodeKind::Attribute { attrname, .. } => {
                    md.tree.s(*attrname) == "field"
                }
                _ => return,
            };
            if !name_ok {
                return;
            }
        }
        let inferred = u::safe_infer(eng, cx.caches, fnode);
        let Some(Value::Node(f)) = &inferred else { return };
        if !is_funcdef(eng, *f) || !is_dataclasses_module(eng, *f) {
            return;
        }
        let mut scope_node = eng.parent(node);
        while let Some(s) = scope_node {
            if is_classdef(eng, s) || eng.kind_is(s, |k| matches!(k, NodeKind::Call { .. })) {
                break;
            }
            scope_node = eng.parent(s);
        }
        let line = u::lineno(eng, node);
        let col = u::col_offset(eng, node) as i64;
        let emit_outside = |cx: &mut WalkCx| {
            cx.emit_node(
                "E3701",
                line,
                col,
                u::format_template(
                    "Invalid usage of field(), %s",
                    &["it should be used within a dataclass or the make_dataclass() function."],
                ),
            );
        };
        match scope_node {
            Some(s) if eng.kind_is(s, |k| matches!(k, NodeKind::Call { .. })) => {
                // _check_invalid_field_call_within_call
                let Some((sf, _, _)) = call_parts(eng, s) else {
                    emit_outside(cx);
                    return;
                };
                let inferred_func = u::safe_infer(eng, cx.caches, sf);
                let name_ok = {
                    let md = eng.md(sf.m);
                    match &md.tree.nodes[sf.n.idx()].kind {
                        NodeKind::Name { name } => {
                            md.tree.s(*name) == "make_dataclass"
                        }
                        NodeKind::AssignName { name } => {
                            md.tree.s(*name) == "make_dataclass"
                        }
                        _ => false,
                    }
                };
                let inferred_ok = matches!(&inferred_func, Some(Value::Node(g))
                    if is_funcdef(eng, *g) && is_dataclasses_module(eng, *g));
                if !(name_ok && inferred_ok) {
                    emit_outside(cx);
                }
            }
            Some(s) if is_classdef(eng, s) => {
                if !eng.is_dataclass_flag.borrow().contains(&s) {
                    emit_outside(cx);
                    return;
                }
                // must be the value of an AnnAssign
                let ok = match eng.parent(node) {
                    Some(p) => {
                        let md = eng.md(p.m);
                        matches!(&md.tree.nodes[p.n.idx()].kind,
                            NodeKind::AnnAssign { value: Some(v), .. } if *v == node.n)
                    }
                    None => false,
                };
                if !ok {
                    cx.emit_node(
                        "E3701",
                        line,
                        col,
                        u::format_template(
                            "Invalid usage of field(), %s",
                            &["it should be the value of an assignment within a dataclass."],
                        ),
                    );
                }
            }
            _ => emit_outside(cx),
        }
    }
}

/// node.root().name — reparent-aware walk to the top, then module name
pub fn root_name(eng: &Engine, g: GNode) -> String {
    let mut cur = g;
    while let Some(p) = eng.parent(cur) {
        cur = p;
    }
    eng.md(cur.m).name.clone()
}

fn is_dataclasses_module(eng: &Engine, f: GNode) -> bool {
    // inferred_func.root().name in DATACLASS_MODULES
    let root = root_name(eng, f);
    matches!(
        root.as_str(),
        "dataclasses" | "marshmallow_dataclass" | "pydantic.dataclasses"
    )
}

// ---------------------------------------------------------------------------
// ModifiedIterationChecker — E4702/E4703 + W4701
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ModIterCk;

impl ModIterCk {
    pub fn visit_for(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (iter_obj, body): (GNode, Vec<NodeId>) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::For(d) => (GNode { m: node.m, n: d.iter }, d.body.clone()),
                _ => return,
            }
        };
        for b in body {
            self.check_on_node_and_children(cx, GNode { m: node.m, n: b }, iter_obj);
        }
    }

    fn check_on_node_and_children(&mut self, cx: &mut WalkCx, body_node: GNode, iter_obj: GNode) {
        self.modified_iterating_check(cx, body_node, iter_obj);
        let children: Vec<NodeId> = cx.eng.md(body_node.m).tree.children(body_node.n);
        for c in children {
            self.check_on_node_and_children(cx, GNode { m: body_node.m, n: c }, iter_obj);
        }
    }

    fn modified_iterating_check(&mut self, cx: &mut WalkCx, node: GNode, iter_obj: GNode) {
        let eng = cx.eng;
        let mut msg_id: Option<&'static str> = None;
        let is_delete = eng.kind_is(node, |k| matches!(k, NodeKind::Delete { .. }));
        if is_delete && self.delete_targets_iteration_target(cx, node, iter_obj) {
            let inferred = u::safe_infer(eng, cx.caches, iter_obj);
            msg_id = match &inferred {
                Some(Value::Node(g)) => {
                    let md = eng.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::List { .. } => Some("W4701"),
                        NodeKind::Dict { .. } => Some("E4702"),
                        NodeKind::Set { .. } => Some("E4703"),
                        _ => None,
                    }
                }
                _ => None,
            };
        } else if !eng.kind_is(iter_obj, |k| {
            matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. })
        }) {
            // bail
        } else if self.modified_iterating_list_cond(cx, node, iter_obj) {
            msg_id = Some("W4701");
        } else if self.modified_iterating_dict_cond(cx, node, iter_obj) {
            msg_id = Some("E4702");
        } else if self.modified_iterating_set_cond(cx, node, iter_obj) {
            msg_id = Some("E4703");
        }
        if let Some(msg) = msg_id {
            // iter_obj.repr_name(): Name -> name; Attribute -> attrname;
            // container literals carry a class-level `name` attr in astroid
            // (List="list", Dict="dict", Set="set", Tuple="tuple"), so
            // node_ng.repr_name() returns it (node_ng.py:178-185).
            let repr_name = {
                let md = eng.md(iter_obj.m);
                match &md.tree.nodes[iter_obj.n.idx()].kind {
                    NodeKind::Name { name } => md.tree.s(*name).to_string(),
                    NodeKind::Attribute { attrname, .. } => {
                        md.tree.s(*attrname).to_string()
                    }
                    NodeKind::List { .. } => "list".to_string(),
                    NodeKind::Dict { .. } => "dict".to_string(),
                    NodeKind::Set { .. } => "set".to_string(),
                    NodeKind::Tuple { .. } => "tuple".to_string(),
                    _ => String::new(),
                }
            };
            let template = match msg {
                "W4701" => "Iterated list '%s' is being modified inside for loop body, consider iterating through a copy of it instead.",
                "E4702" => "Iterated dict '%s' is being modified inside for loop body, iterate through a copy of it instead.",
                _ => "Iterated set '%s' is being modified inside for loop body, iterate through a copy of it instead.",
            };
            cx.emit_node(
                msg,
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                u::format_template(template, &[&repr_name]),
            );
        }
    }

    /// node is Expr(Call(Attribute(Name))) -> (expr_name_node, attrname)
    fn expr_calls_attribute_name(&self, eng: &Engine, node: GNode) -> Option<(GNode, String)> {
        let md = eng.md(node.m);
        let NodeKind::Expr { value } = &md.tree.nodes[node.n.idx()].kind else { return None };
        let NodeKind::Call { func, .. } = &md.tree.nodes[value.idx()].kind else { return None };
        let NodeKind::Attribute { expr, attrname, .. } = &md.tree.nodes[func.idx()].kind else {
            return None;
        };
        if !matches!(md.tree.nodes[expr.idx()].kind, NodeKind::Name { .. }) {
            return None;
        }
        Some((
            GNode { m: node.m, n: *expr },
            md.tree.s(*attrname).to_string(),
        ))
    }

    fn common_cond_list_set(
        &self,
        cx: &mut WalkCx,
        expr_name: GNode,
        iter_obj: GNode,
        infer_val: &Value,
    ) -> bool {
        let eng = cx.eng;
        let iter_inferred = u::safe_infer(eng, cx.caches, iter_obj);
        let same = match (&infer_val, &iter_inferred) {
            (Value::Node(a), Some(Value::Node(b))) => a == b,
            _ => false,
        };
        if !same {
            return false;
        }
        let iter_obj_name = {
            let md = eng.md(iter_obj.m);
            match &md.tree.nodes[iter_obj.n.idx()].kind {
                NodeKind::Attribute { attrname, .. } => {
                    md.tree.s(*attrname).to_string()
                }
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return false,
            }
        };
        let expr_name_str = {
            let md = eng.md(expr_name.m);
            match &md.tree.nodes[expr_name.n.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return false,
            }
        };
        expr_name_str == iter_obj_name
    }

    fn modified_iterating_list_cond(&self, cx: &mut WalkCx, node: GNode, iter_obj: GNode) -> bool {
        let eng = cx.eng;
        let Some((expr_name, attrname)) = self.expr_calls_attribute_name(eng, node) else {
            return false;
        };
        let infer_val = u::safe_infer(eng, cx.caches, expr_name);
        let Some(v) = infer_val else { return false };
        let is_list = matches!(&v, Value::Node(g)
            if eng.kind_is(*g, |k| matches!(k, NodeKind::List { .. })));
        if !is_list {
            return false;
        }
        self.common_cond_list_set(cx, expr_name, iter_obj, &v)
            && matches!(attrname.as_str(), "append" | "remove")
    }

    fn modified_iterating_dict_cond(&self, cx: &mut WalkCx, node: GNode, iter_obj: GNode) -> bool {
        let eng = cx.eng;
        // Assign(targets=[Subscript(value=Name), ...])
        let (sub_value, sub_slice): (GNode, GNode) = {
            let md = eng.md(node.m);
            let NodeKind::Assign { targets, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return false;
            };
            let Some(&t0) = targets.first() else { return false };
            let NodeKind::Subscript { value, slice, .. } = &md.tree.nodes[t0.idx()].kind else {
                return false;
            };
            if !matches!(md.tree.nodes[value.idx()].kind, NodeKind::Name { .. }) {
                return false;
            }
            (GNode { m: node.m, n: *value }, GNode { m: node.m, n: *slice })
        };
        // same-key exemption
        {
            let md = eng.md(iter_obj.m);
            let iter_is_name = matches!(md.tree.nodes[iter_obj.n.idx()].kind, NodeKind::Name { .. });
            if iter_is_name {
                let iter_name = match &md.tree.nodes[iter_obj.n.idx()].kind {
                    NodeKind::Name { name } => md.tree.s(*name).to_string(),
                    _ => unreachable!(),
                };
                let sub_value_name = match &md.tree.nodes[sub_value.n.idx()].kind {
                    NodeKind::Name { name } => md.tree.s(*name).to_string(),
                    _ => String::new(),
                };
                if iter_name == sub_value_name {
                    if let Some(p) = eng.parent(iter_obj) {
                        if let NodeKind::For(d) = &md.tree.nodes[p.n.idx()].kind {
                            let target_is_an = matches!(
                                md.tree.nodes[d.target.idx()].kind,
                                NodeKind::AssignName { .. }
                            );
                            let slice_is_name = matches!(
                                md.tree.nodes[sub_slice.n.idx()].kind,
                                NodeKind::Name { .. }
                            );
                            if target_is_an && slice_is_name {
                                let tname = match &md.tree.nodes[d.target.idx()].kind {
                                    NodeKind::AssignName { name } => {
                                        md.tree.s(*name).to_string()
                                    }
                                    _ => String::new(),
                                };
                                let sname = match &md.tree.nodes[sub_slice.n.idx()].kind {
                                    NodeKind::Name { name } => {
                                        md.tree.s(*name).to_string()
                                    }
                                    _ => String::new(),
                                };
                                if tname == sname {
                                    return false;
                                }
                            }
                        }
                    }
                }
            }
        }
        let infer_val = u::safe_infer(eng, cx.caches, sub_value);
        let Some(v) = infer_val else { return false };
        let is_dict = matches!(&v, Value::Node(g)
            if eng.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })));
        if !is_dict {
            return false;
        }
        let iter_inferred = u::safe_infer(eng, cx.caches, iter_obj);
        let same = match (&v, &iter_inferred) {
            (Value::Node(a), Some(Value::Node(b))) => a == b,
            _ => false,
        };
        if !same {
            return false;
        }
        let iter_obj_name = {
            let md = eng.md(iter_obj.m);
            match &md.tree.nodes[iter_obj.n.idx()].kind {
                NodeKind::Attribute { attrname, .. } => {
                    md.tree.s(*attrname).to_string()
                }
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return false,
            }
        };
        let sub_value_name = {
            let md = eng.md(sub_value.m);
            match &md.tree.nodes[sub_value.n.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return false,
            }
        };
        sub_value_name == iter_obj_name
    }

    fn modified_iterating_set_cond(&self, cx: &mut WalkCx, node: GNode, iter_obj: GNode) -> bool {
        let eng = cx.eng;
        let Some((expr_name, attrname)) = self.expr_calls_attribute_name(eng, node) else {
            return false;
        };
        let infer_val = u::safe_infer(eng, cx.caches, expr_name);
        let Some(v) = infer_val else { return false };
        let is_set = matches!(&v, Value::Node(g)
            if eng.kind_is(*g, |k| matches!(k, NodeKind::Set { .. })));
        if !is_set {
            return false;
        }
        self.common_cond_list_set(cx, expr_name, iter_obj, &v)
            && matches!(attrname.as_str(), "add" | "clear" | "discard" | "pop" | "remove")
    }

    /// _deleted_iteration_target_cond over Delete targets
    fn delete_targets_iteration_target(
        &self,
        cx: &mut WalkCx,
        node: GNode,
        iter_obj: GNode,
    ) -> bool {
        let eng = cx.eng;
        let targets: Vec<NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Delete { targets } => targets.clone(),
                _ => return false,
            }
        };
        targets.iter().any(|&t| {
            let tg = GNode { m: node.m, n: t };
            let md = eng.md(tg.m);
            let NodeKind::DelName { name } = &md.tree.nodes[t.idx()].kind else {
                return false;
            };
            let del_name = md.tree.s(*name).to_string();
            drop(md);
            let Some(parent) = eng.parent(iter_obj) else { return false };
            let md = eng.md(parent.m);
            let NodeKind::For(d) = &md.tree.nodes[parent.n.idx()].kind else { return false };
            let target = GNode { m: parent.m, n: d.target };
            let ok_kind = matches!(
                md.tree.nodes[d.target.idx()].kind,
                NodeKind::AssignName { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::List { .. }
                    | NodeKind::Set { .. }
            );
            drop(md);
            if !ok_kind {
                return false;
            }
            // find_assigned_names_recursive
            let mut names: Vec<String> = Vec::new();
            collect_assigned_names(eng, target, &mut names);
            names.iter().any(|n| *n == del_name)
        })
    }
}

fn collect_assigned_names(eng: &Engine, target: GNode, out: &mut Vec<String>) {
    let md = eng.md(target.m);
    match &md.tree.nodes[target.n.idx()].kind {
        NodeKind::AssignName { name } => out.push(md.tree.s(*name).to_string()),
        NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } | NodeKind::Set { elts, .. } => {
            let elts = elts.clone();
            drop(md);
            for e in elts {
                collect_assigned_names(eng, GNode { m: target.m, n: e }, out);
            }
        }
        NodeKind::Starred { value, .. } => {
            let v = *value;
            drop(md);
            collect_assigned_names(eng, GNode { m: target.m, n: v }, out);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// StdlibChecker — E1507/E1519/E1520 (+W-burn paths)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct StdlibCk;

impl StdlibCk {
    /// visit_call (stdlib.py:690-721)
    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        // DeprecatedMixin pieces (stdlib.py:690-693): W4904 class-in-call
        // first, then per-inferred check_deprecated_method at the loop tail.
        crate::deprecated::check_deprecated_class_in_call(cx, node);
        let eng = cx.eng;
        let Some((fnode, args, keywords)) = call_parts(eng, node) else { return };
        let vals = u::infer_all(eng, cx.caches, fnode);
        for v in vals.iter() {
            if v.is_uninferable() {
                continue;
            }
            let root = value_root_name(eng, v);
            if matches!(root.as_deref(), Some("_io") | Some("pathlib") | Some("pathlib._local")) {
                let open_func_name: Option<String> = {
                    let md = eng.md(fnode.m);
                    match &md.tree.nodes[fnode.n.idx()].kind {
                        NodeKind::Name { name } => {
                            Some(md.tree.s(*name).to_string())
                        }
                        NodeKind::Attribute { attrname, .. } => {
                            Some(md.tree.s(*attrname).to_string())
                        }
                        _ => None,
                    }
                };
                if let Some(ofn) = &open_func_name {
                    if matches!(ofn.as_str(), "open" | "file" | "read_text" | "write_text") {
                        self.check_open_call(
                            cx,
                            node,
                            root.as_deref().unwrap(),
                            ofn,
                            &args,
                            &keywords,
                        );
                    }
                }
            } else if root.as_deref() == Some("unittest.case") {
                self.check_redundant_assert(cx, node, v, &args);
            } else if let Value::Node(g) = v {
                if is_classdef(eng, *g) {
                    let q = eng.value_qname(v);
                    if q.as_deref() == Some("threading.Thread") {
                        self.check_bad_thread_instantiation(cx, node, &args, &keywords);
                    } else if q.as_deref() == Some("subprocess.Popen") {
                        self.check_preexec_fn_in_popen(cx, node, &keywords);
                    }
                } else if is_funcdef(eng, *g) {
                    let q = eng.value_qname(v).unwrap_or_default();
                    match q.as_str() {
                        "copy.copy" => self.check_shallow_copy_environ(cx, node, &args, &keywords),
                        "os.getenv" => self.check_env_function(cx, node, &args, &keywords),
                        "subprocess.run" => self.check_check_kw_in_run(cx, node, &keywords),
                        "builtins.breakpoint" | "sys.breakpointhook" | "pdb.set_trace" => {
                            cx.emit_node(
                                "W1515",
                                u::lineno(eng, node),
                                u::col_offset(eng, node) as i64,
                                "Leaving functions creating breakpoints in production code is not recommended"
                                    .into(),
                            );
                        }
                        _ => {}
                    }
                }
            }
            // check_deprecated_method runs for EVERY non-Uninferable
            // inferred value (stdlib.py:721) — W4902/W4903
            crate::deprecated::check_deprecated_method(cx, node, v);
        }
    }

    /// visit_functiondef (stdlib.py:746-750) — NOT registered for async
    pub fn visit_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let decs = crate::typecheck::decorator_nodes_pub(eng, node);
        if decs.is_empty() {
            return;
        }
        if let Some(p) = eng.parent(node) {
            if is_classdef(eng, p) {
                self.check_lru_cache_decorators(cx, node, &decs);
            }
        }
        self.check_dispatch_decorators(cx, node, &decs);
    }

    /// _check_lru_cache_decorators — W1518 burn + emission
    fn check_lru_cache_decorators(&mut self, cx: &mut WalkCx, node: GNode, decs: &[GNode]) {
        let eng = cx.eng;
        // any(utils.is_enum(ancestor) for ancestor in node.parent.ancestors())
        let parent = eng.parent(node).unwrap();
        for anc in eng.ancestors(parent, true, None) {
            if eng.node_name(anc).as_deref() == Some("Enum")
                && root_name(eng, anc) == "enum"
            {
                return;
            }
        }
        let mut lru_cache_nodes: Vec<GNode> = Vec::new();
        for &d in decs {
            let flow = eng.infer(d, &Ctx::new());
            if flow.err.is_some() && flow.vals.is_empty() {
                continue;
            }
            for v in &flow.vals {
                let q = eng.value_qname(v).unwrap_or_default();
                // NON_INSTANCE_METHODS (stdlib.py:44).
                if matches!(q.as_str(), "builtins.classmethod" | "builtins.staticmethod") {
                    return;
                }
                // LRU_CACHE set (stdlib.py:39-43): the qnames @lru_cache and
                // @lru_cache() infer to across py versions.
                if matches!(
                    q.as_str(),
                    "functools.lru_cache"
                        | "functools._lru_cache_wrapper.wrapper"
                        | "functools.lru_cache.decorating_function"
                ) && eng.kind_is(d, |k| matches!(k, NodeKind::Call { .. }))
                {
                    // get_argument_from_call(d, position=0, keyword="maxsize")
                    let Some((_, dargs, dkws)) = call_parts(eng, d) else { break };
                    let mut arg: Option<GNode> = dargs.first().copied();
                    let mut arg_is_value_node = false;
                    if arg.is_none() {
                        for &k in &dkws {
                            let md = eng.md(k.m);
                            if let NodeKind::Keyword { arg: Some(s), value } =
                                &md.tree.nodes[k.n.idx()].kind
                            {
                                if md.tree.s(*s) == "maxsize" {
                                    arg = Some(GNode { m: k.m, n: *value });
                                }
                            }
                        }
                    }
                    if arg.is_none() {
                        // NoSuchArgumentError -> infer_kwarg_from_call: scan the
                        // `**dict` unpack keywords, infer each to a Dict and
                        // return the value node for key "maxsize"
                        // (utils.py:747-760). The returned node is checked as
                        // `isinstance(arg, Const) and arg.value is None` below.
                        arg = infer_maxsize_kwarg(eng, cx, &dkws);
                        arg_is_value_node = arg.is_some();
                    }
                    let is_const_none = match arg {
                        Some(a) => eng.kind_is(a, |k| {
                            matches!(k, NodeKind::Const(ConstValue::None))
                        }),
                        None => false,
                    };
                    let _ = arg_is_value_node;
                    if !is_const_none {
                        break;
                    }
                    lru_cache_nodes.push(d);
                    break;
                }
                if q == "functools.cache" {
                    lru_cache_nodes.push(d);
                    break;
                }
            }
        }
        for d in lru_cache_nodes {
            cx.emit_node(
                "W1518",
                u::lineno(eng, d),
                u::col_offset(eng, d) as i64,
                "'lru_cache(maxsize=None)' or 'cache' will keep all method args alive indefinitely, including 'self'"
                    .into(),
            );
        }
    }

    /// _check_dispatch_decorators (stdlib.py:794-820) — E1519/E1520
    fn check_dispatch_decorators(&mut self, cx: &mut WalkCx, node: GNode, decs: &[GNode]) {
        let eng = cx.eng;
        let mut decorators_map: indexmap::IndexMap<String, GNode> = indexmap::IndexMap::new();
        for &d in decs {
            let is_name = {
                let md = eng.md(d.m);
                match &md.tree.nodes[d.n.idx()].kind {
                    NodeKind::Name { name } => {
                        Some(md.tree.s(*name).to_string())
                    }
                    _ => None,
                }
            };
            if let Some(n) = is_name {
                if !n.is_empty() {
                    decorators_map.insert(n, d);
                    continue;
                }
            }
            if is_registered_in_singledispatch_function(eng, cx, node) {
                decorators_map.insert("singledispatch".into(), d);
            } else if is_registered_in_singledispatchmethod_function(eng, cx, node) {
                decorators_map.insert("singledispatchmethod".into(), d);
            }
        }
        if is_method(eng, node) {
            if let Some(&d) = decorators_map.get("singledispatch") {
                cx.emit_node(
                    "E1519",
                    u::lineno(eng, d),
                    u::col_offset(eng, d) as i64,
                    "singledispatch decorator should not be used with methods, use singledispatchmethod instead."
                        .into(),
                );
            }
        } else if let Some(&d) = decorators_map.get("singledispatchmethod") {
            cx.emit_node(
                "E1520",
                u::lineno(eng, d),
                u::col_offset(eng, d) as i64,
                "singledispatchmethod decorator should not be used with functions, use singledispatch instead."
                    .into(),
            );
        }
    }

    /// _check_open_call (stdlib.py:847-922) — W1501/W1514 + burn
    fn check_open_call(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        open_module: &str,
        func_name: &str,
        args: &[GNode],
        keywords: &[GNode],
    ) {
        let eng = cx.eng;
        let kw_value = |name: &str| -> Option<GNode> {
            for &k in keywords {
                let md = eng.md(k.m);
                if let NodeKind::Keyword { arg: Some(s), value } = &md.tree.nodes[k.n.idx()].kind {
                    if md.tree.s(*s) == name {
                        return Some(GNode { m: k.m, n: *value });
                    }
                }
            }
            None
        };
        let get_arg = |position: usize, keyword: &str| -> Option<GNode> {
            args.get(position).copied().or_else(|| kw_value(keyword))
        };
        let mut mode_arg_node: Option<GNode> = None;
        match open_module {
            "_io" => mode_arg_node = get_arg(1, "mode"),
            "pathlib" | "pathlib._local" => mode_arg_node = get_arg(0, "mode"),
            _ => {}
        }
        // NoSuchArgumentError -> infer_kwarg_from_call (open(..., **{"mode": ..}))
        if mode_arg_node.is_none()
            && matches!(open_module, "_io" | "pathlib" | "pathlib._local")
        {
            mode_arg_node = infer_kwarg_from_call(eng, cx, keywords, "mode");
        }
        let mode_val: Option<Value> = mode_arg_node.and_then(|a| u::safe_infer(eng, cx.caches, a));
        let mode_const: Option<ConstValue> = match &mode_val {
            Some(Value::Node(g)) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(c.clone()),
                    _ => None,
                }
            }
            Some(Value::SynthConst(c)) => Some((**c).clone()),
            _ => None,
        };
        if let Some(c) = &mode_const {
            // W1501: only for "open"/"file" with an invalid mode string
            let mode_ok = match c {
                ConstValue::Str(s) => check_mode_str(s),
                _ => false, // _check_mode_str: non-str -> False
            };
            if (func_name == "open" || func_name == "file") && !mode_ok {
                cx.emit_node(
                    "W1501",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    format!("\"{}\" is not a valid mode for open.", const_str_of(c)),
                );
            }
        }
        // `if not mode_arg or (isinstance(mode_arg, Const) and not
        // (mode_arg.value and "b" in str(mode_arg.value)))` — mode_arg is
        // the INFERRED value here (reassigned by safe_infer)
        let truthy_with_b = match &mode_const {
            Some(c) => crate::ckutils::const_truthy(c) && const_str_of(c).contains('b'),
            None => false,
        };
        let encoding_path = mode_arg_node.is_none()
            || mode_val.is_none()
            || (mode_const.is_some() && !truthy_with_b);
        if encoding_path {
            let encoding_arg_node: Option<GNode> = if open_module != "_io" {
                match func_name {
                    "read_text" => get_arg(0, "encoding"),
                    "write_text" => get_arg(1, "encoding"),
                    _ => get_arg(2, "encoding"),
                }
            } else {
                get_arg(3, "encoding")
            };
            match encoding_arg_node {
                None => {
                    cx.emit_node(
                        "W1514",
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        "Using open without explicitly specifying an encoding".into(),
                    );
                }
                Some(a) => {
                    let enc = u::safe_infer(eng, cx.caches, a);
                    let is_none = match &enc {
                        Some(Value::Node(g)) => eng.kind_is(*g, |k| {
                            matches!(k, NodeKind::Const(ConstValue::None))
                        }),
                        Some(Value::SynthConst(c)) => matches!(&**c, ConstValue::None),
                        _ => false,
                    };
                    if is_none {
                        cx.emit_node(
                            "W1514",
                            u::lineno(eng, node),
                            u::col_offset(eng, node) as i64,
                            "Using open without explicitly specifying an encoding".into(),
                        );
                    }
                }
            }
        }
    }

    /// _check_redundant_assert — W1503
    fn check_redundant_assert(&mut self, cx: &mut WalkCx, node: GNode, v: &Value, args: &[GNode]) {
        let eng = cx.eng;
        let Value::BoundMethod { func, .. } = v else { return };
        let name = eng.node_name(*func).unwrap_or_default();
        if args.is_empty() || !matches!(name.as_str(), "assertTrue" | "assertFalse") {
            return;
        }
        let first = args[0];
        let md = eng.md(first.m);
        let NodeKind::Const(c) = &md.tree.nodes[first.n.idx()].kind else { return };
        let val_repr = const_py_repr(c);
        drop(md);
        cx.emit_node(
            "W1503",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            format!("Redundant use of {name} with constant value {val_repr}"),
        );
    }

    fn check_bad_thread_instantiation(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        args: &[GNode],
        keywords: &[GNode],
    ) {
        let eng = cx.eng;
        let mut has_target = false;
        let mut has_dstar = false;
        for &k in keywords {
            let md = eng.md(k.m);
            match &md.tree.nodes[k.n.idx()].kind {
                NodeKind::Keyword { arg: Some(s), .. } => {
                    if md.tree.s(*s) == "target" {
                        has_target = true;
                    }
                }
                NodeKind::Keyword { arg: None, .. } => has_dstar = true,
                _ => {}
            }
        }
        if has_target {
            return;
        }
        let _ = has_dstar;
        if args.len() < 2 {
            cx.emit_node(
                "W1506",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "threading.Thread needs the target function".into(),
            );
        }
    }

    fn check_preexec_fn_in_popen(&mut self, cx: &mut WalkCx, node: GNode, keywords: &[GNode]) {
        let eng = cx.eng;
        for &k in keywords {
            let md = eng.md(k.m);
            if let NodeKind::Keyword { arg: Some(s), .. } = &md.tree.nodes[k.n.idx()].kind {
                if md.tree.s(*s) == "preexec_fn" {
                    drop(md);
                    cx.emit_node(
                        "W1509",
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        "Using preexec_fn keyword which may be unsafe in the presence of threads"
                            .into(),
                    );
                }
            }
        }
    }

    fn check_check_kw_in_run(&mut self, cx: &mut WalkCx, node: GNode, keywords: &[GNode]) {
        let eng = cx.eng;
        let has_check = keywords.iter().any(|&k| {
            let md = eng.md(k.m);
            matches!(&md.tree.nodes[k.n.idx()].kind,
                NodeKind::Keyword { arg: Some(s), .. }
                    if md.tree.s(*s) == "check")
        });
        if !has_check {
            cx.emit_node(
                "W1510",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "'subprocess.run' used without explicitly defining the value for 'check'.".into(),
            );
        }
    }

    fn check_shallow_copy_environ(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        args: &[GNode],
        keywords: &[GNode],
    ) {
        let eng = cx.eng;
        let mut arg: Option<GNode> = args.first().copied().or_else(|| {
            for &k in keywords {
                let md = eng.md(k.m);
                if let NodeKind::Keyword { arg: Some(s), value } = &md.tree.nodes[k.n.idx()].kind {
                    if md.tree.s(*s) == "x" {
                        return Some(GNode { m: k.m, n: *value });
                    }
                }
            }
            None
        });
        // NoSuchArgumentError -> infer_kwarg_from_call (copy.copy(**{"x": ..}))
        if arg.is_none() {
            arg = infer_kwarg_from_call(eng, cx, keywords, "x");
        }
        let Some(arg) = arg else { return };
        let vals = u::infer_all(eng, cx.caches, arg);
        for v in vals.iter() {
            if eng.value_qname(v).as_deref() == Some("os._Environ") {
                cx.emit_node(
                    "W1507",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    "Using copy.copy(os.environ). Use os.environ.copy() instead.".into(),
                );
                break;
            }
        }
    }

    /// _check_env_function + _check_invalid_envvar_value — E1507 / W1508
    fn check_env_function(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        args: &[GNode],
        keywords: &[GNode],
    ) {
        let eng = cx.eng;
        let kw_value = |name: &str| -> Option<GNode> {
            for &k in keywords {
                let md = eng.md(k.m);
                if let NodeKind::Keyword { arg: Some(s), value } = &md.tree.nodes[k.n.idx()].kind {
                    if md.tree.s(*s) == name {
                        return Some(GNode { m: k.m, n: *value });
                    }
                }
            }
            None
        };
        let env_name_arg = args.first().copied().or_else(|| kw_value("key"));
        if let Some(a) = env_name_arg {
            let call_arg = u::safe_infer(eng, cx.caches, a);
            self.check_invalid_envvar_value(cx, node, "E1507", &call_arg, false);
        }
        let env_value_arg = if args.len() == 2 {
            Some(args[1])
        } else {
            kw_value("default")
        };
        if let Some(a) = env_value_arg {
            let call_arg = u::safe_infer(eng, cx.caches, a);
            self.check_invalid_envvar_value(cx, node, "W1508", &call_arg, true);
        }
    }

    fn check_invalid_envvar_value(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        message: &'static str,
        call_arg: &Option<Value>,
        allow_none: bool,
    ) {
        let eng = cx.eng;
        let Some(v) = call_arg else { return };
        if v.is_uninferable() {
            return;
        }
        let name = "os.getenv";
        let const_kind: Option<&ConstValue> = match v {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(_) => {
                        drop(md);
                        // re-borrow below
                        None.or(Some(()))
                            .and_then(|_| None) // placeholder; handled after
                    }
                    _ => None,
                }
            }
            Value::SynthConst(_) => None,
            _ => None,
        };
        let _ = const_kind;
        // determine Const-ness + emit decision
        let const_value: Option<ConstValue> = match v {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(c.clone()),
                    _ => None,
                }
            }
            Value::SynthConst(c) => Some((**c).clone()),
            _ => None,
        };
        let templates = |pt: &str| -> String {
            if message == "E1507" {
                format!("{name} does not support {pt} type argument")
            } else {
                format!("{name} default type is {pt}. Expected str or None.")
            }
        };
        match const_value {
            Some(c) => {
                let emit = match &c {
                    ConstValue::None => !allow_none,
                    ConstValue::Str(_) => false,
                    _ => true,
                };
                if emit {
                    let pt = u::value_pytype(eng, v).unwrap_or_default();
                    cx.emit_node(
                        message,
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        templates(&pt),
                    );
                }
            }
            None => {
                let pt = u::value_pytype(eng, v).unwrap_or_default();
                cx.emit_node(
                    message,
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    templates(&pt),
                );
            }
        }
    }
}

/// inferred.root().name — reparent-aware module root of a value
fn value_root_name(eng: &Engine, v: &Value) -> Option<String> {
    match v {
        Value::Node(g) => Some(root_name(eng, *g)),
        Value::BoundMethod { func, .. }
        | Value::DescBM { func, .. }
        | Value::UnboundMethod { func }
        | Value::Property { func, .. }
        | Value::Partial { func, .. } => Some(root_name(eng, *func)),
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(root_name(eng, *cls)),
        Value::Generator { func, .. } => Some(root_name(eng, *func)),
        _ => None,
    }
}

/// is_registered_in_singledispatchmethod_function (utils.py:1568-1581)
fn is_registered_in_singledispatchmethod_function(
    eng: &Engine,
    cx: &mut WalkCx,
    node: GNode,
) -> bool {
    for dec in crate::typecheck::decorator_nodes_pub(eng, node) {
        // find_inferred_fn_from_register
        let md = eng.md(dec.m);
        let func_part = match &md.tree.nodes[dec.n.idx()].kind {
            NodeKind::Call { func, .. } => GNode { m: dec.m, n: *func },
            NodeKind::Attribute { .. } => dec,
            _ => continue,
        };
        let target = match &md.tree.nodes[func_part.n.idx()].kind {
            NodeKind::Attribute { expr, attrname, .. }
                if md.tree.s(*attrname) == "register" =>
            {
                GNode { m: dec.m, n: *expr }
            }
            _ => continue,
        };
        drop(md);
        let func_def = u::safe_infer(eng, cx.caches, target);
        if let Some(Value::Node(g)) = func_def {
            if is_funcdef(eng, g) {
                return decorated_with(
                    eng,
                    g,
                    &[
                        "functools.singledispatchmethod",
                        "singledispatch.singledispatchmethod",
                    ],
                );
            }
        }
    }
    false
}

fn check_mode_str(mode: &str) -> bool {
    let modes: std::collections::HashSet<char> = mode.chars().collect();
    let valid: std::collections::HashSet<char> = "rwatb+Ux".chars().collect();
    let creating = modes.contains(&'x');
    if !modes.is_subset(&valid) || mode.chars().count() > modes.len() {
        return false;
    }
    let mut reading = modes.contains(&'r');
    let writing = modes.contains(&'w');
    let appending = modes.contains(&'a');
    let text = modes.contains(&'t');
    let binary = modes.contains(&'b');
    if modes.contains(&'U') {
        if writing || appending || creating {
            return false;
        }
        reading = true;
    }
    if text && binary {
        return false;
    }
    let total = reading as u32 + writing as u32 + appending as u32 + creating as u32;
    if total > 1 {
        return false;
    }
    if !(reading || writing || appending || creating) {
        return false;
    }
    true
}

/// python repr() of a Const value for W1503 %r
fn const_py_repr(c: &ConstValue) -> String {
    match c {
        ConstValue::None => "None".into(),
        ConstValue::Bool(b) => if *b { "True".into() } else { "False".into() },
        ConstValue::Int(pyast::tree::IntValue::Small(i)) => i.to_string(),
        ConstValue::Int(pyast::tree::IntValue::Big(s)) => s.to_string(),
        ConstValue::Float(f) => pyast::pyrepr::repr_float(*f),
        ConstValue::Complex { .. } => "complex".into(),
        ConstValue::Str(s) => u::py_repr_str(s),
        ConstValue::Bytes(_) => "b'...'".into(),
        ConstValue::Ellipsis => "Ellipsis".into(),
        ConstValue::NotImplemented => "NotImplemented".into(),
        ConstValue::StrSurrogate(_) => String::new(),
    }
}

/// python str(value) of a Const
fn const_str_of(c: &ConstValue) -> String {
    match c {
        ConstValue::None => "None".into(),
        ConstValue::Bool(b) => if *b { "True".into() } else { "False".into() },
        ConstValue::Int(pyast::tree::IntValue::Small(i)) => i.to_string(),
        ConstValue::Int(pyast::tree::IntValue::Big(s)) => s.to_string(),
        ConstValue::Float(f) => pyast::pyrepr::repr_float(*f),
        ConstValue::Complex { .. } => "complex".into(),
        ConstValue::Str(s) => s.to_string(),
        ConstValue::Bytes(_) => const_py_repr(c),
        ConstValue::Ellipsis => "Ellipsis".into(),
        ConstValue::NotImplemented => "NotImplemented".into(),
        ConstValue::StrSurrogate(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// EllipsisChecker — W2301 unnecessary-ellipsis (ellipsis_checker.py)
// ---------------------------------------------------------------------------

/// `EllipsisChecker.visit_const` (ellipsis_checker.py:33-54). Fires when an
/// `...` Const wrapped in an Expr is either preceded by a docstring on its
/// scope (ClassDef/FunctionDef with doc_node) or shares its `.body` with at
/// least one other statement.
pub fn ellipsis_visit_const(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    // node.pytype() == "builtins.Ellipsis"
    if !eng.kind_is(node, |k| matches!(k, NodeKind::Const(ConstValue::Ellipsis))) {
        return;
    }
    // isinstance(node.parent, Expr)
    let Some(parent) = eng.parent(node) else { return };
    if !eng.kind_is(parent, |k| matches!(k, NodeKind::Expr { .. })) {
        return;
    }
    let Some(pp) = eng.parent(parent) else { return };
    let md = eng.md(pp.m);
    let fires = match &md.tree.nodes[pp.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
            d.doc_node.is_some() || d.body.len() > 1
        }
        NodeKind::ClassDef(d) => d.doc_node.is_some() || d.body.len() > 1,
        // any other node: only the len(parent.parent.body) > 1 branch (the
        // doc_node branch requires ClassDef/FunctionDef). pylint reads the
        // literal `.body` attribute, so only nodes that HAVE a `.body` field
        // qualify (Module/For/While/If/With/Try/ExceptHandler/MatchCase);
        // others would AttributeError -> never the ellipsis statement's scope.
        k => primary_body(k).is_some_and(|b| b.len() > 1),
    };
    if fires {
        cx.emit_node(
            "W2301",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            "Unnecessary ellipsis constant".to_string(),
        );
    }
}

/// The primary `.body` statement list of a node, mirroring the astroid `.body`
/// attribute (NOT orelse/handlers/finalbody). None for nodes without a body.
fn primary_body(k: &NodeKind) -> Option<&[NodeId]> {
    match k {
        NodeKind::Module(d) => Some(&d.body),
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => Some(&d.body),
        NodeKind::ClassDef(d) => Some(&d.body),
        NodeKind::For(d) | NodeKind::AsyncFor(d) => Some(&d.body),
        NodeKind::While { body, .. } | NodeKind::If { body, .. } => Some(body),
        NodeKind::With(d) | NodeKind::AsyncWith(d) => Some(&d.body),
        NodeKind::Try(d) | NodeKind::TryStar(d) => Some(&d.body),
        NodeKind::ExceptHandler { body, .. } => Some(body),
        NodeKind::MatchCase { body, .. } => Some(body),
        _ => None,
    }
}

fn infer_maxsize_kwarg(eng: &Engine, cx: &mut WalkCx, keywords: &[GNode]) -> Option<GNode> {
    infer_kwarg_from_call(eng, cx, keywords, "maxsize")
}

/// `infer_kwarg_from_call(call, keyword)` (utils.py:747-760): scan a Call's
/// `**`-unpack keywords (Keyword arg=None), safe_infer each to a Dict, and
/// return the VALUE node for the first item whose key Const value == `keyword`.
/// Used by W1518 / W1501 / W1507 for `**{...}` / `**KWARGS` argument passing.
pub fn infer_kwarg_from_call(
    eng: &Engine,
    cx: &mut WalkCx,
    keywords: &[GNode],
    keyword: &str,
) -> Option<GNode> {
    for &k in keywords {
        let value_node = {
            let md = eng.md(k.m);
            match &md.tree.nodes[k.n.idx()].kind {
                NodeKind::Keyword { arg: None, value } => GNode { m: k.m, n: *value },
                _ => continue,
            }
        };
        let inferred = u::safe_infer(eng, cx.caches, value_node);
        // Only a Dict node carries item KEY nodes we can compare; this mirrors
        // astroid's isinstance(inferred, nodes.Dict) for `**{...}` sources.
        if let Some(Value::Node(g)) = inferred {
            let md = eng.md(g.m);
            if let NodeKind::Dict { items } = &md.tree.nodes[g.n.idx()].kind {
                for &(key, val) in items {
                    if let NodeKind::Const(ConstValue::Str(s)) = &md.tree.nodes[key.idx()].kind {
                        if &**s == keyword {
                            return Some(GNode { m: g.m, n: val });
                        }
                    }
                }
            }
        }
    }
    None
}
