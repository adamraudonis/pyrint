//! ClassChecker + SpecialMethodsChecker + NewStyleConflictChecker ports
//! (pylint 4.0.5 `pylint/checkers/classes/{class_checker,
//! special_methods_checker}.py`, `pylint/checkers/newstyle.py`).
//! In-scope codes: E0202/E0203/E0211/E0213/E0236-E0245/F0202,
//! E0301-E0313, E1003. Disabled-message paths that burn inference are
//! ported where they share caches with in-scope decisions.

use std::rc::Rc;

use pyast::tree::{ConstValue, NodeKind};
use pyinfer::ctx::Ctx;
use pyinfer::graph::{Engine, FType};
use pyinfer::value::{GNode, GSym, Value, NV};
use rustc_hash::FxHashMap;

use crate::ckutils as u;
use crate::typecheck as tc;
use crate::walker::WalkCx;

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

fn is_classdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_)))
}
fn is_funcdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)))
}

/// FunctionDef.is_method (scoped_nodes.py:1435-1446)
pub fn is_method(eng: &Engine, func: GNode) -> bool {
    if eng.func_type(func) == FType::Function {
        return false;
    }
    match eng.parent(func) {
        Some(p) => is_classdef(eng, eng.frame(p)),
        None => false,
    }
}

/// utils.node_frame_class (utils.py:677-699): walk frames to the class
fn node_frame_class(eng: &Engine, node: GNode) -> Option<GNode> {
    let mut klass = eng.frame(node);
    loop {
        if is_classdef(eng, klass) {
            return Some(klass);
        }
        let p = eng.parent(klass)?;
        klass = eng.frame(p);
    }
}

/// utils.is_attr_private: `^_{2,10}.*[^_]+_?$`
pub fn is_attr_private(name: &str) -> bool {
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i] == b'_' {
        i += 1;
    }
    if !(2..=10).contains(&i) {
        return false;
    }
    // `.*[^_]+_?$`: needs at least one non-underscore after the prefix
    let rest = &b[i..];
    let trimmed = rest.strip_suffix(b"_").unwrap_or(rest);
    trimmed.iter().any(|&c| c != b'_')
}

fn class_body(eng: &Engine, cls: GNode) -> Vec<GNode> {
    let md = eng.md(cls.m);
    match &md.tree.nodes[cls.n.idx()].kind {
        NodeKind::ClassDef(d) => d.body.iter().map(|&n| GNode { m: cls.m, n }).collect(),
        _ => Vec::new(),
    }
}

fn class_bases_nodes(eng: &Engine, cls: GNode) -> Vec<GNode> {
    eng.class_bases(cls)
}

/// ClassDef.ilookup("__slots__")-ish: infer every local stmt bound to the
/// name; Err(()) on InferenceError (first-stmt error aborts like
/// _infer_stmts raising).
fn ilookup_slots(eng: &Engine, cls: GNode) -> Result<Vec<Value>, ()> {
    let sym = eng.sym("__slots__");
    let stmts = eng.class_locals_get(cls, sym);
    if stmts.is_empty() {
        return Err(());
    }
    let mut out: Vec<Value> = Vec::new();
    let mut any_ok = false;
    for s in stmts {
        let flow = eng.infer(s, &Ctx::new());
        if !flow.vals.is_empty() {
            any_ok = true;
        }
        out.extend(flow.vals);
    }
    if !any_ok {
        return Err(()); // all raised -> InferenceError
    }
    Ok(out)
}

/// elements of an inferred __slots__ value: Ok(list of NV) or Err to skip
enum SlotsElts {
    Elts(Vec<NV>),
    NoItered,
}

fn slots_elements(eng: &Engine, v: &Value) -> SlotsElts {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } | NodeKind::Set { elts } => {
                    SlotsElts::Elts(elts.iter().map(|&e| NV::N(GNode { m: g.m, n: e })).collect())
                }
                NodeKind::Dict { items } => SlotsElts::Elts(
                    items.iter().map(|(k, _)| NV::N(GNode { m: g.m, n: *k })).collect(),
                ),
                _ => SlotsElts::NoItered,
            }
        }
        Value::SynthSeq { elems, .. } | Value::FrozenSet { elems } => {
            SlotsElts::Elts(elems.iter().map(|e| NV::V(e.clone())).collect())
        }
        Value::SynthDict { items } => {
            SlotsElts::Elts(items.iter().map(|(k, _)| NV::V(k.clone())).collect())
        }
        _ => SlotsElts::NoItered,
    }
}

fn nv_const(eng: &Engine, nv: &NV) -> Option<ConstValue> {
    match nv {
        NV::N(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Const(c) => Some(tc::clone_const_pub(c)),
                _ => None,
            }
        }
        NV::V(Value::SynthConst(c)) => Some(tc::clone_const_pub(c)),
        NV::V(Value::Node(g)) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Const(c) => Some(tc::clone_const_pub(c)),
                _ => None,
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// SpecialMethodsChecker (E0301-E0313)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct SpecialCk;

/// _SPECIAL_METHODS_PARAMS (utils.py:78-186): None -> variadic
fn special_method_params(name: &str) -> Option<Option<(i64, Option<i64>)>> {
    // returns Some(expected) when name in PYMETHODS;
    // expected: None = variadic; Some((n, None)) = exactly n;
    // Some((a, Some(b))) = the (a, b) tuple form
    const P_NONE: &[&str] = &["__new__", "__init__", "__call__", "__init_subclass__"];
    const P0: &[&str] = &[
        "__del__", "__repr__", "__str__", "__bytes__", "__hash__", "__bool__", "__dir__",
        "__len__", "__length_hint__", "__iter__", "__reversed__", "__neg__", "__pos__",
        "__abs__", "__invert__", "__complex__", "__int__", "__float__", "__index__",
        "__trunc__", "__floor__", "__ceil__", "__enter__", "__aenter__",
        "__getnewargs_ex__", "__getnewargs__", "__getstate__", "__reduce__", "__copy__",
        "__unicode__", "__nonzero__", "__await__", "__aiter__", "__anext__", "__fspath__",
        "__subclasses__",
    ];
    const P1: &[&str] = &[
        "__format__", "__lt__", "__le__", "__eq__", "__ne__", "__gt__", "__ge__",
        "__getattr__", "__getattribute__", "__delattr__", "__delete__", "__instancecheck__",
        "__subclasscheck__", "__getitem__", "__missing__", "__delitem__", "__contains__",
        "__add__", "__sub__", "__mul__", "__truediv__", "__floordiv__", "__rfloordiv__",
        "__mod__", "__divmod__", "__lshift__", "__rshift__", "__and__", "__xor__", "__or__",
        "__radd__", "__rsub__", "__rmul__", "__rtruediv__", "__rmod__", "__rdivmod__",
        "__rpow__", "__rlshift__", "__rrshift__", "__rand__", "__rxor__", "__ror__",
        "__iadd__", "__isub__", "__imul__", "__itruediv__", "__ifloordiv__", "__imod__",
        "__ilshift__", "__irshift__", "__iand__", "__ixor__", "__ior__", "__ipow__",
        "__setstate__", "__reduce_ex__", "__deepcopy__", "__cmp__", "__matmul__",
        "__rmatmul__", "__imatmul__", "__div__",
    ];
    const P2: &[&str] = &["__setattr__", "__get__", "__set__", "__setitem__", "__set_name__"];
    const P3: &[&str] = &["__exit__", "__aexit__"];
    if P_NONE.contains(&name) {
        return Some(None);
    }
    if P0.contains(&name) {
        return Some(Some((0, None)));
    }
    if P1.contains(&name) {
        return Some(Some((1, None)));
    }
    if P2.contains(&name) {
        return Some(Some((2, None)));
    }
    if P3.contains(&name) {
        return Some(Some((3, None)));
    }
    if name == "__round__" {
        return Some(Some((0, Some(1))));
    }
    if name == "__pow__" {
        return Some(Some((1, Some(2))));
    }
    None
}

/// _safe_infer_call_result (special_methods_checker.py:30-53):
/// EXACTLY two pulls — value + ambiguity probe.
fn safe_infer_call_result(eng: &Engine, func: GNode) -> Option<Value> {
    let mut vals: Vec<Value> = Vec::new();
    let end = eng.function_infer_call_result_to(func, Some(func), None, &mut |v| {
        vals.push(v);
        if vals.len() >= 2 {
            pyinfer::value::Drive::Stop
        } else {
            pyinfer::value::Drive::Go
        }
    });
    match (vals.len(), &end) {
        (0, _) => None,                                  // error or no values
        (1, pyinfer::value::End::Raised(_)) => None,     // ambiguity-probe error
        (1, _) => Some(vals.pop().unwrap()),
        _ => None,                                       // ambiguity
    }
}

/// is_function_body_ellipsis (utils.py:1925-1930)
fn is_function_body_ellipsis(eng: &Engine, func: GNode) -> bool {
    let md = eng.md(func.m);
    let body = match &md.tree.nodes[func.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => &d.body,
        _ => return false,
    };
    if body.len() != 1 {
        return false;
    }
    match &md.tree.nodes[body[0].idx()].kind {
        NodeKind::Expr { value } => matches!(
            md.tree.nodes[value.idx()].kind,
            NodeKind::Const(ConstValue::Ellipsis)
        ),
        _ => false,
    }
}

/// _is_wrapped_type: bases.Instance named T, NOT a Const
fn is_wrapped_type(eng: &Engine, v: &Value, t: &str) -> bool {
    let is_instance_not_const = match v {
        Value::Inst { .. } | Value::ExcInst { .. } => true,
        Value::Node(g) => {
            let md = eng.md(g.m);
            matches!(
                md.tree.nodes[g.n.idx()].kind,
                NodeKind::List { .. } | NodeKind::Tuple { .. } | NodeKind::Set { .. }
                    | NodeKind::Dict { .. }
            )
        }
        Value::SynthSeq { .. } | Value::SynthDict { .. } | Value::FrozenSet { .. } => true,
        _ => false,
    };
    is_instance_not_const && tc::value_name(eng, v).as_deref() == Some(t)
}

fn value_const(eng: &Engine, v: &Value) -> Option<ConstValue> {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Const(c) => Some(tc::clone_const_pub(c)),
                _ => None,
            }
        }
        Value::SynthConst(c) => Some(tc::clone_const_pub(c)),
        _ => None,
    }
}

fn is_int_v(eng: &Engine, v: &Value) -> bool {
    if is_wrapped_type(eng, v, "int") {
        return true;
    }
    matches!(value_const(eng, v), Some(ConstValue::Int(_)) | Some(ConstValue::Bool(_)))
}

fn is_str_v(eng: &Engine, v: &Value) -> bool {
    if is_wrapped_type(eng, v, "str") {
        return true;
    }
    matches!(
        value_const(eng, v),
        Some(ConstValue::Str(_)) | Some(ConstValue::StrSurrogate(_))
    )
}

fn is_bool_v(eng: &Engine, v: &Value) -> bool {
    if is_wrapped_type(eng, v, "bool") {
        return true;
    }
    matches!(value_const(eng, v), Some(ConstValue::Bool(_)))
}

fn is_bytes_v(eng: &Engine, v: &Value) -> bool {
    if is_wrapped_type(eng, v, "bytes") {
        return true;
    }
    matches!(value_const(eng, v), Some(ConstValue::Bytes(_)))
}

fn is_tuple_v(eng: &Engine, v: &Value) -> bool {
    if is_wrapped_type(eng, v, "tuple") {
        return true;
    }
    // Const holding a python tuple: not representable here
    matches!(v, Value::Node(g) if eng.kind_is(*g, |k| matches!(k, NodeKind::Tuple { .. })))
        || matches!(v, Value::SynthSeq { kind: pyinfer::value::SeqKind::Tuple, .. })
}

fn is_dict_v(eng: &Engine, v: &Value) -> bool {
    if is_wrapped_type(eng, v, "dict") {
        return true;
    }
    matches!(v, Value::Node(g) if eng.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })))
        || matches!(v, Value::SynthDict { .. })
}

/// _is_iterator (special_methods_checker.py:299-320)
fn is_iterator(eng: &Engine, v: &Value) -> bool {
    let next_sym = eng.sym("__next__");
    match v {
        Value::Generator { .. } => true,
        Value::Node(g)
            if eng.kind_is(*g, |k| {
                matches!(
                    k,
                    NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_)
                        | NodeKind::GeneratorExp(_)
                )
            }) =>
        {
            true
        }
        Value::Node(g) if is_classdef(eng, *g) => {
            if let Some(Value::Node(meta)) = eng.metaclass(*g, None) {
                if is_classdef(eng, meta)
                    && !tc::class_local_attr(eng, meta, "__next__").is_empty()
                {
                    return true;
                }
            }
            false
        }
        _ => {
            // bases.Instance arm: local_attr on the proxied class
            let is_instance = matches!(v, Value::Inst { .. } | Value::ExcInst { .. })
                || tc::value_is_instance(eng, v);
            if is_instance {
                if let Some(cls) = eng.proxied_class(v) {
                    let vals = eng.class_locals_get(cls, next_sym);
                    if !vals.is_empty() {
                        return true;
                    }
                    // local_attr searches ancestors too
                    for anc in eng.ancestors(cls, true, None) {
                        if !eng.class_locals_get(anc, next_sym).is_empty() {
                            return true;
                        }
                    }
                }
            }
            false
        }
    }
}

impl SpecialCk {
    pub fn visit_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if !is_method(eng, node) {
            return;
        }
        let name = eng.node_name(node).unwrap_or_default();
        let inferred = safe_infer_call_result(eng, node);
        if let Some(v) = &inferred {
            let in_protocol_map = matches!(
                name.as_str(),
                "__iter__" | "__len__" | "__bool__" | "__index__" | "__repr__" | "__str__"
                    | "__bytes__" | "__hash__" | "__length_hint__" | "__format__"
                    | "__getnewargs__" | "__getnewargs_ex__"
            );
            // `if inferred and ...`: Uninferable is falsy
            if !v.is_uninferable() && in_protocol_map && !is_function_body_ellipsis(eng, node) {
                self.dispatch_protocol(cx, node, &name, v);
            }
        }
        if special_method_params(&name).is_some() {
            self.check_unexpected_method_signature(cx, node, &name);
        }
    }

    fn dispatch_protocol(&mut self, cx: &mut WalkCx, node: GNode, name: &str, v: &Value) {
        let eng = cx.eng;
        let emit = |cx: &mut WalkCx, msgid: &'static str, text: &str| {
            cx.emit_node(msgid, u::msg_line(cx.eng, node), u::msg_col(cx.eng, node),
                text.to_string());
        };
        match name {
            "__iter__" => {
                if !is_iterator(eng, v) {
                    emit(cx, "E0301", "__iter__ returns non-iterator");
                }
            }
            "__len__" => {
                if !is_int_v(eng, v) {
                    emit(cx, "E0303", "__len__ does not return non-negative integer");
                } else if let Some(c) = value_const(eng, v) {
                    if const_lt_zero(&c) {
                        emit(cx, "E0303", "__len__ does not return non-negative integer");
                    }
                }
            }
            "__bool__" => {
                if !is_bool_v(eng, v) {
                    emit(cx, "E0304", "__bool__ does not return bool");
                }
            }
            "__index__" => {
                if !is_int_v(eng, v) {
                    emit(cx, "E0305", "__index__ does not return int");
                }
            }
            "__repr__" => {
                if !is_str_v(eng, v) {
                    emit(cx, "E0306", "__repr__ does not return str");
                }
            }
            "__str__" => {
                if !is_str_v(eng, v) {
                    emit(cx, "E0307", "__str__ does not return str");
                }
            }
            "__bytes__" => {
                if !is_bytes_v(eng, v) {
                    emit(cx, "E0308", "__bytes__ does not return bytes");
                }
            }
            "__hash__" => {
                if !is_int_v(eng, v) {
                    emit(cx, "E0309", "__hash__ does not return int");
                }
            }
            "__length_hint__" => {
                if !is_int_v(eng, v) {
                    emit(cx, "E0310", "__length_hint__ does not return non-negative integer");
                } else if let Some(c) = value_const(eng, v) {
                    if const_lt_zero(&c) {
                        emit(cx, "E0310", "__length_hint__ does not return non-negative integer");
                    }
                }
            }
            "__format__" => {
                if !is_str_v(eng, v) {
                    emit(cx, "E0311", "__format__ does not return str");
                }
            }
            "__getnewargs__" => {
                if !is_tuple_v(eng, v) {
                    emit(cx, "E0312", "__getnewargs__ does not return a tuple");
                }
            }
            "__getnewargs_ex__" => {
                self.check_getnewargs_ex(cx, node, v);
            }
            _ => {}
        }
    }

    fn check_getnewargs_ex(&mut self, cx: &mut WalkCx, node: GNode, v: &Value) {
        let eng = cx.eng;
        const TEXT: &str = "__getnewargs_ex__ does not return a tuple containing (tuple, dict)";
        if !is_tuple_v(eng, v) {
            cx.emit_node("E0313", u::msg_line(eng, node), u::msg_col(eng, node),
                TEXT.to_string());
            return;
        }
        // only literal Tuple nodes are analyzed further
        let elts: Vec<GNode> = match v {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Tuple { elts, .. } => {
                        elts.iter().map(|&e| GNode { m: g.m, n: e }).collect()
                    }
                    _ => return,
                }
            }
            _ => return,
        };
        let mut found_error = false;
        if elts.len() != 2 {
            found_error = true;
        } else {
            for (i, &arg) in elts.iter().enumerate() {
                let mut argv: Option<Value> = Some(Value::Node(arg));
                if eng.kind_is(arg, |k| matches!(k, NodeKind::Call { .. } | NodeKind::Name { .. })) {
                    argv = u::safe_infer(eng, cx.caches, arg);
                }
                let Some(av) = argv else { continue };
                if av.is_uninferable() {
                    continue;
                }
                let ok = if i == 0 { is_tuple_v(eng, &av) } else { is_dict_v(eng, &av) };
                if !ok {
                    found_error = true;
                    break;
                }
            }
        }
        if found_error {
            cx.emit_node("E0313", u::msg_line(eng, node), u::msg_col(eng, node),
                TEXT.to_string());
        }
    }

    /// special_methods_checker.py:197-245 — E0302
    fn check_unexpected_method_signature(&mut self, cx: &mut WalkCx, node: GNode, name: &str) {
        let eng = cx.eng;
        let Some(expected) = special_method_params(name) else { return };
        let Some(expected) = expected else { return }; // variadic
        let Some(spec) = eng.arg_spec(node) else { return };
        if spec.args_unknown {
            return;
        }
        if spec.args.is_empty() && spec.vararg.is_none() {
            // no parameters at all: E0211's job
            return;
        }
        let static_dec = tc::decorated_with(eng, node, &["builtins.staticmethod"]);
        let all_args: i64 = if static_dec {
            spec.args.len() as i64
        } else {
            (spec.args.len() as i64) - 1
        };
        let mandatory = all_args - spec.defaults.len() as i64;
        let optional = spec.defaults.len() as i64;
        let current_params = mandatory + optional;
        let (emit, expected_str) = match expected {
            (a, Some(b)) => (
                mandatory != a && mandatory != b,
                format!("between {a} or {b}"),
            ),
            (n, None) => {
                let rest = n - mandatory;
                let e = if rest == 0 {
                    false
                } else if rest < 0 {
                    true
                } else {
                    !((optional - rest) >= 0 || spec.vararg.is_some())
                };
                (e, n.to_string())
            }
        };
        if emit {
            let verb = if current_params <= 1 { "was" } else { "were" };
            let nrepr = u::py_repr_str(name);
            let text = format!(
                "The special method {nrepr} expects {expected_str} param(s), {current_params} {verb} given"
            );
            cx.emit_node("E0302", u::msg_line(eng, node), u::msg_col(eng, node), text);
        }
    }
}

fn const_lt_zero(c: &ConstValue) -> bool {
    use pyast::tree::IntValue;
    match c {
        ConstValue::Int(IntValue::Small(i)) => *i < 0,
        ConstValue::Int(IntValue::Big(s)) => s.starts_with('-'),
        _ => false, // bool is never < 0
    }
}

// ---------------------------------------------------------------------------
// NewStyleConflictChecker — E1003 (newstyle.py:46-108)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct NewStyleCk;

impl NewStyleCk {
    pub fn visit_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if !is_method(eng, node) {
            return;
        }
        let klass = eng.frame(match eng.parent(node) {
            Some(p) => p,
            None => return,
        });
        // for stmt in node.nodes_of_class(nodes.Call)
        let calls = tc::nodes_of_class_skip_pub(eng, node, |k| matches!(k, NodeKind::Call { .. }));
        for stmt in calls {
            if node_frame_class(eng, stmt) != node_frame_class(eng, node) {
                continue;
            }
            let md = eng.md(stmt.m);
            let func = match &md.tree.nodes[stmt.n.idx()].kind {
                NodeKind::Call { func, .. } => GNode { m: stmt.m, n: *func },
                _ => continue,
            };
            // expr must be an Attribute whose expr is `super(arg0, ...)`
            let expr_expr = match &md.tree.nodes[func.n.idx()].kind {
                NodeKind::Attribute { expr, .. } => GNode { m: stmt.m, n: *expr },
                _ => continue,
            };
            let (super_args, call) = match &md.tree.nodes[expr_expr.n.idx()].kind {
                NodeKind::Call { func: sf, args, .. } => {
                    let is_super = matches!(
                        &md.tree.nodes[sf.idx()].kind,
                        NodeKind::Name { name } if md.tree.s(*name) == "super"
                    );
                    if !is_super || args.is_empty() {
                        continue;
                    }
                    (
                        args.iter().map(|&a| GNode { m: stmt.m, n: a }).collect::<Vec<_>>(),
                        expr_expr,
                    )
                }
                _ => continue,
            };
            let arg0 = super_args[0];
            // super(type(self), self)
            if let NodeKind::Call { func: tf, .. } = &md.tree.nodes[arg0.n.idx()].kind {
                if matches!(
                    &md.tree.nodes[tf.idx()].kind,
                    NodeKind::Name { name } if md.tree.s(*name) == "type"
                ) {
                    drop(md);
                    cx.emit_node("E1003", u::lineno(eng, call), u::col_offset(eng, call) as i64,
                        u::format_template("Bad first argument %r given to super()", &["type"]));
                    continue;
                }
            }
            // super(self.__class__, self)
            if super_args.len() >= 2 {
                let a0_isclassattr = matches!(
                    &md.tree.nodes[arg0.n.idx()].kind,
                    NodeKind::Attribute { attrname, .. } if md.tree.s(*attrname) == "__class__"
                );
                let a1_isself = matches!(
                    &md.tree.nodes[super_args[1].n.idx()].kind,
                    NodeKind::Name { name } if md.tree.s(*name) == "self"
                );
                if a0_isclassattr && a1_isself {
                    drop(md);
                    cx.emit_node("E1003", u::lineno(eng, call), u::col_offset(eng, call) as i64,
                        u::format_template("Bad first argument %r given to super()", &["self.__class__"]));
                    continue;
                }
            }
            drop(md);
            // supcls = next(call.args[0].infer(), None)
            let first = match eng.first_value(arg0, &Ctx::new()) {
                Ok(v) => v,
                Err(_) => continue, // InferenceError
            };
            // klass is not supcls and all(i != supcls for i in ancestors)
            let supcls_node: Option<GNode> = match &first {
                Some(Value::Node(g)) => Some(*g),
                _ => None,
            };
            let same = supcls_node == Some(klass)
                || supcls_node
                    .map(|s| eng.ancestors(klass, true, None).contains(&s))
                    .unwrap_or(false);
            if same {
                continue;
            }
            // name resolution: supcls truthy -> its name; else arg0.name
            let name: Option<String> = match &first {
                Some(v) if !v.is_uninferable() => tc::value_name(eng, v),
                _ => {
                    let md = eng.md(arg0.m);
                    match &md.tree.nodes[arg0.n.idx()].kind {
                        NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
                        _ => None,
                    }
                }
            };
            if let Some(n) = name {
                cx.emit_node("E1003", u::lineno(eng, call), u::col_offset(eng, call) as i64,
                    u::format_template("Bad first argument %r given to super()", &[&n]));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ClassChecker (class_checker.py) — E0202/E0203/E0211/E0213/E0236-E0245/F0202
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ClassCk {
    /// ScopeAccessMap: (class, attrname) -> access nodes, insertion-ordered
    accessed: FxHashMap<GNode, indexmap::IndexMap<GSym, Vec<GNode>>>,
    accessed_order: Vec<GNode>,
    /// _first_attrs stack
    first_attrs: Vec<Option<GSym>>,
}

impl ClassCk {
    fn set_accessed(&mut self, eng: &Engine, node: GNode, attrname: GSym) {
        let Some(frame) = node_frame_class(eng, node) else { return };
        if !self.accessed.contains_key(&frame) {
            self.accessed_order.push(frame);
        }
        self.accessed
            .entry(frame)
            .or_default()
            .entry(attrname)
            .or_default()
            .push(node);
    }

    /// _is_mandatory_method_param (class_checker.py:2368-2388)
    fn is_mandatory_method_param(&self, eng: &Engine, expr: GNode) -> bool {
        let first_attr: Option<GSym> = if !self.first_attrs.is_empty() {
            match self.first_attrs.last().unwrap() {
                Some(s) => Some(*s),
                None => return false, // static method: never matches a Name
            }
        } else {
            // closest enclosing FunctionDef, bound, first positional arg
            let Some(f) = u::first_ancestor(eng, expr, |k| {
                matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
            }) else {
                return false;
            };
            let bound = matches!(eng.func_type(f), FType::Method | FType::ClassMethod);
            if !bound {
                return false;
            }
            match eng.arg_spec(f) {
                Some(spec) if !spec.args.is_empty() => eng.assign_name_of(spec.args[0]),
                _ => return false,
            }
        };
        let Some(first_attr) = first_attr else { return false };
        let md = eng.md(expr.m);
        match &md.tree.nodes[expr.n.idx()].kind {
            NodeKind::Name { name } => eng.g(&md, *name) == first_attr,
            _ => false,
        }
    }

    /// ClassChecker.visit_attribute (class_checker.py:1680-1697)
    pub fn visit_attribute(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // _check_super_without_brackets: W0245 disabled, no inference burn
        let md = eng.md(node.m);
        let (expr, attrname) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Attribute { expr, attrname, .. } => {
                (GNode { m: node.m, n: *expr }, eng.g(&md, *attrname))
            }
            _ => return,
        };
        drop(md);
        if self.is_mandatory_method_param(eng, expr) {
            self.set_accessed(eng, node, attrname);
        }
        // protected-access disabled -> return
    }

    /// ClassChecker.visit_assignattr (class_checker.py:1714-1723)
    pub fn visit_assignattr(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let (expr, attrname) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::AssignAttr { expr, attrname } => {
                (GNode { m: node.m, n: *expr }, eng.g(&md, *attrname))
            }
            _ => return,
        };
        drop(md);
        // assign_type() is AugAssign
        let assign_is_aug = u::assign_parent(eng, node) != node
            || eng
                .parent(node)
                .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::AugAssign { .. })))
                .unwrap_or(false);
        let aug = {
            // AssignAttr.assign_type(): climb Tuple/List/Starred to the
            // Assign/AugAssign/AnnAssign/For... parent
            let mut cur = node;
            loop {
                match eng.parent(cur) {
                    Some(p)
                        if eng.kind_is(p, |k| {
                            matches!(
                                k,
                                NodeKind::Tuple { .. } | NodeKind::List { .. }
                                    | NodeKind::Starred { .. }
                            )
                        }) =>
                    {
                        cur = p;
                    }
                    Some(p) => break eng.kind_is(p, |k| matches!(k, NodeKind::AugAssign { .. })),
                    None => break false,
                }
            }
        };
        let _ = assign_is_aug;
        if aug && self.is_mandatory_method_param(eng, expr) {
            self.set_accessed(eng, node, attrname);
        }
        self.check_in_slots(cx, node, expr, attrname);
        self.check_invalid_class_object(cx, node, attrname);
    }

    /// _check_invalid_class_object (class_checker.py:1725-1749) — E0243
    fn check_invalid_class_object(&mut self, cx: &mut WalkCx, node: GNode, attrname: GSym) {
        let eng = cx.eng;
        if eng.sname(attrname) != "__class__" {
            return;
        }
        let parent = match eng.parent(node) {
            Some(p) => p,
            None => return,
        };
        let inferred: Option<Value> = if eng.kind_is(parent, |k| matches!(k, NodeKind::Tuple { .. })) {
            // unpacking assignment
            let md = eng.md(parent.m);
            let elts: Vec<pyast::NodeId> = match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::Tuple { elts, .. } => elts.clone(),
                _ => return,
            };
            let mut class_index: i64 = -1;
            for (i, &e) in elts.iter().enumerate() {
                let has_class_attr = match &md.tree.nodes[e.idx()].kind {
                    NodeKind::AssignAttr { attrname: a, .. }
                    | NodeKind::Attribute { attrname: a, .. } => md.tree.s(*a) == "__class__",
                    _ => false,
                };
                if has_class_attr {
                    class_index = i as i64;
                }
            }
            if class_index == -1 {
                return;
            }
            // node.parent.parent.value.elts[class_index]
            let gp = match eng.parent(parent) {
                Some(g) => g,
                None => return,
            };
            let value = match &md.tree.nodes[gp.n.idx()].kind {
                NodeKind::Assign { value, .. } => *value,
                _ => return,
            };
            let velts: Vec<pyast::NodeId> = match &md.tree.nodes[value.idx()].kind {
                NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } => elts.clone(),
                _ => return, // AttributeError risk path: no .elts -> crash in
                             // pylint; never hit in corpora
            };
            let Some(&target) = velts.get(class_index as usize) else { return };
            drop(md);
            u::safe_infer(eng, cx.caches, GNode { m: parent.m, n: target })
        } else {
            let md = eng.md(parent.m);
            let value = match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::Assign { value, .. } => Some(*value),
                NodeKind::AugAssign { value, .. } | NodeKind::AnnAssign { value: Some(value), .. } => {
                    Some(*value)
                }
                _ => None,
            };
            let Some(value) = value else { return };
            drop(md);
            u::safe_infer(eng, cx.caches, GNode { m: parent.m, n: value })
        };
        let inferred = match inferred {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        if matches!(&inferred, Value::Node(g) if is_classdef(eng, *g)) {
            return;
        }
        let cls_name = value_astroid_class_name(eng, &inferred);
        cx.emit_node("E0243", u::lineno(eng, node), u::col_offset(eng, node) as i64,
            u::format_template(
                "Invalid assignment to '__class__'. Should be a class definition but got a '%s'",
                &[cls_name]));
    }

    /// _check_in_slots (class_checker.py:1751-1824) — E0237
    fn check_in_slots(&mut self, cx: &mut WalkCx, node: GNode, expr: GNode, attrname: GSym) {
        let eng = cx.eng;
        let inferred = match u::safe_infer(eng, cx.caches, expr) {
            Some(v) => v,
            None => return,
        };
        let klass = match &inferred {
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => *cls,
            _ => return, // only astroid.Instance proper? literals are
                         // Instances too but never have __slots__
        };
        if !tc::has_known_bases(eng, cx.caches, klass) {
            return;
        }
        let slots_sym = eng.sym("__slots__");
        if eng.class_locals_get(klass, slots_sym).is_empty() {
            return;
        }
        let mro = match eng.mro(klass, None) {
            Ok(m) => m,
            Err(_) => return, // klass.mro() raise would propagate; bail
        };
        let setattr_sym = eng.sym("__setattr__");
        if mro.iter().any(|&b| {
            eng.qname(b) != "builtins.object" && !eng.class_locals_get(b, setattr_sym).is_empty()
        }) {
            return;
        }
        let slots = match eng.all_slots(klass) {
            Ok(Some(s)) => s,
            Ok(None) => return,
            Err(_) => return,
        };
        // any slot-less ancestor (not Generic/object) -> __dict__ exists
        if eng.ancestors(klass, true, None).iter().any(|&anc| {
            eng.class_locals_get(anc, slots_sym).is_empty()
                && !matches!(eng.node_name(anc).as_deref(), Some("Generic") | Some("object"))
        }) {
            return;
        }
        let attr = eng.sname(attrname);
        if slots.iter().any(|s| s == &attr) {
            return;
        }
        if slots.iter().any(|s| s == "__dict__") {
            return;
        }
        if is_attribute_property(eng, cx.caches, &attr, klass) {
            return;
        }
        if attr != "__class__" && is_class_attr(eng, &attr, klass) {
            return;
        }
        if !eng.class_locals_get(klass, attrname).is_empty() {
            for local in eng.class_locals_get(klass, attrname) {
                if let Some(stmt) = eng.statement(local) {
                    let md = eng.md(stmt.m);
                    if matches!(
                        md.tree.nodes[stmt.n.idx()].kind,
                        NodeKind::AnnAssign { value: None, .. }
                    ) {
                        return;
                    }
                }
            }
            if has_data_descriptor(eng, klass, attrname) {
                return;
            }
        }
        if attr == "__class__" {
            // _has_same_layout_slots(slots, node.parent.value)
            if let Some(parent) = eng.parent(node) {
                let md = eng.md(parent.m);
                let value = match &md.tree.nodes[parent.n.idx()].kind {
                    NodeKind::Assign { value, .. } | NodeKind::AugAssign { value, .. } => {
                        Some(*value)
                    }
                    NodeKind::AnnAssign { value, .. } => *value,
                    _ => None,
                };
                drop(md);
                if let Some(v) = value {
                    let vg = GNode { m: parent.m, n: v };
                    if let Ok(Some(Value::Node(other))) = eng.first_value(vg, &Ctx::new()) {
                        if is_classdef(eng, other) {
                            if let Ok(Some(other_slots)) = eng.all_slots(other) {
                                if slots.len() == other_slots.len()
                                    && slots.iter().zip(other_slots.iter()).all(|(a, b)| a == b)
                                {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }
        cx.emit_node("E0237", u::lineno(eng, node), u::col_offset(eng, node) as i64,
            u::format_template("Assigning to attribute %r not defined in class slots", &[&attr]));
    }

    /// ClassChecker.visit_classdef (class_checker.py:876-883)
    pub fn visit_classdef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // _check_bases_classes: W0223 disabled; class_is_abstract burns
        let _ = tc::class_is_abstract(cx.caches, eng, node);
        self.check_slots(cx, node);
        self.check_proper_bases(cx, node);
        self.check_typing_final(cx, node);
        self.check_consistent_mro(cx, node);
        self.check_declare_non_slot(cx, node);
    }

    fn check_consistent_mro(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if let Err(e) = eng.mro(node, None) {
            if matches!(e, pyinfer::value::ErrKind::Mro) {
                let name = eng.node_name(node).unwrap_or_default();
                if eng.last_mro_dup.get() {
                    cx.emit_node("E0241", u::msg_line(eng, node), u::msg_col(eng, node),
                        u::format_template("Duplicate bases for class %r", &[&name]));
                } else {
                    cx.emit_node("E0240", u::msg_line(eng, node), u::msg_col(eng, node),
                        u::format_template("Inconsistent method resolution order for class %r", &[&name]));
                }
            }
        }
    }

    /// _check_proper_bases (class_checker.py:995-1022) — E0239 + E0244
    fn check_proper_bases(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        for base in class_bases_nodes(eng, node) {
            let ancestor = match u::safe_infer(eng, cx.caches, base) {
                Some(v) if !v.is_uninferable() => v,
                _ => continue,
            };
            // Instance subtype of builtins.type / .Protocol
            if matches!(&ancestor, Value::Inst { .. } | Value::ExcInst { .. }) {
                if let Some(cls) = eng.proxied_class(&ancestor) {
                    if eng.is_subtype_of(cls, "builtins.type", None)
                        || eng.is_subtype_of(cls, ".Protocol", None)
                    {
                        continue;
                    }
                }
            }
            let ancestor_cls: Option<GNode> = match &ancestor {
                Value::Node(g) if is_classdef(eng, *g) => Some(*g),
                _ => None,
            };
            let invalid = match ancestor_cls {
                Some(cls) => {
                    const INVALID: &[&str] = &["bool", "range", "slice", "memoryview"];
                    let name = eng.node_name(cls).unwrap_or_default();
                    INVALID.contains(&name.as_str()) && eng.md(cls.m).name == "builtins"
                }
                None => true,
            };
            if invalid {
                let txt = pyinfer::asstr::as_string(eng, base);
                cx.emit_node("E0239", u::msg_line(eng, node), u::msg_col(eng, node),
                    u::format_template("Inheriting %r, which is not a class.", &[&txt]));
            }
            if let Some(cls) = ancestor_cls {
                if eng.is_subtype_of(cls, "enum.Enum", None) {
                    self.check_enum_base(cx, node, cls);
                }
            }
            // useless-object-inheritance: R0205 disabled, no burn
        }
    }

    /// _check_enum_base (class_checker.py:937-993) — E0244 (+ W0213 burn)
    fn check_enum_base(&mut self, cx: &mut WalkCx, node: GNode, ancestor: GNode) {
        let eng = cx.eng;
        let members_sym = eng.sym("__members__");
        if let Ok(attrs) = eng.class_getattr(ancestor, members_sym, None, true) {
            if let Some(NV::N(first)) = attrs.first() {
                // the enum transform stores __members__ as a placeholder
                // redirecting to a SynthDict (astroid: a real Dict node with
                // lazy Name values); member names come from the keys/values
                let mut member_names: Vec<String> = Vec::new();
                {
                    let md = eng.md(first.m);
                    if let NodeKind::Dict { items } = &md.tree.nodes[first.n.idx()].kind {
                        for (_, vn) in items {
                            if let Some(n) = eng.node_name(GNode { m: first.m, n: *vn }) {
                                member_names.push(n);
                            }
                        }
                    }
                }
                if member_names.is_empty() {
                    if let Some(NV::V(Value::SynthDict { items })) =
                        eng.redirects.borrow().get(first).cloned()
                    {
                        for (k, _) in items.iter() {
                            if let Value::SynthConst(c) = k {
                                if let ConstValue::Str(sname) = c.as_ref() {
                                    member_names.push(sname.to_string());
                                }
                            }
                        }
                    }
                }
                if !member_names.is_empty() {
                    for member_name in &member_names {
                        let msym = eng.sym(member_name);
                        // all(item.parent is AnnAssign with value None)
                        let all_annotation_only = match eng.class_getattr(ancestor, msym, None, true)
                        {
                            Ok(member_attrs) => member_attrs.iter().all(|nv| match nv {
                                NV::N(g) => eng
                                    .parent(*g)
                                    .map(|p| {
                                        eng.kind_is(p, |k| {
                                            matches!(k, NodeKind::AnnAssign { value: None, .. })
                                        })
                                    })
                                    .unwrap_or(false),
                                _ => false,
                            }),
                            Err(_) => false,
                        };
                        if all_annotation_only {
                            continue;
                        }
                        let aname = eng.node_name(ancestor).unwrap_or_default();
                        let text = format!("Extending inherited Enum class \"{aname}\"");
                        cx.emit_node("E0244", u::msg_line(eng, node), u::msg_col(eng, node),
                            text);
                        break;
                    }
                }
            }
        }
        // W0213 implicit-flag-alias burn: is_subtype_of only
        let _ = eng.is_subtype_of(ancestor, "enum.IntFlag", None);
    }

    /// _check_typing_final (class_checker.py:1024-1043) — W subclassed-final
    /// (disabled): safe_infer(base) burn + decorator inference burn
    fn check_typing_final(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        for base in class_bases_nodes(eng, node) {
            let ancestor = match u::safe_infer(eng, cx.caches, base) {
                Some(v) if !v.is_uninferable() => v,
                _ => continue,
            };
            if let Value::Node(g) = &ancestor {
                if is_classdef(eng, *g) {
                    let _ = tc::decorated_with(eng, *g, &["typing.final"]);
                    // uninferable_final_decorators: safe_infer per decorator
                    for dec in tc::decorator_nodes_pub(eng, *g) {
                        let _ = u::safe_infer(eng, cx.caches, dec);
                    }
                }
            }
        }
    }

    /// _check_slots (class_checker.py:1547-1582) — E0236/E0238/E0242
    fn check_slots(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let slots_sym = eng.sym("__slots__");
        if eng.class_locals_get(node, slots_sym).is_empty() {
            return;
        }
        let Ok(inferred_slots) = ilookup_slots(eng, node) else { return };
        for slots in &inferred_slots {
            if slots.is_uninferable() {
                continue;
            }
            let is_comp = matches!(slots, Value::Node(g) if eng.kind_is(*g, |k| matches!(
                k,
                NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_)
                    | NodeKind::GeneratorExp(_)
            )));
            if !tc::is_iterable(eng, cx.caches, slots, false) && !is_comp {
                cx.emit_node("E0238", u::msg_line(eng, node), u::msg_col(eng, node),
                    "Invalid __slots__ object".to_string());
                continue;
            }
            if value_const(eng, slots).is_some() {
                // single-string-used-for-slots: C0205 disabled
                continue;
            }
            let elts = match slots_elements(eng, slots) {
                SlotsElts::Elts(e) => e,
                SlotsElts::NoItered => continue,
            };
            for elt in &elts {
                self.check_slots_elt(cx, node, elt);
            }
            // _check_redefined_slots: W0244 disabled (mro+slots walks
            // cached) — skipped
        }
    }

    /// _check_slots_elt (class_checker.py:1638-1666) — E0236 + E0242
    fn check_slots_elt(&mut self, cx: &mut WalkCx, cls: GNode, elt: &NV) {
        let eng = cx.eng;
        let (vals, report_node): (Vec<Value>, Option<GNode>) = match elt {
            NV::N(g) => {
                let flow = eng.infer(*g, &Ctx::new());
                if flow.vals.is_empty() {
                    return; // InferenceError -> continue
                }
                (flow.vals, Some(*g))
            }
            NV::V(v) => (vec![v.clone()], None),
        };
        for inferred in &vals {
            if inferred.is_uninferable() {
                continue;
            }
            let cstr = match value_const(eng, inferred) {
                Some(ConstValue::Str(s)) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            };
            let Some(value) = cstr else {
                if let Some(rn) = report_node {
                    let txt = pyinfer::asstr::as_string(eng, rn);
                    cx.emit_node("E0236", u::lineno(eng, rn), u::col_offset(eng, rn) as i64,
                        u::format_template(
                            "Invalid object %r in __slots__, must contain only non empty strings",
                            &[&txt]));
                }
                continue;
            };
            // E0242: conflicts with class locals
            let vsym = eng.sym(&value);
            let class_variable = eng.class_locals_get(cls, vsym);
            if class_variable.len() == 1 {
                // single bare annotation -> STOP the whole element check
                let only = class_variable[0];
                let ann_only = eng
                    .parent(only)
                    .map(|p| {
                        eng.kind_is(p, |k| matches!(k, NodeKind::AnnAssign { value: None, .. }))
                    })
                    .unwrap_or(false);
                if ann_only {
                    return;
                }
            }
            if !class_variable.is_empty() {
                if let Some(rn) = report_node {
                    cx.emit_node("E0242", u::lineno(eng, rn), u::col_offset(eng, rn) as i64,
                        u::format_template("Value %r in slots conflicts with class variable", &[&value]));
                }
            }
        }
    }

    /// _has_valid_slots (class_checker.py:1525-1545)
    fn has_valid_slots(&self, cx: &mut WalkCx, node: GNode) -> bool {
        let eng = cx.eng;
        let slots_sym = eng.sym("__slots__");
        if eng.class_locals_get(node, slots_sym).is_empty() {
            return false;
        }
        let Ok(inferred_slots) = ilookup_slots(eng, node) else { return false };
        for slots in &inferred_slots {
            if slots.is_uninferable() {
                return false;
            }
            let is_comp = matches!(slots, Value::Node(g) if eng.kind_is(*g, |k| matches!(
                k,
                NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_)
                    | NodeKind::GeneratorExp(_)
            )));
            if !tc::is_iterable(eng, cx.caches, slots, false) && !is_comp {
                return false;
            }
            if value_const(eng, slots).is_some() {
                return false;
            }
            if matches!(slots_elements(eng, slots), SlotsElts::NoItered) {
                return false;
            }
        }
        true
    }

    fn classdef_slots_names(&self, cx: &mut WalkCx, node: GNode) -> Vec<String> {
        let eng = cx.eng;
        let mut names: Vec<String> = Vec::new();
        let Ok(inferred_slots) = ilookup_slots(eng, node) else { return names };
        for slots in &inferred_slots {
            let elts = match slots_elements(eng, slots) {
                SlotsElts::Elts(e) => e,
                SlotsElts::NoItered => continue,
            };
            for elt in &elts {
                match nv_const(eng, elt) {
                    Some(ConstValue::Str(s)) => names.push(s.to_string()),
                    Some(_) => {}
                    None => {
                        // safe_infer the element, use .value if str
                        if let NV::N(g) = elt {
                            if let Some(v) = u::safe_infer(eng, cx.caches, *g) {
                                if let Some(ConstValue::Str(s)) = value_const(eng, &v) {
                                    names.push(s.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        names
    }

    /// _check_declare_non_slot (class_checker.py:886-926) — E0245
    fn check_declare_non_slot(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if !self.has_valid_slots(cx, node) {
            return;
        }
        let mut slot_names = self.classdef_slots_names(cx, node);
        if slot_names.is_empty() {
            return;
        }
        if slot_names.iter().any(|s| s == "__dict__") {
            return;
        }
        for base in class_bases_nodes(eng, node) {
            let ancestor = match u::safe_infer(eng, cx.caches, base) {
                Some(Value::Node(g)) if is_classdef(eng, g) => g,
                _ => continue,
            };
            if !self.has_valid_slots(cx, ancestor) {
                return;
            }
            for s in self.classdef_slots_names(cx, ancestor) {
                if s == "__dict__" {
                    return;
                }
                slot_names.push(s);
            }
        }
        for child in class_body(eng, node) {
            let md = eng.md(child.m);
            if let NodeKind::AnnAssign { target, value: None, .. } = &md.tree.nodes[child.n.idx()].kind {
                if let NodeKind::AssignName { name } = &md.tree.nodes[target.idx()].kind {
                    let nm = md.tree.s(*name).to_string();
                    let tg = GNode { m: child.m, n: *target };
                    drop(md);
                    if !slot_names.contains(&nm) {
                        cx.emit_node("E0245", u::lineno(eng, tg), u::col_offset(eng, tg) as i64,
                            u::format_template("No such name %r in __slots__", &[&nm]));
                    }
                }
            }
        }
    }

    /// ClassChecker.visit_functiondef (class_checker.py:1266-1357)
    pub fn visit_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if !is_method(eng, node) {
            return;
        }
        // _check_useless_super_delegation (W0246) / _check_property_with_
        // parameters (R0206): disabled — burn skipped (TODO if FPs)
        let klass = eng.frame(eng.parent(node).unwrap());
        let is_metaclass = eng.class_type(klass) == "metaclass";
        self.check_first_arg_for_type(cx, node, is_metaclass);
        let name = eng.node_name(node).unwrap_or_default();
        if name == "__init__" {
            // _check_init: W0231/W0233 disabled — burn skipped (TODO)
            return;
        }
        // override loop: first MRO ancestor defining the name
        {
            let sym = eng.sym(&name);
            let ancs = match eng.mro(klass, None) {
                Ok(m) => m.get(1..).map(|s| s.to_vec()).unwrap_or_default(),
                Err(_) => eng.ancestors(klass, true, None),
            };
            for anc in ancs {
                let vals = eng.class_locals_get(anc, sym);
                if vals.is_empty() {
                    continue;
                }
                let parent_function = vals[0];
                if !is_funcdef(eng, parent_function) {
                    continue;
                }
                // _check_signature: F0202 unreachable (both are FunctionDefs);
                // W0221/W0222/W0236/W0239 disabled — burn skipped
                break;
            }
        }
        // decorator exemptions (class_checker.py:1298-1331)
        let decs = tc::decorator_nodes_pub(eng, node);
        if !decs.is_empty() {
            for dec in decs {
                let md = eng.md(dec.m);
                match &md.tree.nodes[dec.n.idx()].kind {
                    NodeKind::Attribute { attrname, expr, .. } => {
                        let an = md.tree.s(*attrname).to_string();
                        if matches!(an.as_str(), "getter" | "setter" | "deleter") {
                            return;
                        }
                        // _check_functools_or_not
                        if an == "cached_property" {
                            let e = GNode { m: dec.m, n: *expr };
                            let is_name = matches!(
                                md.tree.nodes[expr.idx()].kind,
                                NodeKind::Name { .. }
                            );
                            drop(md);
                            if is_name && self.functools_import(eng, e) {
                                return;
                            }
                        }
                    }
                    NodeKind::Name { .. } => {
                        // ALLOWED_PROPERTIES = {"bultins.property" (typo!),
                        // "functools.cached_property"} — a bare Name never
                        // contains a dot: dead arm
                    }
                    _ => {}
                }
                let inferred = u::safe_infer(eng, cx.caches, dec);
                let mut inferred = match inferred {
                    Some(v) if !v.is_uninferable() => v,
                    _ => return, // uninferable decorator -> bail
                };
                if let Value::Node(g) = &inferred {
                    if is_funcdef(eng, *g) {
                        // next(inferred.infer_call_result(inferred))
                        match eng.infer_call_result_first(
                            &Value::Node(*g),
                            Some(*g),
                            None,
                        ) {
                            Ok(Some(v)) => inferred = v,
                            _ => return, // InferenceError -> bail
                        }
                    }
                }
                // data descriptor: getattr __get__ AND __set__
                let is_inst_or_cls = matches!(&inferred, Value::Inst { .. } | Value::ExcInst { .. })
                    || matches!(&inferred, Value::Node(g) if is_classdef(eng, *g));
                if is_inst_or_cls {
                    let get_ok = tc::value_getattr(eng, &inferred, eng.sym("__get__"));
                    if let Ok(g1) = get_ok {
                        if !g1.is_empty() {
                            if let Ok(s1) = tc::value_getattr(eng, &inferred, eng.sym("__set__")) {
                                if !s1.is_empty() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }
        // method hidden by an attribute (class_checker.py:1333-1357) — E0202
        let sym = eng.sym(&name);
        let overridden = match eng.instance_attr(klass, sym, None) {
            Ok(v) if !v.is_empty() => v[0],
            _ => return,
        };
        let mut overridden_frame = eng.frame(overridden);
        if is_funcdef(eng, overridden_frame) && eng.func_type(overridden_frame) == FType::Method {
            if let Some(p) = eng.parent(overridden_frame) {
                overridden_frame = eng.frame(p);
            }
        }
        if !is_classdef(eng, overridden_frame) {
            return;
        }
        let of_qname = eng.qname(overridden_frame);
        if !eng.is_subtype_of(klass, &of_qname, None) {
            return;
        }
        for ancestor in eng.ancestors(klass, true, None) {
            if eng.instance_attrs_of(ancestor).contains_key(&sym) && is_attr_private(&name) {
                return;
            }
            let lk = eng.lookup(ancestor, sym);
            for nv in &lk.1 {
                if matches!(nv, NV::N(g) if is_funcdef(eng, *g)) {
                    return;
                }
            }
        }
        let module_name = eng.md(overridden.m).name.clone();
        let line = eng.fromlineno(overridden);
        let text = format!(
            "An attribute defined in {} line {} hides this method",
            module_name, line
        );
        cx.emit_node("E0202", u::msg_line(eng, node), u::msg_col(eng, node), text);
    }

    /// _check_functools_or_not lookup arm (class_checker.py:1507-1523)
    fn functools_import(&self, eng: &Engine, name_node: GNode) -> bool {
        let md = eng.md(name_node.m);
        let sym = match &md.tree.nodes[name_node.n.idx()].kind {
            NodeKind::Name { name } => eng.g(&md, *name),
            _ => return false,
        };
        drop(md);
        let lk = eng.lookup(name_node, sym);
        for nv in &lk.1 {
            if let NV::N(g) = nv {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Import { names } => {
                        if names.iter().any(|(n, _)| md.tree.s(*n) == "functools") {
                            return true;
                        }
                    }
                    NodeKind::ImportFrom { modname, .. } => {
                        if md.tree.s(*modname) == "functools" {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
        }
        false
    }

    /// _check_first_arg_for_type (class_checker.py:2079-2155) — E0211/E0213
    fn check_first_arg_for_type(&mut self, cx: &mut WalkCx, node: GNode, metaclass: bool) {
        let eng = cx.eng;
        let Some(spec) = eng.arg_spec(node) else { return };
        if spec.args_unknown {
            return;
        }
        let first_arg: Option<GSym> = if !spec.posonlyargs.is_empty() {
            eng.assign_name_of(spec.posonlyargs[0])
        } else if !spec.args.is_empty() {
            eng.assign_name_of(spec.args[0])
        } else {
            None
        };
        self.first_attrs.push(first_arg);
        let first = *self.first_attrs.last().unwrap();
        let ftype = eng.func_type(node);
        let name = eng.node_name(node).unwrap_or_default();
        if ftype == FType::StaticMethod {
            let fa = first_arg.map(|s| eng.sname(s));
            if matches!(fa.as_deref(), Some("self") | Some("cls") | Some("mcs")) {
                // bad-staticmethod-argument: W0211 disabled
                return;
            }
            *self.first_attrs.last_mut().unwrap() = None;
        } else if eng
            .decoratornames(node, None)
            .iter()
            .any(|q| q.as_deref() == Some("builtins.staticmethod"))
        {
            return;
        } else if spec.args.is_empty()
            && spec.posonlyargs.is_empty()
            && spec.vararg.is_none()
            && spec.kwarg.is_none()
        {
            cx.emit_node("E0211", u::msg_line(eng, node), u::msg_col(eng, node),
                u::format_template("Method %r has no argument", &[&name]));
        } else if metaclass {
            // bad-mcs-classmethod-argument / bad-mcs-method-argument: C
        } else if ftype == FType::ClassMethod || name == "__class_getitem__" {
            // bad-classmethod-argument: C0202
        } else if first.map(|s| eng.sname(s)).as_deref() != Some("self") {
            cx.emit_node("E0213", u::msg_line(eng, node), u::msg_col(eng, node),
                u::format_template("Method %r should have \"self\" as first argument", &[&name]));
        }
    }

    /// leave_functiondef (class_checker.py:1668-1676)
    pub fn leave_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if is_method(eng, node) {
            let args_known = eng.arg_spec(node).map(|s| !s.args_unknown).unwrap_or(false);
            if args_known {
                self.first_attrs.pop();
            }
        }
    }

    /// leave_classdef -> _check_attribute_defined_outside_init — E0203
    pub fn leave_classdef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // _check_unused_private_* : W disabled; safe_infer burn on
        // type(self)-style calls only — skipped (TODO if FPs)
        let cname = eng.node_name(node).unwrap_or_default();
        // mixin_class_rgx `.*[Mm]ixin` re.match: contains Mixin/mixin
        if cname.contains("Mixin") || cname.contains("mixin") {
            return;
        }
        if eng.class_type(node) != "metaclass" {
            self.check_accessed_members(cx, node);
        }
        // attribute-defined-outside-init disabled -> return
    }

    /// _check_accessed_members (class_checker.py:2017-2077) — E0203
    fn check_accessed_members(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some(accessed) = self.accessed.get(&node) else { return };
        let accessed: Vec<(GSym, Vec<GNode>)> =
            accessed.iter().map(|(k, v)| (*k, v.clone())).collect();
        for (attr, nodes_lst) in accessed {
            // class attribute?
            if !tc::class_local_attr(eng, node, &eng.sname(attr)).is_empty() {
                continue;
            }
            // instance attribute of a parent class?
            let ancs = match eng.mro(node, None) {
                Ok(m) => m.get(1..).map(|s| s.to_vec()).unwrap_or_default(),
                Err(_) => eng.ancestors(node, true, None),
            };
            if ancs.iter().any(|&a| eng.instance_attrs_of(a).contains_key(&attr)) {
                continue;
            }
            // own instance attribute?
            let defstmts = match eng.instance_attr(node, attr, None) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let defstmts: Vec<GNode> = defstmts
                .into_iter()
                .filter(|s| !nodes_lst.contains(s))
                .collect();
            if defstmts.is_empty() {
                continue;
            }
            let scope0 = eng.scope(defstmts[0]);
            let defstmts: Vec<GNode> = defstmts
                .iter()
                .enumerate()
                .filter(|(i, s)| *i == 0 || eng.scope(**s) != scope0)
                .map(|(_, s)| *s)
                .collect();
            if defstmts.len() != 1 {
                continue;
            }
            let defstmt = defstmts[0];
            let frame = eng.frame(defstmt);
            let lno = eng.fromlineno(defstmt);
            for &access in &nodes_lst {
                if eng.frame(access) == frame && eng.fromlineno(access) < lno {
                    let stmt = match eng.statement(access) {
                        Some(s) => s,
                        None => continue,
                    };
                    if !u::are_exclusive_exc(eng, stmt, defstmt,
                        &["AttributeError", "Exception", "BaseException"])
                    {
                        let attr_s = eng.sname(attr);
                        let text = u::format_template(
                            "Access to member %r before its definition line %s",
                            &[&attr_s, &lno.to_string()]);
                        cx.emit_node("E0203", u::lineno(eng, access),
                            u::col_offset(eng, access) as i64, text);
                    }
                }
            }
        }
    }
}

/// `inferred.__class__.__name__` for E0243
fn value_astroid_class_name(eng: &Engine, v: &Value) -> &'static str {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            md.tree.kind_name(g.n)
        }
        Value::Inst { .. } => "Instance",
        Value::ExcInst { .. } => "ExceptionInstance",
        Value::SynthConst(_) => "Const",
        Value::SynthSeq { kind, .. } => match kind {
            pyinfer::value::SeqKind::List => "List",
            pyinfer::value::SeqKind::Tuple => "Tuple",
            pyinfer::value::SeqKind::Set => "Set",
        },
        Value::SynthDict { .. } => "Dict",
        Value::SynthSlice { .. } => "Slice",
        Value::FrozenSet { .. } => "FrozenSet",
        Value::BoundMethod { .. } | Value::DescBM { .. } => "BoundMethod",
        Value::UnboundMethod { .. } => "UnboundMethod",
        Value::Generator { is_async, .. } => {
            if *is_async { "AsyncGenerator" } else { "Generator" }
        }
        Value::Property { .. } => "Property",
        Value::Partial { .. } => "PartialFunction",
        Value::Super { .. } => "Super",
        Value::UnionType => "UnionType",
        Value::DictItems(_) => "DictItems",
        Value::DictKeys(_) => "DictKeys",
        Value::DictValues(_) => "DictValues",
        Value::EvaluatedObject { .. } => "EvaluatedObject",
        Value::Uninferable => "UninferableBase",
    }
}

/// _is_attribute_property (class_checker.py:449-476)
fn is_attribute_property(eng: &Engine, caches: &u::LintCaches, name: &str, klass: GNode) -> bool {
    let sym = eng.sym(name);
    let attrs = match eng.class_getattr(klass, sym, None, true) {
        Ok(a) => a,
        Err(_) => return false,
    };
    for attr in &attrs {
        let inferred = match attr {
            NV::N(g) => match eng.first_value(*g, &Ctx::new()) {
                Ok(Some(v)) => v,
                _ => continue, // InferenceError -> continue
            },
            NV::V(Value::Uninferable) => continue,
            NV::V(v) => v.clone(),
        };
        if let Value::Node(g) = &inferred {
            if is_funcdef(eng, *g) && tc::decorated_with_property(eng, caches, *g) {
                return true;
            }
        }
        if u::value_pytype(eng, &inferred).as_deref() == Some("builtins.property") {
            return true;
        }
    }
    false
}

/// utils.is_class_attr (utils.py:2257-2262)
fn is_class_attr(eng: &Engine, name: &str, klass: GNode) -> bool {
    eng.class_getattr(klass, eng.sym(name), None, true).is_ok()
}

/// _has_data_descriptor (class_checker.py:397-413)
fn has_data_descriptor(eng: &Engine, cls: GNode, attr: GSym) -> bool {
    let attrs = match eng.class_getattr(cls, attr, None, true) {
        Ok(a) => a,
        Err(_) => return false,
    };
    for attribute in &attrs {
        let g = match attribute {
            NV::N(g) => *g,
            NV::V(_) => continue,
        };
        let flow = eng.infer(g, &Ctx::new());
        if flow.vals.is_empty() {
            return true; // InferenceError -> conservative True
        }
        for inferred in &flow.vals {
            let is_inst = matches!(inferred, Value::Inst { .. } | Value::ExcInst { .. })
                || tc::value_is_instance(eng, inferred);
            if is_inst {
                let get_ok = tc::value_getattr(eng, inferred, eng.sym("__get__")).is_ok();
                let set_ok = tc::value_getattr(eng, inferred, eng.sym("__set__")).is_ok();
                if get_ok && set_ok {
                    return true;
                }
            }
        }
    }
    false
}
