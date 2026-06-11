//! ExceptionsChecker (checkers/exceptions.py) — E0701/E0702/E0704/E0705/
//! E0710/E0711/E0712 + the W0702/W0705/W0706/W0707/W0711/W0715/W0718/W0719
//! computations (disabled by flags; emitted for pragma-resurrection and
//! I0021 bookkeeping).

use pyast::tree::{ConstValue, NodeKind};
use pyast::NodeId;
use pyinfer::ctx::Ctx;
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, Value};

use crate::ckutils as u;
use crate::classes::is_method;
use crate::typecheck::value_name;
use crate::walker::WalkCx;

/// CPython 3.12.12 builtins exception __name__ set (exceptions.py:28-33)
static BUILTIN_EXCEPTIONS: &[&str] = &[
    "ArithmeticError", "AssertionError", "AttributeError", "BaseException",
    "BaseExceptionGroup", "BlockingIOError", "BrokenPipeError", "BufferError",
    "BytesWarning", "ChildProcessError", "ConnectionAbortedError", "ConnectionError",
    "ConnectionRefusedError", "ConnectionResetError", "DeprecationWarning", "EOFError",
    "EncodingWarning", "Exception", "ExceptionGroup", "FileExistsError",
    "FileNotFoundError", "FloatingPointError", "FutureWarning", "GeneratorExit",
    "ImportError", "ImportWarning", "IndentationError", "IndexError",
    "InterruptedError", "IsADirectoryError", "KeyError", "KeyboardInterrupt",
    "LookupError", "MemoryError", "ModuleNotFoundError", "NameError",
    "NotADirectoryError", "NotImplementedError", "OSError", "OverflowError",
    "PendingDeprecationWarning", "PermissionError", "ProcessLookupError",
    "RecursionError", "ReferenceError", "ResourceWarning", "RuntimeError",
    "RuntimeWarning", "StopAsyncIteration", "StopIteration", "SyntaxError",
    "SyntaxWarning", "SystemError", "SystemExit", "TabError", "TimeoutError",
    "TypeError", "UnboundLocalError", "UnicodeDecodeError", "UnicodeEncodeError",
    "UnicodeError", "UnicodeTranslateError", "UnicodeWarning", "UserWarning",
    "ValueError", "Warning", "ZeroDivisionError",
];

fn is_classdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_)))
}
fn is_funcdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)))
}

/// utils.inherit_from_std_ex: the value itself or any class ancestor is
/// named Exception/BaseException defined in builtins.
pub fn inherit_from_std_ex(eng: &Engine, v: &Value) -> bool {
    let cls: Option<GNode> = match v {
        Value::Node(g) if is_classdef(eng, *g) => Some(*g),
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(*cls),
        _ => None,
    };
    let Some(cls) = cls else { return false };
    let mut chain = vec![cls];
    chain.extend(eng.ancestors(cls, true, None));
    chain.iter().any(|&c| {
        let name = eng.node_name(c);
        (name.as_deref() == Some("Exception") || name.as_deref() == Some("BaseException"))
            && eng.md(c.m).name == "builtins"
    })
}

/// _annotated_unpack_infer (exceptions.py:36-54). Err(()) = InferenceError.
fn annotated_unpack_infer(
    eng: &Engine,
    cx: &WalkCx,
    stmt: GNode,
) -> Result<Vec<(GNode, Value)>, ()> {
    let md = eng.md(stmt.m);
    if let NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } =
        &md.tree.nodes[stmt.n.idx()].kind
    {
        let elts: Vec<NodeId> = elts.clone();
        drop(md);
        let mut out = Vec::new();
        for e in elts {
            let g = GNode { m: stmt.m, n: e };
            if let Some(v) = u::safe_infer(eng, cx.caches, g) {
                if !v.is_uninferable() {
                    out.push((g, v));
                }
            }
        }
        return Ok(out);
    }
    drop(md);
    let flow = eng.infer(stmt, &Ctx::new());
    if flow.err.is_some() {
        return Err(());
    }
    Ok(flow
        .vals
        .into_iter()
        .filter(|v| !v.is_uninferable())
        .map(|v| (stmt, v))
        .collect())
}

fn is_raising(eng: &Engine, body: &[NodeId], m: pyinfer::value::ModId) -> bool {
    body.iter().any(|&n| {
        eng.kind_is(GNode { m, n }, |k| matches!(k, NodeKind::Raise { .. }))
    })
}

#[derive(Default)]
pub struct ExceptionsCk;

impl ExceptionsCk {
    /// visit_raise (exceptions.py:305-331)
    pub fn visit_raise(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (exc, cause) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Raise { exc, cause } => (
                    exc.map(|n| GNode { m: node.m, n }),
                    cause.map(|n| GNode { m: node.m, n }),
                ),
                _ => return,
            }
        };
        let Some(expr) = exc else {
            self.check_misplaced_bare_raise(cx, node);
            return;
        };
        if cause.is_none() {
            self.check_raise_missing_from(cx, node, expr);
        } else {
            self.check_bad_exception_cause(cx, node, cause.unwrap());
        }
        // ExceptionRaiseRefVisitor on the raw expr
        self.ref_visit(cx, node, expr);
        let inferred = u::safe_infer(eng, cx.caches, expr);
        let Some(inferred) = inferred else { return };
        if inferred.is_uninferable() {
            return;
        }
        self.leaf_visit(cx, node, &inferred);
    }

    /// ExceptionRaiseRefVisitor: visit_name / visit_call
    fn ref_visit(&mut self, cx: &mut WalkCx, raise_node: GNode, expr: GNode) {
        let eng = cx.eng;
        let md = eng.md(expr.m);
        match &md.tree.nodes[expr.n.idx()].kind {
            NodeKind::Name { name } => {
                let nm = md.tree.s(*name).to_string();
                drop(md);
                self.ref_visit_name(cx, raise_node, expr, &nm);
            }
            NodeKind::Call { func, args, .. } => {
                let func = GNode { m: expr.m, n: *func };
                let first_arg_str: Option<String> = args.first().and_then(|&a| {
                    match &md.tree.nodes[a.idx()].kind {
                        NodeKind::Const(ConstValue::Str(s)) => Some(s.to_string()),
                        _ => None,
                    }
                });
                let nargs = args.len();
                drop(md);
                if let NodeKind::Name { name } = &eng.md(func.m).tree.nodes[func.n.idx()].kind {
                    let nm = {
                        let md2 = eng.md(func.m);
                        md2.tree.s(*name).to_string()
                    };
                    self.ref_visit_name(cx, raise_node, func, &nm);
                }
                // W0715 raising-format-tuple: args[0] str Const with a
                // second positional arg and %-or-{} markers
                if nargs >= 2 {
                    if let Some(msg) = first_arg_str {
                        if msg.contains('%') || (msg.contains('{') && msg.contains('}')) {
                            cx.emit_node(
                                "W0715",
                                u::lineno(eng, raise_node),
                                u::col_offset(eng, raise_node) as i64,
                                "Exception arguments suggest string formatting might be intended"
                                    .into(),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn ref_visit_name(&mut self, cx: &mut WalkCx, raise_node: GNode, name_node: GNode, nm: &str) {
        let eng = cx.eng;
        if nm == "NotImplemented" {
            cx.emit_node(
                "E0711",
                u::lineno(eng, raise_node),
                u::col_offset(eng, raise_node) as i64,
                "NotImplemented raised - should raise NotImplementedError".into(),
            );
            return;
        }
        // W0719 broad-exception-raised burn: _annotated_unpack_infer(name)
        let Ok(exceptions) = annotated_unpack_infer(eng, cx, name_node) else {
            return;
        };
        for (_, v) in exceptions {
            if let Value::Node(g) = &v {
                if is_classdef(eng, *g) {
                    let q = eng.value_qname(&v);
                    if q.as_deref() == Some("builtins.BaseException")
                        || q.as_deref() == Some("builtins.Exception")
                    {
                        let name = eng.node_name(*g).unwrap_or_default();
                        cx.emit_node(
                            "W0719",
                            u::lineno(eng, raise_node),
                            u::col_offset(eng, raise_node) as i64,
                            u::format_template("Raising too general exception: %s", &[&name]),
                        );
                    }
                }
            }
        }
    }

    /// ExceptionRaiseLeafVisitor on the inferred value
    fn leaf_visit(&mut self, cx: &mut WalkCx, raise_node: GNode, v: &Value) {
        let eng = cx.eng;
        let line = u::lineno(eng, raise_node);
        let col = u::col_offset(eng, raise_node) as i64;
        let emit_bad_type = |cx: &mut WalkCx, what: &str| {
            cx.emit_node(
                "E0702",
                line,
                col,
                u::format_template(
                    "Raising %s while only classes or instances are allowed",
                    &[what],
                ),
            );
        };
        match v {
            // visit_const: python type name of the const value
            Value::Node(g)
                if eng.kind_is(*g, |k| matches!(k, NodeKind::Const(_))) =>
            {
                let md = eng.md(g.m);
                let NodeKind::Const(c) = &md.tree.nodes[g.n.idx()].kind else { return };
                let tn = const_type_name(c);
                drop(md);
                emit_bad_type(cx, tn);
            }
            Value::SynthConst(c) => emit_bad_type(cx, const_type_name(c)),
            // instances -> their class
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
                self.leaf_visit_classdef(cx, raise_node, *cls);
            }
            Value::Node(g) if is_classdef(eng, *g) => {
                self.leaf_visit_classdef(cx, raise_node, *g);
            }
            // visit_tuple
            Value::Node(g)
                if eng.kind_is(*g, |k| matches!(k, NodeKind::Tuple { .. })) =>
            {
                emit_bad_type(cx, "tuple");
            }
            Value::SynthSeq { kind: pyinfer::value::SeqKind::Tuple, .. } => {
                emit_bad_type(cx, "tuple");
            }
            // visit_default: getattr(node, "name", classname)
            _ => {
                let what = leaf_default_name(eng, v);
                emit_bad_type(cx, &what);
            }
        }
    }

    fn leaf_visit_classdef(&mut self, cx: &mut WalkCx, raise_node: GNode, cls: GNode) {
        let eng = cx.eng;
        if !inherit_from_std_ex(eng, &Value::Node(cls))
            && crate::typecheck::has_known_bases(eng, cx.caches, cls)
        {
            cx.emit_node(
                "E0710",
                u::lineno(eng, raise_node),
                u::col_offset(eng, raise_node) as i64,
                "Raising a class which doesn't inherit from BaseException".into(),
            );
        }
    }

    /// _check_misplaced_bare_raise — E0704
    fn check_misplaced_bare_raise(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let scope = eng.scope(node);
        if is_funcdef(eng, scope)
            && is_method(eng, scope)
            && eng.node_name(scope).as_deref() == Some("__exit__")
        {
            return;
        }
        let mut current = Some(node);
        while let Some(c) = current {
            match eng.parent(c) {
                Some(p) => {
                    let stop = eng.kind_is(p, |k| {
                        matches!(
                            k,
                            NodeKind::ExceptHandler { .. }
                                | NodeKind::FunctionDef(_)
                                | NodeKind::AsyncFunctionDef(_)
                        )
                    });
                    if stop {
                        // found: parent must be an ExceptHandler
                        if !eng.kind_is(p, |k| matches!(k, NodeKind::ExceptHandler { .. })) {
                            break; // FunctionDef -> emit
                        }
                        return;
                    }
                    current = Some(p);
                }
                None => {
                    current = None;
                }
            }
        }
        cx.emit_node(
            "E0704",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            "The raise statement is not inside an except clause".into(),
        );
    }

    /// _check_bad_exception_cause — E0705
    fn check_bad_exception_cause(&mut self, cx: &mut WalkCx, node: GNode, cause: GNode) {
        let eng = cx.eng;
        let inferred = u::safe_infer(eng, cx.caches, cause);
        let Some(v) = inferred else { return };
        if v.is_uninferable() {
            return;
        }
        let emit = |cx: &mut WalkCx| {
            cx.emit_node(
                "E0705",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "Exception cause set to something which is not an exception, nor None".into(),
            );
        };
        // Const branch
        let const_val: Option<bool> = match &v {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(!matches!(c, ConstValue::None)),
                    _ => None,
                }
            }
            Value::SynthConst(c) => Some(!matches!(&**c, ConstValue::None)),
            _ => None,
        };
        if let Some(non_none) = const_val {
            if non_none {
                emit(cx);
            }
            return;
        }
        let is_cls = matches!(&v, Value::Node(g) if is_classdef(eng, *g));
        if !is_cls && !inherit_from_std_ex(eng, &v) {
            emit(cx);
        }
    }

    /// _check_raise_missing_from — W0707 (disabled; resurrection support)
    fn check_raise_missing_from(&mut self, cx: &mut WalkCx, node: GNode, exc: GNode) {
        let eng = cx.eng;
        // find_except_wrapper_node_in_scope
        let mut handler: Option<GNode> = None;
        let mut cur = node;
        while let Some(p) = eng.parent(cur) {
            if eng.is_scope(p) {
                break;
            }
            if eng.kind_is(p, |k| matches!(k, NodeKind::ExceptHandler { .. })) {
                handler = Some(p);
                break;
            }
            cur = p;
        }
        let Some(handler) = handler else { return };
        let (htype, hname): (Option<GNode>, Option<GNode>) = {
            let md = eng.md(handler.m);
            match &md.tree.nodes[handler.n.idx()].kind {
                NodeKind::ExceptHandler { type_, name, .. } => (
                    type_.map(|n| GNode { m: handler.m, n }),
                    name.map(|n| GNode { m: handler.m, n }),
                ),
                _ => return,
            }
        };
        if hname.is_none() {
            let mut class_of_old_error = "Exception".to_string();
            if let Some(t) = htype {
                if eng.kind_is(t, |k| matches!(k, NodeKind::Name { .. } | NodeKind::Tuple { .. })) {
                    class_of_old_error = u::as_string(eng, t);
                }
            }
            cx.emit_node(
                "W0707",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                u::format_template(
                    "Consider explicitly re-raising using %s'%s from %s'",
                    &[
                        &format!("'except {class_of_old_error} as exc' and "),
                        &u::as_string(eng, node),
                        "exc",
                    ],
                ),
            );
            return;
        }
        let hname_str = eng.node_name(hname.unwrap()).unwrap_or_default();
        let md = eng.md(exc.m);
        let exc_is_call_of_name = matches!(&md.tree.nodes[exc.n.idx()].kind,
            NodeKind::Call { func, .. }
                if matches!(md.tree.nodes[func.idx()].kind, NodeKind::Name { .. }));
        let exc_name: Option<String> = match &md.tree.nodes[exc.n.idx()].kind {
            NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
            _ => None,
        };
        drop(md);
        if exc_is_call_of_name
            || (exc_name.is_some() && exc_name.as_deref() != Some(hname_str.as_str()))
        {
            cx.emit_node(
                "W0707",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                u::format_template(
                    "Consider explicitly re-raising using %s'%s from %s'",
                    &["", &u::as_string(eng, node), &hname_str],
                ),
            );
        }
    }

    pub fn visit_trystar(&mut self, cx: &mut WalkCx, node: GNode) {
        self.visit_try(cx, node);
    }

    /// visit_try (exceptions.py:576-651)
    pub fn visit_try(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        self.check_try_except_raise(cx, node);
        let handlers: Vec<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Try(d) | NodeKind::TryStar(d) => {
                    d.handlers.iter().map(|&n| GNode { m: node.m, n }).collect()
                }
                _ => return,
            }
        };
        let nb_handlers = handlers.len();
        let mut exceptions_classes: Vec<Value> = Vec::new();
        for (index, handler) in handlers.iter().enumerate() {
            let (htype, hbody): (Option<GNode>, Vec<NodeId>) = {
                let md = eng.md(handler.m);
                match &md.tree.nodes[handler.n.idx()].kind {
                    NodeKind::ExceptHandler { type_, body, .. } => {
                        (type_.map(|n| GNode { m: handler.m, n }), body.clone())
                    }
                    _ => continue,
                }
            };
            match htype {
                None => {
                    if !is_raising(eng, &hbody, handler.m) {
                        cx.emit_node(
                            "W0702",
                            u::lineno(eng, *handler),
                            u::col_offset(eng, *handler) as i64,
                            "No exception type(s) specified".into(),
                        );
                    }
                    if index < nb_handlers - 1 {
                        cx.emit_node(
                            "E0701",
                            u::lineno(eng, node),
                            u::col_offset(eng, node) as i64,
                            u::format_template(
                                "Bad except clauses order (%s)",
                                &["empty except clause should always appear last"],
                            ),
                        );
                    }
                }
                Some(t) if eng.kind_is(t, |k| matches!(k, NodeKind::BoolOp { .. })) => {
                    let op = {
                        let md = eng.md(t.m);
                        match &md.tree.nodes[t.n.idx()].kind {
                            NodeKind::BoolOp { op, .. } => op.to_string(),
                            _ => String::new(),
                        }
                    };
                    cx.emit_node(
                        "W0711",
                        u::lineno(eng, *handler),
                        u::col_offset(eng, *handler) as i64,
                        u::format_template(
                            "Exception to catch is the result of a binary \"%s\" operation",
                            &[&op],
                        ),
                    );
                }
                Some(t) => {
                    let Ok(exceptions) = annotated_unpack_infer(eng, cx, t) else {
                        continue;
                    };
                    for (part, exc) in &exceptions {
                        let mut exception = exc.clone();
                        if crate::typecheck::value_is_instance(eng, &exception)
                            && inherit_from_std_ex(eng, &exception)
                        {
                            if let Value::Inst { cls, .. } | Value::ExcInst { cls, .. } =
                                &exception
                            {
                                exception = Value::Node(*cls);
                            }
                        }
                        self.check_catching_non_exception(cx, *handler, t, &exception, *part);
                        let Value::Node(excls) = &exception else { continue };
                        if !is_classdef(eng, *excls) {
                            continue;
                        }
                        let exc_ancestors: Vec<GNode> = eng
                            .ancestors(*excls, true, None)
                            .into_iter()
                            .filter(|&a| is_classdef(eng, a))
                            .collect();
                        for previous_exc in &exceptions_classes {
                            if let Value::Node(pg) = previous_exc {
                                if exc_ancestors.contains(pg) {
                                    let msg = format!(
                                        "{} is an ancestor class of {}",
                                        eng.node_name(*pg).unwrap_or_default(),
                                        eng.node_name(*excls).unwrap_or_default()
                                    );
                                    cx.emit_node(
                                        "E0701",
                                        u::lineno(eng, t),
                                        u::col_offset(eng, t) as i64,
                                        u::format_template("Bad except clauses order (%s)", &[&msg]),
                                    );
                                }
                            }
                        }
                        // W0718 broad-exception-caught
                        let q = eng.value_qname(&exception);
                        if (q.as_deref() == Some("builtins.BaseException")
                            || q.as_deref() == Some("builtins.Exception"))
                            && !is_raising(eng, &hbody, handler.m)
                        {
                            let name = eng.node_name(*excls).unwrap_or_default();
                            cx.emit_node(
                                "W0718",
                                u::lineno(eng, t),
                                u::col_offset(eng, t) as i64,
                                u::format_template("Catching too general exception %s", &[&name]),
                            );
                        }
                        // W0705 duplicate-except: identity membership
                        let dup = exceptions_classes.iter().any(|p| match (p, &exception) {
                            (Value::Node(a), Value::Node(b)) => a == b,
                            _ => false,
                        });
                        if dup {
                            let name = eng.node_name(*excls).unwrap_or_default();
                            cx.emit_node(
                                "W0705",
                                u::lineno(eng, t),
                                u::col_offset(eng, t) as i64,
                                u::format_template(
                                    "Catching previously caught exception type %s",
                                    &[&name],
                                ),
                            );
                        }
                    }
                    // :651 accumulate the UN-promoted inferred values
                    exceptions_classes.extend(exceptions.into_iter().map(|(_, e)| e));
                }
            }
        }
    }

    /// _check_catching_non_exception (exceptions.py:417-476) — E0712
    fn check_catching_non_exception(
        &mut self,
        cx: &mut WalkCx,
        handler: GNode,
        htype: GNode,
        exc: &Value,
        part: GNode,
    ) {
        let eng = cx.eng;
        let line = u::lineno(eng, htype);
        let col = u::col_offset(eng, htype) as i64;
        // inferred-Tuple branch (binop folds materialize synthetic tuples
        // whose elements are already inferred values)
        let tuple_elts: Option<Vec<Value>> = match exc {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Tuple { elts, .. } => Some(
                        elts.iter()
                            .map(|&n| Value::Node(GNode { m: g.m, n }))
                            .collect(),
                    ),
                    _ => None,
                }
            }
            Value::SynthSeq { kind: pyinfer::value::SeqKind::Tuple, elems } => {
                Some(elems.iter().cloned().collect())
            }
            _ => None,
        };
        if let Some(elts) = &tuple_elts {
            let inferred: Vec<Option<Value>> = elts
                .iter()
                .map(|e| match e {
                    Value::Node(g) => u::safe_infer(eng, cx.caches, *g),
                    // safe_infer of an EvaluatedObject node yields the
                    // wrapped value (node_classes.py:5010-5028)
                    Value::EvaluatedObject { value } => Some((**value).clone()),
                    other => Some(other.clone()),
                })
                .collect();
            if inferred
                .iter()
                .any(|v| matches!(v, Some(x) if x.is_uninferable()))
            {
                return;
            }
            let all_ok = inferred.iter().all(|v| match v {
                Some(x) => {
                    inherit_from_std_ex(eng, x)
                        || matches!(x, Value::Node(g) if is_classdef(eng, *g)
                            && !crate::typecheck::has_known_bases(eng, cx.caches, *g))
                }
                None => false,
            });
            if all_ok {
                return;
            }
            // fall through to the non-ClassDef branch (Tuple is not ClassDef)
        }
        let is_cls = matches!(exc, Value::Node(g) if is_classdef(eng, *g));
        if !is_cls {
            // Const None special case
            let is_const_none = match exc {
                Value::Node(g) => eng.kind_is(*g, |k| {
                    matches!(k, NodeKind::Const(ConstValue::None))
                }),
                Value::SynthConst(c) => matches!(&**c, ConstValue::None),
                _ => false,
            };
            if is_const_none {
                let htype_is_none = eng.kind_is(htype, |k| {
                    matches!(k, NodeKind::Const(ConstValue::None))
                });
                // handler.type.parent_of(exc): exc node under htype subtree
                let parent_of = match exc {
                    Value::Node(g) => {
                        let mut cur = Some(*g);
                        let mut found = false;
                        while let Some(c) = cur {
                            if c == htype {
                                found = true;
                                break;
                            }
                            cur = eng.parent(c);
                        }
                        found && *g != htype
                    }
                    _ => false,
                };
                if htype_is_none || parent_of {
                    cx.emit_node(
                        "E0712",
                        line,
                        col,
                        u::format_template(
                            "Catching an exception which doesn't inherit from Exception: %s",
                            &[&u::as_string(eng, part)],
                        ),
                    );
                }
            } else {
                cx.emit_node(
                    "E0712",
                    line,
                    col,
                    u::format_template(
                        "Catching an exception which doesn't inherit from Exception: %s",
                        &[&u::as_string(eng, part)],
                    ),
                );
            }
            let _ = handler;
            return;
        }
        let Value::Node(excls) = exc else { return };
        let name = eng.node_name(*excls).unwrap_or_default();
        if !inherit_from_std_ex(eng, exc) && !BUILTIN_EXCEPTIONS.contains(&name.as_str()) {
            if crate::typecheck::has_known_bases(eng, cx.caches, *excls) {
                cx.emit_node(
                    "E0712",
                    line,
                    col,
                    u::format_template(
                        "Catching an exception which doesn't inherit from Exception: %s",
                        &[&name],
                    ),
                );
            }
        }
    }

    /// _check_try_except_raise — W0706 + its safe_infer burn
    fn check_try_except_raise(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let handlers: Vec<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Try(d) | NodeKind::TryStar(d) => {
                    d.handlers.iter().map(|&n| GNode { m: node.m, n }).collect()
                }
                _ => return,
            }
        };
        // gather: Some(vec) | None (= python None: inference failure)
        let gather = |cx: &mut WalkCx, handler: GNode| -> Option<Vec<Value>> {
            let md = eng.md(handler.m);
            let htype = match &md.tree.nodes[handler.n.idx()].kind {
                NodeKind::ExceptHandler { type_, .. } => *type_,
                _ => None,
            };
            drop(md);
            let Some(tn) = htype else { return Some(Vec::new()) };
            let t = GNode { m: handler.m, n: tn };
            let inferred = u::safe_infer(eng, cx.caches, t);
            match inferred {
                Some(Value::Node(g))
                    if eng.kind_is(g, |k| matches!(k, NodeKind::Tuple { .. })) =>
                {
                    let md = eng.md(g.m);
                    let NodeKind::Tuple { elts, .. } = &md.tree.nodes[g.n.idx()].kind else {
                        return Some(Vec::new());
                    };
                    let mut seen = std::collections::HashSet::new();
                    let out: Vec<Value> = elts
                        .iter()
                        .filter(|&&e| {
                            let g2 = GNode { m: g.m, n: e };
                            eng.kind_is(g2, |k| {
                                matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. })
                            }) && seen.insert(e)
                        })
                        .map(|&e| Value::Node(GNode { m: g.m, n: e }))
                        .collect();
                    Some(out)
                }
                Some(v) => Some(vec![v]),
                None => None,
            }
        };
        let mut bare_raise = false;
        let mut handler_having_bare_raise: Option<GNode> = None;
        let mut exceptions_in_bare_handler: Option<Vec<Value>> = Some(Vec::new());
        let mut broke = false;
        for handler in &handlers {
            if bare_raise {
                let excs_in_current = gather(cx, *handler);
                let Some(excs) = excs_in_current else {
                    broke = true;
                    break;
                };
                if excs.is_empty() {
                    broke = true;
                    break;
                }
                let Some(bare_excs) = &exceptions_in_bare_handler else {
                    broke = true;
                    break;
                };
                for exc_cur in &excs {
                    // safe_infer of the gathered value (node values re-infer)
                    let inferred_current: Option<Value> = match exc_cur {
                        Value::Node(g) => u::safe_infer(eng, cx.caches, *g),
                        other => Some(other.clone()),
                    };
                    let any_sub = bare_excs.iter().any(|e| {
                        let ie: Option<Value> = match e {
                            Value::Node(g) => u::safe_infer(eng, cx.caches, *g),
                            other => Some(other.clone()),
                        };
                        is_subclass_of(eng, &ie, &inferred_current)
                    });
                    if any_sub {
                        bare_raise = false;
                        break;
                    }
                }
            }
            // `raise` as the first statement of the handler body
            let (first_is_bare_raise, first_is_raise) = {
                let md = eng.md(handler.m);
                match &md.tree.nodes[handler.n.idx()].kind {
                    NodeKind::ExceptHandler { body, .. } => match body.first() {
                        Some(&b) => match &md.tree.nodes[b.idx()].kind {
                            NodeKind::Raise { exc, .. } => (exc.is_none(), true),
                            _ => (false, false),
                        },
                        None => (false, false),
                    },
                    _ => (false, false),
                }
            };
            if first_is_raise && first_is_bare_raise {
                bare_raise = true;
                handler_having_bare_raise = Some(*handler);
                exceptions_in_bare_handler = gather(cx, *handler);
            }
        }
        if !broke {
            if bare_raise {
                if let Some(h) = handler_having_bare_raise {
                    cx.emit_node(
                        "W0706",
                        u::lineno(eng, h),
                        u::col_offset(eng, h) as i64,
                        "The except handler raises immediately".into(),
                    );
                }
            }
        }
    }
}

/// utils.is_subclass_of(child, parent) (utils.py:1647-1663): for each child
/// ancestor, astroid.helpers.is_subtype(ancestor, parent) — which is
/// _type_check(type1=parent, type2=ancestor): ASTROID-flavor
/// has_known_bases on parent THEN ancestor (memo side effects through
/// Engine.known_bases_cache are load-bearing: they poison the shared
/// `_all_bases_known` memo exactly like pylint), then identity membership
/// `parent in ancestor.mro()[:-1]`. _NonDeducibleTypeHierarchy -> continue.
fn is_subclass_of(eng: &Engine, child: &Option<Value>, parent: &Option<Value>) -> bool {
    let (Some(Value::Node(c)), Some(Value::Node(p))) = (child, parent) else {
        return false;
    };
    if !is_classdef(eng, *c) || !is_classdef(eng, *p) {
        return false;
    }
    for ancestor in eng.ancestors(*c, true, None) {
        // all(map(has_known_bases, (parent, ancestor))) — lazy order
        if !eng.has_known_bases(*p, 0) {
            continue; // _NonDeducibleTypeHierarchy
        }
        if !eng.has_known_bases(ancestor, 0) {
            continue;
        }
        match eng.mro(ancestor, None) {
            Err(_) => continue, // MroError -> _NonDeducibleTypeHierarchy
            Ok(mro) => {
                let upto = mro.len().saturating_sub(1);
                if mro[..upto].contains(p) {
                    return true;
                }
            }
        }
    }
    false
}

fn const_type_name(c: &ConstValue) -> &'static str {
    match c {
        ConstValue::None => "NoneType",
        ConstValue::Bool(_) => "bool",
        ConstValue::Int(_) => "int",
        ConstValue::Float(_) => "float",
        ConstValue::Complex { .. } => "complex",
        ConstValue::Str(_) | ConstValue::StrSurrogate(_) => "str",
        ConstValue::Bytes(_) => "bytes",
        ConstValue::Ellipsis => "ellipsis",
        ConstValue::NotImplemented => "NotImplementedType",
    }
}

/// getattr(node, "name", node.__class__.__name__) for leaf visit_default
fn leaf_default_name(eng: &Engine, v: &Value) -> String {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => {
                    drop(md);
                    eng.node_name(*g).unwrap_or_default()
                }
                NodeKind::Module(_) => {
                    drop(md);
                    eng.node_name(*g).unwrap_or_default()
                }
                NodeKind::Lambda(_) => "<lambda>".into(),
                NodeKind::List { .. } => "List".into(),
                NodeKind::Set { .. } => "Set".into(),
                NodeKind::Dict { .. } => "Dict".into(),
                NodeKind::Slice { .. } => "Slice".into(),
                k => format!("{:?}", kind_class_name(k)),
            }
        }
        Value::BoundMethod { func, .. }
        | Value::DescBM { func, .. }
        | Value::UnboundMethod { func }
        | Value::Partial { func, .. }
        | Value::Property { func, .. } => eng.node_name(*func).unwrap_or_default(),
        Value::Generator { is_async, .. } => {
            if *is_async { "async_generator".into() } else { "generator".into() }
        }
        Value::Super { .. } => "super".into(),
        _ => value_name(eng, v).unwrap_or_default(),
    }
}

fn kind_class_name(k: &NodeKind) -> &'static str {
    match k {
        NodeKind::JoinedStr { .. } => "JoinedStr",
        NodeKind::Compare { .. } => "Compare",
        NodeKind::BinOp { .. } => "BinOp",
        NodeKind::UnaryOp { .. } => "UnaryOp",
        NodeKind::Call { .. } => "Call",
        NodeKind::Subscript { .. } => "Subscript",
        NodeKind::Attribute { .. } => "Attribute",
        NodeKind::Name { .. } => "Name",
        _ => "NodeNG",
    }
}
