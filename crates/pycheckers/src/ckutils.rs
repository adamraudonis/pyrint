//! Shared pylint checker utilities (pylint/checkers/utils.py ports) used by
//! the imports/variables checkers, plus inference-helper caches.
//!
//! Truth: pylint 4.0.5 `pylint/checkers/utils.py` (cited utils.py:NNN) and
//! astroid 4.0.4. Bug-for-bug; do not "improve".

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use pyast::tree::{ConstValue, Ctx as ExprCtx, IntValue, NodeKind};
use pyinfer::ctx::Ctx;
use pyinfer::graph::Engine;
use pyinfer::value::{Drive, GNode, GSym, Value, NV};
use rustc_hash::{FxHashMap, FxHashSet};

// ---------------------------------------------------------------------------
// builtins / scope attrs (notes/05 §13.1)
// ---------------------------------------------------------------------------

/// CPython 3.12.12 `builtins.__dict__` keys (157) — utils.py:283 snapshot.
pub const BUILTINS: &[&str] = &[
    "ArithmeticError", "AssertionError", "AttributeError", "BaseException",
    "BaseExceptionGroup", "BlockingIOError", "BrokenPipeError", "BufferError",
    "BytesWarning", "ChildProcessError", "ConnectionAbortedError", "ConnectionError",
    "ConnectionRefusedError", "ConnectionResetError", "DeprecationWarning", "EOFError",
    "Ellipsis", "EncodingWarning", "EnvironmentError", "Exception", "ExceptionGroup",
    "False", "FileExistsError", "FileNotFoundError", "FloatingPointError",
    "FutureWarning", "GeneratorExit", "IOError", "ImportError", "ImportWarning",
    "IndentationError", "IndexError", "InterruptedError", "IsADirectoryError",
    "KeyError", "KeyboardInterrupt", "LookupError", "MemoryError",
    "ModuleNotFoundError", "NameError", "None", "NotADirectoryError", "NotImplemented",
    "NotImplementedError", "OSError", "OverflowError", "PendingDeprecationWarning",
    "PermissionError", "ProcessLookupError", "RecursionError", "ReferenceError",
    "ResourceWarning", "RuntimeError", "RuntimeWarning", "StopAsyncIteration",
    "StopIteration", "SyntaxError", "SyntaxWarning", "SystemError", "SystemExit",
    "TabError", "TimeoutError", "True", "TypeError", "UnboundLocalError",
    "UnicodeDecodeError", "UnicodeEncodeError", "UnicodeError",
    "UnicodeTranslateError", "UnicodeWarning", "UserWarning", "ValueError", "Warning",
    "ZeroDivisionError", "__build_class__", "__debug__", "__doc__", "__import__",
    "__loader__", "__name__", "__package__", "__spec__", "abs", "aiter", "all",
    "anext", "any", "ascii", "bin", "bool", "breakpoint", "bytearray", "bytes",
    "callable", "chr", "classmethod", "compile", "complex", "copyright", "credits",
    "delattr", "dict", "dir", "divmod", "enumerate", "eval", "exec", "exit",
    "filter", "float", "format", "frozenset", "getattr", "globals", "hasattr",
    "hash", "help", "hex", "id", "input", "int", "isinstance", "issubclass", "iter",
    "len", "license", "list", "locals", "map", "max", "memoryview", "min", "next",
    "object", "oct", "open", "ord", "pow", "print", "property", "quit", "range",
    "repr", "reversed", "round", "set", "setattr", "slice", "sorted", "staticmethod",
    "str", "sum", "super", "tuple", "type", "vars", "zip",
    // SPECIAL_BUILTINS (utils.py:284)
    "__builtins__",
];

/// pylint utils.PYMETHODS = set(SPECIAL_METHODS_PARAMS) (utils.py:78-193)
pub const PYMETHODS: &[&str] = &[
    "__new__", "__init__", "__call__", "__init_subclass__",
    "__del__", "__repr__", "__str__", "__bytes__", "__hash__", "__bool__",
    "__dir__", "__len__", "__length_hint__", "__iter__", "__reversed__",
    "__neg__", "__pos__", "__abs__", "__invert__", "__complex__", "__int__",
    "__float__", "__index__", "__trunc__", "__floor__", "__ceil__",
    "__enter__", "__aenter__", "__getnewargs_ex__", "__getnewargs__",
    "__getstate__", "__reduce__", "__copy__", "__unicode__", "__nonzero__",
    "__await__", "__aiter__", "__anext__", "__fspath__", "__subclasses__",
    "__format__", "__lt__", "__le__", "__eq__", "__ne__", "__gt__", "__ge__",
    "__getattr__", "__getattribute__", "__delattr__", "__delete__",
    "__instancecheck__", "__subclasscheck__", "__getitem__", "__missing__",
    "__delitem__", "__contains__", "__add__", "__sub__", "__mul__",
    "__truediv__", "__floordiv__", "__rfloordiv__", "__mod__", "__divmod__",
    "__lshift__", "__rshift__", "__and__", "__xor__", "__or__", "__radd__",
    "__rsub__", "__rmul__", "__rtruediv__", "__rmod__", "__rdivmod__",
    "__rpow__", "__rlshift__", "__rrshift__", "__rand__", "__rxor__",
    "__ror__", "__iadd__", "__isub__", "__imul__", "__itruediv__",
    "__ifloordiv__", "__imod__", "__ilshift__", "__irshift__", "__iand__",
    "__ixor__", "__ior__", "__ipow__", "__setstate__", "__reduce_ex__",
    "__deepcopy__", "__cmp__", "__matmul__", "__rmatmul__", "__imatmul__",
    "__div__",
    "__setattr__", "__get__", "__set__", "__setitem__", "__set_name__",
    "__exit__", "__aexit__", "__round__", "__pow__",
];

/// astroid `nodes.Module.scope_attrs` (scoped_nodes.py:218-224).
pub const SCOPE_ATTRS: &[&str] = &["__name__", "__doc__", "__file__", "__path__", "__package__"];

// ---------------------------------------------------------------------------
// run-level state (persists across modules, like pylint's lru caches)
// ---------------------------------------------------------------------------

pub struct Lru<V> {
    map: FxHashMap<GNode, (V, u64)>,
    tick: u64,
    cap: usize,
    /// recency index for O(log n) eviction: min-heap of ticks with lazy
    /// deletion + tick->key map. Ticks are unique per live entry, so
    /// "evict the key with the minimum live tick" picks EXACTLY the same
    /// entry the old O(n) `min_by_key` scan did (django: 5.7% self time).
    heap: std::collections::BinaryHeap<std::cmp::Reverse<u64>>,
    by_tick: FxHashMap<u64, GNode>,
}

impl<V: Clone> Lru<V> {
    pub fn new(cap: usize) -> Self {
        Lru {
            map: FxHashMap::default(),
            tick: 0,
            cap,
            heap: std::collections::BinaryHeap::new(),
            by_tick: FxHashMap::default(),
        }
    }
    pub fn get(&mut self, k: GNode) -> Option<V> {
        self.tick += 1;
        let t = self.tick;
        if let Some(e) = self.map.get_mut(&k) {
            let old_t = e.1;
            e.1 = t;
            let v = e.0.clone();
            self.by_tick.remove(&old_t);
            self.by_tick.insert(t, k);
            self.heap.push(std::cmp::Reverse(t));
            self.maybe_compact();
            return Some(v);
        }
        None
    }
    pub fn put(&mut self, k: GNode, v: V) {
        self.tick += 1;
        if self.map.len() >= self.cap {
            // evict the (unique) minimum-tick live entry; stale heap ticks
            // are skipped because they are no longer in by_tick
            while let Some(std::cmp::Reverse(t)) = self.heap.pop() {
                if let Some(old) = self.by_tick.remove(&t) {
                    self.map.remove(&old);
                    break;
                }
            }
        }
        if let Some((_, old_t)) = self.map.insert(k, (v, self.tick)) {
            self.by_tick.remove(&old_t);
        }
        self.by_tick.insert(self.tick, k);
        self.heap.push(std::cmp::Reverse(self.tick));
        self.maybe_compact();
    }
    fn maybe_compact(&mut self) {
        // drop accumulated stale ticks so the heap stays O(cap)
        if self.heap.len() > 8 * self.cap.max(64) {
            self.heap = self.by_tick.keys().map(|&t| std::cmp::Reverse(t)).collect();
        }
    }
}

/// Caches with pylint-process lifetime (`@lru_cache` on utils helpers).
pub struct LintCaches {
    /// utils.infer_all `@lru_cache(maxsize=512)` (utils.py:1413)
    pub infer_all: RefCell<Lru<Rc<Vec<Value>>>>,
    /// utils.safe_infer `@lru_cache(maxsize=1024)` (utils.py:1346)
    pub safe_infer: RefCell<Lru<Option<Value>>>,
    /// safe_infer(compare_constructors=True) keys (same python LRU, distinct
    /// key space via the flag tuple element)
    pub safe_infer_cc: RefCell<Lru<Option<Value>>>,
    /// utils.is_overload_stub @lru_cache(1024) — pure, plain map
    pub overload_stub: RefCell<FxHashMap<GNode, bool>>,
    /// utils.class_is_abstract @lru_cache(1024)
    pub class_abstract: RefCell<FxHashMap<GNode, bool>>,
    builtin_syms: RefCell<Option<Rc<FxHashSet<GSym>>>>,
    scope_attr_syms: RefCell<Option<Rc<FxHashSet<GSym>>>>,
}

impl Default for LintCaches {
    fn default() -> Self {
        LintCaches {
            infer_all: RefCell::new(Lru::new(512)),
            safe_infer: RefCell::new(Lru::new(1024)),
            safe_infer_cc: RefCell::new(Lru::new(1024)),
            overload_stub: RefCell::new(FxHashMap::default()),
            class_abstract: RefCell::new(FxHashMap::default()),
            builtin_syms: RefCell::new(None),
            scope_attr_syms: RefCell::new(None),
        }
    }
}

impl LintCaches {
    pub fn builtin_syms(&self, eng: &Engine) -> Rc<FxHashSet<GSym>> {
        let mut slot = self.builtin_syms.borrow_mut();
        if slot.is_none() {
            let set: FxHashSet<GSym> = BUILTINS.iter().map(|n| eng.sym(n)).collect();
            *slot = Some(Rc::new(set));
        }
        Rc::clone(slot.as_ref().unwrap())
    }
    pub fn scope_attr_syms(&self, eng: &Engine) -> Rc<FxHashSet<GSym>> {
        let mut slot = self.scope_attr_syms.borrow_mut();
        if slot.is_none() {
            let set: FxHashSet<GSym> = SCOPE_ATTRS.iter().map(|n| eng.sym(n)).collect();
            *slot = Some(Rc::new(set));
        }
        Rc::clone(slot.as_ref().unwrap())
    }
    /// utils.is_builtin (utils.py:292-293)
    pub fn is_builtin(&self, eng: &Engine, name: GSym) -> bool {
        self.builtin_syms(eng).contains(&name)
    }
}

// ---------------------------------------------------------------------------
// tree navigation helpers
// ---------------------------------------------------------------------------

/// node.node_ancestors(): parents from nearest to module (excl. self).
pub fn ancestors(eng: &Engine, g: GNode) -> Vec<GNode> {
    let mut out = Vec::new();
    let mut cur = g;
    while let Some(p) = eng.parent(cur) {
        out.push(p);
        cur = p;
    }
    out
}

/// nodes_of_class-style preorder including self (get_children order).
pub fn preorder(eng: &Engine, g: GNode) -> Vec<GNode> {
    let md = eng.md(g.m);
    let mut out = Vec::new();
    let mut stack = vec![g.n];
    let mut buf = Vec::new();
    while let Some(n) = stack.pop() {
        out.push(GNode { m: g.m, n });
        buf.clear();
        md.tree.push_children(n, &mut buf);
        for &c in buf.iter().rev() {
            stack.push(c);
        }
    }
    out
}

pub fn kind_is<F: FnOnce(&NodeKind) -> bool>(eng: &Engine, g: GNode, f: F) -> bool {
    eng.kind_is(g, f)
}

pub fn is_if(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::If { .. }))
}
pub fn is_classdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_)))
}
pub fn is_functiondef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)))
}
pub fn is_lambda(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::Lambda(_)))
}
pub fn is_module(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::Module(_)))
}
pub fn is_excepthandler(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::ExceptHandler { .. }))
}
pub fn is_try(eng: &Engine, g: GNode) -> bool {
    // pylint isinstance(nodes.Try) — astroid TryStar is a separate class
    eng.kind_is(g, |k| matches!(k, NodeKind::Try(_)))
}

/// name sym (module-tree Sym mapped to engine GSym) of Name/AssignName/DelName
pub fn name_gsym(eng: &Engine, g: GNode) -> Option<GSym> {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Name { name } | NodeKind::AssignName { name } | NodeKind::DelName { name } => {
            Some(eng.g(&md, *name))
        }
        _ => None,
    }
}

pub fn lineno(eng: &Engine, g: GNode) -> u32 {
    eng.fromlineno(g)
}

pub fn col_offset(eng: &Engine, g: GNode) -> i32 {
    eng.md(g.m).tree.nodes[g.n.idx()].col_offset
}

/// Message-anchor line for a node: pylint uses `node.position` when set
/// (pylinter.py:1213-1221) — the def/class/async keyword line for
/// FunctionDef/AsyncFunctionDef/ClassDef — else `node.fromlineno`.
pub fn msg_line(eng: &Engine, g: GNode) -> u32 {
    if let Some(&(l, _)) = eng.md(g.m).tree.positions.get(&g.n) {
        return l;
    }
    eng.fromlineno(g)
}

/// Message-anchor column (see `msg_line`). col_offset -1 encodes astroid
/// None — pylint renders `col_offset or 0` over an actual None as 0
/// (vararg/kwarg AssignName anchors, e.g. W0613).
pub fn msg_col(eng: &Engine, g: GNode) -> i64 {
    let md = eng.md(g.m);
    if let Some(&(_, c)) = md.tree.positions.get(&g.n) {
        return c as i64;
    }
    (md.tree.nodes[g.n.idx()].col_offset as i64).max(0)
}

/// _is_before (variables.py:308-317)
pub fn is_before(eng: &Engine, node: GNode, reference: GNode) -> bool {
    let (nl, rl) = (lineno(eng, node), lineno(eng, reference));
    if nl < rl {
        return true;
    }
    nl == rl && col_offset(eng, node) < col_offset(eng, reference)
}

/// utils.get_node_first_ancestor_of_type (utils.py:1963-1970)
pub fn first_ancestor<F: Fn(&NodeKind) -> bool>(eng: &Engine, g: GNode, f: F) -> Option<GNode> {
    let mut cur = g;
    while let Some(p) = eng.parent(cur) {
        if eng.kind_is(p, &f) {
            return Some(p);
        }
        cur = p;
    }
    None
}

/// utils.get_node_first_ancestor_of_type_and_its_child (utils.py:1973-1987)
pub fn first_ancestor_and_child<F: Fn(&NodeKind) -> bool>(
    eng: &Engine,
    g: GNode,
    f: F,
) -> Option<(GNode, GNode)> {
    let mut child = g;
    let mut cur = g;
    while let Some(p) = eng.parent(cur) {
        if eng.kind_is(p, &f) {
            return Some((p, child));
        }
        child = p;
        cur = p;
    }
    None
}

/// utils.assign_parent (utils.py:461-465): climb while the CURRENT node is
/// AssignName/Tuple/List.
pub fn assign_parent(eng: &Engine, g: GNode) -> GNode {
    let mut cur = g;
    loop {
        let climb = eng.kind_is(cur, |k| {
            matches!(
                k,
                NodeKind::AssignName { .. } | NodeKind::Tuple { .. } | NodeKind::List { .. }
            )
        });
        if !climb {
            return cur;
        }
        match eng.parent(cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }
}

/// utils.get_all_elements (utils.py:259-267)
pub fn get_all_elements(eng: &Engine, g: GNode) -> Vec<GNode> {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } => {
            let elts: Vec<pyast::NodeId> = elts.clone();
            drop(md);
            let mut out = Vec::new();
            for e in elts {
                out.extend(get_all_elements(eng, GNode { m: g.m, n: e }));
            }
            out
        }
        _ => vec![g],
    }
}

/// Statement.previous_sibling (_base_nodes.py): previous element of the
/// child sequence (the list field of the parent containing this node).
pub fn previous_sibling(eng: &Engine, g: GNode) -> Option<GNode> {
    let parent = eng.parent(g)?;
    let md = eng.md(parent.m);
    let mut prev: Option<pyast::NodeId> = None;
    let mut found: Option<pyast::NodeId> = None;
    let seqs = child_sequences(&md.tree.nodes[parent.n.idx()].kind);
    for seq in seqs {
        let mut p = None;
        for &n in &seq {
            if n == g.n {
                found = p;
                break;
            }
            p = Some(n);
        }
        if found.is_some() || seq.contains(&g.n) {
            prev = found;
            break;
        }
    }
    prev.map(|n| GNode { m: parent.m, n })
}

/// `len(parent.child_sequence(node))` — size of the body/orelse/... list
/// containing `child` (astroid node_ng.py:325-349); 0 when not found.
pub fn child_sequence_len(parent_kind: &NodeKind, child: pyast::NodeId) -> usize {
    for seq in child_sequences(parent_kind) {
        if seq.contains(&child) {
            return seq.len();
        }
    }
    0
}

/// statement-list fields of a node (the sequences `child_sequence` can find)
fn child_sequences(k: &NodeKind) -> Vec<Vec<pyast::NodeId>> {
    match k {
        NodeKind::Module(d) => vec![d.body.clone()],
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => vec![d.body.clone()],
        NodeKind::ClassDef(d) => vec![d.body.clone()],
        NodeKind::For(d) | NodeKind::AsyncFor(d) => vec![d.body.clone(), d.orelse.clone()],
        NodeKind::While { body, orelse, .. } | NodeKind::If { body, orelse, .. } => {
            vec![body.clone(), orelse.clone()]
        }
        NodeKind::With(d) | NodeKind::AsyncWith(d) => vec![d.body.clone()],
        NodeKind::Try(d) | NodeKind::TryStar(d) => vec![
            d.body.clone(),
            d.handlers.clone(),
            d.orelse.clone(),
            d.finalbody.clone(),
        ],
        NodeKind::ExceptHandler { body, .. } => vec![body.clone()],
        NodeKind::MatchCase { body, .. } => vec![body.clone()],
        NodeKind::Match { cases, .. } => vec![cases.clone()],
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// inference helpers
// ---------------------------------------------------------------------------

/// utils.infer_all (utils.py:1413-1422): list(node.infer()) with
/// InferenceError -> [].
pub fn infer_all(eng: &Engine, caches: &LintCaches, g: GNode) -> Rc<Vec<Value>> {
    if let Some(v) = caches.infer_all.borrow_mut().get(g) {
        return v;
    }
    let flow = eng.infer(g, &Ctx::new());
    let vals = if flow.err.is_some() {
        Rc::new(Vec::new())
    } else {
        Rc::new(flow.vals)
    };
    caches.infer_all.borrow_mut().put(g, Rc::clone(&vals));
    vals
}

/// pylint utils.safe_infer (utils.py:1346-1410, default flags): first value,
/// None on first-pull InferenceError; subsequent values must share the first
/// value's pytype (Uninferable never matches); mid-stream error -> None.
pub fn safe_infer(eng: &Engine, caches: &LintCaches, g: GNode) -> Option<Value> {
    if let Some(v) = caches.safe_infer.borrow_mut().get(g) {
        return v;
    }
    let res = safe_infer_streaming(eng, &Ctx::new(), g, None);
    caches.safe_infer.borrow_mut().put(g, res.clone());
    res
}

/// LAZY pull-by-pull port of pylint utils.safe_infer (utils.py:1346-1410).
/// The python version abandons the inference generator the moment a stop
/// condition fires (`return None` mid-loop): nothing further is explored,
/// so no extra nodes_inferred bumps, no context.path growth and no global
/// inference-cache writes happen past that point. An eager full drain
/// poisons astroid's GLOBAL inference cache with truncated [Uninferable]
/// entries that pylint never writes (probe: twisted _signals.py:90
/// `signal.getsignal(SIGCHLD) == SIG_DFL` — astroid's safe_infer stops the
/// Call generator after [U, Inst:Handlers]; a full drain keeps exploring
/// EnumType.__call__ -> _create_(class_name=value, ...) and caches a
/// clamped [U] for enum.py:759 `value` bn=Handlers, which the W0125
/// inference of line 148 then replays, flipping W0143).
/// `ctor_ambiguous`: the compare_constructors=True per-value check
/// (utils.py:1397-1403), passed as a callback by the typecheck checker.
pub fn safe_infer_streaming(
    eng: &Engine,
    ctx: &Rc<Ctx>,
    g: GNode,
    ctor_ambiguous: Option<&dyn Fn(GNode, GNode) -> bool>,
) -> Option<Value> {
    let mut first: Option<Value> = None;
    // inferred_types: None = empty set (first was Uninferable), Some(t) = {t}
    let mut first_type: Option<Option<String>> = None;
    let mut stopped_none = false;
    // isinstance(x, nodes.FunctionDef) is True for FunctionDef AND the
    // objects.Property / objects.PartialFunction subclasses
    let fnode_of = |val: &Value| -> Option<GNode> {
        match val {
            Value::Node(g) if is_functiondef(eng, *g) => Some(*g),
            Value::Property { func, .. } | Value::Partial { func, .. } => Some(*func),
            _ => None,
        }
    };
    let end = {
        let first = &mut first;
        let first_type = &mut first_type;
        let stopped_none = &mut stopped_none;
        eng.infer_to(g, ctx, &mut |v| {
            if first.is_none() {
                // value = next(infer_gen)
                if !v.is_uninferable() {
                    *first_type = Some(value_pytype(eng, &v));
                }
                *first = Some(v);
                return Drive::Go;
            }
            let fv = first.as_ref().unwrap();
            let t = if v.is_uninferable() {
                // Uninferable "pytype" is the Uninferable singleton — never a str
                Some("\u{0}Uninferable".to_string())
            } else {
                value_pytype(eng, &v)
            };
            // `if inferred_type not in inferred_types: return None`
            let in_set = matches!(&first_type, Some(ft) if *ft == t);
            if !in_set {
                *stopped_none = true;
                return Drive::Stop;
            }
            if let (Some(a), Some(b)) = (fnode_of(fv), fnode_of(&v)) {
                if fn_args_ambiguous(eng, a, b) {
                    *stopped_none = true;
                    return Drive::Stop;
                }
            }
            if let Some(cb) = ctor_ambiguous {
                if let (Value::Node(a), Value::Node(b)) = (fv, &v) {
                    if cb(*a, *b) {
                        *stopped_none = true;
                        return Drive::Stop;
                    }
                }
            }
            Drive::Go
        })
    };
    if stopped_none {
        return None;
    }
    if end.err_opt().is_some() {
        // InferenceError on the first pull or while pulling a later value
        return None;
    }
    first
}

pub fn safe_infer_of_flow(eng: &Engine, flow: &pyinfer::value::Flow) -> Option<Value> {
    if flow.vals.is_empty() {
        return None; // InferenceError on first pull (or empty -> raise_if_nothing)
    }
    let first = &flow.vals[0];
    if flow.vals.len() == 1 {
        if flow.err.is_some() {
            // error raised while pulling the SECOND value -> None
            return None;
        }
        return Some(first.clone());
    }
    if flow.err.is_some() {
        return None;
    }
    // inferred_types set: first value's pytype unless Uninferable
    let mut types: FxHashSet<Option<String>> = FxHashSet::default();
    if !first.is_uninferable() {
        types.insert(value_pytype(eng, first));
    }
    for v in &flow.vals[1..] {
        let t = if v.is_uninferable() {
            // Uninferable "pytype" is the Uninferable singleton — never a str
            Some("\u{0}Uninferable".to_string())
        } else {
            value_pytype(eng, v)
        };
        if !types.contains(&t) {
            return None;
        }
        // isinstance(x, nodes.FunctionDef) is True for FunctionDef AND the
        // objects.Property / objects.PartialFunction subclasses
        let fnode_of = |val: &Value| -> Option<GNode> {
            match val {
                Value::Node(g) if is_functiondef(eng, *g) => Some(*g),
                Value::Property { func, .. } | Value::Partial { func, .. } => Some(*func),
                _ => None,
            }
        };
        if let (Some(a), Some(b)) = (fnode_of(first), fnode_of(v)) {
            if fn_args_ambiguous(eng, a, b) {
                return None;
            }
        }
    }
    if types.len() <= 1 {
        Some(first.clone())
    } else {
        None
    }
}

/// pub re-export for the typecheck checker (class_constructors_are_ambiguous)
pub fn fn_args_ambiguous_pub(eng: &Engine, a: GNode, b: GNode) -> bool {
    fn_args_ambiguous(eng, a, b)
}

/// utils.function_arguments_are_ambiguous (utils.py:1425-1448) FULL port:
/// argnames() inequality, then the defaults pairs — note the early `return`
/// inside the zip loop: only the FIRST zipped pair is actually compared,
/// and (None, None) kw_default pairs hit the `case _: return True` arm.
fn fn_args_ambiguous(eng: &Engine, a: GNode, b: GNode) -> bool {
    let spec_of = |g: GNode| eng.arg_spec(g);
    let (sa, sb) = match (spec_of(a), spec_of(b)) {
        (Some(x), Some(y)) => (x, y),
        _ => return false,
    };
    // argnames() = [elt.name for elt in args.arguments] (one combined list)
    let argnames = |s: &pyinfer::calls::ArgSpec| -> Vec<GSym> {
        s.arguments().iter().filter_map(|&an| eng.assign_name_of(an)).collect()
    };
    if argnames(&sa) != argnames(&sb) {
        return true;
    }
    // pairs_of_defaults: (defaults, defaults) then (kw_defaults, kw_defaults)
    // `None in zippable_default` -> raw-built args (args_unknown) lists None
    enum DefEntry {
        Node(GNode),
        NoneEntry,
    }
    let pairs: Vec<(Option<Vec<DefEntry>>, Option<Vec<DefEntry>>)> = vec![
        (
            if sa.args_unknown { None } else { Some(sa.defaults.iter().map(|&d| DefEntry::Node(d)).collect()) },
            if sb.args_unknown { None } else { Some(sb.defaults.iter().map(|&d| DefEntry::Node(d)).collect()) },
        ),
        (
            if sa.args_unknown { None } else {
                Some(sa.kw_defaults.iter().map(|d| match d {
                    Some(g) => DefEntry::Node(*g),
                    None => DefEntry::NoneEntry,
                }).collect())
            },
            if sb.args_unknown { None } else {
                Some(sb.kw_defaults.iter().map(|d| match d {
                    Some(g) => DefEntry::Node(*g),
                    None => DefEntry::NoneEntry,
                }).collect())
            },
        ),
    ];
    for (da, db) in pairs {
        let (da, db) = match (da, db) {
            (Some(x), Some(y)) => (x, y),
            _ => continue,
        };
        if da.len() != db.len() {
            return true;
        }
        // zip: the FIRST pair decides (early return, utils.py:1437-1444)
        if let (Some(d1), Some(d2)) = (da.first(), db.first()) {
            return match (d1, d2) {
                (DefEntry::Node(g1), DefEntry::Node(g2)) => {
                    let md1 = eng.md(g1.m);
                    let md2 = eng.md(g2.m);
                    let k1 = &md1.tree.nodes[g1.n.idx()].kind;
                    let k2 = &md2.tree.nodes[g2.n.idx()].kind;
                    match (k1, k2) {
                        (NodeKind::Const(c1), NodeKind::Const(c2)) => {
                            // python `!=` over const values
                            py_key(c1) != py_key(c2)
                        }
                        (NodeKind::Name { name: n1 }, NodeKind::Name { name: n2 }) => {
                            eng.g(&md1, *n1) != eng.g(&md2, *n2)
                        }
                        _ => true,
                    }
                }
                _ => true,
            };
        }
        // empty zip: fall through to the next pair
    }
    false
}

/// node.pytype() for inference results (None when the object has no pytype).
pub fn value_pytype(eng: &Engine, v: &Value) -> Option<String> {
    let b = eng.builtins();
    let q = |g: GNode| Some(eng.qname(g));
    match v {
        Value::Uninferable => None,
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Const(c) => q(eng.const_class(c)),
                NodeKind::List { .. } => Some("builtins.list".into()),
                NodeKind::Tuple { .. } => Some("builtins.tuple".into()),
                NodeKind::Set { .. } => Some("builtins.set".into()),
                NodeKind::Dict { .. } => Some("builtins.dict".into()),
                NodeKind::Slice { .. } => Some("builtins.slice".into()),
                NodeKind::ClassDef(_) => Some("builtins.type".into()),
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::Lambda(_) => {
                    Some(funcdef_pytype(eng, *g).into())
                }
                NodeKind::Module(_) => Some("builtins.module".into()),
                _ => None,
            }
        }
        Value::SynthConst(c) => q(eng.const_class(c)),
        Value::SynthSeq { kind, .. } => Some(
            match kind {
                pyinfer::value::SeqKind::List => "builtins.list",
                pyinfer::value::SeqKind::Tuple => "builtins.tuple",
                pyinfer::value::SeqKind::Set => "builtins.set",
            }
            .into(),
        ),
        Value::SynthDict { .. } => Some("builtins.dict".into()),
        Value::SynthSlice { .. } => Some("builtins.slice".into()),
        Value::FrozenSet { .. } => Some("builtins.frozenset".into()),
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => q(*cls),
        // UnboundMethod/BoundMethod are Proxy objects WITHOUT their own
        // pytype: attribute access falls through to the wrapped
        // FunctionDef.pytype() — "builtins.instancemethod" iff "method" is
        // in self.type (scoped_nodes; covers class/staticmethod too)
        Value::BoundMethod { func, .. }
        | Value::DescBM { func, .. }
        | Value::UnboundMethod { func } => Some(funcdef_pytype(eng, *func).into()),
        Value::Generator { is_async, .. } => Some(
            if *is_async { "builtins.async_generator" } else { "builtins.generator" }.into(),
        ),
        Value::Property { .. } => Some("builtins.property".into()),
        // PartialFunction inherits FunctionDef.pytype; its .type is
        // computed with the partial-call parent (see partial_func_type)
        Value::Partial { func, parent, .. } => Some(
            if eng.partial_func_type(*func, *parent) != pyinfer::graph::FType::Function {
                "builtins.instancemethod"
            } else {
                "builtins.function"
            }
            .into(),
        ),
        Value::Super { .. } => q(b.super_),
        Value::UnionType => Some("types.UnionType".into()),
        Value::DictItems(_) => Some("builtins.dict_items".into()),
        Value::DictKeys(_) => Some("builtins.dict_keys".into()),
        Value::DictValues(_) => Some("builtins.dict_values".into()),
        Value::EvaluatedObject { .. } => None,
    }
}

/// FunctionDef/Lambda.pytype(): "builtins.instancemethod" iff "method" in
/// self.type — true for method/classmethod/staticmethod (substring check in
/// astroid!), else "builtins.function".
fn funcdef_pytype(eng: &Engine, func: GNode) -> &'static str {
    if eng.func_type(func) != pyinfer::graph::FType::Function {
        "builtins.instancemethod"
    } else {
        "builtins.function"
    }
}

/// `hasattr(inferred, "qname") and inferred.qname()` for inference results.
pub fn value_qname(eng: &Engine, v: &Value) -> Option<String> {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::ClassDef(_)
                | NodeKind::FunctionDef(_)
                | NodeKind::AsyncFunctionDef(_)
                | NodeKind::Lambda(_) => Some(eng.qname(*g)),
                NodeKind::Module(d) => Some(d.name.to_string()),
                // Const & containers proxy their class (Instance.__getattr__)
                NodeKind::Const(_)
                | NodeKind::List { .. }
                | NodeKind::Tuple { .. }
                | NodeKind::Set { .. }
                | NodeKind::Dict { .. } => eng.proxied_class(v).map(|c| eng.qname(c)),
                _ => None,
            }
        }
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(eng.qname(*cls)),
        Value::BoundMethod { func, .. }
        | Value::DescBM { func, .. }
        | Value::UnboundMethod { func }
        | Value::Property { func, .. } => Some(eng.qname(*func)),
        Value::Partial { .. } => Some("PartialFunction".into()),
        _ => eng.proxied_class(v).map(|c| eng.qname(c)),
    }
}

// ---------------------------------------------------------------------------
// exception-handling predicates (utils.py:997-1159)
// ---------------------------------------------------------------------------

/// find_try_except_wrapper_node (utils.py:997-1008)
fn find_try_except_wrapper(eng: &Engine, g: GNode) -> Option<GNode> {
    let mut current = g;
    loop {
        let p = eng.parent(current)?;
        if eng.kind_is(p, |k| {
            matches!(k, NodeKind::ExceptHandler { .. } | NodeKind::Try(_))
        }) {
            return Some(p);
        }
        current = p;
    }
}

/// ExceptHandler.catch (astroid node_classes.py:2652-2659) with non-None
/// exceptions: any Name node in the handler type expression matches.
pub fn handler_catch(eng: &Engine, handler: GNode, exceptions: &[&str]) -> bool {
    let md = eng.md(handler.m);
    let NodeKind::ExceptHandler { type_, .. } = &md.tree.nodes[handler.n.idx()].kind else {
        return false;
    };
    let Some(t) = type_ else { return true }; // type None -> True
    let t = *t;
    drop(md);
    for n in preorder(eng, GNode { m: handler.m, n: t }) {
        let md = eng.md(n.m);
        if let NodeKind::Name { name } = &md.tree.nodes[n.n.idx()].kind {
            if exceptions.contains(&md.tree.s(*name)) {
                return true;
            }
        }
    }
    false
}

/// error_of_type (utils.py:778-802): handler.type must exist.
fn error_of_type(eng: &Engine, handler: GNode, exc: &str) -> bool {
    let has_type = eng.kind_is(handler, |k| {
        matches!(k, NodeKind::ExceptHandler { type_: Some(_), .. })
    });
    if !has_type {
        return false;
    }
    handler_catch(eng, handler, &[exc])
}

/// node_ignores_exception (utils.py:1148-1159)
pub fn node_ignores_exception(eng: &Engine, caches: &LintCaches, g: GNode, exc: &str) -> bool {
    // get_exception_handlers (utils.py:1061-1078)
    if let Some(wrapper) = find_try_except_wrapper(eng, g) {
        if is_try(eng, wrapper) {
            let md = eng.md(wrapper.m);
            if let NodeKind::Try(d) = &md.tree.nodes[wrapper.n.idx()].kind {
                let handlers = d.handlers.clone();
                drop(md);
                if handlers
                    .iter()
                    .any(|&h| error_of_type(eng, GNode { m: wrapper.m, n: h }, exc))
                {
                    return true;
                }
            }
        }
    }
    // get_contextlib_suppressors (utils.py:1110-1131)
    for anc in ancestors(eng, g) {
        let md = eng.md(anc.m);
        let items: Vec<pyast::NodeId> = match &md.tree.nodes[anc.n.idx()].kind {
            NodeKind::With(d) => d.items.iter().map(|(e, _)| *e).collect(),
            _ => continue,
        };
        drop(md);
        for item in items {
            let ig = GNode { m: anc.m, n: item };
            let md = eng.md(ig.m);
            let NodeKind::Call { func, args, .. } = &md.tree.nodes[ig.n.idx()].kind else {
                continue;
            };
            let (func, args) = (*func, args.clone());
            drop(md);
            let inferred = safe_infer(eng, caches, GNode { m: anc.m, n: func });
            let is_suppress = match &inferred {
                Some(Value::Node(c)) if is_classdef(eng, *c) => {
                    eng.qname(*c) == "contextlib.suppress"
                }
                _ => false,
            };
            if !is_suppress {
                continue;
            }
            // _suppresses_exception (utils.py:1088-1107)
            for a in args {
                let av = safe_infer(eng, caches, GNode { m: anc.m, n: a });
                match &av {
                    Some(Value::Node(c)) if is_classdef(eng, *c) => {
                        if eng.node_name(*c).as_deref() == Some(exc) {
                            return true;
                        }
                    }
                    Some(v) => {
                        // Tuple case: inferred Tuple node with elts
                        if let Some(Value::Node(t)) = Some(v.clone()) {
                            let md = eng.md(t.m);
                            if let NodeKind::Tuple { elts, .. } = &md.tree.nodes[t.n.idx()].kind {
                                let elts = elts.clone();
                                drop(md);
                                for e in elts {
                                    let ev = safe_infer(eng, caches, GNode { m: t.m, n: e });
                                    if let Some(Value::Node(c)) = ev {
                                        if is_classdef(eng, c)
                                            && eng.node_name(c).as_deref() == Some(exc)
                                        {
                                            return true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    None => {}
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// TYPE_CHECKING / sys guards (utils.py:1990-2017, 1845-1865)
// ---------------------------------------------------------------------------

/// in_type_checking_block (utils.py:1990-2017)
pub fn in_type_checking_block(eng: &Engine, caches: &LintCaches, g: GNode) -> bool {
    for anc in ancestors(eng, g) {
        let md = eng.md(anc.m);
        let NodeKind::If { test, .. } = &md.tree.nodes[anc.n.idx()].kind else {
            continue;
        };
        let test = GNode { m: anc.m, n: *test };
        let test_kind = match &md.tree.nodes[test.n.idx()].kind {
            NodeKind::Name { name } => Some((true, *name)),
            NodeKind::Attribute { attrname, expr, .. } => {
                let e = *expr;
                let a = *attrname;
                drop(md);
                if eng.md(test.m).tree.s(a) != "TYPE_CHECKING" {
                    continue;
                }
                // safe_infer(test.expr) is Module named "typing"
                if let Some(Value::Node(mg)) = safe_infer(eng, caches, GNode { m: test.m, n: e }) {
                    let mmd = eng.md(mg.m);
                    if let NodeKind::Module(d) = &mmd.tree.nodes[mg.n.idx()].kind {
                        if &*d.name == "typing" {
                            return true;
                        }
                    }
                }
                continue;
            }
            _ => None,
        };
        let Some((_, name_sym)) = test_kind else { continue };
        let nm = md.tree.s(name_sym).to_string();
        let gsym = eng.g(&md, name_sym);
        drop(md);
        if nm != "TYPE_CHECKING" {
            continue;
        }
        let lookup = eng.lookup(test, gsym);
        if lookup.1.is_empty() {
            return false; // hard return False (utils.py:2003-2005)
        }
        if let NV::N(first) = &lookup.1[0] {
            let md = eng.md(first.m);
            if let NodeKind::ImportFrom { modname, .. } = &md.tree.nodes[first.n.idx()].kind {
                if md.tree.s(*modname) == "typing" {
                    return true;
                }
            }
        }
        if let Some(v) = safe_infer(eng, caches, test) {
            if let Some(ConstValue::Bool(false)) = eng.value_const(&v) {
                return true;
            }
        }
    }
    false
}

/// is_sys_guard (utils.py:1845-1865)
pub fn is_sys_guard(eng: &Engine, if_node: GNode) -> bool {
    let md = eng.md(if_node.m);
    let NodeKind::If { test, .. } = &md.tree.nodes[if_node.n.idx()].kind else {
        return false;
    };
    let test = GNode { m: if_node.m, n: *test };
    match &md.tree.nodes[test.n.idx()].kind {
        NodeKind::Compare { left, .. } => {
            let mut value = GNode { m: if_node.m, n: *left };
            if let NodeKind::Subscript { value: v, .. } = &md.tree.nodes[value.n.idx()].kind {
                value = GNode { m: if_node.m, n: *v };
            }
            if matches!(md.tree.nodes[value.n.idx()].kind, NodeKind::Attribute { .. }) {
                drop(md);
                return as_string(eng, value) == "sys.version_info";
            }
            false
        }
        NodeKind::Attribute { .. } => {
            drop(md);
            let s = as_string(eng, test);
            s == "six.PY2" || s == "six.PY3"
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// is_terminating_func (utils.py:2211-2254)
// ---------------------------------------------------------------------------

const TERMINATING_FUNCS_QNAMES: &[&str] = &[
    "_sitebuiltins.Quitter",
    "sys.exit",
    "posix._exit",
    "nt._exit",
    "unittest.case.TestCase.fail",
];
const TYPING_NORETURN_NEVER: &[&str] = &[
    "typing.NoReturn",
    "typing_extensions.NoReturn",
    "typing.Never",
    "typing_extensions.Never",
];

pub fn is_terminating_func(eng: &Engine, caches: &LintCaches, call: GNode) -> bool {
    let md = eng.md(call.m);
    let NodeKind::Call { func, .. } = &md.tree.nodes[call.n.idx()].kind else {
        return false;
    };
    let func = GNode { m: call.m, n: *func };
    if !matches!(
        md.tree.nodes[func.n.idx()].kind,
        NodeKind::Attribute { .. } | NodeKind::Name { .. }
    ) {
        return false;
    }
    let parent = eng.parent(call);
    if let Some(p) = parent {
        if is_lambda(eng, p) {
            return false;
        }
    }
    let is_awaited = parent
        .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::Await { .. })))
        .unwrap_or(false);
    drop(md);
    let flow = eng.infer(func, &Ctx::new());
    if flow.err.is_some() && flow.vals.is_empty() {
        return false;
    }
    for inferred in &flow.vals {
        if let Some(q) = value_qname(eng, inferred) {
            if TERMINATING_FUNCS_QNAMES.contains(&q.as_str()) {
                return true;
            }
        }
        // FunctionDef with returns annotation Name inferring to NoReturn/Never
        let func_node = match inferred {
            Value::Node(g) if is_functiondef(eng, *g) => Some(*g),
            // BoundMethod(_proxied=UnboundMethod(_proxied=p)) unwrap:
            // Instance.igetattr wraps class-level UnboundMethods into
            // BoundMethods (bases._wrap_attr), so the COMMON instance
            // method access IS the double wrap. Classmethod-style BMs
            // (function_to_method: BoundMethod(func, klass), _proxied = the
            // FunctionDef directly) do NOT match pylint's pattern — mirror
            // via the bound value: instance-like bound -> unwrap, ClassDef
            // bound -> skip. DescriptorBoundMethod also skips.
            Value::BoundMethod { func, bound } => {
                let bound_is_class =
                    matches!(&**bound, Value::Node(b) if eng.kind_is(*b, |k| matches!(k, NodeKind::ClassDef(_))));
                if bound_is_class {
                    None
                } else {
                    Some(*func)
                }
            }
            _ => None,
        };
        let Some(fg) = func_node else { continue };
        let md = eng.md(fg.m);
        let (is_async, returns) = match &md.tree.nodes[fg.n.idx()].kind {
            NodeKind::FunctionDef(d) => (false, d.returns),
            NodeKind::AsyncFunctionDef(d) => (true, d.returns),
            _ => (false, None),
        };
        if is_async && !is_awaited {
            continue;
        }
        let Some(r) = returns else { continue };
        let rg = GNode { m: fg.m, n: r };
        if !matches!(md.tree.nodes[r.idx()].kind, NodeKind::Name { .. }) {
            continue;
        }
        drop(md);
        if let Some(rv) = safe_infer(eng, caches, rg) {
            if let Some(q) = value_qname(eng, &rv) {
                if TYPING_NORETURN_NEVER.contains(&q.as_str()) {
                    return true;
                }
            }
        }
    }
    // mid-stream InferenceError -> swallowed (try/except around the loop)
    false
}

// ---------------------------------------------------------------------------
// are_exclusive with exceptions (astroid node_classes.py:116-186)
// ---------------------------------------------------------------------------

/// astroid.are_exclusive(stmt1, stmt2) with exceptions=None: the If branch
/// is ENABLED ("test" in attrs -> False; different branches -> True) and
/// handler.catch(None) is always True.
pub fn are_exclusive(eng: &Engine, stmt1: GNode, stmt2: GNode) -> bool {
    let mut stmt1_parents: FxHashMap<GNode, GNode> = FxHashMap::default();
    let mut previous = stmt1;
    let mut cur = stmt1;
    while let Some(node) = eng.parent(cur) {
        stmt1_parents.insert(node, previous);
        previous = node;
        cur = node;
    }
    let mut previous2 = stmt2;
    let mut cur2 = stmt2;
    while let Some(node) = eng.parent(cur2) {
        if let Some(&child1) = stmt1_parents.get(&node) {
            if eng.kind_is(node, |k| matches!(k, NodeKind::If { .. })) {
                let c2 = eng.locate_child(node, previous2);
                let c1 = eng.locate_child(node, child1);
                if c1 == Some("test") || c2 == Some("test") {
                    return false;
                }
                if c1 != c2 {
                    return true;
                }
            } else if eng.kind_is(node, |k| matches!(k, NodeKind::Try(_) | NodeKind::TryStar(_))) {
                let c2 = eng.locate_child(node, previous2);
                let c1 = eng.locate_child(node, child1);
                // astroid locate_child returns the FIELD LIST for list
                // members, so `c1node is not c2node` means "different
                // fields" (node_classes.py:101-113 + 156)
                if c1 != c2 {
                    let first_in_body_caught = c2 == Some("handlers") && c1 == Some("body");
                    let second_in_body_caught = c2 == Some("body") && c1 == Some("handlers");
                    let first_in_else = c2 == Some("handlers") && c1 == Some("orelse");
                    let second_in_else = c2 == Some("orelse") && c1 == Some("handlers");
                    if first_in_body_caught
                        || second_in_body_caught
                        || first_in_else
                        || second_in_else
                    {
                        return true;
                    }
                } else if c2 == Some("handlers") && c1 == Some("handlers") {
                    return previous2 != child1;
                }
            }
            return false;
        }
        previous2 = node;
        cur2 = node;
    }
    false
}

/// NodeNG.next_sibling(): delegate up to the enclosing statement, then the
/// next statement in the parent's child sequence (None at end / for Module).
pub fn next_sibling(eng: &Engine, g: GNode) -> Option<GNode> {
    let stmt = eng.statement(g)?;
    let parent = eng.parent(stmt)?;
    let md = eng.md(parent.m);
    let seqs = child_sequences(&md.tree.nodes[parent.n.idx()].kind);
    for seq in seqs {
        if let Some(pos) = seq.iter().position(|&n| n == stmt.n) {
            return seq.get(pos + 1).map(|&n| GNode { m: stmt.m, n });
        }
    }
    None
}

pub fn are_exclusive_exc(eng: &Engine, stmt1: GNode, stmt2: GNode, exceptions: &[&str]) -> bool {
    let mut stmt1_parents: FxHashMap<GNode, GNode> = FxHashMap::default();
    let mut previous = stmt1;
    let mut cur = stmt1;
    while let Some(node) = eng.parent(cur) {
        stmt1_parents.insert(node, previous);
        previous = node;
        cur = node;
    }
    let mut previous2 = stmt2;
    let mut cur2 = stmt2;
    while let Some(node) = eng.parent(cur2) {
        if let Some(&child1) = stmt1_parents.get(&node) {
            // exceptions is not None: the If branch is DISABLED
            if eng.kind_is(node, |k| matches!(k, NodeKind::Try(_))) {
                let c2 = eng.locate_child(node, previous2);
                let c1 = eng.locate_child(node, child1);
                if c1 != c2 {
                    let first_in_body_caught = c2 == Some("handlers")
                        && c1 == Some("body")
                        && handler_catch(eng, previous2, exceptions);
                    let second_in_body_caught = c2 == Some("body")
                        && c1 == Some("handlers")
                        && handler_catch(eng, child1, exceptions);
                    let first_in_else = c2 == Some("handlers") && c1 == Some("orelse");
                    let second_in_else = c2 == Some("orelse") && c1 == Some("handlers");
                    if first_in_body_caught
                        || second_in_body_caught
                        || first_in_else
                        || second_in_else
                    {
                        return true;
                    }
                } else if c2 == Some("handlers") && c1 == Some("handlers") {
                    return previous2 != child1;
                }
            }
            return false;
        }
        previous2 = node;
        cur2 = node;
    }
    false
}

// ---------------------------------------------------------------------------
// is_defined_before (utils.py:353-408) + defnode_in_scope (utils.py:305-350)
// ---------------------------------------------------------------------------

fn defnode_in_scope(eng: &Engine, var_node: GNode, varname: GSym, scope: GNode) -> Option<GNode> {
    let md = eng.md(scope.m);
    let k = &md.tree.nodes[scope.n.idx()].kind;
    match k {
        NodeKind::If { body, .. } => {
            let body = body.clone();
            drop(md);
            for n in body {
                let g = GNode { m: scope.m, n };
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Nonlocal { names } => {
                        if names.iter().any(|&s| eng.g(&md, s) == varname) {
                            return Some(g);
                        }
                    }
                    NodeKind::Assign { targets, .. } => {
                        for &t in targets {
                            if let NodeKind::AssignName { name } = &md.tree.nodes[t.idx()].kind {
                                if eng.g(&md, *name) == varname {
                                    return Some(GNode { m: g.m, n: t });
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        NodeKind::ListComp(_)
        | NodeKind::SetComp(_)
        | NodeKind::DictComp(_)
        | NodeKind::GeneratorExp(_)
        | NodeKind::For(_)
        | NodeKind::AsyncFor(_) => {
            drop(md);
            for n in preorder(eng, scope) {
                if name_gsym(eng, n) == Some(varname)
                    && eng.kind_is(n, |k| matches!(k, NodeKind::AssignName { .. }))
                {
                    return Some(n);
                }
            }
            None
        }
        NodeKind::With(d) | NodeKind::AsyncWith(d) => {
            let items = d.items.clone();
            drop(md);
            for (expr, ids) in items {
                let eg = GNode { m: scope.m, n: expr };
                if eng.parent_of(eg, var_node) {
                    break;
                }
                if let Some(idn) = ids {
                    let ig = GNode { m: scope.m, n: idn };
                    if eng.kind_is(ig, |k| matches!(k, NodeKind::AssignName { .. }))
                        && name_gsym(eng, ig) == Some(varname)
                    {
                        return Some(ig);
                    }
                }
            }
            None
        }
        NodeKind::Lambda(_) | NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => {
            let args_n = match k {
                NodeKind::Lambda(d) => d.args,
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
                _ => unreachable!(),
            };
            drop(md);
            let ag = GNode { m: scope.m, n: args_n };
            if args_is_argument(eng, ag, varname) {
                if eng.parent_of(ag, var_node) {
                    // scope.args.default_value(varname): on success redo in
                    // the parent scope
                    if args_has_default(eng, ag, varname) {
                        let outer = eng.parent(scope)?;
                        return defnode_in_scope(eng, var_node, varname, outer);
                    }
                }
                return Some(scope);
            }
            // getattr(scope, "name", None) == varname
            let md = eng.md(scope.m);
            if let NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) =
                &md.tree.nodes[scope.n.idx()].kind
            {
                if eng.g(&md, d.name) == varname {
                    return Some(scope);
                }
            }
            None
        }
        NodeKind::ExceptHandler { name, .. } => {
            let name = *name;
            drop(md);
            if let Some(nn) = name {
                let ng = GNode { m: scope.m, n: nn };
                if name_gsym(eng, ng) == Some(varname) {
                    return Some(ng);
                }
            }
            None
        }
        _ => None,
    }
}

fn with_args_data<R>(
    eng: &Engine,
    args: GNode,
    f: impl FnOnce(&pyast::tree::ArgumentsData, &pyast::tree::Tree) -> R,
) -> Option<R> {
    let md = eng.md(args.m);
    match &md.tree.nodes[args.n.idx()].kind {
        NodeKind::Arguments(d) => Some(f(d, &md.tree)),
        _ => None,
    }
}

/// Arguments.is_argument(name)
fn args_is_argument(eng: &Engine, args: GNode, varname: GSym) -> bool {
    with_args_data(eng, args, |d, tree| {
        let m = |n: &pyast::NodeId| -> bool {
            if let NodeKind::AssignName { name } = &tree.nodes[n.idx()].kind {
                eng.g(&eng.md(args.m), *name) == varname
            } else {
                false
            }
        };
        if d.posonlyargs.iter().any(|n| m(n))
            || d.args.iter().any(|n| m(n))
            || d.kwonlyargs.iter().any(|n| m(n))
        {
            return true;
        }
        let md = eng.md(args.m);
        if let Some(v) = d.vararg {
            if eng.g(&md, v) == varname {
                return true;
            }
        }
        if let Some(kw) = d.kwarg {
            if eng.g(&md, kw) == varname {
                return true;
            }
        }
        false
    })
    .unwrap_or(false)
}

/// Arguments.default_value(name) would NOT raise NoDefault
fn args_has_default(eng: &Engine, args: GNode, varname: GSym) -> bool {
    with_args_data(eng, args, |d, tree| {
        let md = eng.md(args.m);
        let name_at = |n: &pyast::NodeId| -> Option<GSym> {
            if let NodeKind::AssignName { name } = &tree.nodes[n.idx()].kind {
                Some(eng.g(&md, *name))
            } else {
                None
            }
        };
        let combined: Vec<Option<GSym>> = d
            .posonlyargs
            .iter()
            .chain(d.args.iter())
            .map(name_at)
            .collect();
        if let Some(i) = combined.iter().position(|s| *s == Some(varname)) {
            let no_default = combined.len() - d.defaults.len();
            return i >= no_default;
        }
        for (i, n) in d.kwonlyargs.iter().enumerate() {
            if name_at(n) == Some(varname) {
                return d.kw_defaults.get(i).map(|o| o.is_some()).unwrap_or(false);
            }
        }
        false
    })
    .unwrap_or(false)
}

/// utils.is_defined_before (utils.py:353-408)
pub fn is_defined_before(eng: &Engine, var_node: GNode) -> bool {
    let Some(varname) = name_gsym(eng, var_node) else { return false };
    for parent in ancestors(eng, var_node) {
        let Some(defnode) = defnode_in_scope(eng, var_node, varname, parent) else {
            continue;
        };
        let defnode_scope = eng.scope(defnode);
        let scope_is_callable_or_comp = eng.is_comprehension_scope(defnode_scope)
            || is_lambda(eng, defnode_scope)
            || is_functiondef(eng, defnode_scope);
        if scope_is_callable_or_comp {
            if is_functiondef(eng, defnode_scope) {
                let var_node_scope = eng.scope(var_node);
                if var_node_scope != defnode_scope && is_functiondef(eng, var_node_scope) {
                    return false;
                }
            }
            return true;
        }
        if lineno(eng, defnode) < lineno(eng, var_node) {
            return true;
        }
        // defnode and var_node on the same line
        for defnode_anc in ancestors(eng, defnode) {
            if lineno(eng, defnode_anc) != lineno(eng, var_node) {
                continue;
            }
            if eng.kind_is(defnode_anc, |k| {
                matches!(
                    k,
                    NodeKind::For(_)
                        | NodeKind::AsyncFor(_)
                        | NodeKind::While { .. }
                        | NodeKind::With(_)
                        | NodeKind::AsyncWith(_)
                        | NodeKind::Try(_)
                        | NodeKind::ExceptHandler { .. }
                )
            }) {
                return true;
            }
        }
    }
    // semicolon-separated earlier statements on the same line
    let Some(stmt) = eng.statement(var_node) else { return false };
    let line = lineno(eng, stmt);
    let mut node = previous_sibling(eng, stmt);
    while let Some(n) = node {
        if lineno(eng, n) != line {
            break;
        }
        for sub in preorder(eng, n) {
            let md = eng.md(sub.m);
            match &md.tree.nodes[sub.n.idx()].kind {
                NodeKind::AssignName { name } => {
                    if eng.g(&md, *name) == varname {
                        return true;
                    }
                }
                NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } => {
                    for (nm, asnm) in names {
                        let eff = asnm.unwrap_or(*nm);
                        if eng.g(&md, eff) == varname {
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        node = previous_sibling(eng, n);
    }
    false
}

// ---------------------------------------------------------------------------
// annotation-context helpers
// ---------------------------------------------------------------------------

/// is_postponed_evaluation_enabled (utils.py:1608-1611)
pub fn is_postponed_evaluation_enabled(eng: &Engine, mid: pyinfer::value::ModId) -> bool {
    let md = eng.md(mid);
    if let NodeKind::Module(d) = &md.tree.nodes[0].kind {
        return d
            .future_imports
            .iter()
            .any(|&s| md.tree.s(s) == "annotations");
    }
    false
}

/// is_node_in_type_annotation_context (utils.py:1614-1637)
pub fn is_node_in_type_annotation_context(eng: &Engine, node: GNode) -> bool {
    let mut current = node;
    let mut parent = match eng.parent(node) {
        Some(p) => p,
        None => return false,
    };
    loop {
        let md = eng.md(parent.m);
        match &md.tree.nodes[parent.n.idx()].kind {
            NodeKind::AnnAssign { annotation, .. } if *annotation == current.n => return true,
            NodeKind::Arguments(d) => {
                let in_anns = d
                    .annotations
                    .iter()
                    .chain(d.posonlyargs_annotations.iter())
                    .chain(d.kwonlyargs_annotations.iter())
                    .any(|o| *o == Some(current.n))
                    || d.varargannotation == Some(current.n)
                    || d.kwargannotation == Some(current.n);
                if in_anns {
                    return true;
                }
            }
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                if d.returns == Some(current.n) {
                    return true;
                }
            }
            NodeKind::Module(_) => return false,
            _ => {}
        }
        drop(md);
        current = parent;
        parent = match eng.parent(parent) {
            Some(p) => p,
            None => return false,
        };
        if is_module(eng, parent) {
            // loop checks AFTER the hop: `if isinstance(parent_node, Module): return False`
            return false;
        }
    }
}

/// is_node_in_pep695_type_context (utils.py:1640-1644)
pub fn is_node_in_pep695_type_context(eng: &Engine, node: GNode) -> bool {
    first_ancestor(eng, node, |k| {
        matches!(
            k,
            NodeKind::TypeAlias { .. }
                | NodeKind::TypeVar { .. }
                | NodeKind::ParamSpec { .. }
                | NodeKind::TypeVarTuple { .. }
        )
    })
    .is_some()
}

/// utils.is_ancestor_name (utils.py:447-453): node occurs (by identity)
/// among Name nodes inside frame.bases.
pub fn is_ancestor_name(eng: &Engine, frame: GNode, node: GNode) -> bool {
    let md = eng.md(frame.m);
    let NodeKind::ClassDef(d) = &md.tree.nodes[frame.n.idx()].kind else {
        return false;
    };
    let bases = d.bases.clone();
    drop(md);
    for b in bases {
        for n in preorder(eng, GNode { m: frame.m, n: b }) {
            if n == node && eng.kind_is(n, |k| matches!(k, NodeKind::Name { .. })) {
                return true;
            }
        }
    }
    false
}

/// utils.is_default_argument (utils.py:411-427)
pub fn is_default_argument(eng: &Engine, node: GNode, scope: Option<GNode>) -> bool {
    let scope = scope.unwrap_or_else(|| eng.scope(node));
    let md = eng.md(scope.m);
    let args_n = match &md.tree.nodes[scope.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
        NodeKind::Lambda(d) => d.args,
        _ => return false,
    };
    let NodeKind::Arguments(ad) = &md.tree.nodes[args_n.idx()].kind else {
        return false;
    };
    let defaults: Vec<pyast::NodeId> = ad
        .defaults
        .iter()
        .copied()
        .chain(ad.kw_defaults.iter().filter_map(|o| *o))
        .collect();
    drop(md);
    for d in defaults {
        for n in preorder(eng, GNode { m: scope.m, n: d }) {
            if n == node && eng.kind_is(n, |k| matches!(k, NodeKind::Name { .. })) {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// message-template formatting + python repr
// ---------------------------------------------------------------------------

/// Python `repr(str)`
pub fn py_repr_str(s: &str) -> String {
    pyast::pyrepr::repr_str(s)
}

/// `template % args` for the message templates in scope (%s / %r / %d / %%).
pub fn format_template(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut it = template.chars().peekable();
    let mut ai = 0usize;
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('%') => out.push('%'),
            Some('s') | Some('d') => {
                out.push_str(args.get(ai).copied().unwrap_or(""));
                ai += 1;
            }
            Some('r') => {
                out.push_str(&py_repr_str(args.get(ai).copied().unwrap_or("")));
                ai += 1;
            }
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// as_string — astroid AsStringVisitor subset (self-consistent rendering)
// ---------------------------------------------------------------------------

pub fn as_string(eng: &Engine, g: GNode) -> String {
    let md = eng.md(g.m);
    let s = |n: pyast::NodeId| as_string(eng, GNode { m: g.m, n });
    let tree = &md.tree;
    match &tree.nodes[g.n.idx()].kind {
        NodeKind::Name { name } | NodeKind::AssignName { name } | NodeKind::DelName { name } => {
            tree.s(*name).to_string()
        }
        NodeKind::Attribute { expr, attrname, .. } | NodeKind::AssignAttr { expr, attrname } => {
            // visit_attribute (as_string.py): receiver gets precedence parens;
            // a bare-digit receiver is also parenthesized (1 .real).
            let mut left = precedence_parens(eng, g, GNode { m: g.m, n: *expr }, true);
            if !left.is_empty() && left.chars().all(|c| c.is_ascii_digit()) {
                left = format!("({left})");
            }
            format!("{}.{}", left, tree.s(*attrname))
        }
        // visit_const (as_string.py): Ellipsis -> "...", else repr(value).
        NodeKind::Const(ConstValue::Ellipsis) => "...".to_string(),
        NodeKind::Const(c) => const_repr(c),
        NodeKind::Call { func, args, keywords } => {
            let mut parts: Vec<String> = args.iter().map(|&a| s(a)).collect();
            for &kw in keywords {
                parts.push(s(kw));
            }
            format!("{}({})", s(*func), parts.join(", "))
        }
        NodeKind::Keyword { arg, value } => match arg {
            Some(a) => format!("{}={}", tree.s(*a), s(*value)),
            None => format!("**{}", s(*value)),
        },
        NodeKind::BoolOp { op, values } => {
            let parts: Vec<String> = values.iter().map(|&v| paren_if_needed(eng, g, GNode { m: g.m, n: v })).collect();
            parts.join(&format!(" {op} "))
        }
        NodeKind::BinOp { left, op, right } => {
            let l = precedence_parens(eng, g, GNode { m: g.m, n: *left }, true);
            let r = precedence_parens(eng, g, GNode { m: g.m, n: *right }, false);
            if &**op == "**" {
                format!("{l}{op}{r}")
            } else {
                format!("{l} {op} {r}")
            }
        }
        NodeKind::UnaryOp { op, operand } => {
            let o = paren_if_needed(eng, g, GNode { m: g.m, n: *operand });
            if &**op == "not" {
                format!("not {o}")
            } else {
                format!("{op}{o}")
            }
        }
        NodeKind::Compare { left, ops } => {
            let mut out = precedence_parens(eng, g, GNode { m: g.m, n: *left }, true);
            for (op, r) in ops {
                out.push_str(&format!(
                    " {} {}",
                    op,
                    precedence_parens(eng, g, GNode { m: g.m, n: *r }, false)
                ));
            }
            out
        }
        NodeKind::Subscript { value, slice, .. } => {
            // visit_subscript (as_string.py): value gets precedence parens;
            // a non-empty Tuple slice has its outer parens stripped
            // (a[0, idx] not a[(0, idx)]).
            let mut idxstr = s(*slice);
            let is_nonempty_tuple = matches!(
                &tree.nodes[slice.idx()].kind,
                NodeKind::Tuple { elts, .. } if !elts.is_empty()
            );
            if is_nonempty_tuple {
                // strip one leading '(' and trailing ')'
                let chars: Vec<char> = idxstr.chars().collect();
                if chars.len() >= 2 && chars[0] == '(' && *chars.last().unwrap() == ')' {
                    idxstr = chars[1..chars.len() - 1].iter().collect();
                }
            }
            format!("{}[{}]", precedence_parens(eng, g, GNode { m: g.m, n: *value }, true), idxstr)
        }
        NodeKind::Slice { lower, upper, step } => {
            let f = |o: &Option<pyast::NodeId>| o.map(s).unwrap_or_default();
            match step {
                Some(st) => format!("{}:{}:{}", f(lower), f(upper), s(*st)),
                None => format!("{}:{}", f(lower), f(upper)),
            }
        }
        NodeKind::Tuple { elts, .. } => {
            if elts.is_empty() {
                "()".to_string()
            } else if elts.len() == 1 {
                format!("({}, )", s(elts[0]))
            } else {
                format!("({})", elts.iter().map(|&e| s(e)).collect::<Vec<_>>().join(", "))
            }
        }
        NodeKind::List { elts, .. } => {
            format!("[{}]", elts.iter().map(|&e| s(e)).collect::<Vec<_>>().join(", "))
        }
        NodeKind::Set { elts } => {
            format!("{{{}}}", elts.iter().map(|&e| s(e)).collect::<Vec<_>>().join(", "))
        }
        NodeKind::Dict { items } => {
            let parts: Vec<String> = items
                .iter()
                .map(|&(k, v)| {
                    if matches!(tree.nodes[k.idx()].kind, NodeKind::DictUnpack) {
                        format!("**{}", s(v))
                    } else {
                        format!("{}: {}", s(k), s(v))
                    }
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        NodeKind::Starred { value, .. } => format!("*{}", s(*value)),
        // Import/ImportFrom (astroid as_string.py visit_import/visit_importfrom)
        NodeKind::Import { names } => {
            let parts: Vec<String> = names
                .iter()
                .map(|(n, alias)| match alias {
                    Some(a) => format!("{} as {}", tree.s(*n), tree.s(*a)),
                    None => tree.s(*n).to_string(),
                })
                .collect();
            format!("import {}", parts.join(", "))
        }
        NodeKind::ImportFrom { modname, names, level } => {
            let parts: Vec<String> = names
                .iter()
                .map(|(n, alias)| match alias {
                    Some(a) => format!("{} as {}", tree.s(*n), tree.s(*a)),
                    None => tree.s(*n).to_string(),
                })
                .collect();
            let dots = ".".repeat(level.unwrap_or(0) as usize);
            format!("from {}{} import {}", dots, tree.s(*modname), parts.join(", "))
        }
        NodeKind::NamedExpr { target, value } => format!("({} := {})", s(*target), s(*value)),
        NodeKind::IfExp { test, body, orelse } => {
            format!("{} if {} else {}", s(*body), s(*test), s(*orelse))
        }
        NodeKind::Await { value } => format!("await {}", s(*value)),
        NodeKind::Yield { value } => match value {
            Some(v) => format!("yield {}", s(*v)),
            None => "yield".to_string(),
        },
        NodeKind::YieldFrom { value } => format!("yield from {}", s(*value)),
        NodeKind::Lambda(d) => {
            // visit_lambda (as_string.py): no space before ':' when no args.
            let args = args_as_string(eng, GNode { m: g.m, n: d.args });
            if args.is_empty() {
                format!("lambda: {}", s(d.body))
            } else {
                format!("lambda {}: {}", args, s(d.body))
            }
        }
        NodeKind::JoinedStr { values } => {
            // astroid visit_joinedstr (as_string.py): repr(Const)[1:-1] with
            // brace-doubling for literal parts, FormattedValue accept() for
            // the rest, then pick the first quote not present in the string.
            let mut string = String::new();
            for &v in values {
                match &tree.nodes[v.idx()].kind {
                    NodeKind::Const(ConstValue::Str(st)) => {
                        let r = pyast::pyrepr::repr_str(st);
                        // strip the leading/trailing quote char (one each)
                        let inner = &r[1..r.len() - 1];
                        string.push_str(&inner.replace('{', "{{").replace('}', "}}"));
                    }
                    _ => string.push_str(&s(v)),
                }
            }
            let quote = ["'", "\"", "\"\"\"", "'''"]
                .iter()
                .find(|q| !string.contains(*q))
                .copied()
                .unwrap_or("'");
            format!("f{q}{string}{q}", q = quote)
        }
        NodeKind::FormattedValue { value, conversion, format_spec } => {
            let mut result = s(*value);
            if *conversion >= 0 {
                if let Some(c) = char::from_u32(*conversion as u32) {
                    result.push('!');
                    result.push(c);
                }
            }
            if let Some(spec) = format_spec {
                // format_spec is itself a JoinedStr; strip the leading f+quote
                // (2 chars) and trailing quote (1 char).
                let spec_str = s(*spec);
                let chars: Vec<char> = spec_str.chars().collect();
                let trimmed: String = if chars.len() >= 3 {
                    chars[2..chars.len() - 1].iter().collect()
                } else {
                    String::new()
                };
                result.push(':');
                result.push_str(&trimmed);
            }
            format!("{{{result}}}")
        }
        NodeKind::Comprehension { target, iter, ifs, is_async } => {
            let ifs_str: String = ifs.iter().map(|&n| format!(" if {}", s(n))).collect();
            let generated = format!("for {} in {}{}", s(*target), s(*iter), ifs_str);
            if *is_async {
                format!("async {generated}")
            } else {
                generated
            }
        }
        NodeKind::ListComp(d) => {
            let gens: Vec<String> = d.generators.iter().map(|&n| s(n)).collect();
            format!("[{} {}]", s(d.elt), gens.join(" "))
        }
        NodeKind::SetComp(d) => {
            let gens: Vec<String> = d.generators.iter().map(|&n| s(n)).collect();
            format!("{{{} {}}}", s(d.elt), gens.join(" "))
        }
        NodeKind::GeneratorExp(d) => {
            let gens: Vec<String> = d.generators.iter().map(|&n| s(n)).collect();
            format!("({} {})", s(d.elt), gens.join(" "))
        }
        NodeKind::DictComp(d) => {
            let gens: Vec<String> = d.generators.iter().map(|&n| s(n)).collect();
            format!("{{{}: {} {}}}", s(d.key), s(d.value), gens.join(" "))
        }
        _ => {
            // fallback: deterministic join of children (self-consistent)
            let kids = tree.children(g.n);
            kids.iter()
                .map(|&c| as_string(eng, GNode { m: g.m, n: c }))
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

/// astroid OP_PRECEDENCE (nodes/const.py): operator/node-class -> precedence
/// rank (lower = binds looser). Highest rank (never wrapped) = the table len.
fn op_precedence_rank(eng: &Engine, g: GNode) -> i32 {
    const TABLE_LEN: i32 = 15; // 15 precedence levels (0..=14)
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Lambda(_) => 0,
        NodeKind::IfExp { .. } => 1,
        NodeKind::BoolOp { op, .. } => {
            if &**op == "or" {
                2
            } else {
                3
            }
        }
        NodeKind::UnaryOp { op, .. } => {
            if &**op == "not" {
                4 // "not"
            } else {
                12 // "UnaryOp" (+, -, ~)
            }
        }
        NodeKind::Compare { .. } => 5,
        NodeKind::BinOp { op, .. } => match &**op {
            "|" => 6,
            "^" => 7,
            "&" => 8,
            "<<" | ">>" => 9,
            "+" | "-" => 10,
            "*" | "@" | "/" | "//" | "%" => 11,
            "**" => 13,
            _ => TABLE_LEN,
        },
        NodeKind::Await { .. } => 14,
        _ => TABLE_LEN,
    }
}

/// op_left_associative (node_classes.py): everything left-assoc except `**`
/// and IfExp.
fn op_left_associative(eng: &Engine, g: GNode) -> bool {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::BinOp { op, .. } => &**op != "**",
        NodeKind::IfExp { .. } => false,
        _ => true,
    }
}

/// as_string._precedence_parens / _should_wrap (as_string.py:54-85).
fn precedence_parens(eng: &Engine, parent: GNode, child: GNode, is_left: bool) -> String {
    let np = op_precedence_rank(eng, parent);
    let cp = op_precedence_rank(eng, child);
    let wrap = if np > cp {
        true
    } else {
        np == cp && is_left != op_left_associative(eng, parent)
    };
    let s = as_string(eng, child);
    if wrap {
        format!("({s})")
    } else {
        s
    }
}

/// Back-compat wrapper used by BoolOp/UnaryOp/Compare (is_left = True).
fn paren_if_needed(eng: &Engine, parent: GNode, child: GNode) -> String {
    precedence_parens(eng, parent, child, true)
}

fn args_as_string(eng: &Engine, args: GNode) -> String {
    let md = eng.md(args.m);
    let NodeKind::Arguments(d) = &md.tree.nodes[args.n.idx()].kind else {
        return String::new();
    };
    let mut parts = Vec::new();
    for &a in d.posonlyargs.iter().chain(d.args.iter()).chain(d.kwonlyargs.iter()) {
        if let NodeKind::AssignName { name } = &md.tree.nodes[a.idx()].kind {
            parts.push(md.tree.s(*name).to_string());
        }
    }
    parts.join(", ")
}

fn const_repr(c: &ConstValue) -> String {
    match c {
        ConstValue::None => "None".to_string(),
        ConstValue::NotImplemented => "NotImplemented".to_string(),
        ConstValue::Ellipsis => "Ellipsis".to_string(),
        ConstValue::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        ConstValue::Int(IntValue::Small(i)) => i.to_string(),
        ConstValue::Int(IntValue::Big(s)) => s.to_string(),
        ConstValue::Float(f) => pyast::pyrepr::repr_float(*f),
        ConstValue::Complex { real, imag } => {
            if *real == 0.0 {
                format!("{imag}j")
            } else {
                format!("({real}+{imag}j)")
            }
        }
        ConstValue::Str(s) => pyast::pyrepr::repr_str(s),
        ConstValue::StrSurrogate(p) => pyast::pyrepr::repr_str_points(p),
        ConstValue::Bytes(b) => pyinfer::dump::repr_bytes_pub(b),
    }
}

// ---------------------------------------------------------------------------
// python-value equality keys for `{const.value for ...}` set comparisons
// ---------------------------------------------------------------------------

/// Normalized hash key with Python equality semantics (True == 1, 1.0 == 1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PyKey {
    None,
    NotImplemented,
    Ellipsis,
    Int(String),
    Str(String),
    Bytes(Vec<u8>),
    FloatBits(u64),
}

pub fn py_key(c: &ConstValue) -> PyKey {
    match c {
        ConstValue::None => PyKey::None,
        ConstValue::NotImplemented => PyKey::NotImplemented,
        ConstValue::Ellipsis => PyKey::Ellipsis,
        ConstValue::Bool(b) => PyKey::Int(if *b { "1" } else { "0" }.to_string()),
        ConstValue::Int(IntValue::Small(i)) => PyKey::Int(i.to_string()),
        ConstValue::Int(IntValue::Big(s)) => PyKey::Int(s.to_string()),
        ConstValue::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                PyKey::Int(format!("{}", *f as i64))
            } else {
                PyKey::FloatBits(f.to_bits())
            }
        }
        ConstValue::Complex { real, imag } => {
            PyKey::FloatBits(real.to_bits() ^ imag.to_bits().rotate_left(17))
        }
        ConstValue::Str(s) => PyKey::Str(s.to_string()),
        ConstValue::StrSurrogate(p) => PyKey::Str(format!("{p:?}")),
        ConstValue::Bytes(b) => PyKey::Bytes(b.to_vec()),
    }
}

/// `bool(const.value)` truthiness
pub fn const_truthy(c: &ConstValue) -> bool {
    pyinfer::infer::const_truth(c)
}

/// Marker for Ctx import so callers don't need pyinfer::ctx directly.
pub fn fresh_ctx() -> Rc<Ctx> {
    Ctx::new()
}

/// helper: thread-local-free tick source for ad-hoc LRUs
pub struct Tick(pub Cell<u64>);

/// strip Load/Store distinction helpers
pub fn is_store_container(k: &NodeKind) -> bool {
    matches!(k, NodeKind::Tuple { ctx: ExprCtx::Store, .. } | NodeKind::List { ctx: ExprCtx::Store, .. })
}
