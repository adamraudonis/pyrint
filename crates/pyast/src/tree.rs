//! The astroid-equivalent tree.
//!
//! Nodes live in an arena (`Tree.nodes`); `NodeId(0)` is the Module.
//! Positions mirror astroid 4.0.4 exactly:
//! - `fromlineno`: astroid's fromlineno (Module = 0; def/class = keyword line)
//! - `col_offset`/`end_col_offset`: byte columns, `-1` encodes Python `None`
//! - `end_lineno`: `0` encodes `None`
//! - `tolineno`: astroid's tolineno (recursive last-child line)
//!
//! Children order replicates astroid's `get_children()` per node class.

use std::fmt::Write as _;

use indexmap::IndexMap;
use rustc_hash::FxHashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const MODULE: NodeId = NodeId(0);
    pub fn idx(self) -> usize {
        self.0 as usize
    }
}

/// Interned string symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sym(pub u32);

#[derive(Default)]
pub struct Interner {
    map: FxHashMap<Box<str>, Sym>,
    vec: Vec<Box<str>>,
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&sym) = self.map.get(s) {
            return sym;
        }
        let sym = Sym(self.vec.len() as u32);
        let boxed: Box<str> = s.into();
        self.vec.push(boxed.clone());
        self.map.insert(boxed, sym);
        sym
    }
    pub fn get(&self, sym: Sym) -> &str {
        &self.vec[sym.0 as usize]
    }
    pub fn len(&self) -> usize {
        self.vec.len()
    }
    pub fn is_empty(&self) -> bool {
        self.vec.is_empty()
    }
}

/// Python constant value (astroid Const.value).
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    None,
    /// only created synthetically by pyinfer (operator protocol results);
    /// never produced by the parser
    NotImplemented,
    Ellipsis,
    Bool(bool),
    Int(IntValue),
    Float(f64),
    Complex { real: f64, imag: f64 },
    Str(Box<str>),
    /// String containing lone surrogates (e.g. "\ud800" escapes), which
    /// Rust strings cannot hold. Stored as raw code points; only the dump
    /// (length + repr) consumes this.
    StrSurrogate(Box<[u32]>),
    Bytes(Box<[u8]>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum IntValue {
    Small(i64),
    /// Decimal digits for ints that do not fit i64.
    Big(Box<str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    Load,
    Store,
    Del,
}

/// All astroid node kinds. Payload boxed where large.
#[derive(Debug)]
pub enum NodeKind {
    Module(Box<ModuleData>),
    // ---- statements ----
    FunctionDef(Box<FunctionData>),
    AsyncFunctionDef(Box<FunctionData>),
    ClassDef(Box<ClassData>),
    Return { value: Option<NodeId> },
    Delete { targets: Vec<NodeId> },
    Assign { targets: Vec<NodeId>, value: NodeId },
    AugAssign { target: NodeId, op: Box<str>, value: NodeId },
    AnnAssign { target: NodeId, annotation: NodeId, value: Option<NodeId>, simple: bool },
    TypeAlias { name: NodeId, type_params: Vec<NodeId>, value: NodeId },
    For(Box<ForData>),
    AsyncFor(Box<ForData>),
    While { test: NodeId, body: Vec<NodeId>, orelse: Vec<NodeId> },
    If { test: NodeId, body: Vec<NodeId>, orelse: Vec<NodeId> },
    With(Box<WithData>),
    AsyncWith(Box<WithData>),
    Match { subject: NodeId, cases: Vec<NodeId> },
    Raise { exc: Option<NodeId>, cause: Option<NodeId> },
    Try(Box<TryData>),
    TryStar(Box<TryData>),
    Assert { test: NodeId, fail: Option<NodeId> },
    Import { names: Vec<(Sym, Option<Sym>)> },
    ImportFrom { modname: Sym, names: Vec<(Sym, Option<Sym>)>, level: Option<u32> },
    Global { names: Vec<Sym> },
    Nonlocal { names: Vec<Sym> },
    Expr { value: NodeId },
    Pass,
    Break,
    Continue,
    // ---- match patterns ----
    MatchCase { pattern: NodeId, guard: Option<NodeId>, body: Vec<NodeId> },
    MatchValue { value: NodeId },
    MatchSingleton { value: ConstValue },
    MatchSequence { patterns: Vec<NodeId> },
    MatchMapping { keys: Vec<NodeId>, patterns: Vec<NodeId>, rest: Option<NodeId> },
    MatchClass { cls: NodeId, patterns: Vec<NodeId>, kwd_attrs: Vec<Sym>, kwd_patterns: Vec<NodeId> },
    MatchStar { name: Option<NodeId> },
    MatchAs { pattern: Option<NodeId>, name: Option<NodeId> },
    MatchOr { patterns: Vec<NodeId> },
    // ---- expressions ----
    BoolOp { op: Box<str>, values: Vec<NodeId> },
    NamedExpr { target: NodeId, value: NodeId },
    BinOp { left: NodeId, op: Box<str>, right: NodeId },
    UnaryOp { op: Box<str>, operand: NodeId },
    Lambda(Box<LambdaData>),
    IfExp { test: NodeId, body: NodeId, orelse: NodeId },
    Dict { items: Vec<(NodeId, NodeId)> },
    Set { elts: Vec<NodeId> },
    ListComp(Box<CompData>),
    SetComp(Box<CompData>),
    DictComp(Box<DictCompData>),
    GeneratorExp(Box<CompData>),
    Await { value: NodeId },
    Yield { value: Option<NodeId> },
    YieldFrom { value: NodeId },
    Compare { left: NodeId, ops: Vec<(Box<str>, NodeId)> },
    Call { func: NodeId, args: Vec<NodeId>, keywords: Vec<NodeId> },
    FormattedValue { value: NodeId, conversion: i32, format_spec: Option<NodeId> },
    JoinedStr { values: Vec<NodeId> },
    Const(ConstValue),
    Starred { value: NodeId, ctx: Ctx },
    Attribute { expr: NodeId, attrname: Sym, ctx: Ctx },
    Subscript { value: NodeId, slice: NodeId, ctx: Ctx },
    Name { name: Sym },
    AssignName { name: Sym },
    DelName { name: Sym },
    List { elts: Vec<NodeId>, ctx: Ctx },
    Tuple { elts: Vec<NodeId>, ctx: Ctx },
    Slice { lower: Option<NodeId>, upper: Option<NodeId>, step: Option<NodeId> },
    // ---- helpers ----
    Comprehension { target: NodeId, iter: NodeId, ifs: Vec<NodeId>, is_async: bool },
    ExceptHandler { type_: Option<NodeId>, name: Option<NodeId>, body: Vec<NodeId> },
    Arguments(Box<ArgumentsData>),
    Keyword { arg: Option<Sym>, value: NodeId },
    Decorators { nodes: Vec<NodeId> },
    AssignAttr { expr: NodeId, attrname: Sym },
    DelAttr { expr: NodeId, attrname: Sym },
    /// `**` inside a dict literal (astroid DictUnpack placeholder key)
    DictUnpack,
    // ---- PEP 695 type params ----
    TypeVar { name: NodeId, bound: Option<NodeId> },
    ParamSpec { name: NodeId },
    TypeVarTuple { name: NodeId },
    EmptyNode,
    Unknown,
}

#[derive(Debug)]
pub struct ModuleData {
    pub name: Box<str>,
    pub file: Box<str>,
    pub package: bool,
    pub body: Vec<NodeId>,
    pub doc_node: Option<NodeId>,
    /// `from __future__ import ...` names
    pub future_imports: Vec<Sym>,
}

#[derive(Debug)]
pub struct FunctionData {
    pub name: Sym,
    pub decorators: Option<NodeId>,
    pub args: NodeId,
    pub returns: Option<NodeId>,
    pub type_params: Vec<NodeId>,
    pub body: Vec<NodeId>,
    pub doc_node: Option<NodeId>,
}

#[derive(Debug)]
pub struct ClassData {
    pub name: Sym,
    pub decorators: Option<NodeId>,
    pub bases: Vec<NodeId>,
    /// keywords minus `metaclass`
    pub keywords: Vec<NodeId>,
    pub metaclass: Option<NodeId>,
    pub type_params: Vec<NodeId>,
    pub body: Vec<NodeId>,
    pub doc_node: Option<NodeId>,
}

#[derive(Debug)]
pub struct ForData {
    pub target: NodeId,
    pub iter: NodeId,
    pub body: Vec<NodeId>,
    pub orelse: Vec<NodeId>,
}

#[derive(Debug)]
pub struct WithData {
    /// astroid flattens with-items to (context_expr, optional_vars) pairs
    pub items: Vec<(NodeId, Option<NodeId>)>,
    pub body: Vec<NodeId>,
}

#[derive(Debug)]
pub struct TryData {
    pub body: Vec<NodeId>,
    pub handlers: Vec<NodeId>,
    pub orelse: Vec<NodeId>,
    pub finalbody: Vec<NodeId>,
}

#[derive(Debug)]
pub struct LambdaData {
    pub args: NodeId,
    pub body: NodeId,
}

#[derive(Debug)]
pub struct CompData {
    pub elt: NodeId,
    pub generators: Vec<NodeId>,
}

#[derive(Debug)]
pub struct DictCompData {
    pub key: NodeId,
    pub value: NodeId,
    pub generators: Vec<NodeId>,
}

#[derive(Debug)]
pub struct ArgumentsData {
    pub posonlyargs: Vec<NodeId>,
    pub args: Vec<NodeId>,
    pub vararg: Option<Sym>,
    pub vararg_node: Option<NodeId>, // AssignName for *args (astroid keeps name str; node for annotation pos)
    pub kwonlyargs: Vec<NodeId>,
    pub kwarg: Option<Sym>,
    pub kwarg_node: Option<NodeId>,
    pub defaults: Vec<NodeId>,
    pub kw_defaults: Vec<Option<NodeId>>,
    pub annotations: Vec<Option<NodeId>>,
    pub posonlyargs_annotations: Vec<Option<NodeId>>,
    pub kwonlyargs_annotations: Vec<Option<NodeId>>,
    pub varargannotation: Option<NodeId>,
    pub kwargannotation: Option<NodeId>,
    /// Whether the LAST posonly/regular/kwonly parameter carries a valid
    /// per-arg type comment (`a,  # type: int`). astroid keeps the parsed
    /// nodes in type_comment_* fields, which only matter for tolineno.
    pub tc_last_posonly: bool,
    pub tc_last_arg: bool,
    pub tc_last_kwonly: bool,
}

pub struct Node {
    pub kind: NodeKind,
    pub parent: NodeId,
    pub fromlineno: u32,
    pub col_offset: i32,
    pub end_lineno: u32,
    pub end_col_offset: i32,
    pub tolineno: u32,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    pub interner: Interner,
    /// locals per scope node, in astroid insertion order
    pub locals: FxHashMap<NodeId, IndexMap<Sym, Vec<NodeId>>>,
    /// astroid `node.position` (def/class/async keyword line+col) for
    /// FunctionDef/AsyncFunctionDef/ClassDef nodes (rebuilder
    /// `_get_position_info`). pylint anchors node messages here when set
    /// (pylinter.py:1213-1221).
    pub positions: FxHashMap<NodeId, (u32, u32)>,
}

impl Tree {
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.idx()]
    }
    pub fn kind(&self, id: NodeId) -> &NodeKind {
        &self.nodes[id.idx()].kind
    }
    pub fn s(&self, sym: Sym) -> &str {
        self.interner.get(sym)
    }

    /// astroid get_children() order.
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.push_children(id, &mut out);
        out
    }

    pub fn push_children(&self, id: NodeId, out: &mut Vec<NodeId>) {
        use NodeKind::*;
        let push_opt = |out: &mut Vec<NodeId>, x: &Option<NodeId>| {
            if let Some(n) = x {
                out.push(*n);
            }
        };
        match self.kind(id) {
            Module(d) => out.extend(&d.body),
            FunctionDef(d) | AsyncFunctionDef(d) => {
                push_opt(out, &d.decorators);
                out.push(d.args);
                push_opt(out, &d.returns);
                out.extend(&d.type_params);
                out.extend(&d.body);
            }
            ClassDef(d) => {
                push_opt(out, &d.decorators);
                out.extend(&d.bases);
                out.extend(&d.keywords);
                out.extend(&d.type_params);
                out.extend(&d.body);
            }
            Return { value } => push_opt(out, value),
            Delete { targets } => out.extend(targets),
            Assign { targets, value } => {
                out.extend(targets);
                out.push(*value);
            }
            AugAssign { target, value, .. } => {
                out.push(*target);
                out.push(*value);
            }
            AnnAssign { target, annotation, value, .. } => {
                out.push(*target);
                out.push(*annotation);
                push_opt(out, value);
            }
            TypeAlias { name, type_params, value } => {
                out.push(*name);
                out.extend(type_params);
                out.push(*value);
            }
            For(d) | AsyncFor(d) => {
                out.push(d.target);
                out.push(d.iter);
                out.extend(&d.body);
                out.extend(&d.orelse);
            }
            While { test, body, orelse } => {
                out.push(*test);
                out.extend(body);
                out.extend(orelse);
            }
            If { test, body, orelse } => {
                out.push(*test);
                out.extend(body);
                out.extend(orelse);
            }
            With(d) | AsyncWith(d) => {
                for (expr, var) in &d.items {
                    out.push(*expr);
                    push_opt(out, var);
                }
                out.extend(&d.body);
            }
            Match { subject, cases } => {
                out.push(*subject);
                out.extend(cases);
            }
            Raise { exc, cause } => {
                push_opt(out, exc);
                push_opt(out, cause);
            }
            Try(d) | TryStar(d) => {
                out.extend(&d.body);
                out.extend(&d.handlers);
                out.extend(&d.orelse);
                out.extend(&d.finalbody);
            }
            Assert { test, fail } => {
                out.push(*test);
                push_opt(out, fail);
            }
            Import { .. } | ImportFrom { .. } | Global { .. } | Nonlocal { .. } | Pass | Break
            | Continue | Const(_) | Name { .. } | AssignName { .. } | DelName { .. }
            | DictUnpack | EmptyNode | Unknown | MatchSingleton { .. } => {}
            Expr { value } => out.push(*value),
            MatchCase { pattern, guard, body } => {
                out.push(*pattern);
                push_opt(out, guard);
                out.extend(body);
            }
            MatchValue { value } => out.push(*value),
            MatchSequence { patterns } => out.extend(patterns),
            MatchMapping { keys, patterns, rest } => {
                // astroid _astroid_fields = ("keys", "patterns", "rest"):
                // all keys first, then all patterns (NOT interleaved)
                out.extend(keys);
                out.extend(patterns);
                push_opt(out, rest);
            }
            MatchClass { cls, patterns, kwd_patterns, .. } => {
                out.push(*cls);
                out.extend(patterns);
                out.extend(kwd_patterns);
            }
            MatchStar { name } => push_opt(out, name),
            MatchAs { pattern, name } => {
                push_opt(out, pattern);
                push_opt(out, name);
            }
            MatchOr { patterns } => out.extend(patterns),
            BoolOp { values, .. } => out.extend(values),
            NamedExpr { target, value } => {
                out.push(*target);
                out.push(*value);
            }
            BinOp { left, right, .. } => {
                out.push(*left);
                out.push(*right);
            }
            UnaryOp { operand, .. } => out.push(*operand),
            Lambda(d) => {
                out.push(d.args);
                out.push(d.body);
            }
            IfExp { test, body, orelse } => {
                out.push(*test);
                out.push(*body);
                out.push(*orelse);
            }
            Dict { items } => {
                for (k, v) in items {
                    out.push(*k);
                    out.push(*v);
                }
            }
            Set { elts } => out.extend(elts),
            ListComp(d) | SetComp(d) | GeneratorExp(d) => {
                out.push(d.elt);
                out.extend(&d.generators);
            }
            DictComp(d) => {
                out.push(d.key);
                out.push(d.value);
                out.extend(&d.generators);
            }
            Await { value } => out.push(*value),
            Yield { value } => push_opt(out, value),
            YieldFrom { value } => out.push(*value),
            Compare { left, ops } => {
                out.push(*left);
                for (_, c) in ops {
                    out.push(*c);
                }
            }
            Call { func, args, keywords } => {
                out.push(*func);
                out.extend(args);
                out.extend(keywords);
            }
            FormattedValue { value, format_spec, .. } => {
                out.push(*value);
                push_opt(out, format_spec);
            }
            JoinedStr { values } => out.extend(values),
            Starred { value, .. } => out.push(*value),
            Attribute { expr, .. } | AssignAttr { expr, .. } | DelAttr { expr, .. } => {
                out.push(*expr)
            }
            Subscript { value, slice, .. } => {
                out.push(*value);
                out.push(*slice);
            }
            List { elts, .. } | Tuple { elts, .. } => out.extend(elts),
            Slice { lower, upper, step } => {
                push_opt(out, lower);
                push_opt(out, upper);
                push_opt(out, step);
            }
            Comprehension { target, iter, ifs, .. } => {
                out.push(*target);
                out.push(*iter);
                out.extend(ifs);
            }
            ExceptHandler { type_, name, body } => {
                push_opt(out, type_);
                push_opt(out, name);
                out.extend(body);
            }
            Arguments(d) => {
                // astroid Arguments.get_children (node_classes.py): posonly,
                // posonly annotations, args, defaults, kwonly, kw_defaults,
                // annotations, varargannotation, kwargannotation, kwonly
                // annotations (in that exact order, Nones skipped).
                out.extend(&d.posonlyargs);
                out.extend(d.posonlyargs_annotations.iter().flatten());
                out.extend(&d.args);
                out.extend(&d.defaults);
                out.extend(&d.kwonlyargs);
                out.extend(d.kw_defaults.iter().flatten());
                out.extend(d.annotations.iter().flatten());
                push_opt(out, &d.varargannotation);
                push_opt(out, &d.kwargannotation);
                out.extend(d.kwonlyargs_annotations.iter().flatten());
            }
            Keyword { value, .. } => out.push(*value),
            Decorators { nodes } => out.extend(nodes),
            TypeVar { name, bound } => {
                out.push(*name);
                push_opt(out, bound);
            }
            ParamSpec { name } | TypeVarTuple { name } => out.push(*name),
        }
    }

    pub fn kind_name(&self, id: NodeId) -> &'static str {
        use NodeKind::*;
        match self.kind(id) {
            Module(_) => "Module",
            FunctionDef(_) => "FunctionDef",
            AsyncFunctionDef(_) => "AsyncFunctionDef",
            ClassDef(_) => "ClassDef",
            Return { .. } => "Return",
            Delete { .. } => "Delete",
            Assign { .. } => "Assign",
            AugAssign { .. } => "AugAssign",
            AnnAssign { .. } => "AnnAssign",
            TypeAlias { .. } => "TypeAlias",
            For(_) => "For",
            AsyncFor(_) => "AsyncFor",
            While { .. } => "While",
            If { .. } => "If",
            With(_) => "With",
            AsyncWith(_) => "AsyncWith",
            Match { .. } => "Match",
            Raise { .. } => "Raise",
            Try(_) => "Try",
            TryStar(_) => "TryStar",
            Assert { .. } => "Assert",
            Import { .. } => "Import",
            ImportFrom { .. } => "ImportFrom",
            Global { .. } => "Global",
            Nonlocal { .. } => "Nonlocal",
            Expr { .. } => "Expr",
            Pass => "Pass",
            Break => "Break",
            Continue => "Continue",
            MatchCase { .. } => "MatchCase",
            MatchValue { .. } => "MatchValue",
            MatchSingleton { .. } => "MatchSingleton",
            MatchSequence { .. } => "MatchSequence",
            MatchMapping { .. } => "MatchMapping",
            MatchClass { .. } => "MatchClass",
            MatchStar { .. } => "MatchStar",
            MatchAs { .. } => "MatchAs",
            MatchOr { .. } => "MatchOr",
            BoolOp { .. } => "BoolOp",
            NamedExpr { .. } => "NamedExpr",
            BinOp { .. } => "BinOp",
            UnaryOp { .. } => "UnaryOp",
            Lambda(_) => "Lambda",
            IfExp { .. } => "IfExp",
            Dict { .. } => "Dict",
            Set { .. } => "Set",
            ListComp(_) => "ListComp",
            SetComp(_) => "SetComp",
            DictComp(_) => "DictComp",
            GeneratorExp(_) => "GeneratorExp",
            Await { .. } => "Await",
            Yield { .. } => "Yield",
            YieldFrom { .. } => "YieldFrom",
            Compare { .. } => "Compare",
            Call { .. } => "Call",
            FormattedValue { .. } => "FormattedValue",
            JoinedStr { .. } => "JoinedStr",
            Const(_) => "Const",
            Starred { .. } => "Starred",
            Attribute { .. } => "Attribute",
            Subscript { .. } => "Subscript",
            Name { .. } => "Name",
            AssignName { .. } => "AssignName",
            DelName { .. } => "DelName",
            List { .. } => "List",
            Tuple { .. } => "Tuple",
            Slice { .. } => "Slice",
            Comprehension { .. } => "Comprehension",
            ExceptHandler { .. } => "ExceptHandler",
            Arguments(_) => "Arguments",
            Keyword { .. } => "Keyword",
            Decorators { .. } => "Decorators",
            AssignAttr { .. } => "AssignAttr",
            DelAttr { .. } => "DelAttr",
            DictUnpack => "DictUnpack",
            TypeVar { .. } => "TypeVar",
            ParamSpec { .. } => "ParamSpec",
            TypeVarTuple { .. } => "TypeVarTuple",
            EmptyNode => "EmptyNode",
            Unknown => "Unknown",
        }
    }

    /// Dump in the harness/dump_ast.py format for differential testing.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_node(NodeId::MODULE, 0, &mut out);
        out
    }

    fn fmt_pos(&self, id: NodeId) -> String {
        let n = self.node(id);
        let col = if n.col_offset < 0 {
            "-".to_string()
        } else {
            n.col_offset.to_string()
        };
        let endl = if n.end_lineno == 0 {
            "-".to_string()
        } else {
            n.end_lineno.to_string()
        };
        let endc = if n.end_col_offset < 0 {
            "-".to_string()
        } else {
            n.end_col_offset.to_string()
        };
        let tol = if n.tolineno == 0 && n.end_lineno == 0 && matches!(n.kind, NodeKind::Unknown) {
            "-".to_string()
        } else {
            n.tolineno.to_string()
        };
        format!("{}:{}-{}({}):{}", n.fromlineno, col, tol, endl, endc)
    }

    fn dump_node(&self, id: NodeId, depth: usize, out: &mut String) {
        let _ = write!(out, "{}{} {}", "  ".repeat(depth), self.kind_name(id), self.fmt_pos(id));
        let e = self.dump_extra(id);
        if !e.is_empty() {
            let _ = write!(out, " {e}");
        }
        out.push('\n');
        for c in self.children(id) {
            self.dump_node(c, depth + 1, out);
        }
    }

    fn dump_extra(&self, id: NodeId) -> String {
        use NodeKind::*;
        let mut parts: Vec<String> = Vec::new();
        match self.kind(id) {
            Name { name } | AssignName { name } | DelName { name } => {
                parts.push(format!("name={}", self.s(*name)));
            }
            FunctionDef(d) | AsyncFunctionDef(d) => {
                parts.push(format!("name={}", self.s(d.name)));
                if let Some(doc) = d.doc_node {
                    parts.push(format!("doc@{}", self.node(doc).fromlineno));
                }
            }
            ClassDef(d) => {
                parts.push(format!("name={}", self.s(d.name)));
                if let Some(doc) = d.doc_node {
                    parts.push(format!("doc@{}", self.node(doc).fromlineno));
                }
            }
            Attribute { attrname, .. } | AssignAttr { attrname, .. } | DelAttr { attrname, .. } => {
                parts.push(format!("attr={}", self.s(*attrname)));
            }
            Const(v) => parts.push(format!("const={}", fmt_const(v))),
            Import { names } => parts.push(format!("names={}", self.fmt_names(names))),
            ImportFrom { modname, names, level } => {
                let lvl = match level {
                    None => "None".to_string(),
                    Some(l) => l.to_string(),
                };
                parts.push(format!(
                    "from={}:{}:{}",
                    self.s(*modname),
                    lvl,
                    self.fmt_names(names)
                ));
            }
            BinOp { op, .. } | BoolOp { op, .. } | UnaryOp { op, .. }
            | AugAssign { op, .. } => parts.push(format!("op={op}")),
            Compare { ops, .. } => {
                let v: Vec<String> = ops.iter().map(|(o, _)| format!("'{o}'")).collect();
                parts.push(format!("ops=[{}]", v.join(", ")));
            }
            Keyword { arg, .. } => match arg {
                Some(a) => parts.push(format!("arg='{}'", self.s(*a))),
                None => parts.push("arg=None".to_string()),
            },
            Arguments(d) => {
                let f = |o: &Option<Sym>| match o {
                    Some(s) => format!("'{}'", self.s(*s)),
                    None => "None".to_string(),
                };
                parts.push(format!("vararg={} kwarg={}", f(&d.vararg), f(&d.kwarg)));
            }
            Global { names } | Nonlocal { names } => {
                let v: Vec<String> = names.iter().map(|n| format!("'{}'", self.s(*n))).collect();
                parts.push(format!("names=[{}]", v.join(", ")));
            }
            Module(d) => {
                parts.push(format!("name={} package={}", d.name, if d.package { "True" } else { "False" }));
                if let Some(doc) = d.doc_node {
                    parts.push(format!("doc@{}", self.node(doc).fromlineno));
                }
            }
            _ => {}
        }
        if let Some(locals) = self.locals.get(&id) {
            let keys: Vec<&str> = locals.keys().map(|k| self.s(*k)).collect();
            parts.push(format!("locals=[{}]", keys.join(",")));
        }
        parts.join(" ")
    }

    fn fmt_names(&self, names: &[(Sym, Option<Sym>)]) -> String {
        let v: Vec<String> = names
            .iter()
            .map(|(n, a)| {
                let alias = match a {
                    Some(s) => format!("'{}'", self.s(*s)),
                    None => "None".to_string(),
                };
                format!("('{}', {})", self.s(*n), alias)
            })
            .collect();
        format!("[{}]", v.join(", "))
    }
}

fn fmt_const(v: &ConstValue) -> String {
    match v {
        ConstValue::None => "NoneType".to_string(),
        ConstValue::NotImplemented => "NotImplementedType".to_string(),
        ConstValue::Ellipsis => "ellipsis".to_string(),
        ConstValue::Bool(b) => format!("bool:{}", if *b { "True" } else { "False" }),
        ConstValue::Int(IntValue::Small(i)) => {
            if i.abs() < 1_000_000_000_000_000 {
                format!("int:{i}")
            } else {
                "int:big".to_string()
            }
        }
        ConstValue::Int(IntValue::Big(_)) => "int:big".to_string(),
        ConstValue::Float(f) => format!("float:{}", crate::pyrepr::repr_float(*f)),
        ConstValue::Complex { .. } => "complex".to_string(),
        ConstValue::Str(s) => {
            let truncated: String = s.chars().take(20).collect();
            format!("str:{}:{}", s.chars().count(), crate::pyrepr::repr_str(&truncated))
        }
        ConstValue::StrSurrogate(points) => {
            let truncated: Vec<u32> = points.iter().take(20).copied().collect();
            format!(
                "str:{}:{}",
                points.len(),
                crate::pyrepr::repr_str_points(&truncated)
            )
        }
        ConstValue::Bytes(b) => format!("bytes:{}", b.len()),
    }
}
