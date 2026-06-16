//! ClassChecker + SpecialMethodsChecker + NewStyleConflictChecker ports
//! (pylint 4.0.5 `pylint/checkers/classes/{class_checker,
//! special_methods_checker}.py`, `pylint/checkers/newstyle.py`).
//! In-scope codes: E0202/E0203/E0211/E0213/E0236-E0245/F0202,
//! E0301-E0313, E1003. Disabled-message paths that burn inference are
//! ported where they share caches with in-scope decisions.

use std::rc::Rc;

use pyast::tree::{ConstValue, NodeKind};
use pyast::NodeId;
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

/// exclude-protected default config (class_checker.py:806-816)
const EXCLUDE_PROTECTED: &[&str] = &["_asdict", "_fields", "_replace", "_source", "_make", "os._exit"];

/// utils.is_attr_protected (utils.py:666-674)
fn is_attr_protected(attrname: &str) -> bool {
    attrname.starts_with('_')
        && attrname != "_"
        && !(attrname.starts_with("__") && attrname.ends_with("__"))
}

/// utils.get_outer_class (utils.py:702-706)
fn get_outer_class(eng: &Engine, class_node: GNode) -> Option<GNode> {
    let parent = eng.parent(class_node)?;
    let pk = eng.frame(parent);
    if is_classdef(eng, pk) {
        Some(pk)
    } else {
        None
    }
}

/// ClassDef.basenames: as_string of each base expression
fn class_basenames(eng: &Engine, cls: GNode) -> Vec<String> {
    class_bases_nodes(eng, cls)
        .into_iter()
        .map(|b| u::as_string(eng, b))
        .collect()
}

/// _is_class_or_instance_attribute (class_checker.py:2001-2016)
fn is_class_or_instance_attribute(eng: &Engine, name: GSym, klass: GNode) -> bool {
    if eng.class_getattr(klass, name, None, true).is_ok() {
        return true;
    }
    matches!(eng.instance_attr(klass, name, None), Ok(v) if !v.is_empty())
}

/// _is_called_inside_special_method (class_checker.py:1971-1975)
fn is_called_inside_special_method(eng: &Engine, node: GNode) -> bool {
    let frame = eng.frame(node);
    match eng.node_name(frame) {
        Some(n) => !n.is_empty() && u::PYMETHODS.contains(&n.as_str()),
        None => false,
    }
}

/// utils._is_property_kind (utils.py:818-830)
fn is_property_kind(eng: &Engine, func: GNode, kinds: &[&str]) -> bool {
    if !is_funcdef(eng, func) {
        return false;
    }
    for dec in tc::decorator_nodes_pub(eng, func) {
        let md = eng.md(dec.m);
        if let NodeKind::Attribute { attrname, .. } = &md.tree.nodes[dec.n.idx()].kind {
            if kinds.contains(&md.tree.s(*attrname)) {
                return true;
            }
        }
    }
    false
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
    // `^_{2,10}.*[^_]+_?$` (re.match): >=2 leading underscores, at most ONE
    // trailing underscore (dunders are NOT private), some non-underscore.
    let b = name.as_bytes();
    let lead = b.iter().take_while(|&&c| c == b'_').count();
    if lead < 2 {
        return false;
    }
    let trail = b.iter().rev().take_while(|&&c| c == b'_').count();
    if trail > 1 {
        return false;
    }
    b.iter().any(|&c| c != b'_')
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

/// _get_slots_names per element (class_checker.py:1600-1610): a Const slot
/// contributes its string value directly; anything else is safe_infer'd and
/// kept only if the inferred value is a str.
fn slot_name_of(eng: &Engine, caches: &u::LintCaches, elt: &NV) -> Option<String> {
    // Const branch: the element is literally a Const node / SynthConst.
    let is_const = match elt {
        NV::N(g) | NV::V(Value::Node(g)) => {
            eng.kind_is(*g, |k| matches!(k, NodeKind::Const(_)))
        }
        NV::V(Value::SynthConst(_)) => true,
        _ => false,
    };
    if is_const {
        return match nv_const(eng, elt) {
            Some(ConstValue::Str(s)) => Some(s.to_string()),
            _ => None,
        };
    }
    // else: safe_infer(slot); keep .value if it is a str.
    let inferred = match elt {
        NV::N(g) => u::safe_infer(eng, caches, *g)?,
        NV::V(v) => v.clone(),
    };
    match value_const(eng, &inferred) {
        Some(ConstValue::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// Position of the inferred __slots__ value used as the W0244 anchor. A real
/// container node carries its own (fromlineno, col_offset); a synthesized
/// binop-concat container has no position, so pylint uses node.fromlineno
/// (first child's line) with col_offset None -> 0.
fn slots_node_pos(eng: &Engine, slots: &Value) -> (u32, i64) {
    match slots {
        Value::Node(g) => (u::lineno(eng, *g), u::col_offset(eng, *g) as i64),
        Value::SynthSeq { elems, .. } => {
            // fromlineno of a fresh container = first element's line.
            let line = elems
                .iter()
                .find_map(|e| match e {
                    Value::Node(g) => Some(u::lineno(eng, *g)),
                    _ => None,
                })
                .unwrap_or(0);
            (line, 0)
        }
        Value::SynthDict { items } => {
            let line = items
                .iter()
                .find_map(|(k, _)| match k {
                    Value::Node(g) => Some(u::lineno(eng, *g)),
                    _ => None,
                })
                .unwrap_or(0);
            (line, 0)
        }
        _ => (0, 0),
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
        if cx.full {
            self.check_super_without_brackets(cx, node);
        }
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
            return;
        }
        if (cx.cfg_enabled)("W0212") {
            self.check_protected_attribute_access(cx, node, expr, attrname);
        }
    }

    /// _check_super_without_brackets (class_checker.py:1699-1712) — W0245
    fn check_super_without_brackets(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let frame = eng.frame(node);
        if !is_funcdef(eng, frame) {
            return;
        }
        let parent_frame_is_class = eng
            .parent(frame)
            .map(|p| is_classdef(eng, eng.frame(p)))
            .unwrap_or(false);
        if !parent_frame_is_class {
            return;
        }
        if !eng
            .parent(node)
            .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::Call { .. })))
            .unwrap_or(false)
        {
            return;
        }
        let expr: Option<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Attribute { expr, .. } => Some(GNode { m: node.m, n: *expr }),
                _ => None,
            }
        };
        let Some(expr) = expr else { return };
        let is_super_name = {
            let md = eng.md(expr.m);
            matches!(&md.tree.nodes[expr.n.idx()].kind,
                NodeKind::Name { name } if md.tree.s(*name) == "super")
        };
        if is_super_name {
            cx.emit_node(
                "W0245",
                u::lineno(eng, expr),
                u::col_offset(eng, expr).max(0) as i64,
                "Super call without brackets".to_string(),
            );
        }
    }

    /// ClassChecker.visit_assign (class_checker.py:1826-1837)
    pub fn visit_assign(&mut self, cx: &mut WalkCx, assign_node: GNode) {
        let eng = cx.eng;
        self.check_classmethod_declaration(cx, assign_node);
        let t0: Option<GNode> = {
            let md = eng.md(assign_node.m);
            match &md.tree.nodes[assign_node.n.idx()].kind {
                NodeKind::Assign { targets, .. } => {
                    targets.first().map(|&t| GNode { m: assign_node.m, n: t })
                }
                _ => None,
            }
        };
        let Some(node) = t0 else { return };
        let (expr, attrname) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::AssignAttr { expr, attrname } => {
                    (GNode { m: node.m, n: *expr }, eng.g(&md, *attrname))
                }
                _ => return,
            }
        };
        if self.is_mandatory_method_param(eng, expr) {
            return;
        }
        self.check_protected_attribute_access(cx, node, expr, attrname);
    }

    /// _check_classmethod_declaration (class_checker.py:1839-1870) —
    /// R0202/R0203
    fn check_classmethod_declaration(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (value, t0): (GNode, Option<NodeId>) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Assign { value, targets } => {
                    (GNode { m: node.m, n: *value }, targets.first().copied())
                }
                _ => return,
            }
        };
        let (func_kind, method_name): (u8, GSym) = {
            let md = eng.md(value.m);
            let NodeKind::Call { func, args, .. } = &md.tree.nodes[value.n.idx()].kind else {
                return;
            };
            let kind = match &md.tree.nodes[func.idx()].kind {
                NodeKind::Name { name } => match md.tree.s(*name) {
                    "classmethod" => 1u8,
                    "staticmethod" => 2u8,
                    _ => return,
                },
                _ => return,
            };
            let Some(&a0) = args.first() else { return };
            let mn = match &md.tree.nodes[a0.idx()].kind {
                NodeKind::Name { name } => eng.g(&md, *name),
                _ => return,
            };
            (kind, mn)
        };
        let parent_class = eng.scope(node);
        if !is_classdef(eng, parent_class) {
            return;
        }
        // mymethods(): locals values that are FunctionDef
        let has_method = {
            let md = eng.md(parent_class.m);
            let l = md.locals.borrow();
            match l.get(&parent_class.n) {
                Some(map) => map.iter().any(|(_, v)| {
                    v.first()
                        .map(|&g| {
                            is_funcdef(eng, g)
                                && eng.node_name(g).as_deref()
                                    == Some(eng.sname(method_name).as_str())
                        })
                        .unwrap_or(false)
                }),
                None => false,
            }
        };
        if has_method {
            let Some(t0) = t0 else { return };
            let tg = GNode { m: node.m, n: t0 };
            let (msgid, text) = if func_kind == 1 {
                ("R0202", "Consider using a decorator instead of calling classmethod")
            } else {
                ("R0203", "Consider using a decorator instead of calling staticmethod")
            };
            cx.emit_node(
                msgid,
                u::lineno(eng, tg),
                u::col_offset(eng, tg).max(0) as i64,
                text.to_string(),
            );
        }
    }

    /// _check_protected_attribute_access (class_checker.py:1872-1969) — W0212
    fn check_protected_attribute_access(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        expr: GNode,
        attrname: GSym,
    ) {
        let eng = cx.eng;
        let attr = eng.sname(attrname);
        if !is_attr_protected(&attr) || EXCLUDE_PROTECTED.contains(&attr.as_str()) {
            return;
        }
        if u::is_node_in_type_annotation_context(eng, node) {
            return;
        }
        let emit = |cx: &mut WalkCx| {
            cx.emit_node(
                "W0212",
                u::lineno(cx.eng, node),
                u::col_offset(cx.eng, node).max(0) as i64,
                u::format_template(
                    "Access to a protected member %s of a client class",
                    &[&attr],
                ),
            );
        };
        // module/class-level exclude-protected qualification
        let inferred = u::safe_infer(eng, cx.caches, expr);
        if let Some(Value::Node(g)) = &inferred {
            if (is_classdef(eng, *g) || u::is_module(eng, *g))
                && EXCLUDE_PROTECTED.contains(
                    &format!("{}.{}", eng.node_name(*g).unwrap_or_default(), attr).as_str(),
                )
            {
                return;
            }
        }
        let klass = node_frame_class(eng, node);
        let Some(klass) = klass else {
            emit(cx);
            return;
        };
        // super() call prefix
        {
            let md = eng.md(expr.m);
            if let NodeKind::Call { func, .. } = &md.tree.nodes[expr.n.idx()].kind {
                if matches!(&md.tree.nodes[func.idx()].kind,
                    NodeKind::Name { name } if md.tree.s(*name) == "super")
                {
                    return;
                }
            }
        }
        if self.is_type_self_call(eng, expr) {
            return;
        }
        // nested-class scope walk over the dotted callee (REPLICATE the
        // leaked `callee` loop variable: after a break it holds the
        // mismatching component)
        let mut inside_klass = true;
        let mut outer_klass: Option<GNode> = Some(klass);
        let full_callee = u::as_string(eng, expr);
        let mut callee: &str = &full_callee;
        for component in full_callee.split('.').rev() {
            callee = component;
            let ok = outer_klass
                .map(|ok| eng.node_name(ok).as_deref() == Some(component))
                .unwrap_or(false);
            if !ok {
                inside_klass = false;
                break;
            }
            outer_klass = get_outer_class(eng, outer_klass.unwrap());
        }
        let in_basenames = class_basenames(eng, klass).iter().any(|b| b == callee);
        if !(inside_klass || in_basenames) {
            // property assignment in the class body
            if let Some(stmt) = eng.parent(node).and_then(|p| eng.statement(p)) {
                let prop_name: Option<String> = {
                    let md = eng.md(stmt.m);
                    match &md.tree.nodes[stmt.n.idx()].kind {
                        NodeKind::Assign { targets, .. } if targets.len() == 1 => {
                            match &md.tree.nodes[targets[0].idx()].kind {
                                NodeKind::AssignName { name } => {
                                    Some(md.tree.s(*name).to_string())
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                };
                if let Some(pn) = prop_name {
                    if is_attribute_property(eng, cx.caches, &pn, klass) {
                        return;
                    }
                }
            }
            if self.is_classmethod_frame(eng, eng.frame(node))
                && self.is_inferred_instance(cx, expr, klass)
                && is_class_or_instance_attribute(eng, attrname, klass)
            {
                return;
            }
            let licit_protected_member = !attr.starts_with("__");
            if licit_protected_member && is_called_inside_special_method(eng, node) {
                return;
            }
            emit(cx);
        }
    }

    /// _is_type_self_call (class_checker.py:1977-1981)
    fn is_type_self_call(&self, eng: &Engine, expr: GNode) -> bool {
        let md = eng.md(expr.m);
        let NodeKind::Call { func, args, .. } = &md.tree.nodes[expr.n.idx()].kind else {
            return false;
        };
        if !matches!(&md.tree.nodes[func.idx()].kind,
            NodeKind::Name { name } if md.tree.s(*name) == "type")
        {
            return false;
        }
        if args.len() != 1 {
            return false;
        }
        let a0 = GNode { m: expr.m, n: args[0] };
        drop(md);
        self.is_mandatory_method_param(eng, a0)
    }

    /// _is_classmethod (class_checker.py:1983-1988)
    fn is_classmethod_frame(&self, eng: &Engine, func: GNode) -> bool {
        is_funcdef(eng, func)
            && (eng.func_type(func) == FType::ClassMethod
                || eng.node_name(func).as_deref() == Some("__class_getitem__"))
    }

    /// _is_inferred_instance (class_checker.py:1990-1998)
    fn is_inferred_instance(&self, cx: &mut WalkCx, expr: GNode, klass: GNode) -> bool {
        let eng = cx.eng;
        match u::safe_infer(eng, cx.caches, expr) {
            Some(Value::Inst { cls, .. }) | Some(Value::ExcInst { cls, .. }) => cls == klass,
            _ => false,
        }
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
        if cx.full {
            self.check_bases_classes(cx, node);
        } else {
            // -E pipeline: class_is_abstract burn only (frozen behavior)
            let _ = tc::class_is_abstract(cx.caches, eng, node);
        }
        self.check_slots(cx, node);
        self.check_proper_bases(cx, node);
        self.check_typing_final(cx, node);
        self.check_consistent_mro(cx, node);
        self.check_declare_non_slot(cx, node);
    }

    /// _check_bases_classes (class_checker.py:2173-2204) — W0223
    fn check_bases_classes(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if tc::class_is_abstract(cx.caches, eng, node) {
            return;
        }
        // utils.unimplemented_abstract_methods (utils.py:945-994), strict
        // is_abstract callback (W0223): pass_is_abstract=False.
        let visited = match unimplemented_abstract_methods(cx, node, AbstractCb::Strict) {
            Some(v) => v,
            None => return, // ResolveError -> {}
        };
        let mut methods: Vec<(String, GNode)> =
            visited.iter().map(|(k, v)| (k.clone(), *v)).collect();
        methods.sort_by(|a, b| a.0.cmp(&b.0));
        let node_name = eng.node_name(node).unwrap_or_default();
        for (name, method) in methods {
            let owner = eng.frame(match eng.parent(method) {
                Some(p) => p,
                None => continue,
            });
            if owner == node {
                continue;
            }
            if locals_contains_name(eng, node, &name) {
                continue;
            }
            let owner_name = eng.node_name(owner).unwrap_or_default();
            cx.emit_node(
                "W0223",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template(
                    "Method %r is abstract in class %r but is not overridden in child class %r",
                    &[&name, &owner_name, &node_name],
                ),
            );
        }
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
            // R0205 useless-object-inheritance: name-only check
            if cx.full {
                let aname: Option<String> = match &ancestor {
                    Value::Node(g) => eng.node_name(*g),
                    Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => eng.node_name(*cls),
                    _ => None,
                };
                if aname.as_deref() == Some("object") {
                    let cname = eng.node_name(node).unwrap_or_default();
                    cx.emit_node(
                        "R0205",
                        u::msg_line(eng, node),
                        u::msg_col(eng, node),
                        u::format_template(
                            "Class %r inherits from object, can be safely removed from bases in python3",
                            &[&cname],
                        ),
                    );
                }
            }
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
        // implicit-flag-alias (W0213): ancestor is a subtype of enum.IntFlag.
        if eng.is_subtype_of(ancestor, "enum.IntFlag", None) {
            self.check_implicit_flag_alias(cx, node);
        }
    }

    /// class_checker.py:956-993 — W0213 implicit-flag-alias. Insertion order
    /// matters: Python defaultdicts preserve first-seen order, which drives
    /// the emit order (per overlap value, then per assignment node).
    fn check_implicit_flag_alias(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let cname = eng.node_name(node).unwrap_or_default();
        // assignments: int value -> list of AssignName GNodes (preorder).
        // `for assign_name in node.nodes_of_class(AssignName):
        //    match assign_name.parent: case Assign(value=object(value=int())):`
        // insertion order = first-seen value.
        let mut assign_keys: Vec<i64> = Vec::new();
        let mut assignments: FxHashMap<i64, Vec<GNode>> = FxHashMap::default();
        let assign_names = crate::basicerr::nodes_of_class(
            eng,
            node,
            |k| matches!(k, NodeKind::AssignName { .. }),
            |_| false,
        );
        for an in assign_names {
            let Some(parent) = eng.parent(an) else { continue };
            let value_node = {
                let md = eng.md(parent.m);
                match &md.tree.nodes[parent.n.idx()].kind {
                    NodeKind::Assign { value, .. } => Some(*value),
                    _ => None,
                }
            };
            let Some(vn) = value_node else { continue };
            // object(value=int()): the Assign value must be a Const int.
            // bool is an int subclass in Python, so True/False match too.
            let ival: Option<i64> = {
                let md = eng.md(parent.m);
                match &md.tree.nodes[vn.idx()].kind {
                    NodeKind::Const(ConstValue::Int(pyast::tree::IntValue::Small(i))) => Some(*i),
                    NodeKind::Const(ConstValue::Bool(b)) => Some(*b as i64),
                    _ => None,
                }
            };
            let Some(value) = ival else { continue };
            if !assignments.contains_key(&value) {
                assign_keys.push(value);
            }
            assignments.entry(value).or_default().push(an);
        }

        // bit_flags: bit position -> set of flags (insertion-ordered).
        // `for flag in assignments` iterates assignments in insertion order.
        let mut bit_order: Vec<u32> = Vec::new();
        let mut bit_flags: FxHashMap<u32, Vec<i64>> = FxHashMap::default();
        for &flag in &assign_keys {
            if flag < 0 {
                // bin() of a negative is "-0b..."; pylint's bit scan over
                // reversed(bin(flag)) would still find '1' chars but the
                // mypy/socket flags are all non-negative. Guard to avoid
                // surprising behavior; pylint would scan the magnitude bits.
            }
            let mut bit = 0u32;
            let mut v = flag;
            // enumerate set bits of `flag` (pylint scans reversed(bin(flag))).
            while v != 0 {
                if v & 1 == 1 {
                    if !bit_flags.contains_key(&bit) {
                        bit_order.push(bit);
                    }
                    let set = bit_flags.entry(bit).or_default();
                    if !set.contains(&flag) {
                        set.push(flag);
                    }
                }
                v = ((v as u64) >> 1) as i64;
                bit += 1;
            }
        }

        // overlaps: conflict value -> list of source values (insertion order).
        let mut overlap_keys: Vec<i64> = Vec::new();
        let mut overlaps: FxHashMap<i64, Vec<i64>> = FxHashMap::default();
        for &bit in &bit_order {
            let mut flags = bit_flags.get(&bit).cloned().unwrap_or_default();
            flags.sort_unstable();
            if flags.is_empty() {
                continue;
            }
            let source = flags[0];
            for &conflict in &flags[1..] {
                if !overlaps.contains_key(&conflict) {
                    overlap_keys.push(conflict);
                }
                overlaps.entry(conflict).or_default().push(source);
            }
        }

        // Report (per overlap value, then per assignment node).
        for &overlap in &overlap_keys {
            let sources = overlaps.get(&overlap).cloned().unwrap_or_default();
            for &assignment_node in assignments.get(&overlap).unwrap_or(&Vec::new()) {
                let aname = eng.node_name(assignment_node).unwrap_or_default();
                let overlap_str = format!("<{cname}.{aname}: {overlap}>");
                let sources_str = sources
                    .iter()
                    .map(|&source| {
                        let sname = assignments
                            .get(&source)
                            .and_then(|v| v.first())
                            .and_then(|&g| eng.node_name(g))
                            .unwrap_or_default();
                        format!(
                            "<{cname}.{sname}: {source}> ({overlap} & {source} = {})",
                            overlap & source
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                // template: "Flag member %(overlap)s shares bit positions
                // with %(sources)s" — named %s substitutions of pre-built
                // strings (no repr).
                let text = format!(
                    "Flag member {overlap_str} shares bit positions with {sources_str}"
                );
                cx.emit_node(
                    "W0213",
                    u::msg_line(eng, assignment_node),
                    u::msg_col(eng, assignment_node),
                    text,
                );
            }
        }
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
                    if cx.full {
                        // W0240 subclassed-final-class
                        if tc::decorated_with(eng, *g, &["typing.final"])
                            || has_uninferable_final_decorators(cx, *g)
                        {
                            let cname = eng.node_name(node).unwrap_or_default();
                            let aname = eng.node_name(*g).unwrap_or_default();
                            cx.emit_node(
                                "W0240",
                                u::msg_line(eng, node),
                                u::msg_col(eng, node),
                                u::format_template(
                                    "Class %r is a subclass of a class decorated with typing.final: %r",
                                    &[&cname, &aname],
                                ),
                            );
                        }
                    } else {
                        let _ = tc::decorated_with(eng, *g, &["typing.final"]);
                        // uninferable_final_decorators: safe_infer burn
                        for dec in tc::decorator_nodes_pub(eng, *g) {
                            let _ = u::safe_infer(eng, cx.caches, dec);
                        }
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
                // C0205 single-string-used-for-slots (node = the ClassDef)
                cx.emit_node(
                    "C0205",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    "Class __slots__ should be a non-string iterable".to_string(),
                );
                continue;
            }
            let elts = match slots_elements(eng, slots) {
                SlotsElts::Elts(e) => e,
                SlotsElts::NoItered => continue,
            };
            for elt in &elts {
                self.check_slots_elt(cx, node, elt);
            }
            self.check_redefined_slots(cx, node, slots, &elts);
        }
    }

    /// _check_redefined_slots (class_checker.py:1612-1636) — W0244.
    /// Emits when this class's __slots__ redefines a slot already declared in
    /// an ancestor. The message anchors on `slots_node` (the inferred
    /// __slots__ value): a real Tuple/List node carries its own position; a
    /// binop-concat result is a synthesized container with no position, so
    /// pylint falls back to `node.fromlineno` (first child's line) and
    /// `col_offset` None -> 0.
    fn check_redefined_slots(&mut self, cx: &mut WalkCx, node: GNode, slots: &Value, elts: &[NV]) {
        let eng = cx.eng;
        // _get_slots_names(slots_list): Const.value, else safe_infer -> .value
        let slots_names: Vec<String> = elts
            .iter()
            .filter_map(|elt| slot_name_of(eng, cx.caches, elt))
            .collect();
        if slots_names.is_empty() {
            return;
        }
        // ancestors_slots_names = {slot.value for ancestor in
        //   node.local_attr_ancestors("__slots__") for slot in ancestor.slots()}
        // local_attr_ancestors = mro[1:] (fallback ancestors()) of `node`
        // containing __slots__ in their locals; ancestor.slots() ==
        // ancestor._all_slots (our all_slots) which returns None — yielding no
        // names — whenever ANY class in the ancestor's MRO lacks __slots__
        // (e.g. a `str`/`object` base). That None-collapse is load-bearing: it
        // suppresses W0244 for subclasses of non-slotted classes.
        let slots_sym = eng.sym("__slots__");
        let ancestors: Vec<GNode> = match eng.mro(node, None) {
            Ok(m) => m.into_iter().skip(1).collect(),
            Err(_) => eng.ancestors(node, true, None),
        };
        let mut ancestors_slots_names: rustc_hash::FxHashSet<String> = Default::default();
        for anc in &ancestors {
            let anc = *anc;
            if eng.class_locals_get(anc, slots_sym).is_empty() {
                continue;
            }
            // astroid evaluates `ancestor.slots()` (== ancestor._all_slots)
            // here, whose _islots igetattr('__slots__') walk over the
            // ancestor's MRO has an inference SIDE EFFECT: it warms the global
            // inference cache for the deep __class_getitem__ / metaclass /
            // TypeVar sub-chain of generic-slotted bases. A later same-class
            // _check_init (W0231) then REPLAYS that warm sub-chain instead of
            // re-inferring it cold and over-budgeting the cap — without it,
            // _DeclarativeMapped's __init__ over-caps to Uninferable and yields
            // a spurious super-init-not-called (the sole sqlalchemy full-mode
            // FP). This warming is a full-mode-only concern (the -E pipeline
            // emits no W0231/W0244 and its byte output must stay identical, so
            // it keeps the cheaper structural slot-name derivation below).
            if cx.full {
                let _ = eng.all_slots(anc);
            }
            // ancestor.slots() collapses to None when ANY class in the
            // ancestor's MRO (except object) lacks a local __slots__; replicate
            // that None-test structurally (mro + local presence) and pull the
            // names from each slotted class's own __slots__.
            let anc_mro = match eng.mro(anc, None) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mut none_collapse = false;
            for c in &anc_mro {
                if eng.qname(*c) == "builtins.object" {
                    continue;
                }
                if eng.class_locals_get(*c, slots_sym).is_empty() {
                    none_collapse = true;
                    break;
                }
            }
            if none_collapse {
                continue;
            }
            for c in &anc_mro {
                if eng.qname(*c) == "builtins.object" {
                    continue;
                }
                for nm in self.classdef_slots_names(cx, *c) {
                    ancestors_slots_names.insert(nm);
                }
            }
        }
        // redefined = ancestors ∩ slots_names; emit if non-empty.
        let redefined: rustc_hash::FxHashSet<&String> = slots_names
            .iter()
            .filter(|n| ancestors_slots_names.contains(n.as_str()))
            .collect();
        if redefined.is_empty() {
            return;
        }
        // args = [name for name in slots_names if name in redefined_slots]
        let arg_list: Vec<&str> = slots_names
            .iter()
            .filter(|n| redefined.contains(*n))
            .map(|s| s.as_str())
            .collect();
        let formatted = format!(
            "[{}]",
            arg_list
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let (line, col) = slots_node_pos(eng, slots);
        cx.emit_node(
            "W0244",
            line,
            col,
            u::format_template("Redefined slots %s in subclass", &[&formatted]),
        );
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
            // E0242: conflicts with class locals.
            // astroid ClassDef.implicit_locals() injects __module__/
            // __qualname__/__annotations__ synthetic Consts FIRST in every
            // class's locals (scoped_nodes.py:1911-1933) — node.locals.get()
            // in _check_slots_elt sees them, so these names ALWAYS conflict.
            // The synthetic Const's parent is the ClassDef, never an
            // AnnAssign, and being first it also defeats the single-bare-
            // annotation skip (list len >= 2 with any explicit entry).
            let vsym = eng.sym(&value);
            let class_variable = eng.class_locals_get(cls, vsym);
            let implicit_local = matches!(
                value.as_str(),
                "__module__" | "__qualname__" | "__annotations__"
            );
            if !implicit_local && class_variable.len() == 1 {
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
            if implicit_local || !class_variable.is_empty() {
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
        if cx.full {
            self.check_useless_super_delegation(cx, node);
            self.check_property_with_parameters(cx, node);
        }
        let klass = eng.frame(eng.parent(node).unwrap());
        let is_metaclass = eng.class_type(klass) == "metaclass";
        self.check_first_arg_for_type(cx, node, is_metaclass);
        let name = eng.node_name(node).unwrap_or_default();
        if name == "__init__" {
            if (cx.cfg_enabled)("W0231") || (cx.cfg_enabled)("W0233") {
                self.check_init(cx, node, klass);
            }
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
                // F0202 unreachable here (both are FunctionDefs)
                if cx.full {
                    self.check_signature(cx, node, parent_function, klass);
                    self.check_invalid_overridden_method(cx, node, parent_function);
                }
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
                // W0211 bad-staticmethod-argument; NOTE _first_attrs[-1]
                // keeps the bad name (no None overwrite before return)
                cx.emit_node(
                    "W0211",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    u::format_template(
                        "Static method with %r as first argument",
                        &[fa.as_deref().unwrap_or("")],
                    ),
                );
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
            if ftype == FType::ClassMethod {
                self.check_first_arg_config(cx, node, first, &["mcs"], "C0204",
                    "Metaclass class method %s should have %s as first argument", &name);
            } else {
                self.check_first_arg_config(cx, node, first, &["cls"], "C0203",
                    "Metaclass method %s should have %s as first argument", &name);
            }
        } else if ftype == FType::ClassMethod || name == "__class_getitem__" {
            self.check_first_arg_config(cx, node, first, &["cls"], "C0202",
                "Class method %s should have %s as first argument", &name);
        } else if first.map(|s| eng.sname(s)).as_deref() != Some("self") {
            cx.emit_node("E0213", u::msg_line(eng, node), u::msg_col(eng, node),
                u::format_template("Method %r should have \"self\" as first argument", &[&name]));
        }
    }

    /// _check_first_arg_config (class_checker.py:2157-2171)
    #[allow(clippy::too_many_arguments)]
    fn check_first_arg_config(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        first: Option<GSym>,
        config: &[&str],
        msgid: &'static str,
        template: &'static str,
        method_name: &str,
    ) {
        let eng = cx.eng;
        let first_str = first.map(|s| eng.sname(s));
        if first_str.as_deref().map(|f| config.contains(&f)).unwrap_or(false) {
            return;
        }
        let valid = if config.len() == 1 {
            u::py_repr_str(config[0])
        } else {
            let mut v = config[..config.len() - 1]
                .iter()
                .map(|c| u::py_repr_str(c))
                .collect::<Vec<_>>()
                .join(", ");
            v.push_str(&format!(" or {}", u::py_repr_str(config[config.len() - 1])));
            v
        };
        cx.emit_node(
            msgid,
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            u::format_template(template, &[method_name, &valid]),
        );
    }

    /// _check_useless_super_delegation (class_checker.py:1361-1447) — W0246
    fn check_useless_super_delegation(&mut self, cx: &mut WalkCx, function: GNode) {
        let eng = cx.eng;
        // ---- _is_trivial_super_delegation (class_checker.py:146-196) ----
        if !is_method(eng, function) {
            return;
        }
        let (body, decorators, name) = {
            let md = eng.md(function.m);
            match &md.tree.nodes[function.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    (d.body.clone(), d.decorators, eng.g(&md, d.name))
                }
                _ => return,
            }
        };
        if decorators.is_some() || body.len() != 1 {
            return;
        }
        let stmt = GNode { m: function.m, n: body[0] };
        let call: Option<GNode> = {
            let md = eng.md(stmt.m);
            match &md.tree.nodes[stmt.n.idx()].kind {
                NodeKind::Expr { value } | NodeKind::Return { value: Some(value) } => {
                    Some(GNode { m: stmt.m, n: *value })
                }
                _ => None,
            }
        };
        let Some(call) = call else { return };
        let (func_attr, super_expr): (GSym, GNode) = {
            let md = eng.md(call.m);
            let NodeKind::Call { func, .. } = &md.tree.nodes[call.n.idx()].kind else {
                return;
            };
            match &md.tree.nodes[func.idx()].kind {
                NodeKind::Attribute { expr, attrname, .. } => {
                    (eng.g(&md, *attrname), GNode { m: call.m, n: *expr })
                }
                _ => return,
            }
        };
        let super_call = u::safe_infer(eng, cx.caches, super_expr);
        let Some(Value::Super { mro_pointer, mro_type, .. }) = super_call else {
            return;
        };
        if func_attr != name {
            return;
        }
        let current_scope = eng.scope(eng.parent(function).unwrap_or(function));
        if mro_pointer != current_scope {
            return;
        }
        let type_name_ok = match &*mro_type {
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
                eng.node_name(*cls) == eng.node_name(current_scope)
            }
            _ => false,
        };
        if !type_name_ok {
            return;
        }
        // ---- main body ----
        let name_str = eng.sname(name);
        if name_str == "__hash__" {
            // mymethods scan for __eq__
            let parent = eng.parent(function).map(|p| eng.frame(p));
            if let Some(parent) = parent {
                let md = eng.md(parent.m);
                let l = md.locals.borrow();
                if let Some(map) = l.get(&parent.n) {
                    let has_eq = map.iter().any(|(_, v)| {
                        v.first()
                            .map(|&g| {
                                is_funcdef(eng, g)
                                    && eng.node_name(g).as_deref() == Some("__eq__")
                            })
                            .unwrap_or(false)
                    });
                    if has_eq {
                        return;
                    }
                }
            }
        }
        let klass = eng.frame(eng.parent(function).unwrap_or(function));
        let sym = name;
        let mut meth_node: Option<GNode> = None;
        let ancs = match eng.mro(klass, None) {
            Ok(m) => m.get(1..).map(|s| s.to_vec()).unwrap_or_default(),
            Err(_) => eng.ancestors(klass, true, None),
        };
        for anc in ancs {
            let vals = eng.class_locals_get(anc, sym);
            let Some(&mn) = vals.first() else { continue };
            meth_node = Some(mn);
            let f_spec = eng.arg_spec(function);
            let m_spec = eng.arg_spec(mn);
            let bad = !is_funcdef(eng, mn)
                || has_different_parameters_default_value(cx, m_spec.as_ref(), f_spec.as_ref())
                || (m_spec.as_ref().map(|sp| sp.args_unknown).unwrap_or(true) && {
                    // function.argnames() != ["self"]
                    let f_names: Vec<String> = f_spec
                        .as_ref()
                        .map(|sp| {
                            sp.arguments()
                                .iter()
                                .filter_map(|&g| eng.assign_name_of(g).map(|s| eng.sname(s)))
                                .collect()
                        })
                        .unwrap_or_default();
                    f_names != ["self"]
                });
            if bad {
                return;
            }
            break;
        }
        let f_spec = match eng.arg_spec(function) {
            Some(sp) => sp,
            None => return,
        };
        if let Some(mn) = meth_node {
            let Some(m_spec) = eng.arg_spec(mn) else { return };
            // vararg guard
            if m_spec.vararg.is_some()
                && (f_spec.vararg.is_none() || f_spec.args.len() > m_spec.args.len())
            {
                return;
            }
            // annotation guard: posonly annotations + annotations as_string
            let form = |g: GNode, spec: &pyinfer::calls::ArgSpec| -> Vec<String> {
                let md = eng.md(g.m);
                let args_id = spec.arguments_node;
                let NodeKind::Arguments(a) = &md.tree.nodes[args_id.n.idx()].kind else {
                    return Vec::new();
                };
                let mut out = Vec::new();
                for ann in a.posonlyargs_annotations.iter().chain(a.annotations.iter()) {
                    if let Some(an) = ann {
                        out.push(u::as_string(eng, GNode { m: g.m, n: *an }));
                    }
                }
                out
            };
            let called = form(function, &f_spec);
            let overridden_anns = form(mn, &m_spec);
            if !called.is_empty() && !overridden_anns.is_empty() && called != overridden_anns {
                return;
            }
            // return annotations
            let returns_of = |g: GNode| -> Option<GNode> {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                        d.returns.map(|r| GNode { m: g.m, n: r })
                    }
                    _ => None,
                }
            };
            if let (Some(fr), Some(mr)) = (returns_of(function), returns_of(mn)) {
                if u::as_string(eng, mr) != u::as_string(eng, fr) {
                    return;
                }
            }
        }
        if definition_equivalent_to_call(eng, &f_spec, call) {
            cx.emit_node(
                "W0246",
                u::msg_line(eng, function),
                u::msg_col(eng, function),
                u::format_template(
                    "Useless parent or super() delegation in method %r",
                    &[&name_str],
                ),
            );
        }
    }

    /// _check_property_with_parameters (class_checker.py:1449-1455) — R0206
    fn check_property_with_parameters(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some(spec) = eng.arg_spec(node) else { return };
        if spec.arguments().len() > 1
            && tc::decorated_with_property(eng, cx.caches, node)
            && !is_property_kind(eng, node, &["setter"])
        {
            cx.emit_node(
                "R0206",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Cannot have defined parameters for properties".to_string(),
            );
        }
    }

    /// _check_init (class_checker.py:2206-2270) — W0231/W0233
    fn check_init(&mut self, cx: &mut WalkCx, node: GNode, klass_node: GNode) {
        let eng = cx.eng;
        let init_sym = eng.sym("__init__");
        // _ancestors_to_call
        let mut not_called_yet: indexmap::IndexMap<GNode, GNode> = indexmap::IndexMap::new();
        for base in eng.ancestors(klass_node, false, None) {
            match eng.class_igetattr_first(base, init_sym, None, true) {
                Ok(Some(Value::UnboundMethod { func }))
                | Ok(Some(Value::BoundMethod { func, .. })) => {
                    // BoundMethod IS an UnboundMethod subclass in astroid
                    if func_is_abstract_default(cx, func) {
                        continue;
                    }
                    not_called_yet.insert(base, func);
                }
                _ => continue,
            }
        }
        let mut parents_with_called_inits: rustc_hash::FxHashSet<Option<GNode>> =
            rustc_hash::FxHashSet::default();
        let direct_ancestors: Vec<GNode> = eng.ancestors(klass_node, false, None);
        for stmt in u::preorder(eng, node) {
            let (expr_attr, expr_expr): (GSym, GNode) = {
                let md = eng.md(stmt.m);
                let NodeKind::Call { func, .. } = &md.tree.nodes[stmt.n.idx()].kind else {
                    continue;
                };
                match &md.tree.nodes[func.idx()].kind {
                    NodeKind::Attribute { expr, attrname, .. } => {
                        (eng.g(&md, *attrname), GNode { m: stmt.m, n: *expr })
                    }
                    _ => continue,
                }
            };
            if eng.sname(expr_attr) != "__init__" {
                continue;
            }
            // skip the whole check on super().__init__
            {
                let md = eng.md(expr_expr.m);
                if let NodeKind::Call { func, .. } = &md.tree.nodes[expr_expr.n.idx()].kind {
                    if matches!(&md.tree.nodes[func.idx()].kind,
                        NodeKind::Name { name } if md.tree.s(*name) == "super")
                    {
                        return;
                    }
                }
            }
            let flow = eng.infer(expr_expr, &Ctx::new());
            if flow.err.is_some() && flow.vals.is_empty() {
                continue; // InferenceError
            }
            for klass in &flow.vals {
                if klass.is_uninferable() {
                    continue;
                }
                match klass {
                    Value::Inst { cls, .. }
                        if eng.node_name(*cls).as_deref() == Some("super")
                            && eng.md(cls.m).name == "builtins" =>
                    {
                        return;
                    }
                    Value::Super { .. } => return,
                    _ => {}
                }
                let klass_cls: Option<GNode> = match klass {
                    Value::Node(g) if is_classdef(eng, *g) => Some(*g),
                    _ => None,
                };
                if let Some(g) = klass_cls {
                    if let Some(method) = not_called_yet.shift_remove(&g) {
                        parents_with_called_inits.insert(node_frame_class(eng, method));
                        continue;
                    }
                }
                // KeyError branch
                let in_direct = klass_cls
                    .map(|g| direct_ancestors.contains(&g))
                    .unwrap_or(false);
                if !in_direct {
                    let kname: Option<String> = match klass {
                        Value::Node(g) => eng.node_name(*g),
                        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
                            eng.node_name(*cls)
                        }
                        _ => None,
                    };
                    if let Some(kn) = kname {
                        // node = expr (the Attribute func)
                        let func_node: Option<GNode> = {
                            let md = eng.md(stmt.m);
                            match &md.tree.nodes[stmt.n.idx()].kind {
                                NodeKind::Call { func, .. } => {
                                    Some(GNode { m: stmt.m, n: *func })
                                }
                                _ => None,
                            }
                        };
                        if let Some(fnode) = func_node {
                            cx.emit_node(
                                "W0233",
                                u::lineno(eng, fnode),
                                u::col_offset(eng, fnode).max(0) as i64,
                                u::format_template(
                                    "__init__ method from a non direct base class %r is called",
                                    &[&kn],
                                ),
                            );
                        }
                    }
                }
            }
        }
        let entries: Vec<(GNode, GNode)> =
            not_called_yet.iter().map(|(k, v)| (*k, *v)).collect();
        for (klass, method) in entries {
            if parents_with_called_inits.contains(&node_frame_class(eng, method)) {
                return; // NOTE: return, not continue
            }
            if is_classdef(eng, klass) && tc::is_protocol_class(eng, klass) {
                return; // also return!
            }
            if tc::decorated_with(eng, node, &["typing.overload"]) {
                continue;
            }
            let kname = eng.node_name(klass).unwrap_or_default();
            cx.emit_node(
                "W0231",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template(
                    "__init__ method from base class %r is not called",
                    &[&kname],
                ),
            );
        }
    }

    /// _check_signature (class_checker.py:2272-2357) — W0221/W0222/W0237
    fn check_signature(
        &mut self,
        cx: &mut WalkCx,
        method1: GNode,
        refmethod: GNode,
        _cls: GNode,
    ) {
        let eng = cx.eng;
        let Some(m1_spec) = eng.arg_spec(method1) else { return };
        let Some(ref_spec) = eng.arg_spec(refmethod) else { return };
        if m1_spec.args_unknown || ref_spec.args_unknown {
            return;
        }
        let name = eng.node_name(method1).unwrap_or_default();
        if is_attr_private(&name) {
            return;
        }
        if is_property_kind(eng, method1, &["setter"]) {
            return;
        }
        let arg_differ_output =
            different_parameters(cx, refmethod, &ref_spec, method1, &m1_spec);
        let class_type = "overriding";
        if !arg_differ_output.is_empty() {
            let frame_name = |f: GNode| -> String {
                eng.parent(f)
                    .map(|p| eng.node_name(eng.frame(p)).unwrap_or_default())
                    .unwrap_or_default()
            };
            for msg in &arg_differ_output {
                let (msgid, first_arg): (&'static str, String) = if msg.contains("Number") {
                    let total = |sp: &pyinfer::calls::ArgSpec| -> usize {
                        sp.args.len()
                            + usize::from(sp.vararg.is_some())
                            + usize::from(sp.kwarg.is_some())
                            + sp.kwonlyargs.len()
                    };
                    (
                        "W0221",
                        format!(
                            "{}was {} in '{}.{}' and is now {} in",
                            msg,
                            total(&ref_spec),
                            frame_name(refmethod),
                            eng.node_name(refmethod).unwrap_or_default(),
                            total(&m1_spec)
                        ),
                    )
                } else if msg.contains("renamed") {
                    ("W0237", msg.clone())
                } else {
                    ("W0221", msg.clone())
                };
                let third = format!("{}.{}", frame_name(method1), name);
                cx.emit_node(
                    msgid,
                    u::msg_line(eng, method1),
                    u::msg_col(eng, method1),
                    u::format_template("%s %s %r method", &[&first_arg, class_type, &third]),
                );
            }
        } else if m1_spec.defaults.len() < ref_spec.defaults.len() && m1_spec.vararg.is_none() {
            cx.emit_node(
                "W0222",
                u::msg_line(eng, method1),
                u::msg_col(eng, method1),
                u::format_template(
                    "Signature differs from %s %r method",
                    &["overridden", &name],
                ),
            );
        }
    }

    /// _check_invalid_overridden_method (class_checker.py:1457-1505) —
    /// W0236/W0239
    fn check_invalid_overridden_method(
        &mut self,
        cx: &mut WalkCx,
        function_node: GNode,
        parent_function: GNode,
    ) {
        let eng = cx.eng;
        let parent_is_property = tc::decorated_with_property(eng, cx.caches, parent_function)
            || is_property_kind(eng, parent_function, &["setter", "deleter"]);
        let current_is_property = tc::decorated_with_property(eng, cx.caches, function_node)
            || is_property_kind(eng, function_node, &["setter", "deleter"]);
        let name = eng.node_name(function_node).unwrap_or_default();
        let emit_w0236 = |cx: &mut WalkCx, a2: &str, a3: &str| {
            cx.emit_node(
                "W0236",
                u::msg_line(cx.eng, function_node),
                u::msg_col(cx.eng, function_node),
                u::format_template(
                    "Method %r was expected to be %r, found it instead as %r",
                    &[&name, a2, a3],
                ),
            );
        };
        if parent_is_property && !current_is_property {
            emit_w0236(cx, "property", &astroid_type_str(eng, function_node));
        } else if !parent_is_property && current_is_property {
            emit_w0236(cx, "method", "property");
        }
        let parent_is_async =
            eng.kind_is(parent_function, |k| matches!(k, NodeKind::AsyncFunctionDef(_)));
        let current_is_async =
            eng.kind_is(function_node, |k| matches!(k, NodeKind::AsyncFunctionDef(_)));
        if parent_is_async && !current_is_async {
            emit_w0236(cx, "async", "non-async");
        } else if !parent_is_async && current_is_async {
            emit_w0236(cx, "non-async", "async");
        }
        // W0239 (py38+ always true on 3.12)
        if tc::decorated_with(eng, parent_function, &["typing.final"])
            || has_uninferable_final_decorators(cx, parent_function)
        {
            let owner = eng
                .parent(parent_function)
                .map(|p| eng.node_name(eng.frame(p)).unwrap_or_default())
                .unwrap_or_default();
            cx.emit_node(
                "W0239",
                u::msg_line(eng, function_node),
                u::msg_col(eng, function_node),
                u::format_template(
                    "Method %r overrides a method decorated with typing.final which is defined in class %r",
                    &[&name, &owner],
                ),
            );
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
        if cx.full {
            self.check_unused_private_functions(cx, node);
            self.check_unused_private_variables(cx, node);
            self.check_unused_private_attributes(cx, node);
        }
        let cname = eng.node_name(node).unwrap_or_default();
        // mixin_class_rgx `.*[Mm]ixin` re.match: contains Mixin/mixin
        if cname.contains("Mixin") || cname.contains("mixin") {
            return;
        }
        if eng.class_type(node) != "metaclass" {
            self.check_accessed_members(cx, node);
        }
        if !(cx.cfg_enabled)("W0201") {
            return;
        }
        // _check_attribute_defined_outside_init body (class_checker.py:1210+)
        const DEFINING: &[&str] = &["__init__", "__new__", "setUp", "asyncSetUp", "__post_init__"];
        let current_module = node.m;
        let instance_attrs: Vec<(GSym, Vec<GNode>)> = eng
            .instance_attrs_of(node)
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        for (attr, nodes_lst) in instance_attrs {
            let attr_s = eng.sname(attr);
            if attr_s == "__dict__" {
                continue;
            }
            let nodes_lst: Vec<GNode> = nodes_lst
                .into_iter()
                .filter(|&n| {
                    let st = eng.statement(n);
                    let bad_stmt = st
                        .map(|st| {
                            eng.kind_is(st, |k| {
                                matches!(k, NodeKind::Delete { .. } | NodeKind::AugAssign { .. })
                            })
                        })
                        .unwrap_or(false);
                    !bad_stmt && n.m == current_module
                })
                .collect();
            if nodes_lst.is_empty() {
                continue;
            }
            let frame_ok = nodes_lst.iter().any(|&n| {
                let frame = eng.frame(n);
                let fname = eng.node_name(frame).unwrap_or_default();
                DEFINING.contains(&fname.as_str()) || is_property_kind(eng, frame, &["setter"])
            });
            if frame_ok {
                continue;
            }
            // instance_attr_ancestors: ancestors() (recursive), NOT mro
            let mut attr_defined_in_ancestor = false;
            let ancs = eng.ancestors(node, true, None);
            for parent in ancs {
                let pattrs = eng.instance_attrs_of(parent);
                let Some(pnodes) = pattrs.get(&attr) else { continue };
                let mut attr_defined = false;
                for &pn in pnodes {
                    let fname = eng.node_name(eng.frame(pn)).unwrap_or_default();
                    if DEFINING.contains(&fname.as_str()) {
                        attr_defined = true;
                    }
                }
                if attr_defined {
                    attr_defined_in_ancestor = true;
                    break;
                }
            }
            if attr_defined_in_ancestor {
                continue;
            }
            // class attribute? (cnode.local_attr)
            if !tc::class_local_attr(eng, node, &attr_s).is_empty() {
                continue;
            }
            for &n in &nodes_lst {
                let frame = eng.frame(n);
                let fname = eng.node_name(frame).unwrap_or_default();
                if !DEFINING.contains(&fname.as_str()) {
                    if called_in_methods(cx, frame, node, DEFINING) {
                        continue;
                    }
                    cx.emit_node(
                        "W0201",
                        u::lineno(eng, n),
                        u::col_offset(eng, n).max(0) as i64,
                        u::format_template(
                            "Attribute %r defined outside __init__",
                            &[&attr_s],
                        ),
                    );
                }
            }
        }
    }

    /// _check_unused_private_functions (class_checker.py:1061-1115) — W0238
    fn check_unused_private_functions(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let node_name = eng.node_name(node).unwrap_or_default();
        let funcs: Vec<GNode> = u::preorder(eng, node)
            .into_iter()
            .filter(|&g| is_funcdef(eng, g))
            .collect();
        let uses: Vec<GNode> = u::preorder(eng, node)
            .into_iter()
            .filter(|&g| {
                eng.kind_is(g, |k| {
                    matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. })
                })
            })
            .collect();
        'funcs: for function_def in funcs {
            let fname = eng.node_name(function_def).unwrap_or_default();
            if !is_attr_private(&fname) {
                continue;
            }
            let parent_scope = eng.scope(eng.parent(function_def).unwrap_or(function_def));
            if is_funcdef(eng, parent_scope) {
                // nested function: a Name use in the enclosing function
                let used = u::preorder(eng, parent_scope).into_iter().any(|g| {
                    let md = eng.md(g.m);
                    matches!(&md.tree.nodes[g.n.idx()].kind,
                        NodeKind::Name { name } if md.tree.s(*name) == fname)
                });
                if used {
                    continue;
                }
            }
            for &child in &uses {
                let md = eng.md(child.m);
                match &md.tree.nodes[child.n.idx()].kind {
                    NodeKind::Name { name } => {
                        if md.tree.s(*name) == fname {
                            continue 'funcs; // used (break)
                        }
                    }
                    NodeKind::Attribute { expr, attrname, .. } => {
                        let an = md.tree.s(*attrname).to_string();
                        let expr = GNode { m: child.m, n: *expr };
                        drop(md);
                        if an != fname || eng.scope(child) == function_def {
                            continue;
                        }
                        let md = eng.md(expr.m);
                        if let NodeKind::Name { name } = &md.tree.nodes[expr.n.idx()].kind {
                            let en = md.tree.s(*name);
                            if en == "self" || en == "cls" || en == node_name {
                                continue 'funcs; // used
                            }
                        } else if matches!(
                            md.tree.nodes[expr.n.idx()].kind,
                            NodeKind::Call { .. }
                        ) {
                            drop(md);
                            // type(self).__attrname
                            if let Some(Value::Node(g)) = u::safe_infer(eng, cx.caches, expr) {
                                if is_classdef(eng, g)
                                    && eng.node_name(g).as_deref() == Some(node_name.as_str())
                                {
                                    continue 'funcs; // used
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            // unused: build the dotted repr through enclosing scopes
            let mut name_stack: Vec<String> = Vec::new();
            let mut curr = parent_scope;
            while curr != node {
                name_stack.push(eng.node_name(curr).unwrap_or_default());
                curr = eng.scope(eng.parent(curr).unwrap_or(curr));
            }
            name_stack.reverse();
            let outer = name_stack.join(".");
            let args_str = {
                let spec = eng.arg_spec(function_def);
                match spec {
                    Some(sp) => pyinfer::asstr::as_string(
                        eng,
                        sp.arguments_node,
                    ),
                    None => String::new(),
                }
            };
            let function_repr =
                format!("{}.{}({})", outer, fname, args_str);
            let text = u::format_template(
                "Unused private member `%s.%s`",
                &[&node_name, function_repr.trim_start_matches('.')],
            );
            cx.emit_node(
                "W0238",
                u::msg_line(eng, function_def),
                u::msg_col(eng, function_def),
                text,
            );
        }
    }

    /// _check_unused_private_variables (class_checker.py:1117-1138) — W0238
    fn check_unused_private_variables(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let node_name = eng.node_name(node).unwrap_or_default();
        let assigns: Vec<GNode> = u::preorder(eng, node)
            .into_iter()
            .filter(|&g| eng.kind_is(g, |k| matches!(k, NodeKind::AssignName { .. })))
            .collect();
        let uses: Vec<GNode> = u::preorder(eng, node)
            .into_iter()
            .filter(|&g| {
                eng.kind_is(g, |k| {
                    matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. })
                })
            })
            .collect();
        'assigns: for assign_name in assigns {
            if eng
                .parent(assign_name)
                .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::Arguments(_))))
                .unwrap_or(false)
            {
                continue;
            }
            let aname = name_of_str(eng, assign_name);
            if !is_attr_private(&aname) {
                continue;
            }
            for &child in &uses {
                let md = eng.md(child.m);
                match &md.tree.nodes[child.n.idx()].kind {
                    NodeKind::Name { name } => {
                        if md.tree.s(*name) == aname {
                            continue 'assigns;
                        }
                    }
                    NodeKind::Attribute { expr, attrname, .. } => {
                        let is_name_expr = matches!(
                            md.tree.nodes[expr.idx()].kind,
                            NodeKind::Name { .. }
                        );
                        if !is_name_expr {
                            continue 'assigns; // break (counted as used)
                        }
                        if md.tree.s(*attrname) == aname {
                            if let NodeKind::Name { name } = &md.tree.nodes[expr.idx()].kind {
                                let en = md.tree.s(*name);
                                if en == "self" || en == "cls" || en == node_name {
                                    continue 'assigns;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            let text = u::format_template(
                "Unused private member `%s.%s`",
                &[&node_name, &aname],
            );
            cx.emit_node(
                "W0238",
                u::msg_line(eng, assign_name),
                u::msg_col(eng, assign_name),
                text,
            );
        }
    }

    /// _check_unused_private_attributes (class_checker.py:1140-1190) — W0238
    fn check_unused_private_attributes(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let node_name = eng.node_name(node).unwrap_or_default();
        let assign_attrs: Vec<GNode> = u::preorder(eng, node)
            .into_iter()
            .filter(|&g| eng.kind_is(g, |k| matches!(k, NodeKind::AssignAttr { .. })))
            .collect();
        let attributes: Vec<GNode> = u::preorder(eng, node)
            .into_iter()
            .filter(|&g| eng.kind_is(g, |k| matches!(k, NodeKind::Attribute { .. })))
            .collect();
        'outer: for assign_attr in assign_attrs {
            let (aname, aexpr): (String, GNode) = {
                let md = eng.md(assign_attr.m);
                match &md.tree.nodes[assign_attr.n.idx()].kind {
                    NodeKind::AssignAttr { expr, attrname } => (
                        md.tree.s(*attrname).to_string(),
                        GNode { m: assign_attr.m, n: *expr },
                    ),
                    _ => continue,
                }
            };
            if !is_attr_private(&aname) {
                continue;
            }
            let aexpr_name: Option<String> = {
                let md = eng.md(aexpr.m);
                match &md.tree.nodes[aexpr.n.idx()].kind {
                    NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
                    _ => None,
                }
            };
            let Some(aexpr_name) = aexpr_name else { continue };
            // __new__ returned object names
            let mut acceptable: Vec<String> = vec!["self".to_string()];
            let scope = eng.scope(assign_attr);
            if is_funcdef(eng, scope) && eng.node_name(scope).as_deref() == Some("__new__") {
                for r in u::preorder(eng, scope) {
                    let md = eng.md(r.m);
                    if let NodeKind::Return { value: Some(v) } = &md.tree.nodes[r.n.idx()].kind {
                        if let NodeKind::Name { name } = &md.tree.nodes[v.idx()].kind {
                            acceptable.push(md.tree.s(*name).to_string());
                        }
                    }
                }
            }
            for &attribute in &attributes {
                let md = eng.md(attribute.m);
                let NodeKind::Attribute { expr, attrname, .. } =
                    &md.tree.nodes[attribute.n.idx()].kind
                else {
                    continue;
                };
                if md.tree.s(*attrname) != aname {
                    continue;
                }
                let en: Option<&str> = match &md.tree.nodes[expr.idx()].kind {
                    NodeKind::Name { name } => Some(md.tree.s(*name)),
                    _ => None,
                };
                let Some(en) = en else { continue };
                if (aexpr_name == "cls" || aexpr_name == node_name)
                    && (en == "cls" || en == "self" || en == node_name)
                {
                    continue 'outer;
                }
                if acceptable.iter().any(|a| *a == aexpr_name) && en == "self" {
                    continue 'outer;
                }
                if aexpr_name == en && en == node_name {
                    continue 'outer;
                }
            }
            let text = u::format_template(
                "Unused private member `%s.%s`",
                &[&node_name, &aname],
            );
            cx.emit_node(
                "W0238",
                u::msg_line(eng, assign_attr),
                u::msg_col(eng, assign_attr),
                text,
            );
        }
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

/// astroid Arguments.default_value(name) (node_classes.py:930-955):
/// Ok(node) or Err(NoDefault)
fn default_value(eng: &Engine, spec: &pyinfer::calls::ArgSpec, argname: &str) -> Result<GNode, ()> {
    let name_of = |g: GNode| eng.assign_name_of(g).map(|s| eng.sname(s));
    // kwonly first
    if let Some(idx) = spec
        .kwonlyargs
        .iter()
        .position(|&g| name_of(g).as_deref() == Some(argname))
    {
        if spec.kw_defaults.len() > idx {
            return match spec.kw_defaults[idx] {
                Some(d) => Ok(d),
                None => Err(()),
            };
        }
    }
    // args = arguments minus vararg/kwarg NAMES
    let vararg = spec.vararg.map(|s| eng.sname(s));
    let kwarg = spec.kwarg.map(|s| eng.sname(s));
    let args: Vec<GNode> = spec
        .arguments()
        .into_iter()
        .filter(|&g| {
            let n = name_of(g);
            n != vararg || n.is_none()
        })
        .filter(|&g| {
            let n = name_of(g);
            n != kwarg || n.is_none()
        })
        .collect();
    if let Some(index) = args.iter().position(|&g| name_of(g).as_deref() == Some(argname)) {
        let total = args.len() as i64;
        let idx = index as i64 - (total - spec.defaults.len() as i64 - spec.kw_defaults.len() as i64);
        if idx >= 0 && (idx as usize) < spec.defaults.len() {
            return Ok(spec.defaults[idx as usize]);
        }
    }
    Err(())
}

/// ASTROID_TYPE_COMPARATORS-driven equality of two default-value nodes
/// (class_checker.py:54-61). Returns None when "unhandled" (treated as
/// different by callers).
fn astroid_default_eq(cx: &mut WalkCx, a: GNode, b: GNode) -> Option<bool> {
    let eng = cx.eng;
    let (ka, kb) = {
        let mda = eng.md(a.m);
        let ka = std::mem::discriminant(&mda.tree.nodes[a.n.idx()].kind);
        drop(mda);
        let mdb = eng.md(b.m);
        let kb = std::mem::discriminant(&mdb.tree.nodes[b.n.idx()].kind);
        (ka, kb)
    };
    if ka != kb {
        // `not isinstance(overridden_default, original_type)` -> different
        return Some(false);
    }
    let mda = eng.md(a.m);
    match &mda.tree.nodes[a.n.idx()].kind {
        NodeKind::Const(ca) => {
            let ca = ca.clone();
            drop(mda);
            let mdb = eng.md(b.m);
            let NodeKind::Const(cb) = &mdb.tree.nodes[b.n.idx()].kind else {
                return Some(false);
            };
            Some(const_py_eq(&ca, cb))
        }
        NodeKind::ClassDef(_) => {
            // a.qname == b.qname compares BOUND METHOD objects -> identity
            Some(a == b)
        }
        NodeKind::Tuple { elts: ea, .. } | NodeKind::List { elts: ea, .. } => {
            let ea = ea.clone();
            drop(mda);
            let mdb = eng.md(b.m);
            let eb: Vec<pyast::NodeId> = match &mdb.tree.nodes[b.n.idx()].kind {
                NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => elts.clone(),
                _ => return Some(false),
            };
            drop(mdb);
            // a.elts == b.elts: element-wise NODE IDENTITY
            Some(
                ea.len() == eb.len()
                    && ea
                        .iter()
                        .zip(eb.iter())
                        .all(|(&x, &y)| GNode { m: a.m, n: x } == GNode { m: b.m, n: y }),
            )
        }
        NodeKind::Dict { items: ia } => {
            let la = ia.len();
            drop(mda);
            let mdb = eng.md(b.m);
            let NodeKind::Dict { items: ib } = &mdb.tree.nodes[b.n.idx()].kind else {
                return Some(false);
            };
            // items lists of node pairs -> identity equality ([]==[] True)
            Some((la == 0 && ib.is_empty()) || a == b)
        }
        NodeKind::Name { .. } => {
            drop(mda);
            // set(a.infer()) == set(b.infer()) — node results by identity
            let fa = u::infer_all(eng, cx.caches, a);
            let fb = u::infer_all(eng, cx.caches, b);
            let key = |v: &Value| -> Option<(usize, usize)> {
                match v {
                    Value::Node(g) => Some((g.m.0 as usize, g.n.idx())),
                    Value::Uninferable => Some((usize::MAX, usize::MAX)),
                    _ => None,
                }
            };
            let mut sa = std::collections::HashSet::new();
            for v in fa.iter() {
                match key(v) {
                    Some(k) => {
                        sa.insert(k);
                    }
                    None => return Some(false), // non-keyable: treat as different
                }
            }
            let mut sb = std::collections::HashSet::new();
            for v in fb.iter() {
                match key(v) {
                    Some(k) => {
                        sb.insert(k);
                    }
                    None => return Some(false),
                }
            }
            Some(sa == sb)
        }
        _ => None, // unhandled comparator
    }
}

/// python `==` on const values (for default comparison)
fn const_py_eq(a: &ConstValue, b: &ConstValue) -> bool {
    u::py_key(a) == u::py_key(b)
}

/// _has_different_parameters_default_value (class_checker.py:216-259)
fn has_different_parameters_default_value(
    cx: &mut WalkCx,
    original: Option<&pyinfer::calls::ArgSpec>,
    overridden: Option<&pyinfer::calls::ArgSpec>,
) -> bool {
    let eng = cx.eng;
    let (Some(original), Some(overridden)) = (original, overridden) else { return false };
    if original.args_unknown || overridden.args_unknown {
        return false;
    }
    let params: Vec<String> = original
        .args
        .iter()
        .chain(original.kwonlyargs.iter())
        .filter_map(|&g| eng.assign_name_of(g).map(|s| eng.sname(s)))
        .collect();
    for pname in params {
        let od = default_value(eng, original, &pname);
        let vd = default_value(eng, overridden, &pname);
        match (od, vd) {
            (Err(()), Err(())) => continue,
            (Err(()), Ok(_)) => return true,  // only the original missing
            (Ok(_), Err(())) => return true,  // only the override has none
            (Ok(a), Ok(b)) => match astroid_default_eq(cx, a, b) {
                Some(true) => continue,
                Some(false) => return true,
                None => return true, // unhandled comparator
            },
        }
    }
    false
}

/// _signature_from_call + _definition_equivalent_to_call
/// (class_checker.py:81-143)
fn definition_equivalent_to_call(
    eng: &Engine,
    spec: &pyinfer::calls::ArgSpec,
    call: GNode,
) -> bool {
    // _signature_from_call
    let mut kws: Vec<Option<String>> = Vec::new(); // keys only matter
    let mut call_args: Vec<Option<String>> = Vec::new();
    let mut starred_args: Vec<String> = Vec::new();
    let mut starred_kws: Vec<String> = Vec::new();
    {
        let md = eng.md(call.m);
        let NodeKind::Call { args, keywords, .. } = &md.tree.nodes[call.n.idx()].kind else {
            return false;
        };
        for &kw in keywords {
            if let NodeKind::Keyword { arg, value } = &md.tree.nodes[kw.idx()].kind {
                let vname: Option<String> = match &md.tree.nodes[value.idx()].kind {
                    NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
                    _ => None,
                };
                match (arg, vname) {
                    (None, Some(n)) => starred_kws.push(n),
                    (Some(a), _) => kws.push(Some(md.tree.s(*a).to_string())),
                    (None, None) => kws.push(None), // kws[None] = None
                }
            }
        }
        for &a in args {
            match &md.tree.nodes[a.idx()].kind {
                NodeKind::Starred { value, .. } => {
                    if let NodeKind::Name { name } = &md.tree.nodes[value.idx()].kind {
                        starred_args.push(md.tree.s(*name).to_string());
                    } else {
                        // non-name Starred: pylint appends nothing? — the
                        // match has no catch for Starred(non-Name): falls to
                        // `case _: args.append(None)`
                        call_args.push(None);
                    }
                }
                NodeKind::Name { name } => call_args.push(Some(md.tree.s(*name).to_string())),
                _ => call_args.push(None),
            }
        }
    }
    // _signature_from_arguments: posonly+args names, any arg NAMED "self"
    // dropped regardless of position
    let def_args: Vec<String> = spec
        .posonlyargs
        .iter()
        .chain(spec.args.iter())
        .filter_map(|&g| eng.assign_name_of(g).map(|s| eng.sname(s)))
        .filter(|n| n != "self")
        .collect();
    let def_kwonly: Vec<String> = spec
        .kwonlyargs
        .iter()
        .filter_map(|&g| eng.assign_name_of(g).map(|s| eng.sname(s)))
        .collect();
    let def_vararg = spec.vararg.map(|s| eng.sname(s));
    let def_kwarg = spec.kwarg.map(|s| eng.sname(s));
    // _definition_equivalent_to_call
    if let Some(k) = &def_kwarg {
        if !starred_kws.iter().any(|x| x == k) {
            return false;
        }
    } else if !starred_kws.is_empty() {
        return false;
    }
    if let Some(v) = &def_vararg {
        if !starred_args.iter().any(|x| x == v) {
            return false;
        }
    } else if !starred_args.is_empty() {
        return false;
    }
    let kw_names: Vec<&str> = kws.iter().filter_map(|o| o.as_deref()).collect();
    if def_kwonly.iter().any(|k| !kw_names.contains(&k.as_str())) {
        return false;
    }
    let call_arg_names: Vec<Option<&str>> = call_args.iter().map(|o| o.as_deref()).collect();
    let def_arg_opts: Vec<Option<&str>> = def_args.iter().map(|s| Some(s.as_str())).collect();
    if def_arg_opts != call_arg_names {
        return false;
    }
    // no extra kwargs in call: `kw in call.args or kw in definition.kwonlyargs`
    kws.iter().all(|kw| {
        call_arg_names.iter().any(|a| *a == kw.as_deref())
            || kw
                .as_deref()
                .map(|k| def_kwonly.iter().any(|a| a == k))
                .unwrap_or(false)
    })
}

/// _positional_parameters (class_checker.py:202-206) after
/// function_to_method wrapping: drop the first arg iff classmethod
fn positional_parameters(eng: &Engine, func: GNode, spec: &pyinfer::calls::ArgSpec) -> Vec<GNode> {
    let mut positional: Vec<GNode> = spec.args.clone();
    if eng.func_type(func) == FType::ClassMethod && !positional.is_empty() {
        positional.remove(0);
    }
    positional
}

/// _different_parameters (class_checker.py:316-390)
fn different_parameters(
    cx: &mut WalkCx,
    original: GNode,
    original_spec: &pyinfer::calls::ArgSpec,
    overridden: GNode,
    overridden_spec: &pyinfer::calls::ArgSpec,
) -> Vec<String> {
    let eng = cx.eng;
    let name_of = |g: GNode| eng.assign_name_of(g).map(|s| eng.sname(s)).unwrap_or_default();
    let mut output_messages: Vec<String> = Vec::new();
    let mut original_parameters = positional_parameters(eng, original, original_spec);
    let overridden_parameters = positional_parameters(eng, overridden, overridden_spec);
    let mut original_kwonlyargs: Vec<GNode> = original_spec.kwonlyargs.clone();
    if overridden_spec.vararg.is_some() {
        let overridden_names: Vec<String> =
            overridden_parameters.iter().map(|&g| name_of(g)).collect();
        original_parameters.retain(|&g| overridden_names.contains(&name_of(g)));
    }
    if overridden_spec.kwarg.is_some() {
        let overridden_names: Vec<String> = overridden_spec
            .kwonlyargs
            .iter()
            .map(|&g| name_of(g))
            .collect();
        original_kwonlyargs.retain(|&g| overridden_names.contains(&name_of(g)));
    }
    // _has_different_parameters (zip_longest)
    let mut different_positional: Vec<String> = Vec::new();
    let maxlen = original_parameters.len().max(overridden_parameters.len());
    for i in 0..maxlen {
        let op = original_parameters.get(i);
        let vp = overridden_parameters.get(i);
        let Some(&vp) = vp else {
            different_positional = vec!["Number of parameters ".to_string()];
            break;
        };
        let Some(&op) = op else {
            // overridden_param.parent.default_value(name): NoDefault -> Number
            if default_value(eng, overridden_spec, &name_of(vp)).is_err() {
                different_positional = vec!["Number of parameters ".to_string()];
                break;
            }
            continue;
        };
        let on = name_of(op);
        let vn = name_of(vp);
        if dummy_param_match(&on) || dummy_param_match(&vn) {
            continue;
        }
        if on != vn {
            different_positional
                .push(format!("Parameter '{on}' has been renamed to '{vn}' in"));
        }
    }
    // _has_different_keyword_only_parameters
    let mut different_kwonly: Vec<String> = Vec::new();
    {
        let original_names: Vec<String> =
            original_kwonlyargs.iter().map(|&g| name_of(g)).collect();
        let overridden_names: Vec<String> = overridden_spec
            .kwonlyargs
            .iter()
            .map(|&g| name_of(g))
            .collect();
        if original_names.iter().any(|n| !overridden_names.contains(n)) {
            different_kwonly = vec!["Number of parameters ".to_string()];
        } else {
            for name in &overridden_names {
                if original_names.contains(name) {
                    continue;
                }
                if default_value(eng, overridden_spec, name).is_err() {
                    different_kwonly = vec!["Number of parameters ".to_string()];
                    break;
                }
            }
        }
    }
    if !different_kwonly.is_empty() && !different_positional.is_empty() {
        if different_positional[0].contains("Number ") && different_kwonly[0].contains("Number ")
        {
            output_messages.push("Number of parameters ".to_string());
            output_messages.extend(different_positional[1..].iter().cloned());
            output_messages.extend(different_kwonly[1..].iter().cloned());
        } else {
            output_messages.extend(different_positional);
            output_messages.extend(different_kwonly);
        }
    } else {
        output_messages.extend(different_positional);
        output_messages.extend(different_kwonly);
    }
    let kwarg_lost = original_spec.kwarg.is_some() && overridden_spec.kwarg.is_none();
    let vararg_lost = original_spec.vararg.is_some() && overridden_spec.vararg.is_none();
    if kwarg_lost || vararg_lost {
        output_messages.push("Variadics removed in".to_string());
    }
    let original_name = eng.node_name(original).unwrap_or_default();
    if u::PYMETHODS.contains(&original_name.as_str()) {
        output_messages.clear();
    }
    let _ = cx;
    output_messages
}

/// dummy-variables-rgx re.match for parameter-name comparison
fn dummy_param_match(name: &str) -> bool {
    crate::variables::dummy_rgx_match_pub(name)
}

/// astroid FunctionDef.type as a string (for the W0236 third arg)
fn astroid_type_str(eng: &Engine, func: GNode) -> String {
    match eng.func_type(func) {
        FType::ClassMethod => "classmethod".to_string(),
        FType::StaticMethod => "staticmethod".to_string(),
        FType::Method => "method".to_string(),
        FType::Function => "function".to_string(),
    }
}

/// utils.uninferable_final_decorators truthiness (utils.py:894-941)
fn has_uninferable_final_decorators(cx: &mut WalkCx, func: GNode) -> bool {
    let eng = cx.eng;
    for decorator in tc::decorator_nodes_pub(eng, func) {
        let import_nodes: Vec<NV> = {
            let md = eng.md(decorator.m);
            match &md.tree.nodes[decorator.n.idx()].kind {
                NodeKind::Attribute { expr, .. } => {
                    let e = GNode { m: decorator.m, n: *expr };
                    drop(md);
                    match u::safe_infer(eng, cx.caches, e) {
                        Some(Value::Node(g))
                            if u::is_module(eng, g) && eng.md(g.m).name == "typing" =>
                        {
                            let sym = match u::name_gsym(eng, e) {
                                Some(s) => s,
                                None => continue,
                            };
                            eng.lookup(e, sym).1.clone()
                        }
                        _ => continue,
                    }
                }
                NodeKind::Name { name } => {
                    let sym = eng.g(&md, *name);
                    drop(md);
                    eng.lookup(decorator, sym).1.clone()
                }
                _ => continue,
            }
        };
        let Some(NV::N(import_node)) = import_nodes.first() else { continue };
        let md = eng.md(import_node.m);
        let (is_from_import, is_import) = match &md.tree.nodes[import_node.n.idx()].kind {
            NodeKind::ImportFrom { modname, names, .. } => {
                let from_ok = names.iter().any(|(n, _)| md.tree.s(*n) == "final")
                    && md.tree.s(*modname) == "typing";
                (from_ok, false)
            }
            NodeKind::Import { names } => {
                let has_typing = names.iter().any(|(n, _)| md.tree.s(*n) == "typing");
                let attr_final = {
                    let md2 = eng.md(decorator.m);
                    matches!(&md2.tree.nodes[decorator.n.idx()].kind,
                        NodeKind::Attribute { attrname, .. } if md2.tree.s(*attrname) == "final")
                };
                (false, has_typing && attr_final)
            }
            _ => (false, false),
        };
        drop(md);
        if is_from_import || is_import {
            match u::safe_infer(eng, cx.caches, decorator) {
                None => return true,
                Some(v) if v.is_uninferable() => return true,
                _ => {}
            }
        }
    }
    false
}

/// _ancestors_to_call's init_node.is_abstract() (pass_is_abstract=True)
fn func_is_abstract_default(cx: &mut WalkCx, func: GNode) -> bool {
    crate::variables::func_is_abstract_pub(cx, func)
}

/// `name in node.locals`
fn locals_contains_name(eng: &Engine, scope: GNode, name: &str) -> bool {
    let sym = eng.sym(name);
    let md = eng.md(scope.m);
    let l = md.locals.borrow();
    l.get(&scope.n).map(|m| m.contains_key(&sym)).unwrap_or(false)
}

/// is_abstract callback selector for unimplemented_abstract_methods.
#[derive(Clone, Copy)]
pub enum AbstractCb {
    /// W0223: FunctionDef.is_abstract(pass_is_abstract=False).
    Strict,
    /// default (utils.unimplemented_abstract_methods): decorated_with(ABC_METHODS).
    AbcDecorated,
}

pub const ABC_METHODS: &[&str] = &[
    "abc.abstractproperty",
    "abc.abstractmethod",
    "abc.abstractclassmethod",
    "abc.abstractstaticmethod",
];

fn abstract_cb(cx: &mut WalkCx, func: GNode, cb: AbstractCb) -> bool {
    match cb {
        AbstractCb::Strict => func_is_abstract_strict(cx, func),
        AbstractCb::AbcDecorated => tc::decorated_with(cx.eng, func, ABC_METHODS),
    }
}

/// utils.unimplemented_abstract_methods (utils.py:945-994): reversed(mro());
/// last definition along the walk wins. Returns None on ResolveError (mro fail).
pub fn unimplemented_abstract_methods(
    cx: &mut WalkCx,
    node: GNode,
    cb: AbstractCb,
) -> Option<indexmap::IndexMap<String, GNode>> {
    let eng = cx.eng;
    let mro = eng.mro(node, None).ok()?;
    if std::env::var("PRYLINT_DBG_UNIMPL").is_ok() {
        let qn = eng.qname(node);
        if qn.ends_with(&std::env::var("PRYLINT_DBG_UNIMPL").unwrap_or_default()) {
            eprintln!(
                "UNIMPL {} mro={:?}",
                qn,
                mro.iter().map(|&g| eng.qname(g)).collect::<Vec<_>>()
            );
        }
    }
    let mut visited: indexmap::IndexMap<String, GNode> = indexmap::IndexMap::new();
    for &ancestor in mro.iter().rev() {
        let values: Vec<(String, GNode)> = {
            let md = eng.md(ancestor.m);
            let l = md.locals.borrow();
            match l.get(&ancestor.n) {
                Some(map) => map
                    .iter()
                    .filter_map(|(k, v)| v.first().map(|&g| (eng.sname(*k), g)))
                    .collect(),
                None => Vec::new(),
            }
        };
        for (obj_name, obj) in values {
            let mut inferred: Option<GNode> = Some(obj);
            if eng.kind_is(obj, |k| matches!(k, NodeKind::AssignName { .. })) {
                match u::safe_infer(eng, cx.caches, obj) {
                    None => {
                        visited.shift_remove(&obj_name);
                        continue;
                    }
                    Some(v) if v.is_uninferable() => {
                        visited.shift_remove(&obj_name);
                        continue;
                    }
                    Some(Value::Node(g)) if is_funcdef(eng, g) => {
                        inferred = Some(g);
                    }
                    Some(_) => {
                        visited.shift_remove(&obj_name);
                        inferred = None;
                    }
                }
            }
            if let Some(g) = inferred {
                if is_funcdef(eng, g) {
                    let abstract_ = abstract_cb(cx, g, cb);
                    if abstract_ {
                        visited.insert(obj_name.clone(), g);
                    } else if visited.contains_key(&obj_name) {
                        visited.shift_remove(&obj_name);
                    }
                }
            }
        }
    }
    Some(visited)
}

/// astroid FunctionDef.is_abstract(pass_is_abstract=False) (W0223 callback)
fn func_is_abstract_strict(cx: &mut WalkCx, func: GNode) -> bool {
    let eng = cx.eng;
    for dec in tc::decorator_nodes_pub(eng, func) {
        let inferred = match eng.first_value(dec, &Ctx::new()) {
            Ok(Some(v)) => v,
            _ => continue,
        };
        if let Some(q) = eng.value_qname(&inferred) {
            if q == "abc.abstractproperty" || q == "abc.abstractmethod" {
                return true;
            }
        }
    }
    let body: Vec<pyast::NodeId> = {
        let md = eng.md(func.m);
        match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
            _ => return false,
        }
    };
    for &child in &body {
        let g = GNode { m: func.m, n: child };
        let exc: Option<Option<pyast::NodeId>> = {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Raise { exc, .. } => Some(*exc),
                _ => None,
            }
        };
        if let Some(Some(e)) = exc {
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
        // pass_is_abstract=False: a `pass` body does NOT count
        return false;
    }
    // empty body (unreachable for parsed source)
    false
}

/// _called_in_methods (class_checker.py:416-446)
fn called_in_methods(cx: &mut WalkCx, func: GNode, klass: GNode, methods: &[&str]) -> bool {
    let eng = cx.eng;
    if !is_funcdef(eng, func) {
        return false;
    }
    let func_name = eng.node_name(func).unwrap_or_default();
    for method in methods {
        let attrs = match eng.class_getattr(klass, eng.sym(method), None, true) {
            Ok(a) => a,
            Err(_) => continue,
        };
        for infer_method in &attrs {
            let NV::N(im) = infer_method else { continue };
            for call in u::preorder(eng, *im) {
                let cfunc: Option<GNode> = {
                    let md = eng.md(call.m);
                    match &md.tree.nodes[call.n.idx()].kind {
                        NodeKind::Call { func, .. } => Some(GNode { m: call.m, n: *func }),
                        _ => None,
                    }
                };
                let Some(cf) = cfunc else { continue };
                let bound = match eng.first_value(cf, &Ctx::new()) {
                    Ok(Some(v)) => v,
                    _ => continue,
                };
                let bfunc: Option<GNode> = match &bound {
                    Value::BoundMethod { func, .. } | Value::DescBM { func, .. } => Some(*func),
                    _ => None,
                };
                if let Some(bf) = bfunc {
                    if eng.node_name(bf).as_deref() == Some(func_name.as_str()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn name_of_str(eng: &Engine, g: GNode) -> String {
    u::name_gsym(eng, g).map(|s| eng.sname(s)).unwrap_or_default()
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
