//! TypeChecker + IterableChecker port (pylint 4.0.5
//! `pylint/checkers/typecheck.py`, file:line cites refer to that file).
//! Bug-for-bug; emission-disabled paths that burn inference are kept where
//! they may affect shared cache state.

use std::cell::RefCell;
use std::rc::Rc;

use pyast::tree::{ConstValue, Ctx as ExprCtx, NodeKind};
use pyinfer::ctx::{CallCtx, Ctx};
use pyinfer::graph::{Engine, FType};
use pyinfer::value::{GNode, GSym, Value, NV};
use rustc_hash::FxHashMap;

use crate::ckutils as u;
use crate::walker::WalkCx;

/// py3.12 `sys.builtin_module_names` (pinned venv snapshot).
const BUILTIN_MODULE_NAMES: &[&str] = &[
    "_abc", "_ast", "_asyncio", "_bisect", "_blake2", "_bz2", "_codecs", "_codecs_cn",
    "_codecs_hk", "_codecs_iso2022", "_codecs_jp", "_codecs_kr", "_codecs_tw", "_collections",
    "_contextvars", "_csv", "_ctypes", "_curses", "_curses_panel", "_datetime", "_decimal",
    "_elementtree", "_functools", "_hashlib", "_heapq", "_imp", "_io", "_json", "_locale",
    "_lsprof", "_lzma", "_md5", "_multibytecodec", "_multiprocessing", "_opcode", "_operator",
    "_pickle", "_posixshmem", "_posixsubprocess", "_queue", "_random", "_scproxy", "_sha1",
    "_sha2", "_sha3", "_signal", "_socket", "_sqlite3", "_sre", "_ssl", "_stat", "_statistics",
    "_string", "_struct", "_symtable", "_testbuffer", "_testimportmultiple", "_testinternalcapi",
    "_testmultiphase", "_testsinglephase", "_thread", "_tokenize", "_tracemalloc", "_typing",
    "_uuid", "_warnings", "_weakref", "_xxinterpchannels", "_xxsubinterpreters", "_xxtestfuzz",
    "_zoneinfo", "array", "atexit", "audioop", "binascii", "builtins", "cmath", "errno",
    "faulthandler", "fcntl", "gc", "grp", "itertools", "marshal", "math", "mmap", "posix",
    "pwd", "pyexpat", "readline", "resource", "select", "sys", "syslog", "termios", "time",
    "unicodedata", "xxsubtype", "zlib",
];

/// typecheck.py:69
const STR_FORMAT: &[&str] = &["builtins.str.format"];
/// typecheck.py:416-426
const SEQUENCE_TYPES: &[&str] = &[
    "str", "unicode", "list", "tuple", "bytearray", "xrange", "range", "bytes", "memoryview",
];

#[derive(Default)]
pub struct TypeCk {
    /// visit_module: `from __future__ import annotations` (py-version 3.12)
    pub postponed_eval: bool,
}

// ---------------------------------------------------------------------------
// small node helpers
// ---------------------------------------------------------------------------

fn is_classdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_)))
}
fn is_funcdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)))
}
fn is_lambda(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::Lambda(_)))
}

fn ftype_str(t: FType) -> &'static str {
    match t {
        FType::Function => "function",
        FType::Method => "method",
        FType::ClassMethod => "classmethod",
        FType::StaticMethod => "staticmethod",
    }
}

fn func_name(eng: &Engine, g: GNode) -> Option<String> {
    eng.node_name(g)
}

/// Call node parts: (func, args, keyword nodes)
pub fn call_parts(eng: &Engine, call: GNode) -> Option<(GNode, Vec<GNode>, Vec<GNode>)> {
    let md = eng.md(call.m);
    match &md.tree.nodes[call.n.idx()].kind {
        NodeKind::Call { func, args, keywords } => Some((
            GNode { m: call.m, n: *func },
            args.iter().map(|&a| GNode { m: call.m, n: a }).collect(),
            keywords.iter().map(|&k| GNode { m: call.m, n: k }).collect(),
        )),
        _ => None,
    }
}

/// Keyword node -> (arg name or None for **, value node)
fn keyword_parts(eng: &Engine, kw: GNode) -> (Option<GSym>, GNode) {
    let md = eng.md(kw.m);
    match &md.tree.nodes[kw.n.idx()].kind {
        NodeKind::Keyword { arg, value } => {
            (arg.map(|s| eng.g(&md, s)), GNode { m: kw.m, n: *value })
        }
        _ => (None, kw),
    }
}

/// `value.name` like astroid proxies see it: node name, or the proxied
/// class name for instances/literals (bases.py:140-145 Proxy.__getattr__).
pub fn value_name(eng: &Engine, v: &Value) -> Option<String> {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Const(c) => eng.node_name(eng.const_class(c)),
                NodeKind::List { .. } => Some("list".into()),
                NodeKind::Tuple { .. } => Some("tuple".into()),
                NodeKind::Set { .. } => Some("set".into()),
                NodeKind::Dict { .. } => Some("dict".into()),
                NodeKind::Slice { .. } => Some("slice".into()),
                _ => eng.node_name(*g),
            }
        }
        Value::SynthConst(c) => eng.node_name(eng.const_class(c)),
        Value::SynthSeq { kind, .. } => Some(
            match kind {
                pyinfer::value::SeqKind::List => "list",
                pyinfer::value::SeqKind::Tuple => "tuple",
                pyinfer::value::SeqKind::Set => "set",
            }
            .into(),
        ),
        Value::SynthDict { .. } => Some("dict".into()),
        Value::SynthSlice { .. } => Some("slice".into()),
        Value::FrozenSet { .. } => Some("frozenset".into()),
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => eng.node_name(*cls),
        Value::BoundMethod { func, .. }
        | Value::DescBM { func, .. }
        | Value::UnboundMethod { func }
        | Value::Property { func, .. }
        | Value::Partial { func, .. } => eng.node_name(*func),
        Value::Generator { is_async, .. } => {
            Some(if *is_async { "async_generator" } else { "generator" }.into())
        }
        Value::Super { .. } => Some("super".into()),
        Value::UnionType => Some("UnionType".into()),
        Value::DictItems(_) => Some("dict_items".into()),
        Value::DictKeys(_) => Some("dict_keys".into()),
        Value::DictValues(_) => Some("dict_values".into()),
        Value::Uninferable | Value::EvaluatedObject { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// pylint safe_infer with compare_constructors=True (typecheck.py:1459)
// ---------------------------------------------------------------------------

pub fn safe_infer_cc(eng: &Engine, caches: &u::LintCaches, g: GNode) -> Option<Value> {
    if let Some(v) = caches.safe_infer_cc.borrow_mut().get(g) {
        return v;
    }
    let flow = eng.infer(g, &Ctx::new());
    let mut res = u::safe_infer_of_flow(eng, &flow);
    // class_constructors_are_ambiguous (utils.py:1451-1463): applies per
    // subsequent ClassDef with the FIRST value also a ClassDef
    if res.is_some() && flow.vals.len() > 1 {
        if let Value::Node(first) = &flow.vals[0] {
            if is_classdef(eng, *first) {
                for v in &flow.vals[1..] {
                    if let Value::Node(other) = v {
                        if is_classdef(eng, *other)
                            && class_constructors_ambiguous(eng, *first, *other)
                        {
                            res = None;
                            break;
                        }
                    }
                }
            }
        }
    }
    caches.safe_infer_cc.borrow_mut().put(g, res.clone());
    res
}

/// utils.py:1451-1463
fn class_constructors_ambiguous(eng: &Engine, c1: GNode, c2: GNode) -> bool {
    let init1 = class_local_attr(eng, c1, "__init__").first().copied();
    let init2 = class_local_attr(eng, c2, "__init__").first().copied();
    let (Some(i1), Some(i2)) = (init1, init2) else { return false };
    if !is_funcdef(eng, i1) || !is_funcdef(eng, i2) {
        return false;
    }
    u::fn_args_ambiguous_pub(eng, i1, i2)
}

/// astroid ClassDef.local_attr (scoped_nodes.py): own locals, else first
/// MRO ancestor's locals; DelAttr filtered; empty = NotFoundError.
pub fn class_local_attr(eng: &Engine, cls: GNode, name: &str) -> Vec<GNode> {
    let sym = eng.sym(name);
    let mut result = eng.class_locals_get(cls, sym);
    if result.is_empty() {
        let ancs = match eng.mro(cls, None) {
            Ok(m) => m.get(1..).map(|s| s.to_vec()).unwrap_or_default(),
            Err(_) => eng.ancestors(cls, true, None),
        };
        for a in ancs {
            let r = eng.class_locals_get(a, sym);
            if !r.is_empty() {
                result = r;
                break;
            }
        }
    }
    result.retain(|g| !eng.kind_is(*g, |k| matches!(k, NodeKind::DelAttr { .. })));
    result
}

// ---------------------------------------------------------------------------
// pylint has_known_bases (utils.py:1466-1484) — shares astroid's
// `_all_bases_known` node memo (engine.known_bases_cache) bug-for-bug.
// ---------------------------------------------------------------------------

pub fn has_known_bases(eng: &Engine, caches: &u::LintCaches, cls: GNode) -> bool {
    if let Some(&v) = eng.known_bases_cache.borrow().get(&cls) {
        return v;
    }
    for base in eng.class_bases(cls) {
        let ok = match u::safe_infer(eng, caches, base) {
            Some(Value::Node(g)) if is_classdef(eng, g) && g != cls => {
                has_known_bases(eng, caches, g)
            }
            _ => false,
        };
        if !ok {
            eng.known_bases_cache.borrow_mut().insert(cls, false);
            return false;
        }
    }
    eng.known_bases_cache.borrow_mut().insert(cls, true);
    true
}

/// has_known_bases over an inferred VALUE (Instance proxies to its class;
/// non-class values never reach it in-scope).
pub fn value_has_known_bases(eng: &Engine, caches: &u::LintCaches, v: &Value) -> bool {
    match v {
        Value::Node(g) if is_classdef(eng, *g) => has_known_bases(eng, caches, *g),
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => has_known_bases(eng, caches, *cls),
        _ => {
            // literals proxy builtin classes (known bases)
            match eng.proxied_class(v) {
                Some(c) => has_known_bases(eng, caches, c),
                None => true,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// decorated_with (utils.py:870-891) + is_overload_stub (utils.py:1666-1674)
// ---------------------------------------------------------------------------

fn decorator_nodes(eng: &Engine, func: GNode) -> Vec<GNode> {
    let md = eng.md(func.m);
    let dec = match &md.tree.nodes[func.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.decorators,
        NodeKind::ClassDef(d) => d.decorators,
        _ => None,
    };
    match dec {
        Some(dn) => match &md.tree.nodes[dn.idx()].kind {
            NodeKind::Decorators { nodes } => {
                nodes.iter().map(|&n| GNode { m: func.m, n }).collect()
            }
            _ => Vec::new(),
        },
        None => Vec::new(),
    }
}

/// utils.decorated_with: name-or-qname match over inferred decorator values
/// (Call decorators unwrap to .func). InferenceError per decorator -> skip.
pub fn decorated_with(eng: &Engine, func: GNode, qnames: &[&str]) -> bool {
    for mut dn in decorator_nodes(eng, func) {
        let md = eng.md(dn.m);
        if let NodeKind::Call { func: f, .. } = &md.tree.nodes[dn.n.idx()].kind {
            dn = GNode { m: dn.m, n: *f };
        }
        let flow = eng.infer(dn, &Ctx::new());
        for v in &flow.vals {
            let is_cls_or_fn = match v {
                Value::Node(g) => is_classdef(eng, *g) || is_funcdef(eng, *g),
                _ => false,
            };
            if !is_cls_or_fn {
                continue;
            }
            let name = value_name(eng, v);
            let qname = eng.value_qname(v);
            if name.as_deref().map(|n| qnames.contains(&n)).unwrap_or(false)
                || qname.as_deref().map(|n| qnames.contains(&n)).unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

pub fn is_overload_stub(caches: &u::LintCaches, eng: &Engine, func: GNode) -> bool {
    if let Some(&v) = caches.overload_stub.borrow().get(&func) {
        return v;
    }
    let has_dec = !decorator_nodes(eng, func).is_empty();
    let v = has_dec && decorated_with(eng, func, &["typing.overload", "overload"]);
    caches.overload_stub.borrow_mut().insert(func, v);
    v
}

// ---------------------------------------------------------------------------
// _determine_callable (typecheck.py:607-659)
// ---------------------------------------------------------------------------

pub struct Determined {
    /// the resolved called object (FunctionDef-flavored value)
    pub value: Value,
    /// the FunctionDef/Lambda node providing args
    pub func: GNode,
    pub implicit: usize,
    pub name: &'static str,
    /// FunctionModel attr___get__ DescriptorBoundMethod: synthetic args =
    /// func args + appended mandatory 'type' (objectmodel.py:416-459)
    pub desc_get: bool,
}

pub fn determine_callable(eng: &Engine, called: &Option<Value>) -> Option<Determined> {
    let v = called.as_ref()?;
    match v {
        Value::DescBM { func, .. } => {
            // DescriptorBoundMethod.implicit_parameters = 0
            // (objectmodel.py:360-363); .type proxies to the function
            if !is_funcdef(eng, *func) && !is_lambda(eng, *func) {
                return None;
            }
            Some(Determined {
                value: v.clone(),
                func: *func,
                implicit: 0,
                name: ftype_str(eng.func_type(*func)),
                desc_get: true,
            })
        }
        Value::BoundMethod { func, .. } => {
            let implicit =
                if func_name(eng, *func).as_deref() == Some("__new__") { 0 } else { 1 };
            // callable_obj.type proxies to the function's type
            let name = if is_lambda(eng, *func) {
                // Lambda.type: "method" if first arg self & class parent
                ftype_str(eng.func_type(*func))
            } else if is_funcdef(eng, *func) {
                ftype_str(eng.func_type(*func))
            } else {
                return None;
            };
            Some(Determined { value: v.clone(), func: *func, implicit, name, desc_get: false })
        }
        Value::UnboundMethod { func } => {
            Some(Determined { value: v.clone(), func: *func, implicit: 0, name: "unbound method", desc_get: false })
        }
        Value::Partial { func, parent, .. } => {
            // PartialFunction: FunctionDef arm. Its .type is computed with
            // parent = the partial(...) Call's parent (brain_functools):
            // a partial assigned in a class body is a "method", and
            // implicit_parameters() = 1 (is_bound, scoped_nodes.py:1412).
            let t = eng.partial_func_type(*func, *parent);
            let implicit = matches!(t, FType::Method | FType::ClassMethod) as usize;
            Some(Determined { value: v.clone(), func: *func, implicit, name: ftype_str(t), desc_get: false })
        }
        Value::Property { func, .. } => {
            // objects.Property.type = "property" (objects.py:349)
            Some(Determined { value: v.clone(), func: *func, implicit: 0, name: "property", desc_get: false })
        }
        Value::Node(g) if is_funcdef(eng, *g) => {
            let t = eng.func_type(*g);
            let implicit = matches!(t, FType::Method | FType::ClassMethod) as usize;
            Some(Determined { value: v.clone(), func: *g, implicit, name: ftype_str(t), desc_get: false })
        }
        Value::Node(g) if is_lambda(eng, *g) => {
            Some(Determined { value: v.clone(), func: *g, implicit: 0, name: "lambda", desc_get: false })
        }
        Value::Node(g) if is_classdef(eng, *g) => {
            // constructor resolution: last local __new__, object/builtin
            // fallback to last local __init__ (typecheck.py:625-655)
            let new = class_local_attr(eng, *g, "__new__").last().copied();
            let from_object = new
                .map(|n| {
                    let scope = eng.parent(n).map(|p| eng.scope(p));
                    scope
                        .and_then(|s| eng.node_name(s))
                        .as_deref()
                        == Some("object")
                })
                .unwrap_or(false);
            let from_builtins = new
                .map(|n| {
                    let root = eng.md(n.m).name.clone();
                    BUILTIN_MODULE_NAMES.contains(&root.as_str())
                })
                .unwrap_or(false);
            let callable_obj = if new.is_none() || from_object || from_builtins {
                let init = class_local_attr(eng, *g, "__init__").last().copied();
                init?
            } else {
                new.unwrap()
            };
            if !is_funcdef(eng, callable_obj) {
                return None;
            }
            Some(Determined {
                value: Value::Node(callable_obj),
                func: callable_obj,
                implicit: 1,
                name: "constructor",
                desc_get: false,
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// callable() on inferred values (§4.5)
// ---------------------------------------------------------------------------

pub fn value_is_instance(eng: &Engine, v: &Value) -> bool {
    // isinstance(v, astroid.Instance): Inst/ExcInst + literal nodes
    // (Const/List/Tuple/Set/Dict are Instance subclasses) + synthetic
    // containers + dict views (objects.DictItems extends Instance? — they
    // proxy builtins classes) + Super? (objects.Super is NOT Instance).
    match v {
        Value::Inst { .. } | Value::ExcInst { .. } => true,
        Value::Node(g) => {
            let md = eng.md(g.m);
            matches!(
                md.tree.nodes[g.n.idx()].kind,
                NodeKind::Const(_)
                    | NodeKind::List { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::Set { .. }
                    | NodeKind::Dict { .. }
            )
        }
        Value::SynthConst(_)
        | Value::SynthSeq { .. }
        | Value::SynthDict { .. }
        | Value::SynthSlice { .. }
        | Value::FrozenSet { .. }
        | Value::DictItems(_)
        | Value::DictKeys(_)
        | Value::DictValues(_) => true,
        _ => false,
    }
}

pub fn value_callable(eng: &Engine, v: &Value) -> bool {
    eng.value_callable(v, &Ctx::new())
}

// ---------------------------------------------------------------------------
// TypeChecker callbacks
// ---------------------------------------------------------------------------

impl TypeCk {
    pub fn visit_module(&mut self, cx: &mut WalkCx, _g: GNode) {
        self.postponed_eval = u::is_postponed_evaluation_enabled(cx.eng, cx.mid);
    }

    /// typecheck.py:1226-1307 — E1111 / E1128
    pub fn visit_assign(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_assignment_from_function_call(cx, node);
        // _check_dundername_is_string: W-only, no inference burn
    }

    fn check_assignment_from_function_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let value = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Assign { value, .. } => GNode { m: node.m, n: *value },
            _ => return,
        };
        let (func, _, _) = match call_parts(eng, value) {
            Some(p) => p,
            None => return,
        };
        let function_node = u::safe_infer(eng, cx.caches, func);
        // FunctionDef | UnboundMethod | BoundMethod (Lambda excluded:
        // nodes.Lambda matches isinstance(FunctionDef)? no — FunctionDef
        // only; bare Lambda values fail the gate)
        let fnode: GNode = match &function_node {
            Some(Value::Node(g)) if is_funcdef(eng, *g) => *g,
            Some(Value::BoundMethod { func, .. })
            | Some(Value::DescBM { func, .. })
            | Some(Value::UnboundMethod { func }) => *func,
            // Property/Partial are FunctionDef subclasses in astroid:
            // isinstance(function_node, funcs) is True for them
            Some(Value::Property { func, .. }) | Some(Value::Partial { func, .. }) => *func,
            _ => return,
        };
        // is_function is True for FunctionDef AND Lambda (scoped_nodes:1109)
        // decorators truthy -> bail
        if !decorator_nodes(eng, fnode).is_empty() {
            return;
        }
        if is_lambda(eng, fnode) {
            // Lambda proxied by a method: is_function True, but the
            // following checks use FunctionDef surfaces; a Lambda has no
            // returns -> nodes_of_class(Return) empty -> E1111. Replicate
            // by falling through (decorators None on Lambda).
        }
        // PropertyFuncAccessor synths: fget body = Property.body = []
        // (objectmodel.py:949), fset body = the setter's body (:998); root()
        // walks accessor -> Property -> the real class -> real module
        let accessor = eng.prop_accessors.borrow().get(&fnode).copied();
        let body_owner: Option<GNode> = match accessor {
            Some((_, 1)) => None,
            Some((w, _)) => Some(w),
            None => Some(fnode),
        };
        if let Some(bo) = body_owner {
            if self.is_ignored_function(eng, bo) {
                return;
            }
        }
        if self.is_builtin_no_return(cx, value) {
            cx.emit_node("E1111", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                "Assigning result of a function call, where the function has no return".into());
            return;
        }
        // function_node.root().fully_defined(): real .py file.
        // PartialFunction.root() walks its SYNTHETIC parent (the partial
        // call's parent, brain_functools.py:119) — the module of the
        // partial() assignment site, NOT the wrapped function's module
        // (pip urllib3/util/wait.py:53 partial(select.select, ...)).
        {
            let root_node = match accessor {
                Some((w, _)) => w,
                None => match &function_node {
                    Some(Value::Partial { parent: Some(p), .. }) => *p,
                    _ => fnode,
                },
            };
            let root = eng.md(root_node.m);
            if root.file == "<snapshot>" || !root.file.ends_with(".py") {
                return;
            }
        }
        // Return nodes inside, skipping nested FunctionDefs
        let returns = match body_owner {
            Some(bo) => nodes_of_class_skip(eng, bo, |k| matches!(k, NodeKind::Return { .. }),
                |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))),
            None => Vec::new(),
        };
        if returns.is_empty() {
            cx.emit_node("E1111", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                "Assigning result of a function call, where the function has no return".into());
        } else {
            let all_none = returns.iter().all(|&r| {
                let md = eng.md(r.m);
                match &md.tree.nodes[r.n.idx()].kind {
                    NodeKind::Return { value: None } => true,
                    NodeKind::Return { value: Some(v) } => matches!(
                        &md.tree.nodes[v.idx()].kind,
                        NodeKind::Const(ConstValue::None)
                    ),
                    _ => false,
                }
            });
            if all_none {
                cx.emit_node("E1128", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                    "Assigning result of a function call, where the function returns None".into());
            }
        }
    }

    /// typecheck.py:1287-1296
    fn is_ignored_function(&self, eng: &Engine, fnode: GNode) -> bool {
        let is_async = eng.kind_is(fnode, |k| matches!(k, NodeKind::AsyncFunctionDef(_)));
        if is_async {
            return true;
        }
        // utils.is_error: body is exactly one Raise
        if function_body_is_single_raise(eng, fnode) {
            return true;
        }
        if eng.is_generator(fnode) {
            return true;
        }
        eng.is_abstract(fnode, false, false)
    }

    /// typecheck.py:1298-1307 (+ BUILTINS_IMPLICIT_RETURN_NONE table :77-98)
    fn is_builtin_no_return(&self, cx: &mut WalkCx, value: GNode) -> bool {
        let eng = cx.eng;
        let md = eng.md(value.m);
        let (expr, attr) = match &md.tree.nodes[value.n.idx()].kind {
            NodeKind::Call { func, .. } => {
                match &md.tree.nodes[func.idx()].kind {
                    NodeKind::Attribute { expr, attrname, .. } => {
                        (GNode { m: value.m, n: *expr }, md.tree.s(*attrname).to_string())
                    }
                    _ => return false,
                }
            }
            _ => return false,
        };
        let inferred = match u::safe_infer(eng, cx.caches, expr) {
            Some(v) if !v.is_uninferable() => v,
            _ => return false,
        };
        if !value_is_instance(eng, &inferred) {
            return false;
        }
        let pytype = u::value_pytype(eng, &inferred).unwrap_or_default();
        let methods: &[&str] = match pytype.as_str() {
            "builtins.dict" => &["clear", "update"],
            "builtins.list" => &["append", "clear", "extend", "insert", "remove", "reverse", "sort"],
            "builtins.set" => &[
                "add", "clear", "difference_update", "discard", "intersection_update",
                "remove", "symmetric_difference_update", "update",
            ],
            _ => return false,
        };
        methods.contains(&attr.as_str())
    }

    /// typecheck.py:2227-2240 — E1142
    pub fn visit_await(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let mut scope = eng.scope(node);
        loop {
            let md = eng.md(scope.m);
            match &md.tree.nodes[scope.n.idx()].kind {
                NodeKind::Module(_) => break,
                NodeKind::AsyncFunctionDef(_) => return,
                NodeKind::FunctionDef(_) | NodeKind::Lambda(_) => break,
                _ => {
                    let p = match eng.parent(scope) {
                        Some(p) => p,
                        None => break,
                    };
                    scope = eng.scope(p);
                }
            }
        }
        cx.emit_node("E1142", u::lineno(eng, node), u::col_offset(eng, node) as i64,
            "'await' should be used within an async function".into());
    }

    /// typecheck.py:1455-1673 — visit_call (E1102 + E112x family)
    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some((func, args, keywords)) = call_parts(eng, node) else { return };
        let called = safe_infer_cc(eng, cx.caches, func);
        self.check_not_callable(cx, node, func, &called);

        let Some(det) = determine_callable(eng, &called) else { return };

        // args source: PropertyFuncAccessor synths carry the WRAPPED
        // function's args (objectmodel.py:949/:998 postinit(args=func.args));
        // DescriptorBoundMethod appends a mandatory 'type' param
        // (objectmodel.py:416-459)
        let spec_node = match eng.prop_accessors.borrow().get(&det.func) {
            Some(&(wrapped, _)) => wrapped,
            None => det.func,
        };
        let spec = eng.arg_spec(spec_node);
        let mut spec = match spec {
            Some(s) if !s.args_unknown => s,
            _ => {
                // builtins: no argument information (typecheck.py:1470-1475)
                if func_name(eng, det.func).as_deref() == Some("isinstance") {
                    self.check_isinstance_args(cx, node, &args, det.name);
                }
                return;
            }
        };
        // synthetic 'type' param name slot for __get__ calls
        let desc_type_param: Option<GSym> = if det.desc_get { Some(eng.sym("type")) } else { None };
        if det.desc_get {
            // defaults=[], kwonly=[] in the synthetic Arguments
            spec.defaults = Vec::new();
            spec.kwonlyargs = Vec::new();
            spec.kw_defaults = Vec::new();
            spec.vararg = None;
            spec.kwarg = None;
        }

        // duplicate parameter names -> bail (typecheck.py:1477-1480)
        {
            let mut names: Vec<GSym> = spec
                .arguments()
                .iter()
                .filter_map(|&a| eng.assign_name_of(a))
                .collect();
            if let Some(t) = desc_type_param {
                names.push(t);
            }
            let set: std::collections::HashSet<GSym> = names.iter().copied().collect();
            if names.len() != set.len() {
                return;
            }
        }

        // CallSite.from_call(node) (typecheck.py:1482)
        let cc = CallCtx {
            id: eng.next_callctx_id(),
            args: RefCell::new(args.iter().map(|&a| NV::N(a)).collect()),
            keywords: RefCell::new(keywords.iter().map(|&k| {
                let (name, value) = keyword_parts(eng, k);
                (name, value)
            }).collect()),
            callee: RefCell::new(None),
        };
        let site = eng.call_site_from(&cc, &Ctx::new());

        // E1132 repeated-keyword (emitted BEFORE the invalid bail)
        for kw in &site.duplicated_keywords {
            let kws = eng.sname(*kw);
            cx.emit_node("E1132", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                u::format_template("Got multiple values for keyword argument %r in function call", &[&kws]));
        }

        if site.has_invalid_arguments() || site.has_invalid_keywords() {
            return;
        }

        // signature_mutators default [] — decorated_with still burns
        // decorator inference (typecheck.py:1494-1498)
        if decorated_with(eng, det.func, &[]) {
            return;
        }

        let mut num_positional_args = site.positional_arguments().len();
        let mut keyword_args: Vec<GSym> =
            site.keyword_arguments().iter().map(|(k, _)| *k).collect();
        let overload_function = is_overload_stub(cx.caches, eng, det.func);

        // no-context variadics (typecheck.py:1505-1517)
        let node_scope = eng.scope(node);
        let (no_ctx_pos_variadic, no_ctx_kw_variadic) =
            if is_funcdef(eng, node_scope) || is_lambda(eng, node_scope) {
                (
                    no_context_variadic_positional(eng, cx.caches, node, node_scope),
                    no_context_variadic_keywords(eng, cx.caches, node, node_scope),
                )
            } else {
                (false, false)
            };

        // functools.partial filled args (typecheck.py:1519-1524)
        let (already_filled_positionals, already_filled_keywords): (usize, Vec<GSym>) =
            match &det.value {
                Value::Partial { filled_args, filled_keywords, .. } => (
                    filled_args.len(),
                    filled_keywords.iter().map(|(k, _)| *k).collect(),
                ),
                _ => (0, Vec::new()),
            };
        keyword_args.extend(already_filled_keywords.iter().copied());
        num_positional_args += det.implicit + already_filled_positionals;

        // class-attribute assignment decrement (typecheck.py:1526-1536)
        if num_positional_args > 0 {
            let frame = eng.frame(node);
            if is_classdef(eng, frame) {
                if let Value::Node(g) = &det.value {
                    if is_funcdef(eng, *g)
                        && eng.parent(*g) == Some(frame)
                        && !eng
                            .decoratornames(*g, None)
                            .iter()
                            .any(|q| q.as_deref() == Some("builtins.staticmethod"))
                    {
                        num_positional_args -= 1;
                    }
                }
            }
        }

        // formal parameter model (typecheck.py:1538-1561)
        let mut pos_names: Vec<Option<GSym>> = spec
            .posonlyargs
            .iter()
            .chain(spec.args.iter())
            .map(|&a| eng.assign_name_of(a))
            .collect();
        if let Some(t) = desc_type_param {
            pos_names.push(Some(t));
        }
        let num_mandatory = pos_names.len().saturating_sub(spec.defaults.len());
        // (name, has_default), assigned
        let mut parameters: Vec<((Option<GSym>, bool), bool)> = Vec::new();
        let mut param_index: FxHashMap<GSym, usize> = FxHashMap::default();
        for (i, &name) in pos_names.iter().enumerate() {
            if let Some(n) = name {
                param_index.insert(n, i);
            }
            let has_def = i >= num_mandatory;
            parameters.push(((name, has_def), false));
        }
        // kwonly params: name -> (has_default, assigned), in decl order
        let mut kwparams: Vec<(GSym, bool, bool)> = Vec::new();
        for (i, &arg) in spec.kwonlyargs.iter().enumerate() {
            if let Some(n) = eng.assign_name_of(arg) {
                let has_def = spec.kw_defaults.get(i).map(|d| d.is_some()).unwrap_or(false);
                kwparams.push((n, has_def, false));
            }
        }

        // 1. positional matching -> E1121 (typecheck.py:1565-1580)
        for i in 0..num_positional_args {
            if i < parameters.len() {
                parameters[i].1 = true;
            } else if spec.vararg.is_some() {
                break;
            } else if !overload_function {
                cx.emit_node("E1121", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                    u::format_template("Too many positional arguments for %s call", &[det.name]));
                break;
            } else {
                break;
            }
        }

        // 2. keyword matching (typecheck.py:1582-1635)
        let posonly_names: Vec<GSym> = spec
            .posonlyargs
            .iter()
            .filter_map(|&a| eng.assign_name_of(a))
            .collect();
        let called_qname = eng.value_qname(&det.value).unwrap_or_default();
        for &keyword in &keyword_args {
            if spec.kwarg.is_some() && posonly_names.contains(&keyword) {
                // W1117 kwarg-superseded-by-positional-arg: disabled, but
                // the `continue` consumes the keyword
                continue;
            }
            if let Some(&i) = param_index.get(&keyword) {
                if parameters[i].1 {
                    let kws = eng.sname(keyword);
                    if !(kws == "self" && STR_FORMAT.contains(&called_qname.as_str())) {
                        cx.emit_node("E1124", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                            u::format_template("Argument %r passed by position and keyword in %s call", &[&kws, det.name]));
                    }
                } else {
                    parameters[i].1 = true;
                }
            } else if let Some(kp) = kwparams.iter_mut().find(|(n, _, _)| *n == keyword) {
                if kp.2 {
                    let kws = eng.sname(keyword);
                    cx.emit_node("E1124", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                        u::format_template("Argument %r passed by position and keyword in %s call", &[&kws, det.name]));
                } else {
                    kp.2 = true;
                }
            } else if spec.kwarg.is_some() {
                // assigned to **kwargs
            } else if matches!(&det.value, Value::Node(g) if is_funcdef(eng, *g))
                && self.keyword_in_all_decorator_returns(cx, det.func, keyword)
            {
                // consumed by decorator
            } else if !overload_function {
                let kws = eng.sname(keyword);
                cx.emit_node("E1123", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                    u::format_template("Unexpected keyword argument %r in %s call", &[&kws, det.name]));
            }
        }

        // 3. **kwargs at the call site (typecheck.py:1637-1646)
        let has_kwargs_unpack = keywords.iter().any(|&k| keyword_parts(eng, k).0.is_none());
        if has_kwargs_unpack {
            for p in parameters.iter_mut() {
                if p.0 .0.is_some() {
                    p.1 = true;
                }
            }
        }

        // E1120 emissions (typecheck.py:1648-1663)
        for ((name, has_def), assigned) in &parameters {
            if !*has_def && !*assigned {
                let display = match name {
                    Some(n) => u::py_repr_str(&eng.sname(*n)),
                    None => "<tuple>".to_string(),
                };
                if !no_ctx_pos_variadic && !overload_function {
                    cx.emit_node("E1120", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                        u::format_template("No value for argument %s in %s call", &[&display, det.name]));
                }
            }
        }

        // E1125 emissions (typecheck.py:1665-1673)
        for (name, has_def, assigned) in &kwparams {
            if !*has_def && !*assigned && !no_ctx_kw_variadic && !overload_function {
                let ns = eng.sname(*name);
                cx.emit_node("E1125", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                    u::format_template("Missing mandatory keyword argument %r in %s call", &[&ns, det.name]));
            }
        }
    }

    /// typecheck.py:1423-1452
    fn check_isinstance_args(&mut self, cx: &mut WalkCx, node: GNode, args: &[GNode], callable_name: &str) {
        let eng = cx.eng;
        if args.len() > 2 {
            cx.emit_node("E1121", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                u::format_template("Too many positional arguments for %s call", &[callable_name]));
        } else if args.len() < 2 {
            let parameters = ["'_obj'", "'__class_or_tuple'"];
            for p in &parameters[args.len()..] {
                cx.emit_node("E1120", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                    u::format_template("No value for argument %s in %s call", &[p, callable_name]));
            }
        } else {
            // W1116 disabled; burn its safe_infer chain for cache parity
            burn_invalid_isinstance_type(eng, cx.caches, args[1]);
        }
    }

    /// typecheck.py:1675-1713
    fn keyword_in_all_decorator_returns(&self, cx: &mut WalkCx, func: GNode, keyword: GSym) -> bool {
        let eng = cx.eng;
        let decs = decorator_nodes(eng, func);
        if decs.is_empty() {
            return false;
        }
        for dec in decs {
            let inferred = u::safe_infer(eng, cx.caches, dec);
            let inferred = match inferred {
                None => return true,
                Some(Value::Uninferable) => return true,
                Some(v) => v,
            };
            let fnode = match &inferred {
                Value::Node(g) if is_funcdef(eng, *g) => *g,
                _ => return false,
            };
            // inferred.infer_call_result(caller=None)
            let flow = eng.function_infer_call_result(fnode, None, None);
            if flow.err.is_some() && flow.vals.is_empty() {
                return false;
            }
            for rv in &flow.vals {
                let rg = match rv {
                    Value::Node(g) if is_funcdef(eng, *g) => *g,
                    _ => return false,
                };
                let Some(rspec) = eng.arg_spec(rg) else { return false };
                if rspec.kwarg.is_some() {
                    continue;
                }
                // args.is_argument(keyword)
                let is_arg = rspec.vararg == Some(keyword)
                    || rspec.kwarg == Some(keyword)
                    || rspec
                        .arguments()
                        .iter()
                        .any(|&a| eng.assign_name_of(a) == Some(keyword));
                if is_arg {
                    continue;
                }
                return false;
            }
        }
        true
    }

    /// typecheck.py:1784-1813 — E1102
    fn check_not_callable(&mut self, cx: &mut WalkCx, node: GNode, func: GNode, called: &Option<Value>) {
        let eng = cx.eng;
        let falsy = match called {
            None => true,
            Some(Value::Uninferable) => true,
            _ => false,
        };
        if falsy || value_callable(eng, called.as_ref().unwrap()) {
            self.check_uninferable_call(cx, node, func);
            return;
        }
        let inferred = called.as_ref().unwrap();
        if !value_is_instance(eng, inferred) {
            let txt = u::as_string(eng, func);
            cx.emit_node("E1102", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                u::format_template("%s is not callable", &[&txt]));
            return;
        }
        if !value_has_known_bases(eng, cx.caches, inferred) {
            return;
        }
        // descriptor / NamedTuple skips (typecheck.py:1271-1278): both
        // proxy to the instance's class; scope() of a ClassDef is itself
        if let Value::Inst { cls, .. } | Value::ExcInst { cls, .. } = inferred {
            if eng.parent(*cls).is_some() {
                let get_sym = eng.sym("__get__");
                if !eng.class_locals_get(*cls, get_sym).is_empty() {
                    return;
                }
                if eng.qname(*cls) == "typing.NamedTuple" {
                    return;
                }
            }
        }
        let txt = u::as_string(eng, func);
        cx.emit_node("E1102", u::lineno(eng, node), u::col_offset(eng, node) as i64,
            u::format_template("%s is not callable", &[&txt]));
    }

    /// typecheck.py:1330-1375
    fn check_uninferable_call(&mut self, cx: &mut WalkCx, node: GNode, func: GNode) {
        let eng = cx.eng;
        let md = eng.md(func.m);
        let (expr, attrname) = match &md.tree.nodes[func.n.idx()].kind {
            NodeKind::Attribute { expr, attrname, .. } => {
                (GNode { m: func.m, n: *expr }, md.tree.s(*attrname).to_string())
            }
            _ => return,
        };
        let klass = u::safe_infer(eng, cx.caches, expr);
        let cls = match &klass {
            Some(Value::Inst { cls, .. }) | Some(Value::ExcInst { cls, .. }) => *cls,
            _ => return,
        };
        // klass._proxied.getattr(attrname): ClassDef.getattr default
        // class_context=True
        let sym = eng.sym(&attrname);
        let attrs = match eng.class_getattr(cls, sym, None, true) {
            Ok(a) => a,
            Err(_) => return,
        };
        for attr in &attrs {
            let ag = match attr {
                NV::N(g) if is_funcdef(eng, *g) => *g,
                _ => continue,
            };
            if !decorated_with_property(eng, cx.caches, ag) {
                continue;
            }
            // attr.infer_call_result(node)
            let flow = eng.function_infer_call_result(ag, Some(node), None);
            if flow.err.is_some() && flow.vals.is_empty() {
                continue;
            }
            if flow.err.is_some() {
                continue;
            }
            let rets = &flow.vals;
            if rets.iter().all(|v| v.is_uninferable()) {
                continue;
            }
            if rets.iter().any(|v| !v.is_uninferable() && value_callable(eng, v)) {
                continue;
            }
            let txt = u::as_string(eng, func);
            cx.emit_node("E1102", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                u::format_template("%s is not callable", &[&txt]));
        }
    }
}

// ---------------------------------------------------------------------------
// decorated_with_property (utils.py:805-867)
// ---------------------------------------------------------------------------

pub fn decorated_with_property(eng: &Engine, caches: &u::LintCaches, func: GNode) -> bool {
    for dec in decorator_nodes(eng, func) {
        if is_property_decorator(eng, caches, dec) {
            return true;
        }
    }
    false
}

fn is_property_decorator(eng: &Engine, caches: &u::LintCaches, decorator: GNode) -> bool {
    let flow = eng.infer(decorator, &Ctx::new());
    for v in &flow.vals {
        match v {
            Value::Node(g) if is_classdef(eng, *g) => {
                let qn = eng.qname(*g);
                if qn == "builtins.property" || qn == "functools.cached_property" {
                    return true;
                }
                for anc in eng.ancestors(*g, true, None) {
                    if eng.node_name(anc).as_deref() == Some("property")
                        && eng.md(anc.m).name == "builtins"
                    {
                        return true;
                    }
                }
            }
            Value::Node(g) if is_funcdef(eng, *g) => {
                // exactly one return of a Name/Attribute inferring to a
                // Property wrapping a FunctionDef
                let returns = nodes_of_class_skip(
                    eng,
                    *g,
                    |k| matches!(k, NodeKind::Return { .. }),
                    |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::Lambda(_)),
                );
                if returns.len() == 1 {
                    let md = eng.md(returns[0].m);
                    let rv = match &md.tree.nodes[returns[0].n.idx()].kind {
                        NodeKind::Return { value: Some(v) } => {
                            let vg = GNode { m: returns[0].m, n: *v };
                            if eng.kind_is(vg, |k| matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. })) {
                                Some(vg)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(vg) = rv {
                        if let Some(Value::Property { func: pf, .. }) =
                            u::safe_infer(eng, caches, vg)
                        {
                            if is_funcdef(eng, pf) {
                                return decorated_with_property(eng, caches, pf);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}

// ---------------------------------------------------------------------------
// no-context variadic machinery (typecheck.py:662-746)
// ---------------------------------------------------------------------------

fn scope_arg_names(eng: &Engine, scope: GNode) -> (Option<GSym>, Option<GSym>) {
    match eng.arg_spec(scope) {
        Some(s) => (s.vararg, s.kwarg),
        None => (None, None),
    }
}

fn no_context_variadic_positional(
    eng: &Engine,
    caches: &u::LintCaches,
    node: GNode,
    scope: GNode,
) -> bool {
    // variadics = node.starargs + node.kwargs
    let Some((_, args, keywords)) = call_parts(eng, node) else { return false };
    let mut variadics: Vec<GNode> = Vec::new();
    for &a in &args {
        if eng.kind_is(a, |k| matches!(k, NodeKind::Starred { .. })) {
            variadics.push(a);
        }
    }
    for &k in &keywords {
        let (name, _) = keyword_parts(eng, k);
        if name.is_none() {
            variadics.push(k);
        }
    }
    let (vararg, _) = scope_arg_names(eng, scope);
    no_context_variadic(eng, caches, node, vararg, VariadicKind::Starred, &variadics)
}

fn no_context_variadic_keywords(
    eng: &Engine,
    caches: &u::LintCaches,
    node: GNode,
    scope: GNode,
) -> bool {
    let statement = eng.statement(node);
    let mut variadics: Vec<GNode> = Vec::new();
    let scope_is_pure_lambda = is_lambda(eng, scope);
    let stmt_is_with = statement
        .map(|s| eng.kind_is(s, |k| matches!(k, NodeKind::With { .. } | NodeKind::AsyncWith { .. })))
        .unwrap_or(false);
    if scope_is_pure_lambda || stmt_is_with {
        if let Some((_, _, keywords)) = call_parts(eng, node) {
            // list(node.keywords or []) + node.kwargs — kwargs ⊂ keywords,
            // so ** entries appear twice (bug-for-bug harmless: any())
            variadics.extend(keywords.iter().copied());
            for &k in &keywords {
                if keyword_parts(eng, k).0.is_none() {
                    variadics.push(k);
                }
            }
        }
    } else if let Some(stmt) = statement {
        let md = eng.md(stmt.m);
        let value = match &md.tree.nodes[stmt.n.idx()].kind {
            NodeKind::Return { value: Some(v) } => Some(*v),
            NodeKind::Expr { value } => Some(*value),
            NodeKind::Assign { value, .. } => Some(*value),
            _ => None,
        };
        if let Some(v) = value {
            let vg = GNode { m: stmt.m, n: v };
            if let Some((_, _, keywords)) = call_parts(eng, vg) {
                variadics.extend(keywords.iter().copied());
                for &k in &keywords {
                    if keyword_parts(eng, k).0.is_none() {
                        variadics.push(k);
                    }
                }
            }
        }
    }
    let (_, kwarg) = scope_arg_names(eng, scope);
    no_context_variadic(eng, caches, node, kwarg, VariadicKind::Keyword, &variadics)
}

#[derive(Clone, Copy, PartialEq)]
enum VariadicKind {
    Starred,
    Keyword,
}

/// typecheck.py:674-746 _no_context_variadic
fn no_context_variadic(
    eng: &Engine,
    caches: &u::LintCaches,
    node: GNode,
    variadic_name: Option<GSym>,
    variadic_type: VariadicKind,
    variadics: &[GNode],
) -> bool {
    let Some(variadic_name) = variadic_name else { return false };
    let scope = eng.scope(node);
    let is_in_lambda_scope = is_lambda(eng, scope);
    let Some(statement) = eng.statement(node) else { return false };
    for name_node in u::preorder(eng, statement) {
        let Some(nsym) = ({
            let md = eng.md(name_node.m);
            match &md.tree.nodes[name_node.n.idx()].kind {
                NodeKind::Name { name } => Some(eng.g(&md, *name)),
                _ => None,
            }
        }) else { continue };
        if nsym != variadic_name {
            continue;
        }
        let inferred = u::safe_infer(eng, caches, name_node);
        // length + "statement is Lambda/FunctionDef" determination
        let empty_param_repr: bool = match &inferred {
            Some(Value::Node(g)) => {
                let md = eng.md(g.m);
                let len = match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } => elts.len(),
                    NodeKind::Dict { items } => items.len(),
                    _ => continue,
                };
                if len != 0 {
                    false
                } else {
                    // inferred.statement() / lambda-Arguments special
                    let parent = eng.parent(*g);
                    let stmt = if is_in_lambda_scope
                        && parent
                            .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::Arguments(_))))
                            .unwrap_or(false)
                    {
                        parent.and_then(|p| eng.parent(p))
                    } else {
                        eng.statement(*g)
                    };
                    stmt.map(|s| {
                        eng.kind_is(s, |k| {
                            matches!(
                                k,
                                NodeKind::Lambda(_)
                                    | NodeKind::FunctionDef(_)
                                    | NodeKind::AsyncFunctionDef(_)
                            )
                        })
                    })
                    .unwrap_or(false)
                }
            }
            // parameter-representation synthetic values: astroid builds
            // real Tuple/Dict nodes parented to the Arguments node ->
            // statement() is the FunctionDef
            Some(Value::SynthSeq { elems, .. }) => elems.is_empty(),
            Some(Value::SynthDict { items }) => items.is_empty(),
            _ => continue,
        };
        if !empty_param_repr {
            continue;
        }
        // is_in_starred_context: walk node.parent up to the statement
        let mut is_in_starred_context = false;
        {
            let mut cur = node;
            while let Some(p) = eng.parent(cur) {
                if p == statement {
                    break;
                }
                let hit = match variadic_type {
                    VariadicKind::Starred => {
                        eng.kind_is(p, |k| matches!(k, NodeKind::Starred { .. }))
                    }
                    VariadicKind::Keyword => {
                        eng.kind_is(p, |k| matches!(k, NodeKind::Keyword { .. }))
                    }
                };
                if hit {
                    is_in_starred_context = true;
                    break;
                }
                cur = p;
            }
        }
        // used_as_starred_argument
        let mut used_as_starred = false;
        for &v in variadics {
            let md = eng.md(v.m);
            let value = match &md.tree.nodes[v.n.idx()].kind {
                NodeKind::Starred { value, .. } => Some(GNode { m: v.m, n: *value }),
                NodeKind::Keyword { value, .. } => Some(GNode { m: v.m, n: *value }),
                _ => None,
            };
            if let Some(vv) = value {
                if vv == name_node || is_ancestor_of(eng, vv, name_node) {
                    used_as_starred = true;
                    break;
                }
            }
        }
        if is_in_starred_context || used_as_starred {
            return true;
        }
    }
    false
}

fn is_ancestor_of(eng: &Engine, anc: GNode, node: GNode) -> bool {
    let mut cur = node;
    while let Some(p) = eng.parent(cur) {
        if p == anc {
            return true;
        }
        cur = p;
    }
    false
}

/// utils.is_error: function body is exactly one Raise statement
fn function_body_is_single_raise(eng: &Engine, func: GNode) -> bool {
    let md = eng.md(func.m);
    match &md.tree.nodes[func.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
            d.body.len() == 1
                && matches!(md.tree.nodes[d.body[0].idx()].kind, NodeKind::Raise { .. })
        }
        _ => false,
    }
}

/// node.nodes_of_class(target, skip_klass=skip): preorder, skip subtrees of
/// skip kind (the root itself is never skipped).
pub fn nodes_of_class_skip<FT, FS>(eng: &Engine, root: GNode, target: FT, skip: FS) -> Vec<GNode>
where
    FT: Fn(&NodeKind) -> bool,
    FS: Fn(&NodeKind) -> bool,
{
    let mut out = Vec::new();
    fn rec<FT: Fn(&NodeKind) -> bool, FS: Fn(&NodeKind) -> bool>(
        eng: &Engine,
        g: GNode,
        target: &FT,
        skip: &FS,
        is_root: bool,
        out: &mut Vec<GNode>,
    ) {
        let md = eng.md(g.m);
        let kind = &md.tree.nodes[g.n.idx()].kind;
        if !is_root {
            if target(kind) {
                out.push(g);
            }
            if skip(kind) {
                return;
            }
        }
        let children: Vec<pyast::NodeId> = md.tree.children(g.n);
        drop(md);
        for c in children {
            rec(eng, GNode { m: g.m, n: c }, target, skip, false, out);
        }
    }
    rec(eng, root, &target, &skip, true, &mut out);
    out
}

/// burn-only port of _is_invalid_isinstance_type (typecheck.py:806-828):
/// W1116 is disabled; only the safe_infer pulls matter for cache parity.
fn burn_invalid_isinstance_type(eng: &Engine, caches: &u::LintCaches, arg: GNode) {
    let md = eng.md(arg.m);
    if let NodeKind::BinOp { op, left, right } = &md.tree.nodes[arg.n.idx()].kind {
        if &**op == "|" {
            let (l, r) = (GNode { m: arg.m, n: *left }, GNode { m: arg.m, n: *right });
            drop(md);
            // any() short-circuit: left first; `_is_invalid... and not
            // is_none` — is_none is syntactic, no burn
            burn_invalid_isinstance_type(eng, caches, l);
            burn_invalid_isinstance_type(eng, caches, r);
            return;
        }
    }
    let inferred = u::safe_infer(eng, caches, arg);
    if let Some(Value::Node(g)) = &inferred {
        let md = eng.md(g.m);
        if let NodeKind::Tuple { elts, .. } = &md.tree.nodes[g.n.idx()].kind {
            let elts: Vec<GNode> = elts.iter().map(|&e| GNode { m: g.m, n: e }).collect();
            drop(md);
            for e in elts {
                burn_invalid_isinstance_type(eng, caches, e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase B: protocol machinery (utils.py:1189-1344) + the protocol checks
// ---------------------------------------------------------------------------

/// value.getattr(name) dispatch (NO inference): ClassDef.getattr /
/// BaseInstance.getattr / Module.getattr surfaces.
pub fn value_getattr(eng: &Engine, v: &Value, name: GSym) -> Result<Vec<NV>, ()> {
    match v {
        Value::Node(g) if is_classdef(eng, *g) => {
            eng.class_getattr(*g, name, None, true).map_err(|_| ())
        }
        Value::Node(g) if eng.kind_is(*g, |k| matches!(k, NodeKind::Module(_))) => {
            eng.module_getattr(g.m, name, false).map_err(|_| ())
        }
        _ => eng.instance_getattr(v, name, None, true).map_err(|_| ()),
    }
}

/// utils._supports_protocol_method (utils.py:1189-1210)
fn supports_protocol_method(eng: &Engine, v: &Value, name: GSym) -> bool {
    let attrs = match value_getattr(eng, v, name) {
        Ok(a) if !a.is_empty() => a,
        _ => return false,
    };
    let first = &attrs[0];
    if let NV::N(g) = first {
        if eng.kind_is(*g, |k| matches!(k, NodeKind::AssignName { .. })) {
            // enclosing Assign/NamedExpr assigning a Const or all-Const
            // container -> protocol NOT supported
            let assign_parent = u::first_ancestor(eng, *g, |k| {
                matches!(k, NodeKind::Assign { .. } | NodeKind::NamedExpr { .. })
            });
            let Some(ap) = assign_parent else { return true };
            let md = eng.md(ap.m);
            let value = match &md.tree.nodes[ap.n.idx()].kind {
                NodeKind::Assign { value, .. } | NodeKind::NamedExpr { value, .. } => *value,
                _ => return true,
            };
            match &md.tree.nodes[value.idx()].kind {
                NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. } | NodeKind::Set { elts } => {
                    if elts.iter().all(|&e| {
                        matches!(md.tree.nodes[e.idx()].kind, NodeKind::Const(_))
                    }) {
                        return false;
                    }
                }
                NodeKind::Const(_) => return false,
                _ => {}
            }
        }
    }
    true
}

#[derive(Clone, Copy, PartialEq)]
pub enum Protocol {
    Iteration,       // __iter__ OR __getitem__
    AsyncIteration,  // __aiter__
    Mapping,         // __getitem__ AND keys
    Membership,      // __contains__
    GetItem,
    SetItem,
    DelItem,
}

fn protocol_callback(eng: &Engine, v: &Value, p: Protocol) -> bool {
    let s = |n: &str| eng.sym(n);
    match p {
        Protocol::Iteration => {
            supports_protocol_method(eng, v, s("__iter__"))
                || supports_protocol_method(eng, v, s("__getitem__"))
        }
        Protocol::AsyncIteration => supports_protocol_method(eng, v, s("__aiter__")),
        Protocol::Mapping => {
            supports_protocol_method(eng, v, s("__getitem__"))
                && supports_protocol_method(eng, v, s("keys"))
        }
        Protocol::Membership => supports_protocol_method(eng, v, s("__contains__")),
        Protocol::GetItem => supports_protocol_method(eng, v, s("__getitem__")),
        Protocol::SetItem => supports_protocol_method(eng, v, s("__setitem__")),
        Protocol::DelItem => supports_protocol_method(eng, v, s("__delitem__")),
    }
}

/// utils._supports_protocol (utils.py:1275-1301)
fn supports_protocol(eng: &Engine, caches: &u::LintCaches, v: &Value, p: Protocol) -> bool {
    match v {
        Value::Node(g) if is_classdef(eng, *g) => {
            if !has_known_bases(eng, caches, *g) {
                return true;
            }
            // class objects: protocol looked up on the metaclass
            if let Some(meta) = eng.metaclass(*g, None) {
                if protocol_callback(eng, &meta, p) {
                    return true;
                }
            }
            false
        }
        // ComprehensionScope inference results
        Value::Node(g)
            if eng.kind_is(*g, |k| {
                matches!(
                    k,
                    NodeKind::ListComp(_)
                        | NodeKind::SetComp(_)
                        | NodeKind::DictComp(_)
                        | NodeKind::GeneratorExp(_)
                )
            }) =>
        {
            true
        }
        // dict views are bases.Proxy of the dict INSTANCE (objects.py):
        // `case bases.Proxy(_proxied=BaseInstance() as p)` -> callback(dict)
        Value::DictItems(_) | Value::DictKeys(_) | Value::DictValues(_) => {
            let dict_inst = Value::SynthDict { items: std::rc::Rc::new(Vec::new()) };
            protocol_callback(eng, &dict_inst, p)
        }
        // BaseInstance arm: Inst/ExcInst + literals + synths + Generator +
        // UnionType
        _ if value_is_base_instance(eng, v) => {
            if !value_has_known_bases(eng, caches, v) {
                return true;
            }
            if let Some(cls) = eng.proxied_class(v) {
                if eng.has_dynamic_getattr(cls, &Ctx::new()) {
                    return true;
                }
            }
            protocol_callback(eng, v, p)
        }
        _ => false,
    }
}

fn value_is_base_instance(eng: &Engine, v: &Value) -> bool {
    match v {
        Value::Inst { .. }
        | Value::ExcInst { .. }
        | Value::SynthConst(_)
        | Value::SynthSeq { .. }
        | Value::SynthDict { .. }
        | Value::SynthSlice { .. }
        | Value::FrozenSet { .. }
        | Value::Generator { .. }
        | Value::UnionType => true,
        Value::Node(g) => {
            let md = eng.md(g.m);
            matches!(
                md.tree.nodes[g.n.idx()].kind,
                NodeKind::Const(_)
                    | NodeKind::List { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::Set { .. }
                    | NodeKind::Dict { .. }
                    | NodeKind::Slice { .. }
            )
        }
        _ => false,
    }
}

/// utils.is_iterable / is_mapping / supports_membership_test /
/// supports_getitem / supports_setitem / supports_delitem
pub fn is_iterable(eng: &Engine, caches: &u::LintCaches, v: &Value, check_async: bool) -> bool {
    supports_protocol(
        eng,
        caches,
        v,
        if check_async { Protocol::AsyncIteration } else { Protocol::Iteration },
    )
}

pub fn is_mapping(eng: &Engine, caches: &u::LintCaches, v: &Value) -> bool {
    supports_protocol(eng, caches, v, Protocol::Mapping)
}

pub fn supports_membership_test(eng: &Engine, caches: &u::LintCaches, v: &Value) -> bool {
    supports_protocol(eng, caches, v, Protocol::Membership)
        || is_iterable(eng, caches, v, false)
}

// ---------------------------------------------------------------------------
// is_inside_abstract_class (utils.py:1162-1272)
// ---------------------------------------------------------------------------

fn is_abstract_class_name(name: &str) -> bool {
    let lname = name.to_lowercase();
    lname.ends_with("mixin") || lname.starts_with("abstract")
        || lname.starts_with("base") || lname.ends_with("base")
}

/// utils.is_protocol_class (utils.py:1677-1697)
fn is_protocol_class(eng: &Engine, cls: GNode) -> bool {
    const NAMES: &[&str] = &["typing.Protocol", "typing_extensions.Protocol", ".Protocol"];
    if NAMES.contains(&eng.qname(cls).as_str()) {
        return true;
    }
    for base in eng.class_bases(cls) {
        let flow = eng.infer(base, &Ctx::new());
        if flow.vals.iter().any(|v| {
            eng.value_qname(v)
                .map(|q| NAMES.contains(&q.as_str()))
                .unwrap_or(false)
        }) {
            return true;
        }
    }
    false
}

/// utils.class_is_abstract @lru_cache(1024) (utils.py:1162-1186)
pub fn class_is_abstract(caches: &u::LintCaches, eng: &Engine, cls: GNode) -> bool {
    if let Some(&v) = caches.class_abstract.borrow().get(&cls) {
        return v;
    }
    let v = class_is_abstract_uncached(eng, cls);
    caches.class_abstract.borrow_mut().insert(cls, v);
    v
}

fn class_is_abstract_uncached(eng: &Engine, cls: GNode) -> bool {
    if is_protocol_class(eng, cls) {
        return true;
    }
    if let Some(Value::Node(meta)) = eng.declared_metaclass(cls, None) {
        if eng.node_name(meta).as_deref() == Some("ABCMeta") {
            let root = eng.md(meta.m).name.clone();
            if root == "abc" || root == "_py_abc" {
                return true;
            }
        }
    }
    for anc in eng.ancestors(cls, true, None) {
        if eng.node_name(anc).as_deref() == Some("ABC") {
            let root = eng.md(anc.m).name.clone();
            if root == "abc" || root == "_py_abc" {
                return true;
            }
        }
    }
    // node.methods(): own + ancestor FunctionDef locals; filtered to own
    // frame -> own class-body FunctionDefs
    let md = eng.md(cls.m);
    let locals = md.locals.borrow();
    let mut own_methods: Vec<GNode> = Vec::new();
    if let Some(map) = locals.get(&cls.n) {
        for (_, vals) in map.iter() {
            for &g in vals {
                if is_funcdef(eng, g) {
                    own_methods.push(g);
                }
            }
        }
    }
    drop(locals);
    drop(md);
    for m in own_methods {
        if eng.is_abstract(m, false, false) {
            return true;
        }
    }
    false
}

/// utils.is_inside_abstract_class (utils.py:1255-1272): self + ancestors
pub fn is_inside_abstract_class(caches: &u::LintCaches, eng: &Engine, node: GNode) -> bool {
    let mut cur = Some(node);
    while let Some(g) = cur {
        if is_classdef(eng, g) {
            if class_is_abstract(caches, eng, g) {
                return true;
            }
            if let Some(name) = eng.node_name(g) {
                if is_abstract_class_name(&name) {
                    return true;
                }
            }
        }
        cur = eng.parent(g);
    }
    false
}

// ---------------------------------------------------------------------------
// is_hashable (utils.py:2079-2098)
// ---------------------------------------------------------------------------

pub fn is_hashable(eng: &Engine, node: GNode) -> bool {
    let flow = eng.infer(node, &Ctx::new());
    if flow.vals.is_empty() {
        return true; // InferenceError -> True
    }
    let hash_sym = eng.sym("__hash__");
    for v in &flow.vals {
        match v {
            Value::Uninferable => return true,
            Value::Node(g) if is_classdef(eng, *g) => return true,
            // objects without igetattr: Lambda (and plain nodes)
            Value::Node(g) if is_lambda(eng, *g) => return true,
            _ => {}
        }
        // next(inferred.igetattr("__hash__"))
        match eng.igetattr_first(v, hash_sym, None) {
            Ok(Some(hash_fn)) => {
                // `getattr(hash_fn, "value", True) is not None`
                let is_const_none = match &hash_fn {
                    Value::Node(g) => {
                        let md = eng.md(g.m);
                        matches!(md.tree.nodes[g.n.idx()].kind, NodeKind::Const(ConstValue::None))
                    }
                    Value::SynthConst(c) => matches!(c.as_ref(), ConstValue::None),
                    _ => false,
                };
                if !is_const_none {
                    return true;
                }
            }
            Ok(None) => return true,  // StopIteration crash-path: be safe
            Err(_) => return true,    // InferenceError -> True
        }
    }
    // error raised while pulling further values -> True
    if flow.err.is_some() {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// dunder_lookup (astroid/interpreter/dunder_lookup.py)
// ---------------------------------------------------------------------------

/// Returns Some(first found node) or None for AttributeInferenceError.
fn dunder_lookup_first(eng: &Engine, v: &Value, name: GSym) -> Option<GNode> {
    // literal nodes: _builtin_lookup = proxied class OWN locals only
    let literal_cls: Option<GNode> = match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::List { .. }
                | NodeKind::Tuple { .. }
                | NodeKind::Const(_)
                | NodeKind::Dict { .. }
                | NodeKind::Set { .. } => eng.proxied_class(v),
                _ => None,
            }
        }
        Value::SynthConst(_) | Value::SynthSeq { .. } | Value::SynthDict { .. } => {
            eng.proxied_class(v)
        }
        _ => None,
    };
    if let Some(cls) = literal_cls {
        let vals = eng.class_locals_get(cls, name);
        return vals.first().copied();
    }
    match v {
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
            lookup_in_mro_first(eng, *cls, name)
        }
        Value::FrozenSet { .. } | Value::SynthSlice { .. } => {
            let cls = eng.proxied_class(v)?;
            lookup_in_mro_first(eng, cls, name)
        }
        Value::Node(g) if is_classdef(eng, *g) => {
            let meta = eng.metaclass(*g, None)?;
            let meta_cls = match meta {
                Value::Node(mc) if is_classdef(eng, mc) => mc,
                _ => return None,
            };
            lookup_in_mro_first(eng, meta_cls, name)
        }
        _ => None,
    }
}

fn lookup_in_mro_first(eng: &Engine, cls: GNode, name: GSym) -> Option<GNode> {
    let own = eng.class_locals_get(cls, name);
    if let Some(&f) = own.first() {
        return Some(f);
    }
    for anc in eng.ancestors(cls, true, None) {
        let vals = eng.class_locals_get(anc, name);
        if let Some(&f) = vals.first() {
            return Some(f);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Phase B: TypeChecker visitors
// ---------------------------------------------------------------------------

impl TypeCk {
    /// typecheck.py:2094-2114 — E1135
    pub fn visit_compare(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let (op, right) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Compare { ops, .. } if ops.len() == 1 => {
                (ops[0].0.to_string(), GNode { m: node.m, n: ops[0].1 })
            }
            _ => return,
        };
        drop(md);
        if op != "in" && op != "not in" {
            return;
        }
        // _check_membership_test(right)
        if is_inside_abstract_class(cx.caches, eng, right) {
            return;
        }
        if eng.kind_is(right, |k| {
            matches!(
                k,
                NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_)
                    | NodeKind::GeneratorExp(_)
            )
        }) {
            return;
        }
        let inferred = match u::safe_infer(eng, cx.caches, right) {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        if !supports_membership_test(eng, cx.caches, &inferred) {
            let txt = pyinfer::asstr::as_string(eng, right);
            cx.emit_node("E1135", u::lineno(eng, right), u::col_offset(eng, right) as i64,
                u::format_template("Value '%s' doesn't support membership test", &[&txt]));
        }
    }

    /// typecheck.py:2116-2136 — E1143 dict keys / set members
    pub fn visit_dict(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let keys: Vec<GNode> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Dict { items } => {
                items.iter().map(|(k, _)| GNode { m: node.m, n: *k }).collect()
            }
            _ => return,
        };
        drop(md);
        for k in keys {
            if !is_hashable(eng, k) {
                let txt = pyinfer::asstr::as_string(eng, k);
                cx.emit_node("E1143", u::lineno(eng, k), u::col_offset(eng, k) as i64,
                    u::format_template("'%s' is unhashable and can't be used as a %s in a %s",
                        &[&txt, "key", "dict"]));
            }
        }
    }

    pub fn visit_set(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let elts: Vec<GNode> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Set { elts } => elts.iter().map(|&e| GNode { m: node.m, n: e }).collect(),
            _ => return,
        };
        drop(md);
        for e in elts {
            if !is_hashable(eng, e) {
                let txt = pyinfer::asstr::as_string(eng, e);
                cx.emit_node("E1143", u::lineno(eng, e), u::col_offset(eng, e) as i64,
                    u::format_template("'%s' is unhashable and can't be used as a %s in a %s",
                        &[&txt, "member", "set"]));
            }
        }
    }

    /// typecheck.py:2203-2225 — E1141 (TypeChecker.visit_for)
    pub fn visit_for(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let (target, iter) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::For(d) => (GNode { m: node.m, n: d.target }, GNode { m: node.m, n: d.iter }),
            _ => return,
        };
        let target_ok = match &md.tree.nodes[target.n.idx()].kind {
            NodeKind::Tuple { elts, .. } => elts.len() == 2,
            _ => false,
        };
        if !target_ok {
            return;
        }
        if !matches!(md.tree.nodes[iter.n.idx()].kind, NodeKind::Name { .. }) {
            return;
        }
        drop(md);
        let inferred = match u::safe_infer(eng, cx.caches, iter) {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        // isinstance(inferred, nodes.Dict)
        let all_tuple_keys: Option<bool> = match &inferred {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => Some(items.iter().all(|(k, _)| {
                        matches!(md.tree.nodes[k.idx()].kind, NodeKind::Tuple { .. })
                    })),
                    _ => None,
                }
            }
            Value::SynthDict { items } => Some(items.iter().all(|(k, _)| {
                matches!(k, Value::Node(kg) if eng.kind_is(*kg, |kk| matches!(kk, NodeKind::Tuple { .. })))
                    || matches!(k, Value::SynthSeq { kind: pyinfer::value::SeqKind::Tuple, .. })
            })),
            _ => None,
        };
        match all_tuple_keys {
            Some(false) => {
                cx.emit_node("E1141", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                    "Unpacking a dictionary in iteration without calling .items()".into());
            }
            _ => {}
        }
    }

    /// typecheck.py:1026-1057 — E1139
    pub fn visit_classdef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some(metaclass) = eng.declared_metaclass(node, None) else { return };
        let mut metaclass = metaclass;
        if let Value::Node(g) = &metaclass {
            if is_funcdef(eng, *g) {
                // _infer_from_metaclass_constructor (typecheck.py:757-795):
                // CallContext with the quirky Tuple args + callee None — the
                // callee-name gate blocks param consumption, so behaviorally
                // an empty-args callcontext matches
                let ctx = Ctx::new();
                let cc = std::rc::Rc::new(CallCtx {
                    id: eng.next_callctx_id(),
                    args: RefCell::new(Vec::new()),
                    keywords: RefCell::new(Vec::new()),
                    callee: RefCell::new(None),
                });
                *ctx.callcontext.borrow_mut() = Some(cc);
                let flow = eng.function_infer_call_result(*g, Some(*g), Some(&ctx));
                match flow.vals.first() {
                    Some(v) if !v.is_uninferable() => metaclass = v.clone(),
                    _ => return,
                }
            }
        }
        let emit = |cx: &mut WalkCx, name: String| {
            cx.emit_node("E1139", u::lineno(cx.eng, node), u::col_offset(cx.eng, node) as i64,
                u::format_template("Invalid metaclass %r used", &[&name]));
        };
        match &metaclass {
            Value::Node(g) if is_classdef(eng, *g) => {
                // _is_invalid_metaclass: builtins `type` in mro()
                let invalid = match eng.mro(*g, None) {
                    Ok(mro) => !mro.iter().any(|&c| {
                        eng.md(c.m).name == "builtins"
                            && eng.node_name(c).as_deref() == Some("type")
                    }),
                    Err(_) => true,
                };
                if invalid {
                    let name = eng.node_name(*g).unwrap_or_default();
                    emit(cx, name);
                }
            }
            Value::Node(g) if is_funcdef(eng, *g) => {
                let name = eng.node_name(*g).unwrap_or_default();
                emit(cx, name);
            }
            Value::Inst { cls, .. } => {
                // type(metaclass) is bases.Instance -> str(instance)
                let name = format!(
                    "Instance of {}.{}",
                    eng.md(cls.m).name,
                    eng.node_name(*cls).unwrap_or_default()
                );
                emit(cx, name);
            }
            Value::Node(g) => {
                let name = pyinfer::asstr::as_string(eng, *g);
                emit(cx, name);
            }
            Value::SynthConst(c) => {
                let name = pyinfer::asstr::const_repr(c);
                emit(cx, name);
            }
            _ => {
                // other inference objects: as_string() surface
                let name = value_name(eng, &metaclass).unwrap_or_default();
                emit(cx, name);
            }
        }
    }

    /// typecheck.py:1881-1958 — E1129 / E1145
    pub fn visit_with(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let items: Vec<GNode> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::With(d) => d.items.iter().map(|(e, _)| GNode { m: node.m, n: *e }).collect(),
            _ => return,
        };
        drop(md);
        for ctx_mgr in items {
            // fresh InferenceContext kept for context.path inspection
            let ictx = Ctx::new();
            let flow = eng.infer(ctx_mgr, &ictx);
            let inferred = match u::safe_infer_of_flow(eng, &flow) {
                Some(v) if !v.is_uninferable() => v,
                _ => continue,
            };
            match &inferred {
                Value::Generator { func, is_async, .. } => {
                    if decorated_with(eng, *func, &["contextlib.contextmanager"]) {
                        continue;
                    }
                    if *is_async
                        && decorated_with(eng, *func, &["contextlib.asynccontextmanager"])
                    {
                        let name = func_name(eng, *func).unwrap_or_default();
                        cx.emit_node("E1145", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                            u::format_template(
                                "Context manager '%s' is async and should be used with 'async with'.",
                                &[&name]));
                        continue;
                    }
                    // scan context.path for a decorated function scope
                    let mut found = false;
                    let path: Vec<(GNode, Option<GSym>)> =
                        ictx.path.borrow().iter().copied().collect();
                    for (pnode, _) in path {
                        let scope: Option<GNode> = if eng
                            .kind_is(pnode, |k| matches!(k, NodeKind::Call { .. }))
                        {
                            let (f, _, _) = match call_parts(eng, pnode) {
                                Some(p) => p,
                                None => continue,
                            };
                            match u::safe_infer(eng, cx.caches, f) {
                                Some(Value::Node(g)) if is_funcdef(eng, g) => Some(g),
                                Some(Value::BoundMethod { func, .. })
                                | Some(Value::UnboundMethod { func })
                                | Some(Value::Partial { func, .. })
                                | Some(Value::Property { func, .. }) => {
                                    // isinstance(scope, FunctionDef) is FALSE
                                    // for BM/UM proxies in pylint
                                    let _ = func;
                                    None
                                }
                                _ => None,
                            }
                        } else {
                            let sc = eng.scope(pnode);
                            if is_funcdef(eng, sc) { Some(sc) } else { None }
                        };
                        let Some(sc) = scope else { continue };
                        if decorated_with(eng, sc, &["contextlib.contextmanager"]) {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        let name = if *is_async { "async_generator" } else { "generator" };
                        cx.emit_node("E1129", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                            u::format_template(
                                "Context manager '%s' doesn't implement __enter__ and __exit__.",
                                &[name]));
                    }
                }
                _ => {
                    let enter = value_getattr(eng, &inferred, eng.sym("__enter__"));
                    let exit_ok = enter.is_ok()
                        && value_getattr(eng, &inferred, eng.sym("__exit__")).is_ok();
                    if !exit_ok {
                        if matches!(&inferred, Value::Inst { .. } | Value::ExcInst { .. })
                            || value_is_instance(eng, &inferred)
                        {
                            if !value_has_known_bases(eng, cx.caches, &inferred) {
                                continue;
                            }
                            // mixin skip: name[-5:].lower() == "mixin"
                            let name = value_name(eng, &inferred).unwrap_or_default();
                            if name.len() >= 5
                                && name[name.len() - 5..].to_lowercase() == "mixin"
                            {
                                continue;
                            }
                        }
                        let name = value_name(eng, &inferred).unwrap_or_default();
                        cx.emit_node("E1129", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                            u::format_template(
                                "Context manager '%s' doesn't implement __enter__ and __exit__.",
                                &[&name]));
                    }
                }
            }
        }
    }

    /// typecheck.py:1960-1965 + node_classes UnaryOp.type_errors — E1130
    pub fn visit_unaryop(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let (op, operand) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::UnaryOp { op, operand } => {
                (op.to_string(), GNode { m: node.m, n: *operand })
            }
            _ => return,
        };
        drop(md);
        let flow = eng.infer(operand, &Ctx::new());
        if flow.vals.is_empty() {
            return; // InferenceError -> []
        }
        let mut bad: Vec<String> = Vec::new();
        let mut saw_uninferable = flow.err.is_some(); // mid-stream error
        for v in &flow.vals {
            if v.is_uninferable() {
                saw_uninferable = true;
                continue;
            }
            match unary_result(eng, node, &op, v) {
                UnaryRes::Ok => {}
                UnaryRes::Uninferable => saw_uninferable = true,
                UnaryRes::Bad => {
                    let operand_type = value_name(eng, v).unwrap_or_default();
                    bad.push(format!("bad operand type for unary {op}: {operand_type}"));
                }
            }
        }
        if saw_uninferable {
            return; // type_errors: any Uninferable result discards all
        }
        for msg in bad {
            cx.emit_node("E1130", u::lineno(eng, node), u::col_offset(eng, node) as i64, msg);
        }
    }
}

enum UnaryRes {
    Ok,
    Bad,
    Uninferable,
}

/// per-operand-value branch of _infer_unaryop (node_classes.py:4326-4388)
fn unary_result(eng: &Engine, unaryop: GNode, op: &str, v: &Value) -> UnaryRes {
    // literals: infer_unary_op applies the REAL python operator
    let const_of = |v: &Value| -> Option<ConstValue> {
        match v {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(clone_const(c)),
                    _ => None,
                }
            }
            Value::SynthConst(c) => Some(clone_const(c)),
            _ => None,
        }
    };
    let is_container = |v: &Value| -> bool {
        match v {
            Value::Node(g) => eng.kind_is(*g, |k| {
                matches!(
                    k,
                    NodeKind::List { .. } | NodeKind::Tuple { .. } | NodeKind::Set { .. }
                        | NodeKind::Dict { .. }
                )
            }),
            Value::SynthSeq { .. } | Value::SynthDict { .. } => true,
            _ => false,
        }
    };
    if let Some(c) = const_of(v) {
        if op == "not" {
            return UnaryRes::Ok; // operator.not_ never raises
        }
        let ok = match (&c, op) {
            (ConstValue::Bool(_), "+") | (ConstValue::Bool(_), "-") | (ConstValue::Bool(_), "~") => true,
            (ConstValue::Int(_), _) => true,
            (ConstValue::Float(_), "+") | (ConstValue::Float(_), "-") => true,
            (ConstValue::Complex { .. }, "+") | (ConstValue::Complex { .. }, "-") => true,
            _ => false,
        };
        return if ok { UnaryRes::Ok } else { UnaryRes::Bad };
    }
    if is_container(v) {
        if op == "not" {
            return UnaryRes::Ok;
        }
        return UnaryRes::Bad;
    }
    if op == "not" {
        // operand.bool_value(): Uninferable -> U result
        return match eng.bool_value(v, &Ctx::new()) {
            Some(_) => UnaryRes::Ok,
            None => UnaryRes::Uninferable,
        };
    }
    let meth_name = match op {
        "+" => "__pos__",
        "-" => "__neg__",
        "~" => "__invert__",
        _ => return UnaryRes::Ok,
    };
    let is_inst_or_class = matches!(v, Value::Inst { .. } | Value::ExcInst { .. })
        || matches!(v, Value::Node(g) if is_classdef(eng, *g))
        || matches!(v, Value::FrozenSet { .. } | Value::SynthSlice { .. });
    if !is_inst_or_class {
        return UnaryRes::Bad;
    }
    let sym = eng.sym(meth_name);
    let Some(meth) = dunder_lookup_first(eng, v, sym) else {
        return UnaryRes::Bad;
    };
    // inferred = next(meth.infer(context)); U or not callable -> skip
    let ctx = Ctx::new();
    let meth_v = match eng.first_value(meth, &ctx) {
        Ok(Some(mv)) => mv,
        _ => return UnaryRes::Uninferable, // InferenceError -> U
    };
    if meth_v.is_uninferable() || !eng.value_callable(&meth_v, &ctx) {
        return UnaryRes::Ok; // continue: no result at all
    }
    // infer_call_result under boundnode=operand; first result; error -> U
    let cctx = pyinfer::ctx::copy_context(Some(&ctx));
    *cctx.boundnode.borrow_mut() = Some(v.clone());
    let cc = std::rc::Rc::new(CallCtx {
        id: eng.next_callctx_id(),
        args: RefCell::new(Vec::new()),
        keywords: RefCell::new(Vec::new()),
        callee: RefCell::new(Some(meth_v.clone())),
    });
    *cctx.callcontext.borrow_mut() = Some(cc);
    match eng.infer_call_result_first(&meth_v, Some(unaryop), Some(&cctx)) {
        Ok(Some(r)) if r.is_uninferable() => UnaryRes::Uninferable,
        Ok(_) => UnaryRes::Ok,
        Err(_) => UnaryRes::Uninferable, // InferenceError -> yield U
    }
}

fn clone_const(c: &ConstValue) -> ConstValue {
    use pyast::tree::IntValue;
    match c {
        ConstValue::None => ConstValue::None,
        ConstValue::Bool(b) => ConstValue::Bool(*b),
        ConstValue::Int(IntValue::Small(i)) => ConstValue::Int(IntValue::Small(*i)),
        ConstValue::Int(IntValue::Big(s)) => ConstValue::Int(IntValue::Big(s.clone())),
        ConstValue::Float(f) => ConstValue::Float(*f),
        ConstValue::Complex { real, imag } => ConstValue::Complex { real: *real, imag: *imag },
        ConstValue::Str(s) => ConstValue::Str(s.clone()),
        ConstValue::StrSurrogate(p) => ConstValue::StrSurrogate(p.clone()),
        ConstValue::Bytes(b) => ConstValue::Bytes(b.clone()),
        ConstValue::Ellipsis => ConstValue::Ellipsis,
        ConstValue::NotImplemented => ConstValue::NotImplemented,
    }
}

// ---------------------------------------------------------------------------
// visit_subscript family (typecheck.py:1715-1879, 2138-2201)
// ---------------------------------------------------------------------------

impl TypeCk {
    /// typecheck.py:2138-2201
    pub fn visit_subscript(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        self.check_invalid_sequence_index(cx, node);

        let md = eng.md(node.m);
        let (value, slice, sctx) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Subscript { value, slice, ctx } => (
                GNode { m: node.m, n: *value },
                GNode { m: node.m, n: *slice },
                *ctx,
            ),
            _ => return,
        };
        let value_kind_tag: u8 = match &md.tree.nodes[value.n.idx()].kind {
            NodeKind::ListComp(_) | NodeKind::DictComp(_) => 1,
            NodeKind::Dict { .. } => 2,
            NodeKind::SetComp(_) => 3,
            _ => 0,
        };
        drop(md);
        if value_kind_tag == 1 {
            return;
        }
        if value_kind_tag == 2 {
            // dict-literal key hashability (reported at node.value!)
            if !is_hashable(eng, slice) {
                let txt = pyinfer::asstr::as_string(eng, slice);
                cx.emit_node("E1143", u::lineno(eng, value), u::col_offset(eng, value) as i64,
                    u::format_template("'%s' is unhashable and can't be used as a %s in a %s",
                        &[&txt, "key", "dict"]));
            }
        }
        let (protocol, msgid, template) = match sctx {
            ExprCtx::Load => (Protocol::GetItem, "E1136", "Value '%s' is unsubscriptable"),
            ExprCtx::Store => (
                Protocol::SetItem,
                "E1137",
                "%r does not support item assignment",
            ),
            ExprCtx::Del => (
                Protocol::DelItem,
                "E1138",
                "%r does not support item deletion",
            ),
        };
        if value_kind_tag == 3 {
            let txt = pyinfer::asstr::as_string(eng, value);
            cx.emit_node(msgid, u::lineno(eng, value), u::col_offset(eng, value) as i64,
                u::format_template(template, &[&txt]));
            return;
        }
        if is_inside_abstract_class(cx.caches, eng, node) {
            return;
        }
        let mut inferred = match u::safe_infer(eng, cx.caches, value) {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        // decorated inferred values (typecheck.py:2185-2195):
        // `getattr(inferred, "decorators", None)` — BoundMethod/UnboundMethod
        // PROXY the wrapped function's decorators
        let dec_owner: Option<GNode> = match &inferred {
            Value::Node(g) => Some(*g),
            Value::BoundMethod { func, .. }
            | Value::DescBM { func, .. }
            | Value::UnboundMethod { func } => Some(*func),
            // Property/PartialFunction: postinit without decorators -> None
            _ => None,
        };
        if let Some(owner) = dec_owner {
            let decs = decorator_nodes(eng, owner);
            if !decs.is_empty() {
                // astroid.util.safe_infer of the FIRST decorator
                let first_dec = eng.safe_infer(decs[0], &Ctx::new());
                match first_dec {
                    Some(Value::Node(dc)) if is_classdef(eng, dc) => {
                        inferred = eng.instantiate_class(dc);
                    }
                    _ => return,
                }
            }
        }
        let supported = match protocol {
            Protocol::GetItem => self.supports_getitem(cx, &inferred, node),
            p => supports_protocol(eng, cx.caches, &inferred, p),
        };
        if !supported && !u::in_type_checking_block(eng, cx.caches, node) {
            let txt = pyinfer::asstr::as_string(eng, value);
            cx.emit_node(msgid, u::lineno(eng, value), u::col_offset(eng, value) as i64,
                u::format_template(template, &[&txt]));
        }
    }

    /// utils.supports_getitem (utils.py:548-554)
    fn supports_getitem(&self, cx: &mut WalkCx, v: &Value, node: GNode) -> bool {
        let eng = cx.eng;
        if let Value::Node(g) = v {
            if is_classdef(eng, *g) {
                if supports_protocol_method(eng, v, eng.sym("__class_getitem__")) {
                    return true;
                }
                if u::is_postponed_evaluation_enabled(eng, node.m)
                    && u::is_node_in_type_annotation_context(eng, node)
                {
                    return true;
                }
            }
        }
        supports_protocol(eng, cx.caches, v, Protocol::GetItem)
    }

    /// typecheck.py:1715-1782 — E1126 (+ slice recursion E1127/E1144)
    fn check_invalid_sequence_index(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let (value, slice, sctx) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Subscript { value, slice, ctx } => (
                GNode { m: node.m, n: *value },
                GNode { m: node.m, n: *slice },
                *ctx,
            ),
            _ => return,
        };
        drop(md);
        let parent_type = match u::safe_infer(eng, cx.caches, value) {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        let is_cls_or_inst = matches!(&parent_type, Value::Node(g) if is_classdef(eng, *g))
            || value_is_base_instance(eng, &parent_type)
            || matches!(&parent_type, Value::DictItems(_) | Value::DictKeys(_) | Value::DictValues(_));
        if !is_cls_or_inst || !value_has_known_bases(eng, cx.caches, &parent_type) {
            return;
        }
        let methodname = match sctx {
            ExprCtx::Store => "__setitem__",
            ExprCtx::Del => "__delitem__",
            ExprCtx::Load => "__getitem__",
        };
        let Some(itemmethod) = dunder_lookup_first(eng, &parent_type, eng.sym(methodname))
        else {
            return;
        };
        // FunctionDef in builtins whose frame name is a SEQUENCE_TYPES member
        if !is_funcdef(eng, itemmethod) {
            return;
        }
        if eng.md(itemmethod.m).name != "builtins" {
            return;
        }
        let frame_name = eng
            .parent(itemmethod)
            .map(|p| eng.frame(p))
            .and_then(|f| eng.node_name(f))
            .unwrap_or_default();
        if !SEQUENCE_TYPES.contains(&frame_name.as_str()) {
            return;
        }
        let index_type = match u::safe_infer(eng, cx.caches, slice) {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        // Const int (incl bool)
        let index_const = match &index_type {
            Value::Node(g) => {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(clone_const(c)),
                    _ => None,
                }
            }
            Value::SynthConst(c) => Some(clone_const(c)),
            _ => None,
        };
        if let Some(c) = &index_const {
            if matches!(c, ConstValue::Int(_) | ConstValue::Bool(_)) {
                return;
            }
        }
        // inferred Slice node -> slice-component check
        match &index_type {
            Value::Node(g) if eng.kind_is(*g, |k| matches!(k, NodeKind::Slice { .. })) => {
                self.check_invalid_slice_index(cx, *g);
                return;
            }
            Value::SynthSlice { .. } => {
                // brain slice(...) products carry const bounds, no nodes to
                // report on; astroid's tip builds REAL nodes — E1127/E1144
                // have zero corpus mass, skip
                return;
            }
            _ => {}
        }
        // Instance arm (Const non-int falls here too)
        if value_is_base_instance(eng, &index_type) {
            let pt = u::value_pytype(eng, &index_type).unwrap_or_default();
            if pt == "builtins.int" || pt == "builtins.slice" {
                return;
            }
            if value_getattr(eng, &index_type, eng.sym("__index__")).is_ok() {
                return;
            }
        } else if matches!(&index_type, Value::Inst { .. } | Value::ExcInst { .. }) {
            unreachable!()
        }
        // ClassDef / FunctionDef / Module / everything else -> error
        cx.emit_node("E1126", u::lineno(eng, node), u::col_offset(eng, node) as i64,
            "Sequence index is not an int, slice, or instance with __index__".into());
    }

    /// typecheck.py:1815-1879 — E1127 / E1144
    fn check_invalid_slice_index(&mut self, cx: &mut WalkCx, slice_node: GNode) {
        let eng = cx.eng;
        let md = eng.md(slice_node.m);
        let (lower, upper, step) = match &md.tree.nodes[slice_node.n.idx()].kind {
            NodeKind::Slice { lower, upper, step } => (*lower, *upper, *step),
            _ => return,
        };
        drop(md);
        let mut invalid_slices: Vec<GNode> = Vec::new();
        for idx in [lower, upper, step].into_iter().flatten() {
            let ig = GNode { m: slice_node.m, n: idx };
            let index_type = match u::safe_infer(eng, cx.caches, ig) {
                Some(v) if !v.is_uninferable() => v,
                _ => continue,
            };
            let c = match &index_type {
                Value::Node(g) => {
                    let md = eng.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::Const(cv) => Some(clone_const(cv)),
                        _ => None,
                    }
                }
                Value::SynthConst(cv) => Some(clone_const(cv)),
                _ => None,
            };
            if let Some(cv) = &c {
                if matches!(cv, ConstValue::Int(_) | ConstValue::Bool(_) | ConstValue::None) {
                    continue;
                }
            }
            if value_is_base_instance(eng, &index_type) {
                let pt = u::value_pytype(eng, &index_type).unwrap_or_default();
                if pt == "builtins.int" || pt == "builtins.NoneType" {
                    continue;
                }
                if value_getattr(eng, &index_type, eng.sym("__index__")).is_ok() {
                    return; // bails out of the WHOLE check
                }
            }
            invalid_slices.push(ig);
        }
        // literal step Const == 0 (python ==: False/0.0 match)
        let invalid_step = step
            .map(|st| {
                let md = eng.md(slice_node.m);
                match &md.tree.nodes[st.idx()].kind {
                    NodeKind::Const(ConstValue::Int(pyast::tree::IntValue::Small(0))) => true,
                    NodeKind::Const(ConstValue::Bool(false)) => true,
                    NodeKind::Const(ConstValue::Float(f)) => *f == 0.0,
                    _ => false,
                }
            })
            .unwrap_or(false);
        if invalid_slices.is_empty() && !invalid_step {
            return;
        }
        // custom-object gate when the slice's parent is a Subscript
        if let Some(parent) = eng.parent(slice_node) {
            if eng.kind_is(parent, |k| matches!(k, NodeKind::Subscript { .. })) {
                let md = eng.md(parent.m);
                let pvalue = match &md.tree.nodes[parent.n.idx()].kind {
                    NodeKind::Subscript { value, .. } => GNode { m: parent.m, n: *value },
                    _ => return,
                };
                drop(md);
                let inferred = match u::safe_infer(eng, cx.caches, pvalue) {
                    Some(v) if !v.is_uninferable() => v,
                    _ => return,
                };
                let known = match &inferred {
                    Value::Node(g) => {
                        let md = eng.md(g.m);
                        match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::List { .. }
                            | NodeKind::Dict { .. }
                            | NodeKind::Tuple { .. }
                            | NodeKind::Set { .. } => true,
                            NodeKind::Const(ConstValue::Str(_))
                            | NodeKind::Const(ConstValue::StrSurrogate(_))
                            | NodeKind::Const(ConstValue::Bytes(_)) => true,
                            _ => false,
                        }
                    }
                    Value::SynthSeq { .. } | Value::SynthDict { .. } | Value::FrozenSet { .. } => true,
                    Value::SynthConst(c) => {
                        matches!(c.as_ref(), ConstValue::Str(_) | ConstValue::Bytes(_))
                    }
                    Value::Inst { cls, .. } => eng.qname(*cls) == "builtins.range",
                    _ => false,
                };
                if !known {
                    return;
                }
            }
        }
        for snode in invalid_slices {
            cx.emit_node("E1127", u::lineno(eng, snode), u::col_offset(eng, snode) as i64,
                "Slice index is not an int, None, or instance with __index__".into());
        }
        if invalid_step {
            let st = GNode { m: slice_node.m, n: step.unwrap() };
            cx.emit_node("E1144", u::lineno(eng, st), u::col_offset(eng, st) as i64,
                "Slice step cannot be 0".into());
        }
    }
}

// ---------------------------------------------------------------------------
// IterableChecker (typecheck.py:2243-2350) — E1133 / E1134
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct IterCk;

impl IterCk {
    fn check_iterable(&self, cx: &mut WalkCx, node: GNode, check_async: bool) {
        let eng = cx.eng;
        if is_inside_abstract_class(cx.caches, eng, node) {
            return;
        }
        let inferred = match u::safe_infer(eng, cx.caches, node) {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        // is_comprehension(inferred)
        if matches!(&inferred, Value::Node(g) if eng.kind_is(*g, |k| matches!(
            k,
            NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_)
                | NodeKind::GeneratorExp(_)
        ))) {
            return;
        }
        if !is_iterable(eng, cx.caches, &inferred, check_async) {
            let txt = pyinfer::asstr::as_string(eng, node);
            cx.emit_node("E1133", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                u::format_template("Non-iterable value %s is used in an iterating context", &[&txt]));
        }
    }

    fn check_mapping(&self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if is_inside_abstract_class(cx.caches, eng, node) {
            return;
        }
        if eng.kind_is(node, |k| matches!(k, NodeKind::DictComp(_))) {
            return;
        }
        let inferred = match u::safe_infer(eng, cx.caches, node) {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        if !is_mapping(eng, cx.caches, &inferred) {
            let txt = pyinfer::asstr::as_string(eng, node);
            cx.emit_node("E1134", u::lineno(eng, node), u::col_offset(eng, node) as i64,
                u::format_template("Non-mapping value %s is used in a mapping context", &[&txt]));
        }
    }

    pub fn visit_for(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let iter = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::For(d) => GNode { m: node.m, n: d.iter },
            _ => return,
        };
        drop(md);
        self.check_iterable(cx, iter, false);
    }

    pub fn visit_asyncfor(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let iter = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::AsyncFor(d) => GNode { m: node.m, n: d.iter },
            _ => return,
        };
        drop(md);
        self.check_iterable(cx, iter, true);
    }

    pub fn visit_yieldfrom(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let value = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::YieldFrom { value } => GNode { m: node.m, n: *value },
            _ => return,
        };
        drop(md);
        if self.is_asyncio_coroutine(cx, value) {
            return;
        }
        self.check_iterable(cx, value, false);
    }

    /// typecheck.py:2272-2289
    fn is_asyncio_coroutine(&self, cx: &mut WalkCx, node: GNode) -> bool {
        let eng = cx.eng;
        let Some((func, _, _)) = call_parts(eng, node) else { return false };
        let inferred_func = match u::safe_infer(eng, cx.caches, func) {
            Some(Value::Node(g)) if is_funcdef(eng, g) => g,
            _ => return false,
        };
        for dec in decorator_nodes(eng, inferred_func) {
            match u::safe_infer(eng, cx.caches, dec) {
                Some(v) => {
                    if matches!(&v, Value::Node(g) if is_funcdef(eng, *g))
                        && eng.value_qname(&v).as_deref() == Some("asyncio.coroutines.coroutine")
                    {
                        return true;
                    }
                }
                None => continue,
            }
        }
        false
    }

    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some((_, args, keywords)) = call_parts(eng, node) else { return };
        for a in args {
            let md = eng.md(a.m);
            if let NodeKind::Starred { value, .. } = &md.tree.nodes[a.n.idx()].kind {
                let v = GNode { m: a.m, n: *value };
                drop(md);
                self.check_iterable(cx, v, false);
            }
        }
        for k in keywords {
            let (name, value) = keyword_parts(eng, k);
            if name.is_none() {
                self.check_mapping(cx, value);
            }
        }
    }

    pub fn visit_comp(&mut self, cx: &mut WalkCx, node: GNode) {
        // listcomp/dictcomp/setcomp/generatorexp: per-generator iter
        let eng = cx.eng;
        let md = eng.md(node.m);
        let gens: Vec<pyast::NodeId> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ListComp(d) | NodeKind::SetComp(d) | NodeKind::GeneratorExp(d) => {
                d.generators.clone()
            }
            NodeKind::DictComp(d) => d.generators.clone(),
            _ => return,
        };
        let mut items: Vec<(GNode, bool)> = Vec::new();
        for gn in gens {
            if let NodeKind::Comprehension { iter, is_async, .. } = md.tree.nodes[gn.idx()].kind {
                items.push((GNode { m: node.m, n: iter }, is_async));
            }
        }
        drop(md);
        for (iter, is_async) in items {
            self.check_iterable(cx, iter, is_async);
        }
    }
}

// ---------------------------------------------------------------------------
// pub shims for the classes checkers
// ---------------------------------------------------------------------------

pub fn clone_const_pub(c: &ConstValue) -> ConstValue {
    clone_const(c)
}

/// nodes_of_class without skip_klass: full preorder below root
pub fn nodes_of_class_skip_pub<FT>(eng: &Engine, root: GNode, target: FT) -> Vec<GNode>
where
    FT: Fn(&NodeKind) -> bool,
{
    nodes_of_class_skip(eng, root, target, |_| false)
}

pub fn decorator_nodes_pub(eng: &Engine, func: GNode) -> Vec<GNode> {
    decorator_nodes(eng, func)
}
