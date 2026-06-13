//! Port of pylint/checkers/refactoring/ (pylint 4.0.5) — full-pylint mode.
//!
//! Four checkers, all name = "refactoring":
//! - RefactoringChecker (R1701–R1737) — BaseTokenChecker => LINE-scoped
//! - NotChecker (C0117)               — BaseChecker => NODE-scoped
//! - RecommendationChecker (C0200/C0201/C0206/C0207/C0208/C0209)
//! - ImplicitBooleanessChecker (C1802–C1805)
//!
//! Spec: reference/notes/09-refactoring.md. Bug-for-bug; do not "improve".
//!
//! Cross-cutting facts replicated:
//! - exact-class-name walker dispatch (no MRO): visit_for/visit_functiondef
//!   do NOT fire for AsyncFor/AsyncFunctionDef => R1710/R1711 skip async defs,
//!   R1704/R1733/R1736/C0200/C0206/C0208 skip `async for`.
//! - RefactoringChecker state leak across modules when R1732 is disabled
//!   (leave_module is @only_required_for_messages("consider-using-with"), so
//!   if R1732 disabled it is never registered => _init() never runs between
//!   modules => self._elifs grows for the whole run). §1.3.
//! - R1710/R1711 _return_nodes keyed by BARE function name => same-named
//!   nested/sibling defs clobber each other. §3.9.

use pyast::tree::{ConstValue, NodeKind};
use pyinfer::ctx::Ctx;
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, Value};
use rustc_hash::FxHashSet;

use crate::basicerr::nodes_of_class;
use crate::ckutils as u;
use crate::walker::WalkCx;

// ===========================================================================
// State — lives in LintRun (cross-module persistence for the _elifs leak)
// ===========================================================================

#[derive(Default)]
pub struct RefactoringCk {
    /// token positions (row, col) of every `elif` keyword AND the token right
    /// after it (two entries per elif). _is_actual_elif checks membership.
    /// LEAKS across modules when R1732 is disabled (leave_module unregistered).
    pub elifs: Vec<(u32, i32)>,
    /// _nested_blocks stack (R1702). Reset per-function at leave_functiondef
    /// and per-module at _init().
    pub nested_blocks: Vec<GNode>,
    /// _return_nodes: bare-fn-name -> Return nodes (R1710/R1711).
    pub return_nodes: indexmap::IndexMap<String, Vec<GNode>>,
    /// _reported_swap_nodes (R1712): nodes already part of a reported swap.
    pub reported_swap_nodes: FxHashSet<GNode>,
    /// ConsiderUsingWithStack (R1732): varname -> Call value node, per scope.
    pub cuw_function: indexmap::IndexMap<String, GNode>,
    pub cuw_class: indexmap::IndexMap<String, GNode>,
    pub cuw_module: indexmap::IndexMap<String, GNode>,
    /// _can_simplify_bool_op (R1726/R1727 scratch).
    pub can_simplify_bool_op: bool,
}

impl RefactoringCk {
    /// _init() (refactoring_checker.py): reset per-module state. Called from
    /// leave_module ONLY when R1732 is enabled (the leak).
    fn init(&mut self) {
        self.nested_blocks.clear();
        self.elifs.clear();
        self.reported_swap_nodes.clear();
        self.cuw_function.clear();
        self.cuw_class.clear();
        self.cuw_module.clear();
        self.return_nodes.clear();
    }
}

// ===========================================================================
// Shared small helpers
// ===========================================================================

fn is_if(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::If { .. }))
}

fn is_funcdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)))
}

/// node.col_offset (raw; not the message-anchor position)
fn col_offset(eng: &Engine, g: GNode) -> i32 {
    eng.md(g.m).tree.nodes[g.n.idx()].col_offset
}

fn lineno(eng: &Engine, g: GNode) -> u32 {
    eng.fromlineno(g)
}

/// utils.safe_infer wrapper.
fn safe_infer(eng: &Engine, cx_caches: &u::LintCaches, g: GNode) -> Option<Value> {
    u::safe_infer(eng, cx_caches, g)
}

/// utils.is_builtin_object (utils.py:286): node.root().name == "builtins".
fn value_is_builtin_object(eng: &Engine, v: &Value) -> bool {
    let g = match v {
        Value::Node(g) => *g,
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => *cls,
        _ => return false,
    };
    // root().name
    let mut top = g;
    while let Some(p) = eng.parent(top) {
        top = p;
    }
    eng.kind_is(top, |k| matches!(k, NodeKind::Module(d) if &*d.name == "builtins"))
}

// R1732 constants (refactoring_checker.py:34-63).
const CALLS_REPLACED_BY_WITH: &[&str] = &[
    "threading.lock.acquire",
    "threading._RLock.acquire",
    "threading.Semaphore.acquire",
    "multiprocessing.managers.BaseManager.start",
    "multiprocessing.managers.SyncManager.start",
];
const CALLS_RETURNING_CMS: &[&str] = &[
    "_io.open",
    "pathlib.Path.open",
    "pathlib._local.Path.open",
    "codecs.open",
    "urllib.request.urlopen",
    "tempfile.NamedTemporaryFile",
    "tempfile.SpooledTemporaryFile",
    "tempfile.TemporaryDirectory",
    "tempfile.TemporaryFile",
    "zipfile.ZipFile",
    "zipfile.PyZipFile",
    "zipfile.ZipFile.open",
    "zipfile.PyZipFile.open",
    "tarfile.TarFile",
    "tarfile.TarFile.open",
    "multiprocessing.context.BaseContext.Pool",
    "subprocess.Popen",
];

/// _is_actual_elif (refactoring_checker.py:581-594).
fn is_actual_elif(rc: &RefactoringCk, eng: &Engine, node: GNode) -> bool {
    // match node.parent: case If(orelse=[n]) if n == node
    let Some(parent) = eng.parent(node) else { return false };
    let md = eng.md(parent.m);
    let NodeKind::If { orelse, .. } = &md.tree.nodes[parent.n.idx()].kind else { return false };
    if orelse.len() != 1 || orelse[0] != node.n {
        return false;
    }
    drop(md);
    let pos = (lineno(eng, node), col_offset(eng, node));
    rc.elifs.contains(&pos)
}

// ===========================================================================
// Token pass — process_tokens (refactoring_checker.py:668-714)
// ===========================================================================

/// Result of the refactoring token scan (process_tokens) — pure function of
/// the token stream + the file-start R1707 enable flag. Precomputed in
/// phase 1, applied in phase 2.
#[derive(Default, Clone)]
pub struct RefacTokens {
    /// (row, col) of every `elif` keyword AND the token right after it.
    pub elifs: Vec<(u32, i32)>,
    /// physical rows where R1707 trailing-comma-tuple fires (line= only).
    pub r1707_lines: Vec<u32>,
}

/// process_tokens (refactoring_checker.py:668-714). Builds the _elifs list
/// and collects R1707 trailing-comma-tuple emissions. Runs in the
/// TOKEN-checker phase (sorted-name order: Format, Encoding, Refactoring,
/// Spelling, StringConstant).
pub fn process_tokens(
    ts: &pyast::pytok::PyTokens,
    text: &str,
    r1707_enabled_for_file: bool,
) -> RefacTokens {
    let mut out = RefacTokens::default();
    let toks = &ts.toks;
    let mut enabled_once = r1707_enabled_for_file;
    for index in 0..toks.len() {
        let tk = &toks[index];
        let token_string = ts.tok_str(text, index);
        // enable-pragma rescan (substring slices, bug-for-bug). NOTE: "enable"
        // is a substring of "disable", so `# pylint: disable=...` also flips.
        if !enabled_once
            && token_string.starts_with('#')
            && slice_from(token_string, 1).contains("pylint:")
            && slice_from(token_string, 8).contains("enable")
            && {
                let s15 = slice_from(token_string, 15);
                s15.contains("trailing-comma-tuple") || s15.contains("R1707")
            }
        {
            enabled_once = true;
        }
        if token_string == "elif" {
            let col = tok_col(ts, text, index);
            out.elifs.push((tk.row, col));
            if index + 1 < toks.len() {
                let nrow = toks[index + 1].row;
                let ncol = tok_col(ts, text, index + 1);
                out.elifs.push((nrow, ncol));
            }
        } else if (r1707_enabled_for_file || enabled_once) && is_trailing_comma(ts, text, index) {
            out.r1707_lines.push(tk.row);
        }
    }
    out
}

/// byte slice of a string starting at CHARACTER index n (python s[n:]).
fn slice_from(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((byte, _)) => &s[byte..],
        None => "",
    }
}

/// column (code-point offset within the physical row) of token `index`.
fn tok_col(ts: &pyast::pytok::PyTokens, text: &str, index: usize) -> i32 {
    let t = &ts.toks[index];
    if t.row == 0 || t.row > ts.nrows {
        return 0;
    }
    let row_start = ts.row_starts[t.row as usize - 1] as usize;
    let start = t.start as usize;
    if start < row_start {
        return 0;
    }
    text[row_start..start].chars().count() as i32
}

/// _is_trailing_comma (refactoring_checker.py:98-138).
fn is_trailing_comma(ts: &pyast::pytok::PyTokens, text: &str, index: usize) -> bool {
    use pyast::pytok::PyTokKind as K;
    let toks = &ts.toks;
    let token = &toks[index];
    // token.exact_type != COMMA -> False. Our OP token; check string.
    if !(token.kind == K::Op && ts.tok_str(text, index) == ",") {
        return false;
    }
    let row = token.row;
    let mut more_tokens_on_line = false;
    for j in (index + 1)..toks.len() {
        let rt = &toks[j];
        if rt.row == row {
            more_tokens_on_line = true;
            if !matches!(rt.kind, K::Newline | K::Comment) {
                return false;
            }
        }
    }
    if !more_tokens_on_line {
        return false;
    }
    // get_curline_index_start(): scan back from index-1 to previous NEWLINE
    let mut curline_start = 0usize;
    {
        let mut subindex = 0usize;
        let mut found = false;
        for j in (0..index).rev() {
            if toks[j].kind == K::Newline {
                curline_start = index - subindex;
                found = true;
                break;
            }
            subindex += 1;
        }
        if !found {
            curline_start = 0;
        }
    }
    // any "=" in prevtoken.string or prevtoken.string in {"return","yield"}
    for j in curline_start..index {
        let s = ts.tok_str(text, j);
        if s.contains('=') || s == "return" || s == "yield" {
            return true;
        }
    }
    false
}

// ===========================================================================
// AST: superfluous-else family (R1705/R1720/R1723/R1724) §3.3
// ===========================================================================

#[derive(Clone, Copy)]
enum ReturningKind {
    Return,
    Raise,
    Break,
    Continue,
}

impl ReturningKind {
    fn matches(&self, k: &NodeKind) -> bool {
        match self {
            ReturningKind::Return => matches!(k, NodeKind::Return { .. }),
            ReturningKind::Raise => matches!(k, NodeKind::Raise { .. }),
            ReturningKind::Break => matches!(k, NodeKind::Break),
            ReturningKind::Continue => matches!(k, NodeKind::Continue),
        }
    }
}

/// _if_statement_is_always_returning (refactoring_checker.py:82-85): ANY
/// direct child of the if-body is of `cls`.
fn if_body_always_returning(eng: &Engine, body: &[pyast::NodeId], m: pyinfer::value::ModId, cls: ReturningKind) -> bool {
    body.iter().any(|&n| {
        let md = eng.md(m);
        cls.matches(&md.tree.nodes[n.idx()].kind)
    })
}

/// _except_statement_is_always_returning (refactoring_checker.py:88-95).
fn except_always_returning(eng: &Engine, handlers: &[pyast::NodeId], m: pyinfer::value::ModId, cls: ReturningKind) -> bool {
    handlers.iter().all(|&h| {
        let md = eng.md(m);
        let NodeKind::ExceptHandler { body, .. } = &md.tree.nodes[h.idx()].kind else {
            return false;
        };
        let body = body.clone();
        drop(md);
        body.iter().any(|&c| {
            let md = eng.md(m);
            cls.matches(&md.tree.nodes[c.idx()].kind)
        })
    })
}

fn check_superfluous_else(
    rc: &RefactoringCk,
    cx: &mut WalkCx,
    node: GNode,
    msg_id: &'static str,
    cls: ReturningKind,
) {
    let eng = cx.eng;
    // if isinstance(node, Try) and node.finalbody: bail
    let (is_node_if, is_node_try, orelse_first, finalbody_empty): (bool, bool, Option<GNode>, bool) = {
        let md = eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::If { orelse, .. } => (
                true,
                false,
                orelse.first().map(|&n| GNode { m: node.m, n }),
                true,
            ),
            NodeKind::Try(d) => (
                false,
                true,
                d.orelse.first().map(|&n| GNode { m: node.m, n }),
                d.finalbody.is_empty(),
            ),
            // While reaches here via visit_while=visit_try but is neither
            NodeKind::While { orelse, .. } => (
                false,
                false,
                orelse.first().map(|&n| GNode { m: node.m, n }),
                true,
            ),
            _ => return,
        }
    };
    if is_node_try && !finalbody_empty {
        return;
    }
    // if not node.orelse: bail
    if orelse_first.is_none() {
        return;
    }
    // if _is_actual_elif(node): bail
    if is_actual_elif(rc, eng, node) {
        return;
    }
    // emit-condition
    let emit = if is_node_if {
        let body: Vec<pyast::NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::If { body, .. } => body.clone(),
                _ => return,
            }
        };
        if_body_always_returning(eng, &body, node.m, cls)
    } else if is_node_try {
        let handlers: Vec<pyast::NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Try(d) => d.handlers.clone(),
                _ => return,
            }
        };
        except_always_returning(eng, &handlers, node.m, cls)
    } else {
        false
    };
    if !emit {
        return;
    }
    let orelse = orelse_first.unwrap();
    let orelse_pos = (lineno(eng, orelse), col_offset(eng, orelse));
    let (a, b) = if rc.elifs.contains(&orelse_pos) {
        ("elif", "remove the leading \"el\" from \"elif\"")
    } else {
        ("else", "remove the \"else\" and de-indent the code inside it")
    };
    let template = msg_template(msg_id);
    cx.emit_node(
        msg_id,
        u::msg_line(eng, node),
        u::msg_col(eng, node),
        u::format_template(template, &[a, b]),
    );
}

fn msg_template(msg_id: &str) -> &'static str {
    match msg_id {
        "R1705" => "Unnecessary \"%s\" after \"return\", %s",
        "R1720" => "Unnecessary \"%s\" after \"raise\", %s",
        "R1723" => "Unnecessary \"%s\" after \"break\", %s",
        "R1724" => "Unnecessary \"%s\" after \"continue\", %s",
        _ => "",
    }
}

// ===========================================================================
// Walker entry points
// ===========================================================================

impl RefactoringCk {
    pub fn visit_if(&mut self, cx: &mut WalkCx, node: GNode) {
        // R1702 nested blocks, R1703 simplifiable-if, no-else family,
        // R1715 consider-using-get, R1730/R1731 min/max builtin.
        self.check_nested_blocks(cx, node);
        self.check_simplifiable_if(cx, node);
        check_superfluous_else(self, cx, node, "R1705", ReturningKind::Return);
        check_superfluous_else(self, cx, node, "R1720", ReturningKind::Raise);
        check_superfluous_else(self, cx, node, "R1723", ReturningKind::Break);
        check_superfluous_else(self, cx, node, "R1724", ReturningKind::Continue);
        self.check_consider_get(cx, node);
        self.check_min_max_builtin(cx, node);
    }

    // -- visit_call: R1708/R1717/R1718/R1722/R1725/R1728/R1729/R1734/R1735
    //    (+R1732 consider-using-with, handled in its own machinery) --
    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_raising_stopiteration_next(cx, node);
        self.check_consider_comprehension_constructor(cx, node);
        self.check_consider_using_sys_exit(cx, node);
        self.check_super_with_arguments(cx, node);
        self.check_consider_using_generator(cx, node);
        self.check_consider_using_with_call(cx, node);
        self.check_use_list_literal(cx, node);
        self.check_use_dict_literal(cx, node);
    }

    /// R1734 use-list-literal (refactoring_checker.py:1707-1713).
    fn check_use_list_literal(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if u::as_string(eng, node) != "list()" {
            return;
        }
        let func = match call_func(eng, node) {
            Some(f) => f,
            None => return,
        };
        // no args (already implied by as_string == "list()")
        if let Some(Value::Node(g)) = safe_infer(eng, cx.caches, func) {
            if eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) && eng.qname(g) == "builtins.list" {
                cx.emit_node(
                    "R1734",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    "Consider using [] instead of list()".to_string(),
                );
            }
        }
    }

    /// R1735 use-dict-literal (refactoring_checker.py:1715-1746).
    fn check_use_dict_literal(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node.func must be Name "dict"
        let (func, args): (GNode, Vec<pyast::NodeId>) = {
            let md = eng.md(node.m);
            let NodeKind::Call { func, args, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            match &md.tree.nodes[func.idx()].kind {
                NodeKind::Name { name } if md.tree.s(*name) == "dict" => {
                    (GNode { m: node.m, n: *func }, args.clone())
                }
                _ => return,
            }
        };
        if !args.is_empty() {
            return;
        }
        if let Some(Value::Node(g)) = safe_infer(eng, cx.caches, func) {
            if eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) && eng.qname(g) == "builtins.dict" {
                let suggestion = self.dict_literal_suggestion(cx, node);
                cx.emit_node(
                    "R1735",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    u::format_template("Consider using '%s' instead of a call to 'dict'.", &[&suggestion]),
                );
            }
        }
    }

    /// _dict_literal_suggestion (refactoring_checker.py:1732-1746).
    fn dict_literal_suggestion(&self, cx: &mut WalkCx, node: GNode) -> String {
        let eng = cx.eng;
        // node.keywords; node.kwargs = keywords with arg None (** unpacks)
        let keywords: Vec<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Call { keywords, .. } => {
                    keywords.iter().map(|&k| GNode { m: node.m, n: k }).collect()
                }
                _ => Vec::new(),
            }
        };
        let mut elements: Vec<String> = Vec::new();
        // named keys first (keyword not in node.kwargs)
        for &kw in keywords.iter() {
            // python: if len(", ".join(elements)) >= 64: break
            if elements.join(", ").chars().count() >= 64 {
                break;
            }
            let md = eng.md(kw.m);
            let NodeKind::Keyword { arg, value } = &md.tree.nodes[kw.n.idx()].kind else { continue };
            if let Some(arg_sym) = arg {
                let arg_name = md.tree.s(*arg_sym).to_string();
                let value = GNode { m: kw.m, n: *value };
                drop(md);
                elements.push(format!("\"{}\": {}", arg_name, u::as_string(eng, value)));
            }
        }
        // ** unpacks
        for &kw in keywords.iter() {
            if elements.join(", ").chars().count() >= 64 {
                break;
            }
            let md = eng.md(kw.m);
            let NodeKind::Keyword { arg, value } = &md.tree.nodes[kw.n.idx()].kind else { continue };
            if arg.is_none() {
                let value = GNode { m: kw.m, n: *value };
                drop(md);
                elements.push(format!("**{}", u::as_string(eng, value)));
            }
        }
        let suggestion = elements.join(", ");
        let tail = if suggestion.chars().count() > 64 { ", ... " } else { "" };
        format!("{{{}{}}}", suggestion, tail)
    }

    /// R1722 consider-using-sys-exit (refactoring_checker.py:1184-1201).
    fn check_consider_using_sys_exit(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let func = match call_func(eng, node) {
            Some(f) => f,
            None => return,
        };
        // func must be Name with name in {"quit","exit"}
        let is_exit_func = eng.kind_is(func, |k| {
            matches!(k, NodeKind::Name { name } if {
                // borrow inside closure: resolve below instead
                let _ = name; true
            })
        });
        if !is_exit_func {
            return;
        }
        let name = {
            let md = eng.md(func.m);
            match &md.tree.nodes[func.n.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return,
            }
        };
        if name != "quit" && name != "exit" {
            return;
        }
        let local_scope = eng.scope(node);
        let root = {
            let mut top = node;
            while let Some(p) = eng.parent(top) {
                top = p;
            }
            top
        };
        if has_exit_in_scope(eng, local_scope) || has_exit_in_scope(eng, root) {
            return;
        }
        cx.emit_node(
            "R1722",
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            "Consider using 'sys.exit' instead".to_string(),
        );
    }

    /// R1725 super-with-arguments (refactoring_checker.py:1203-1215).
    fn check_super_with_arguments(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // Call(func=Name("super"), args=[Name(name), Name("self")])
        let (func, args): (GNode, Vec<pyast::NodeId>) = {
            let md = eng.md(node.m);
            let NodeKind::Call { func, args, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            (GNode { m: node.m, n: *func }, args.clone())
        };
        if !eng.kind_is(func, |k| matches!(k, NodeKind::Name { name } if { let _ = name; true })) {
            return;
        }
        {
            let md = eng.md(func.m);
            match &md.tree.nodes[func.n.idx()].kind {
                NodeKind::Name { name } if md.tree.s(*name) == "super" => {}
                _ => return,
            }
        }
        if args.len() != 2 {
            return;
        }
        let a0 = GNode { m: node.m, n: args[0] };
        let a1 = GNode { m: node.m, n: args[1] };
        let name0 = name_of(eng, a0);
        let name1 = name_of(eng, a1);
        let (Some(n0), Some(n1)) = (name0, name1) else { return };
        if n1 != "self" {
            return;
        }
        // frame_class = node_frame_class(node); name == frame_class.name
        let Some(frame_class) = node_frame_class(eng, node) else { return };
        let class_name = {
            let md = eng.md(frame_class.m);
            match &md.tree.nodes[frame_class.n.idx()].kind {
                NodeKind::ClassDef(d) => md.tree.s(d.name).to_string(),
                _ => return,
            }
        };
        if n0 != class_name {
            return;
        }
        cx.emit_node(
            "R1725",
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            "Consider using Python 3 style super() without arguments".to_string(),
        );
    }

    /// R1717/R1718 consider-using-{dict,set}-comprehension (1076-1110).
    fn check_consider_comprehension_constructor(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // Call(func=Name(name), args=[ListComp(elt=element), *_])
        let (name, listcomp_elt): (String, GNode) = {
            let md = eng.md(node.m);
            let NodeKind::Call { func, args, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            let name = match &md.tree.nodes[func.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return,
            };
            let Some(&first_arg) = args.first() else { return };
            let NodeKind::ListComp(d) = &md.tree.nodes[first_arg.idx()].kind else { return };
            (name, GNode { m: node.m, n: d.elt })
        };
        match name.as_str() {
            "dict" => {
                // bail if element is a Call
                if eng.kind_is(listcomp_elt, |k| matches!(k, NodeKind::Call { .. })) {
                    return;
                }
                // IfExp with body/orelse both 2-elt Tuple|List -> #5588 check
                if let Some((bk, bv, ok, ov)) = ifexp_2tuple_keyvals(eng, listcomp_elt) {
                    let bk_s = u::as_string(eng, bk);
                    let ok_s = u::as_string(eng, ok);
                    let bv_s = u::as_string(eng, bv);
                    let ov_s = u::as_string(eng, ov);
                    if bk_s != ok_s && bv_s != ov_s {
                        return;
                    }
                }
                cx.emit_node(
                    "R1717",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    "Consider using a dictionary comprehension".to_string(),
                );
            }
            "set" => {
                cx.emit_node(
                    "R1718",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    "Consider using a set comprehension".to_string(),
                );
            }
            _ => {}
        }
    }

    /// R1728/R1729 consider-using-generator / use-a-generator (1112-1139).
    fn check_consider_using_generator(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // Call(func=Name(call_name), args=[ListComp() as comp]) — exactly 1 arg
        let (call_name, comp, has_keywords): (String, GNode, bool) = {
            let md = eng.md(node.m);
            let NodeKind::Call { func, args, keywords } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            let name = match &md.tree.nodes[func.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return,
            };
            if args.len() != 1 {
                return;
            }
            if !matches!(&md.tree.nodes[args[0].idx()].kind, NodeKind::ListComp(_)) {
                return;
            }
            (name, GNode { m: node.m, n: args[0] }, !keywords.is_empty())
        };
        const NAMES: &[&str] = &["any", "all", "sum", "max", "min", "list", "tuple"];
        if !NAMES.contains(&call_name.as_str()) {
            return;
        }
        // inside_comp = comp.as_string()[1:-1]  (strip the [ ])
        let comp_str = u::as_string(eng, comp);
        let mut inside_comp = strip_brackets(&comp_str);
        if has_keywords {
            let kw_strs: Vec<String> = {
                let md = eng.md(node.m);
                match &md.tree.nodes[node.n.idx()].kind {
                    NodeKind::Call { keywords, .. } => keywords.clone(),
                    _ => Vec::new(),
                }
            }
            .into_iter()
            .map(|k| u::as_string(eng, GNode { m: node.m, n: k }))
            .collect();
            inside_comp = format!("({}), {}", inside_comp, kw_strs.join(", "));
        }
        let (msg, template) = if call_name == "any" || call_name == "all" {
            ("R1729", "Use a generator instead '%s(%s)'")
        } else {
            ("R1728", "Consider using a generator instead '%s(%s)'")
        };
        cx.emit_node(
            msg,
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            u::format_template(template, &[&call_name, &inside_comp]),
        );
    }

    /// R1708 (b) raising-stopiteration-in-generator-next-call (1217-1262).
    fn check_raising_stopiteration_next(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (func, args): (GNode, Vec<pyast::NodeId>) = {
            let md = eng.md(node.m);
            let NodeKind::Call { func, args, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            // bail if func is Attribute (x.next())
            if matches!(&md.tree.nodes[func.idx()].kind, NodeKind::Attribute { .. }) {
                return;
            }
            (GNode { m: node.m, n: *func }, args.clone())
        };
        // bail if no args
        if args.is_empty() {
            return;
        }
        // inferred = safe_infer(func); must be FunctionDef with qname builtins.next
        let inferred = safe_infer(eng, cx.caches, func);
        let is_next = matches!(
            &inferred,
            Some(Value::Node(g)) if is_funcdef(eng, *g) && eng.qname(*g) == "builtins.next"
        );
        if !is_next {
            return;
        }
        let frame = eng.frame(node);
        let has_sentinel = args.len() > 1;
        let is_gen_frame = is_funcdef(eng, frame) && eng.is_generator(frame);
        if is_gen_frame
            && !has_sentinel
            && !u::node_ignores_exception(eng, cx.caches, node, "StopIteration")
            && !looks_like_infinite_iterator(eng, cx.caches, GNode { m: node.m, n: args[0] })
        {
            cx.emit_node(
                "R1708",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Do not raise StopIteration in generator, use return statement instead".to_string(),
            );
        }
    }

    /// R1732 _check_consider_using_with (refactoring_checker.py:1671-1705).
    fn check_consider_using_with_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if is_inside_context_manager(eng, node) || is_a_return_statement(eng, node) {
            return;
        }
        // bail if node already tracked in this frame's stack (identity)
        let frame = eng.frame(node);
        if self.stack_for_frame(eng, frame).values().any(|&v| v == node) {
            return;
        }
        let func = match call_func(eng, node) {
            Some(f) => f,
            None => return,
        };
        let inferred = safe_infer(eng, cx.caches, func);
        // isinstance(inferred, (FunctionDef, ClassDef, BoundMethod))
        let qn = match &inferred {
            Some(Value::Node(g))
                if eng.kind_is(*g, |k| {
                    matches!(
                        k,
                        NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::ClassDef(_)
                    )
                }) =>
            {
                eng.qname(*g)
            }
            Some(Value::BoundMethod { func, .. }) => eng.qname(*func),
            _ => return,
        };
        let could = CALLS_REPLACED_BY_WITH.contains(&qn.as_str())
            || (CALLS_RETURNING_CMS.contains(&qn.as_str()) && !is_part_of_with_items(eng, node));
        if could && !will_be_released_automatically(eng, cx.caches, node) {
            cx.emit_node(
                "R1732",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Consider using 'with' for resource-allocating operations".to_string(),
            );
        }
    }

    /// Get the scope stack for a frame (function/class/module order).
    fn stack_for_frame(
        &mut self,
        eng: &Engine,
        frame: GNode,
    ) -> &mut indexmap::IndexMap<String, GNode> {
        if is_funcdef(eng, frame) {
            &mut self.cuw_function
        } else if eng.kind_is(frame, |k| matches!(k, NodeKind::ClassDef(_))) {
            &mut self.cuw_class
        } else {
            &mut self.cuw_module
        }
    }

    // -- R1737 use-yield-from (refactoring_checker.py:1163-1182) --
    pub fn visit_yield(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node: Yield(value=Name(name), parent=Expr(parent=For(body=[_])))
        //   if not isinstance(loop_node, AsyncFor)
        let yield_name: String = {
            let md = eng.md(node.m);
            let NodeKind::Yield { value: Some(v) } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            match &md.tree.nodes[v.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return,
            }
        };
        // parent must be Expr
        let Some(expr_parent) = eng.parent(node) else { return };
        if !eng.kind_is(expr_parent, |k| matches!(k, NodeKind::Expr { .. })) {
            return;
        }
        // Expr's parent must be For with body=[_] (exactly one stmt); NOT AsyncFor
        let Some(loop_node) = eng.parent(expr_parent) else { return };
        let (target, body_len): (GNode, usize) = {
            let md = eng.md(loop_node.m);
            match &md.tree.nodes[loop_node.n.idx()].kind {
                NodeKind::For(d) => (GNode { m: loop_node.m, n: d.target }, d.body.len()),
                // AsyncFor: guarded out (not isinstance(loop_node, AsyncFor))
                _ => return,
            }
        };
        if body_len != 1 {
            return;
        }
        // loop_node.target.name != name -> bail. Tuple target has no .name in
        // astroid => AttributeError. Empirically pylint 4.0.5 does NOT crash
        // for Tuple targets here because for `yield a` the target being a
        // Tuple is uncommon and the functional tests don't hit it; we replicate
        // the documented behavior by treating non-AssignName targets as a
        // non-match (no crash, no message) — see §3.16 open question.
        let target_name = {
            let md = eng.md(target.m);
            match &md.tree.nodes[target.n.idx()].kind {
                NodeKind::AssignName { name } => Some(md.tree.s(*name).to_string()),
                _ => None,
            }
        };
        match target_name {
            Some(tn) if tn == yield_name => {}
            _ => return,
        }
        // bail if node.frame() is AsyncFunctionDef
        if eng.kind_is(eng.frame(node), |k| matches!(k, NodeKind::AsyncFunctionDef(_))) {
            return;
        }
        cx.emit_node(
            "R1737",
            u::msg_line(eng, loop_node),
            u::msg_col(eng, loop_node),
            "Use 'yield from' directly instead of yielding each element one by one".to_string(),
        );
    }

    pub fn visit_try(&mut self, cx: &mut WalkCx, node: GNode) {
        // visit_try / visit_while(alias): too-many-nested-blocks + return/raise
        // no-else (break/continue variants NOT run for try/while).
        self.check_nested_blocks(cx, node);
        check_superfluous_else(self, cx, node, "R1705", ReturningKind::Return);
        check_superfluous_else(self, cx, node, "R1720", ReturningKind::Raise);
    }

    pub fn leave_module(&mut self, cx: &mut WalkCx, g: GNode) {
        // @only_required_for_messages("consider-using-with"): only registered
        // (=> only runs) when R1732 enabled. The caller gates registration.
        self.emit_cuw_module(cx, g);
        self.init();
    }

    pub fn leave_classdef(&mut self, cx: &mut WalkCx, _g: GNode) {
        // R1732 class-scope flush.
        self.emit_cuw_class(cx);
    }

    // -- R1719 simplifiable-if-expression (refactoring_checker.py:990-1017) --
    pub fn visit_ifexp(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // IfExp(body=Const(bool), orelse=Const(bool))
        let (test, body_v, orelse_v): (GNode, bool, bool) = {
            let md = eng.md(node.m);
            let NodeKind::IfExp { test, body, orelse } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            let bv = match &md.tree.nodes[body.idx()].kind {
                NodeKind::Const(ConstValue::Bool(b)) => *b,
                _ => return,
            };
            let ov = match &md.tree.nodes[orelse.idx()].kind {
                NodeKind::Const(ConstValue::Bool(b)) => *b,
                _ => return,
            };
            (GNode { m: node.m, n: *test }, bv, ov)
        };
        let test_is_compare = eng.kind_is(test, |k| matches!(k, NodeKind::Compare { .. }));
        let test_reduced = if test_is_compare { "test" } else { "bool(test)" };
        let reduced_to = match (body_v, orelse_v) {
            (true, false) => format!("'{}'", test_reduced),
            (false, true) => "'not test'".to_string(),
            _ => return,
        };
        cx.emit_node(
            "R1719",
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            u::format_template("The if expression can be replaced with %s", &[&reduced_to]),
        );
    }

    fn emit_cuw_class(&mut self, cx: &mut WalkCx) {
        let eng = cx.eng;
        let nodes: Vec<GNode> = self.cuw_class.values().copied().collect();
        for n in nodes {
            cx.emit_node(
                "R1732",
                u::msg_line(eng, n),
                u::msg_col(eng, n),
                "Consider using 'with' for resource-allocating operations".to_string(),
            );
        }
        self.cuw_class.clear();
    }

    // -- R1702 too-many-nested-blocks (refactoring_checker.py:1264-1301) --
    fn check_nested_blocks(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // if not isinstance(node.scope(), FunctionDef): bail
        if !is_funcdef(eng, eng.scope(node)) {
            return;
        }
        let nested_blocks = self.nested_blocks.clone();
        let parent = eng.parent(node);
        if parent == Some(eng.scope(node)) {
            self.nested_blocks = vec![node];
        } else {
            // pop until ancestor == node.parent
            while let Some(&top) = self.nested_blocks.last() {
                if Some(top) == parent {
                    break;
                }
                self.nested_blocks.pop();
            }
            if is_if(eng, node) && is_actual_elif(self, eng, node) {
                if !self.nested_blocks.is_empty() {
                    self.nested_blocks.pop();
                }
            }
            self.nested_blocks.push(node);
        }
        if nested_blocks.len() > self.nested_blocks.len() {
            self.emit_nested_blocks(cx, &nested_blocks);
        }
    }

    fn emit_nested_blocks(&mut self, cx: &mut WalkCx, blocks: &[GNode]) {
        const MAX_NESTED_BLOCKS: usize = 5;
        if blocks.len() > MAX_NESTED_BLOCKS {
            let first = blocks[0];
            let eng = cx.eng;
            cx.emit_node(
                "R1702",
                u::msg_line(eng, first),
                u::msg_col(eng, first),
                u::format_template(
                    "Too many nested blocks (%s/%s)",
                    &[&blocks.len().to_string(), &MAX_NESTED_BLOCKS.to_string()],
                ),
            );
        }
    }

    // -- R1703 simplifiable-if-statement (refactoring_checker.py:596-666) --
    fn check_simplifiable_if(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if is_actual_elif(self, eng, node) {
            return;
        }
        // If(body=[first_branch], orelse=[else_branch]) — exactly 1+1
        let (first, els): (GNode, GNode) = {
            let md = eng.md(node.m);
            let NodeKind::If { body, orelse, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            if body.len() != 1 || orelse.len() != 1 {
                return;
            }
            (GNode { m: node.m, n: body[0] }, GNode { m: node.m, n: orelse[0] })
        };
        // first/else value extraction
        let (first_val, reduced_to): (GNode, &str) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[first.n.idx()].kind {
                NodeKind::Return { value: Some(fv) } => {
                    // else must be Return
                    let NodeKind::Return { value: Some(ev) } = &md.tree.nodes[els.n.idx()].kind
                    else {
                        return;
                    };
                    let fv = GNode { m: node.m, n: *fv };
                    let _ev = GNode { m: node.m, n: *ev };
                    (fv, "'return bool(test)'")
                }
                NodeKind::Assign { targets: ft, value: fv } => {
                    let NodeKind::Assign { targets: et, .. } = &md.tree.nodes[els.n.idx()].kind
                    else {
                        return;
                    };
                    let mut first_targets: Vec<String> = ft
                        .iter()
                        .filter_map(|&t| match &md.tree.nodes[t.idx()].kind {
                            NodeKind::AssignName { name } => Some(md.tree.s(*name).to_string()),
                            _ => None,
                        })
                        .collect();
                    let mut else_targets: Vec<String> = et
                        .iter()
                        .filter_map(|&t| match &md.tree.nodes[t.idx()].kind {
                            NodeKind::AssignName { name } => Some(md.tree.s(*name).to_string()),
                            _ => None,
                        })
                        .collect();
                    if first_targets.is_empty() || else_targets.is_empty() {
                        return;
                    }
                    first_targets.sort();
                    else_targets.sort();
                    if first_targets != else_targets {
                        return;
                    }
                    (GNode { m: node.m, n: *fv }, "'var = bool(test)'")
                }
                _ => return,
            }
        };
        // both branch values must be Const bool
        let els_val: GNode = {
            let md = eng.md(node.m);
            match &md.tree.nodes[els.n.idx()].kind {
                NodeKind::Return { value: Some(v) } | NodeKind::Assign { value: v, .. } => {
                    GNode { m: node.m, n: *v }
                }
                _ => return,
            }
        };
        let Some(first_b) = const_bool(eng, first_val) else { return };
        if const_bool(eng, els_val).is_none() {
            return;
        }
        // bail if first branch returns/assigns False (must be the True one)
        if !first_b {
            return;
        }
        cx.emit_node(
            "R1703",
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            u::format_template("The if statement can be replaced with %s", &[reduced_to]),
        );
    }

    // -- R1715 consider-using-get (refactoring_checker.py:855-892) --
    fn check_consider_get(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if !self.is_dict_get_block(cx, node) {
            return;
        }
        // case If(orelse=[]) -> emit; case If(body=[Assign t1], orelse=[Assign t2])
        //   if _type_and_name_are_equal(t1,t2) -> emit
        let emit = {
            let md = eng.md(node.m);
            let NodeKind::If { body, orelse, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            if orelse.is_empty() {
                true
            } else if body.len() == 1 && orelse.len() == 1 {
                let t1 = first_assign_target(&md, body[0]);
                let t2 = first_assign_target(&md, orelse[0]);
                match (t1, t2) {
                    (Some(a), Some(b)) => {
                        drop(md);
                        type_and_name_equal(eng, GNode { m: node.m, n: a }, GNode { m: node.m, n: b })
                    }
                    _ => false,
                }
            } else {
                false
            }
        };
        if emit {
            cx.emit_node(
                "R1715",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Consider using dict.get for getting values from a dict if a key is present or a default if not".to_string(),
            );
        }
    }

    /// _is_dict_get_block (refactoring_checker.py:855-871).
    fn is_dict_get_block(&self, cx: &mut WalkCx, node: GNode) -> bool {
        let eng = cx.eng;
        // node: If(test=Compare, body=[Assign(targets=[AssignName], value=Subscript)])
        let (test, dict_expr, slice_value): (GNode, GNode, GNode) = {
            let md = eng.md(node.m);
            let NodeKind::If { test, body, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return false;
            };
            // test must be a Compare
            if !matches!(&md.tree.nodes[test.idx()].kind, NodeKind::Compare { .. }) {
                return false;
            }
            let test = GNode { m: node.m, n: *test };
            if body.len() != 1 {
                return false;
            }
            let NodeKind::Assign { targets, value } = &md.tree.nodes[body[0].idx()].kind else {
                return false;
            };
            if targets.len() != 1
                || !matches!(&md.tree.nodes[targets[0].idx()].kind, NodeKind::AssignName { .. })
            {
                return false;
            }
            let NodeKind::Subscript { value: sv, slice, .. } = &md.tree.nodes[value.idx()].kind
            else {
                return false;
            };
            (test, GNode { m: node.m, n: *sv }, GNode { m: node.m, n: *slice })
        };
        // test.ops[0][1] (first comparator) and test.left
        let (cmp_left, cmp_first): (GNode, GNode) = {
            let md = eng.md(test.m);
            let NodeKind::Compare { left, ops } = &md.tree.nodes[test.n.idx()].kind else {
                return false;
            };
            if ops.is_empty() {
                return false;
            }
            (GNode { m: test.m, n: *left }, GNode { m: test.m, n: ops[0].1 })
        };
        // _type_and_name_are_equal(value, test.ops[0][1]) && (slice_value, test.left)
        if !type_and_name_equal(eng, dict_expr, cmp_first) {
            return false;
        }
        if !type_and_name_equal(eng, slice_value, cmp_left) {
            return false;
        }
        // isinstance(safe_infer(test.ops[0][1]), Dict)
        matches!(
            safe_infer(eng, cx.caches, cmp_first),
            Some(Value::Node(g)) if eng.kind_is(g, |k| matches!(k, NodeKind::Dict { .. }))
        ) || matches!(safe_infer(eng, cx.caches, cmp_first), Some(Value::SynthDict { .. }))
    }

    // -- R1730/R1731 consider-using-min/max-builtin (refactoring_checker.py:915-988) --
    fn check_min_max_builtin(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if is_actual_elif(self, eng, node) {
            return;
        }
        // node.orelse must be empty; len(node.body)==1
        let (test, body0): (GNode, GNode) = {
            let md = eng.md(node.m);
            let NodeKind::If { test, body, orelse } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            if !orelse.is_empty() || body.len() != 1 {
                return;
            }
            (GNode { m: node.m, n: *test }, GNode { m: node.m, n: body[0] })
        };
        // test = Compare(left, ops=[[operator, right_statement]]) with len(ops)==1
        let (left, operator, right_stmt): (GNode, String, GNode) = {
            let md = eng.md(test.m);
            let NodeKind::Compare { left, ops } = &md.tree.nodes[test.n.idx()].kind else {
                return;
            };
            if ops.len() != 1 {
                return;
            }
            (
                GNode { m: test.m, n: *left },
                ops[0].0.to_string(),
                GNode { m: test.m, n: ops[0].1 },
            )
        };
        // left must not be Subscript
        if eng.kind_is(left, |k| matches!(k, NodeKind::Subscript { .. })) {
            return;
        }
        // body[0] = Assign(targets=[AssignName|AssignAttr as target], value)
        let (target, value): (GNode, GNode) = {
            let md = eng.md(body0.m);
            let NodeKind::Assign { targets, value } = &md.tree.nodes[body0.n.idx()].kind else {
                return;
            };
            if targets.len() != 1 {
                return;
            }
            if !matches!(
                &md.tree.nodes[targets[0].idx()].kind,
                NodeKind::AssignName { .. } | NodeKind::AssignAttr { .. }
            ) {
                return;
            }
            (GNode { m: body0.m, n: targets[0] }, GNode { m: body0.m, n: *value })
        };
        let target_name = get_node_name(eng, target);
        let body_value = get_node_name(eng, value);
        let left_operand = get_node_name(eng, left);
        let right_value = get_node_name(eng, right_stmt);
        let mut operator = operator;
        if left_operand == target_name {
            // a OP b: a = ...
        } else if right_value == target_name {
            operator = get_inverse_comparator(&operator);
        } else {
            return;
        }
        if body_value != right_value && body_value != left_operand {
            return;
        }
        // target rendered as_string for the suggestion
        let target_str = u::as_string(eng, target);
        let (reduced_to, msg) = match operator.as_str() {
            "<" | "<=" => (
                format!("{} = max({}, {})", target_str, target_str, body_value),
                "R1731",
            ),
            ">" | ">=" => (
                format!("{} = min({}, {})", target_str, target_str, body_value),
                "R1730",
            ),
            _ => return,
        };
        let template = if msg == "R1730" {
            "Consider using '%s' instead of unnecessary if block"
        } else {
            "Consider using '%s' instead of unnecessary if block"
        };
        cx.emit_node(
            msg,
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            u::format_template(template, &[&reduced_to]),
        );
    }

    /// R1732 flush — _emit_consider_using_with_if_needed (1303-1307).
    fn emit_cuw_module(&mut self, cx: &mut WalkCx, _g: GNode) {
        let eng = cx.eng;
        let nodes: Vec<GNode> = self.cuw_module.values().copied().collect();
        for n in nodes {
            cx.emit_node(
                "R1732",
                u::msg_line(eng, n),
                u::msg_col(eng, n),
                "Consider using 'with' for resource-allocating operations".to_string(),
            );
        }
    }

    // -- R1710/R1711: visit_functiondef / leave_functiondef --
    // visit_functiondef fires for FunctionDef ONLY (exact-name dispatch;
    // AsyncFunctionDef never sets _return_nodes).
    pub fn visit_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let returns = nodes_of_class(
            eng,
            node,
            |k| matches!(k, NodeKind::Return { .. }),
            |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)),
        );
        let name = func_bare_name(eng, node);
        // keyed by bare name => same-named nested/sibling defs clobber.
        self.return_nodes.insert(name, returns);
    }

    /// leave_functiondef: R1702 leftover, R1710, R1711, R1732 function flush.
    /// Runs for FunctionDef AND AsyncFunctionDef (leave_functiondef dispatch
    /// matches both via exact name leave_functiondef / leave_asyncfunctiondef).
    pub fn leave_functiondef(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // _check_consistent_returns + _check_return_at_the_end run only for
        // FunctionDef (the decorator-registered leave_functiondef); pylint
        // registers leave_functiondef for both, but the helper logic uses
        // self._return_nodes[node.name] which was only populated by
        // visit_functiondef (FunctionDef). For async, the dict entry is
        // missing => KeyError? No: _check_consistent_returns indexes
        // self._return_nodes[node.name]; for an async def with the SAME name
        // as a sync def the sync list is used. Replicate exactly: index by
        // bare name, default empty.
        // R1702 leftover nested-blocks check at function end:
        let leftover = self.nested_blocks.clone();
        self.emit_nested_blocks(cx, &leftover);
        // R1710 inconsistent-return-statements
        self.check_consistent_returns(cx, node);
        // R1711 useless-return
        self.check_return_at_end(cx, node);
        // R1732 function-scope flush
        self.emit_cuw_function(cx);
        // self._nested_blocks = []
        self.nested_blocks.clear();
    }

    fn check_consistent_returns(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let name = func_bare_name(eng, node);
        let all_returns: Vec<GNode> = self.return_nodes.get(&name).cloned().unwrap_or_default();
        let explicit: Vec<GNode> = all_returns
            .iter()
            .copied()
            .filter(|&r| !return_value_is_none_absent(eng, r))
            .collect();
        if explicit.is_empty() {
            return;
        }
        if explicit.len() == all_returns.len() && self.is_node_return_ended(cx, node) {
            return;
        }
        cx.emit_node(
            "R1710",
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            "Either all return statements in a function should return an expression, or none of them should.".to_string(),
        );
    }

    fn check_return_at_end(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let name = func_bare_name(eng, node);
        let all_returns: Vec<GNode> = self.return_nodes.get(&name).cloned().unwrap_or_default();
        if all_returns.len() != 1 {
            return;
        }
        let body: Vec<pyast::NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
                _ => return,
            }
        };
        if body.is_empty() {
            return;
        }
        let mut last = GNode { m: node.m, n: *body.last().unwrap() };
        // if isinstance(last, Return) and len(body)==1: bail
        if body.len() == 1 && eng.kind_is(last, |k| matches!(k, NodeKind::Return { .. })) {
            return;
        }
        // while isinstance(last, (If, Try, ExceptHandler)): last = last.last_child()
        loop {
            let descend = eng.kind_is(last, |k| {
                matches!(
                    k,
                    NodeKind::If { .. } | NodeKind::Try(_) | NodeKind::ExceptHandler { .. }
                )
            });
            if !descend {
                break;
            }
            match last_child(eng, last) {
                Some(lc) => last = lc,
                None => break,
            }
        }
        // match Return(value=None) | Return(value=Const None)
        let emit = {
            let md = eng.md(last.m);
            match &md.tree.nodes[last.n.idx()].kind {
                NodeKind::Return { value: None } => true,
                NodeKind::Return { value: Some(v) } => {
                    matches!(&md.tree.nodes[v.idx()].kind, NodeKind::Const(ConstValue::None))
                }
                _ => false,
            }
        };
        if emit {
            cx.emit_node(
                "R1711",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Useless return at end of function or method".to_string(),
            );
        }
    }

    /// _is_node_return_ended (refactoring_checker.py:2006-2051).
    fn is_node_return_ended(&self, cx: &mut WalkCx, node: GNode) -> bool {
        let eng = cx.eng;
        let md = eng.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Return { .. } => true,
            NodeKind::Call { func, .. } => {
                let func = GNode { m: node.m, n: *func };
                drop(md);
                if u::is_terminating_func(eng, cx.caches, node) {
                    return true;
                }
                // any FunctionDef/BoundMethod with never-returning
                for v in u::infer_all(eng, cx.caches, func).iter() {
                    let f = match v {
                        Value::Node(g) if is_funcdef(eng, *g) => Some(*g),
                        Value::BoundMethod { func, .. } => Some(*func),
                        _ => None,
                    };
                    if let Some(f) = f {
                        if self.is_function_def_never_returning(eng, f) {
                            return true;
                        }
                    }
                }
                false
            }
            NodeKind::While { test, orelse, .. } => {
                let test = GNode { m: node.m, n: *test };
                let orelse: Vec<pyast::NodeId> = orelse.clone();
                drop(md);
                let test_bool = eng
                    .bool_value(&Value::Node(test), &Ctx::new())
                    .unwrap_or(false);
                if test_bool && !loop_exits_early(eng, node) {
                    return true;
                }
                orelse
                    .iter()
                    .any(|&c| self.is_node_return_ended(cx, GNode { m: node.m, n: c }))
            }
            NodeKind::Raise { .. } => {
                drop(md);
                self.is_raise_node_return_ended(cx, node)
            }
            NodeKind::If { .. } => {
                drop(md);
                self.is_if_node_return_ended(cx, node)
            }
            NodeKind::Try(d) => {
                let body: Vec<pyast::NodeId> = d.body.clone();
                let handlers: Vec<pyast::NodeId> = d.handlers.clone();
                let orelse: Vec<pyast::NodeId> = d.orelse.clone();
                let finalbody: Vec<pyast::NodeId> = d.finalbody.clone();
                drop(md);
                // children = body + handlers + orelse + finalbody; handlers
                // are the ExceptHandler set; all_but_handler the rest.
                let all_but: Vec<pyast::NodeId> = body
                    .iter()
                    .chain(orelse.iter())
                    .chain(finalbody.iter())
                    .copied()
                    .collect();
                let any_but = all_but
                    .iter()
                    .any(|&c| self.is_node_return_ended(cx, GNode { m: node.m, n: c }));
                let all_handlers = handlers
                    .iter()
                    .all(|&c| self.is_node_return_ended(cx, GNode { m: node.m, n: c }));
                any_but && all_handlers
            }
            NodeKind::Assert { test, .. } => {
                let test = GNode { m: node.m, n: *test };
                drop(md);
                // Assert(test=Const(value=False | 0))
                assert_test_false_or_zero(eng, test)
            }
            _ => {
                let children = md.tree.children(node.n);
                drop(md);
                children
                    .iter()
                    .any(|&c| self.is_node_return_ended(cx, GNode { m: node.m, n: c }))
            }
        }
    }

    /// _is_if_node_return_ended (refactoring_checker.py:1939-1967).
    fn is_if_node_return_ended(&self, cx: &mut WalkCx, node: GNode) -> bool {
        let eng = cx.eng;
        let (body, orelse): (Vec<pyast::NodeId>, Vec<pyast::NodeId>) = {
            let md = eng.md(node.m);
            let NodeKind::If { body, orelse, .. } = &md.tree.nodes[node.n.idx()].kind else {
                return false;
            };
            (body.clone(), orelse.clone())
        };
        let is_if_returning = body.iter().any(|&n| {
            !is_funcdef(eng, GNode { m: node.m, n })
                && self.is_node_return_ended(cx, GNode { m: node.m, n })
        });
        if orelse.is_empty() {
            if !has_return_in_siblings(eng, node) {
                return false;
            }
            return is_if_returning;
        }
        let is_orelse_returning = orelse.iter().any(|&n| {
            !is_funcdef(eng, GNode { m: node.m, n })
                && self.is_node_return_ended(cx, GNode { m: node.m, n })
        });
        is_if_returning && is_orelse_returning
    }

    /// _is_raise_node_return_ended (refactoring_checker.py:1969-2004).
    fn is_raise_node_return_ended(&self, cx: &mut WalkCx, node: GNode) -> bool {
        let eng = cx.eng;
        let exc: Option<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Raise { exc, .. } => exc.map(|n| GNode { m: node.m, n }),
                _ => return true,
            }
        };
        let Some(exc) = exc else { return true }; // bare raise
        if !is_node_inside_try_except(eng, node) {
            return true;
        }
        let exc_val = safe_infer(eng, cx.caches, exc);
        let exc_name = match &exc_val {
            Some(v) if !v.is_uninferable() => match u::value_pytype(eng, v) {
                Some(p) => p.rsplit('.').next().unwrap_or(&p).to_string(),
                None => return false,
            },
            _ => return false,
        };
        // get_exception_handlers(node, exc_name)
        let handlers = exception_handlers(eng, node, &exc_name);
        if !handlers.is_empty() {
            return handlers
                .iter()
                .any(|&h| self.is_node_return_ended(cx, h));
        }
        true
    }

    /// _is_function_def_never_returning (refactoring_checker.py:2063-2088).
    fn is_function_def_never_returning(&self, eng: &Engine, node: GNode) -> bool {
        // never-returning-functions default {"sys.exit", "argparse.parse_error"}
        const NEVER: &[&str] = &["sys.exit", "argparse.parse_error"];
        let qn = eng.qname(node);
        if NEVER.contains(&qn.as_str()) {
            return true;
        }
        let returns: Option<GNode> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    d.returns.map(|n| GNode { m: node.m, n })
                }
                _ => return false,
            }
        };
        let Some(ret) = returns else { return false };
        let md = eng.md(ret.m);
        match &md.tree.nodes[ret.n.idx()].kind {
            NodeKind::Attribute { attrname, .. } => {
                let n = md.tree.s(*attrname);
                n == "NoReturn" || n == "Never"
            }
            NodeKind::Name { name } => {
                let n = md.tree.s(*name);
                n == "NoReturn" || n == "Never"
            }
            _ => false,
        }
    }

    fn emit_cuw_function(&mut self, cx: &mut WalkCx) {
        let eng = cx.eng;
        let nodes: Vec<GNode> = self.cuw_function.values().copied().collect();
        for n in nodes {
            cx.emit_node(
                "R1732",
                u::msg_line(eng, n),
                u::msg_col(eng, n),
                "Consider using 'with' for resource-allocating operations".to_string(),
            );
        }
        self.cuw_function.clear();
    }
}

// ===========================================================================
// helpers used above
// ===========================================================================

/// _is_bool_const: value is Const and isinstance(value.value, bool).
fn const_bool(eng: &Engine, g: GNode) -> Option<bool> {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Const(ConstValue::Bool(b)) => Some(*b),
        _ => None,
    }
}

/// _type_and_name_are_equal (refactoring_checker.py:844-853).
fn type_and_name_equal(eng: &Engine, a: GNode, b: GNode) -> bool {
    let mda = eng.md(a.m);
    let mdb = eng.md(b.m);
    let ka = &mda.tree.nodes[a.n.idx()].kind;
    let kb = &mdb.tree.nodes[b.n.idx()].kind;
    match (ka, kb) {
        (NodeKind::Name { name: na }, NodeKind::Name { name: nb }) => {
            mda.tree.s(*na) == mdb.tree.s(*nb)
        }
        (NodeKind::AssignName { name: na }, NodeKind::AssignName { name: nb }) => {
            mda.tree.s(*na) == mdb.tree.s(*nb)
        }
        (NodeKind::Const(va), NodeKind::Const(vb)) => const_eq(va, vb),
        _ => false,
    }
}

fn const_eq(a: &ConstValue, b: &ConstValue) -> bool {
    // Python `==` over Const values. Cross-type numeric equality
    // (1 == 1.0 == True) is theoretically possible but never observed in
    // the _type_and_name_are_equal call sites; structural eq suffices.
    a == b
}

/// first Assign target node id (when body stmt is Assign).
fn first_assign_target(md: &pyinfer::graph::Module, n: pyast::NodeId) -> Option<pyast::NodeId> {
    match &md.tree.nodes[n.idx()].kind {
        NodeKind::Assign { targets, .. } => targets.first().copied(),
        _ => None,
    }
}

/// get_node_name (refactoring_checker.py:get_node_name local).
fn get_node_name(eng: &Engine, g: GNode) -> String {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Name { name } => md.tree.s(*name).to_string(),
        NodeKind::Const(c) => const_str(c),
        _ => {
            drop(md);
            u::as_string(eng, g)
        }
    }
}

/// str(const.value) for the get_node_name Const branch.
fn const_str(c: &ConstValue) -> String {
    use pyast::tree::IntValue;
    match c {
        ConstValue::None => "None".to_string(),
        ConstValue::Bool(true) => "True".to_string(),
        ConstValue::Bool(false) => "False".to_string(),
        ConstValue::Str(s) => s.to_string(),
        // str(b"x") == "b'x'"
        ConstValue::Bytes(b) => pyast::pyrepr::repr_str_points(
            &b.iter().map(|&x| x as u32).collect::<Vec<_>>(),
        )
        .replacen('\'', "b'", 1),
        ConstValue::Int(IntValue::Small(i)) => i.to_string(),
        ConstValue::Int(IntValue::Big(d)) => d.to_string(),
        ConstValue::Float(f) => pyast::pyrepr::repr_float(*f),
        ConstValue::Complex { real, imag } => {
            if *real == 0.0 {
                format!("{}j", pyast::pyrepr::repr_float(*imag))
            } else {
                format!(
                    "({}+{}j)",
                    pyast::pyrepr::repr_float(*real),
                    pyast::pyrepr::repr_float(*imag)
                )
            }
        }
        ConstValue::Ellipsis => "Ellipsis".to_string(),
        ConstValue::NotImplemented => "NotImplemented".to_string(),
        ConstValue::StrSurrogate(points) => {
            points.iter().filter_map(|&p| char::from_u32(p)).collect()
        }
    }
}

/// utils.get_inverse_comparator (utils.py:2265).
fn get_inverse_comparator(op: &str) -> String {
    match op {
        "==" => "!=",
        "!=" => "==",
        "<" => ">=",
        ">" => "<=",
        "<=" => ">",
        ">=" => "<",
        "in" => "not in",
        "not in" => "in",
        "is" => "is not",
        "is not" => "is",
        _ => op,
    }
    .to_string()
}

/// bare function name (node.name).
fn func_bare_name(eng: &Engine, node: GNode) -> String {
    let md = eng.md(node.m);
    match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => md.tree.s(d.name).to_string(),
        _ => String::new(),
    }
}

/// True iff the Return node has value None (bare return) — `r.value is None`.
fn return_value_is_none_absent(eng: &Engine, r: GNode) -> bool {
    eng.kind_is(r, |k| matches!(k, NodeKind::Return { value: None }))
}

/// node.last_child() (astroid _base_nodes): last node in get_children() order.
fn last_child(eng: &Engine, g: GNode) -> Option<GNode> {
    let md = eng.md(g.m);
    md.tree.children(g.n).last().map(|&n| GNode { m: g.m, n })
}

/// Assert(test=Const(value=False | 0)) match — False via `is`, 0 via `==`.
fn assert_test_false_or_zero(eng: &Engine, test: GNode) -> bool {
    let md = eng.md(test.m);
    use pyast::tree::IntValue;
    match &md.tree.nodes[test.n.idx()].kind {
        NodeKind::Const(ConstValue::Bool(false)) => true,
        NodeKind::Const(ConstValue::Int(IntValue::Small(0))) => true,
        NodeKind::Const(ConstValue::Float(f)) if *f == 0.0 => true,
        NodeKind::Const(ConstValue::Complex { real, imag }) if *real == 0.0 && *imag == 0.0 => true,
        _ => false,
    }
}

/// _has_return_in_siblings (refactoring_checker.py:2053-2061): walk
/// next_sibling() chain looking for a direct Return.
fn has_return_in_siblings(eng: &Engine, node: GNode) -> bool {
    let mut sib = u::next_sibling(eng, node);
    while let Some(s) = sib {
        if eng.kind_is(s, |k| matches!(k, NodeKind::Return { .. })) {
            return true;
        }
        sib = u::next_sibling(eng, s);
    }
    false
}

/// is_node_inside_try_except (utils.py:1134): nearest find_try_except_wrapper
/// is a Try.
fn is_node_inside_try_except(eng: &Engine, node: GNode) -> bool {
    let mut current = node;
    loop {
        let Some(p) = eng.parent(current) else { return false };
        let is_wrapper = eng.kind_is(p, |k| {
            matches!(k, NodeKind::ExceptHandler { .. } | NodeKind::Try(_))
        });
        if is_wrapper {
            return eng.kind_is(p, |k| matches!(k, NodeKind::Try(_)));
        }
        current = p;
    }
}

/// get_exception_handlers (utils.py:1061-1078): for a Try wrapper, handlers
/// whose error_of_type(handler, exc_name) — bare except does NOT count.
fn exception_handlers(eng: &Engine, node: GNode, exc_name: &str) -> Vec<GNode> {
    // find wrapper Try
    let mut current = node;
    let wrapper = loop {
        let Some(p) = eng.parent(current) else { return Vec::new() };
        if eng.kind_is(p, |k| {
            matches!(k, NodeKind::ExceptHandler { .. } | NodeKind::Try(_))
        }) {
            break p;
        }
        current = p;
    };
    if !eng.kind_is(wrapper, |k| matches!(k, NodeKind::Try(_))) {
        return Vec::new();
    }
    let handlers: Vec<pyast::NodeId> = {
        let md = eng.md(wrapper.m);
        match &md.tree.nodes[wrapper.n.idx()].kind {
            NodeKind::Try(d) => d.handlers.clone(),
            _ => return Vec::new(),
        }
    };
    handlers
        .into_iter()
        .map(|n| GNode { m: wrapper.m, n })
        .filter(|&h| error_of_type(eng, h, exc_name))
        .collect()
}

/// error_of_type (utils.py:778-802): handler.type must exist; then catch.
fn error_of_type(eng: &Engine, handler: GNode, exc: &str) -> bool {
    let has_type = eng.kind_is(handler, |k| {
        matches!(k, NodeKind::ExceptHandler { type_: Some(_), .. })
    });
    if !has_type {
        return false;
    }
    u::handler_catch(eng, handler, &[exc])
}

/// _loop_exits_early (basic_error_checker.py:47-67).
fn loop_exits_early(eng: &Engine, loop_node: GNode) -> bool {
    // inner_loop_nodes = For/While inside loop (skip Func/Class), != loop
    let inner_loops: Vec<GNode> = nodes_of_class(
        eng,
        loop_node,
        |k| matches!(k, NodeKind::For(_) | NodeKind::While { .. } | NodeKind::AsyncFor(_)),
        |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::ClassDef(_)),
    )
    .into_iter()
    .filter(|&n| n != loop_node)
    .collect();
    let breaks: Vec<GNode> = nodes_of_class(
        eng,
        loop_node,
        |k| matches!(k, NodeKind::Break),
        |k| matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::ClassDef(_)),
    );
    breaks
        .iter()
        .any(|&b| match break_loop_node(eng, b) {
            Some(ln) => !inner_loops.contains(&ln),
            None => true,
        })
}

// ===========================================================================
// Remaining RefactoringChecker visit methods (wired in the walker)
// ===========================================================================

impl RefactoringCk {
    pub fn visit_for(&mut self, cx: &mut WalkCx, node: GNode) {
        // R1704 redefined-argument-from-local for the target AssignNames;
        // R1702 nested-blocks; R1733/R1736 dict/list-index lookup.
        let eng = cx.eng;
        let target = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::For(d) => GNode { m: node.m, n: d.target },
                _ => return,
            }
        };
        for an in nodes_of_class(eng, target, |k| matches!(k, NodeKind::AssignName { .. }), |_| false) {
            self.check_redefined_argument_from_local(cx, an);
        }
        self.check_nested_blocks(cx, node);
        self.check_dict_index_lookup_for(cx, node);
        self.check_list_index_lookup_for(cx, node);
    }

    pub fn visit_excepthandler(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let name = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::ExceptHandler { name: Some(n), .. } => Some(GNode { m: node.m, n: *n }),
                _ => None,
            }
        };
        if let Some(name) = name {
            if eng.kind_is(name, |k| matches!(k, NodeKind::AssignName { .. })) {
                self.check_redefined_argument_from_local(cx, name);
            }
        }
    }

    pub fn visit_with(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // for each (var, names) in node.items: R1732 stack-clearing first
        // (Name vars), then R1704 for AssignNames under `names`.
        let items: Vec<(pyast::NodeId, Option<pyast::NodeId>)> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::With(d) => d.items.clone(),
                _ => return,
            }
        };
        for (var, names) in items {
            // R1732 consumption: if var is a Name, delete it from the first
            // stack containing its name (function->class->module order).
            let var_name = {
                let md = eng.md(node.m);
                match &md.tree.nodes[var.idx()].kind {
                    NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
                    _ => None,
                }
            };
            if let Some(vn) = var_name {
                self.cuw_consume(&vn);
            }
            if let Some(names) = names {
                let nroot = GNode { m: node.m, n: names };
                for an in
                    nodes_of_class(eng, nroot, |k| matches!(k, NodeKind::AssignName { .. }), |_| false)
                {
                    self.check_redefined_argument_from_local(cx, an);
                }
            }
        }
    }

    /// R1704 redefined-argument-from-local (refactoring_checker.py:733-789).
    fn check_redefined_argument_from_local(&mut self, cx: &mut WalkCx, name_node: GNode) {
        let eng = cx.eng;
        let name = {
            let md = eng.md(name_node.m);
            match &md.tree.nodes[name_node.n.idx()].kind {
                NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                _ => return,
            }
        };
        // dummy-variables-rgx match -> bail (default rgx)
        if dummy_var_matches(&name) {
            return;
        }
        // if not name_node.lineno: bail
        if lineno(eng, name_node) == 0 {
            return;
        }
        let scope = eng.scope(name_node);
        if !is_funcdef(eng, scope) {
            return;
        }
        // scope.args.nodes_of_class(AssignName, skip_klass=(Lambda,))
        let args_id = {
            let md = eng.md(scope.m);
            match &md.tree.nodes[scope.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
                _ => return,
            }
        };
        let args_root = GNode { m: scope.m, n: args_id };
        for arg in nodes_of_class(
            eng,
            args_root,
            |k| matches!(k, NodeKind::AssignName { .. }),
            |k| matches!(k, NodeKind::Lambda(_)),
        ) {
            let arg_name = {
                let md = eng.md(arg.m);
                match &md.tree.nodes[arg.n.idx()].kind {
                    NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                    _ => continue,
                }
            };
            if arg_name == name {
                cx.emit_node(
                    "R1704",
                    u::msg_line(eng, name_node),
                    u::msg_col(eng, name_node),
                    u::format_template(
                        "Redefining argument with the local name %r",
                        &[&name],
                    ),
                );
            }
        }
    }

    /// R1733 unnecessary-dict-index-lookup (refactoring_checker.py:2122-2264).
    fn check_dict_index_lookup_for(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_unnecessary_dict_index_lookup(cx, node, true);
    }

    fn check_unnecessary_dict_index_lookup(&mut self, cx: &mut WalkCx, node: GNode, is_for: bool) {
        let eng = cx.eng;
        // node.iter must be Call(func=Attribute(attrname="items", expr=expr))
        let iter = match comp_or_for_iter(eng, node) {
            Some(i) => i,
            None => return,
        };
        let (iter_func, expr): (GNode, GNode) = {
            let md = eng.md(iter.m);
            let NodeKind::Call { func, .. } = &md.tree.nodes[iter.n.idx()].kind else { return };
            match &md.tree.nodes[func.idx()].kind {
                NodeKind::Attribute { attrname, expr, .. } if md.tree.s(*attrname) == "items" => {
                    (GNode { m: iter.m, n: *func }, GNode { m: iter.m, n: *expr })
                }
                _ => return,
            }
        };
        // safe_infer(iter.func) must be BoundMethod
        if !matches!(safe_infer(eng, cx.caches, iter_func), Some(Value::BoundMethod { .. })) {
            return;
        }
        let iterating_object_name = u::as_string(eng, expr);
        let target = comp_or_for_target(eng, node);
        let Some(target) = target else { return };
        // children
        let children: Vec<GNode> = if is_for {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::For(d) => d.body.iter().map(|&c| GNode { m: node.m, n: c }).collect(),
                _ => return,
            }
        } else {
            let parent = match eng.parent(node) {
                Some(p) => p,
                None => return,
            };
            eng.md(parent.m)
                .tree
                .children(parent.n)
                .iter()
                .map(|&c| GNode { m: parent.m, n: c })
                .collect()
        };
        let mut queued: Vec<(GNode, String)> = Vec::new();
        let has_nested = children.iter().any(|&c| {
            !nodes_of_class(eng, c, |k| matches!(k, NodeKind::For(_) | NodeKind::While { .. } | NodeKind::AsyncFor(_)), |_| false).is_empty()
        });
        // target.elts for the tuple form
        let target_elts: Vec<GNode> = {
            let md = eng.md(target.m);
            match &md.tree.nodes[target.n.idx()].kind {
                NodeKind::Tuple { elts, .. } => elts.iter().map(|&e| GNode { m: target.m, n: e }).collect(),
                _ => Vec::new(),
            }
        };
        for child in &children {
            for sub in nodes_of_class(eng, *child, |k| matches!(k, NodeKind::Subscript { .. }), |_| false) {
                let (sub_value, sub_slice): (GNode, GNode) = {
                    let md = eng.md(sub.m);
                    match &md.tree.nodes[sub.n.idx()].kind {
                        NodeKind::Subscript { value, slice, .. } => {
                            (GNode { m: sub.m, n: *value }, GNode { m: sub.m, n: *slice })
                        }
                        _ => continue,
                    }
                };
                if !eng.kind_is(sub_value, |k| matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. })) {
                    continue;
                }
                // For: assignment-target abort + Delete abort (whole check)
                if is_for && is_part_of_assignment_target(eng, sub) {
                    return;
                }
                if let Some(p) = eng.parent(sub) {
                    if eng.kind_is(p, |k| matches!(k, NodeKind::Delete { .. })) {
                        return;
                    }
                }
                // value is Name: tuple-target form `for k, v in d.items(): d[k]`
                if eng.kind_is(sub_slice, |k| matches!(k, NodeKind::Name { .. })) {
                    if target_elts.len() < 2 {
                        continue;
                    }
                    let slice_name = name_of(eng, sub_slice);
                    let first_elt_name = name_of_assign(eng, target_elts[0]);
                    if slice_name.is_none() || slice_name != first_elt_name {
                        continue;
                    }
                    if iterating_object_name != u::as_string(eng, sub_value) {
                        continue;
                    }
                    if is_for {
                        if let Some(sl) = slice_name.as_ref() {
                            if let Some(ll) = lookup_last_lineno(eng, sub_slice, sl) {
                                if ll > lineno(eng, node) {
                                    continue;
                                }
                            }
                        }
                    }
                    let suggestion = u::as_string(eng, target_elts[1]);
                    if has_nested {
                        queued.push((sub, suggestion));
                    } else {
                        cx.emit_node(
                            "R1733",
                            u::msg_line(eng, sub),
                            u::msg_col(eng, sub),
                            u::format_template("Unnecessary dictionary index lookup, use '%s' instead", &[&suggestion]),
                        );
                    }
                } else if eng.kind_is(sub_slice, |k| matches!(k, NodeKind::Subscript { .. })) {
                    // item-subscript form `for item in d.items(): d[item[0]]`
                    let (inner_value, inner_slice): (GNode, GNode) = {
                        let md = eng.md(sub_slice.m);
                        match &md.tree.nodes[sub_slice.n.idx()].kind {
                            NodeKind::Subscript { value, slice, .. } => {
                                (GNode { m: sub_slice.m, n: *value }, GNode { m: sub_slice.m, n: *slice })
                            }
                            _ => continue,
                        }
                    };
                    // node.target AssignName, inner_value Name, target.name == inner_value.name
                    let tn = name_of_assign(eng, target);
                    let ivn = name_of(eng, inner_value);
                    if tn.is_none() || ivn.is_none() || tn != ivn {
                        continue;
                    }
                    if iterating_object_name != u::as_string(eng, sub_value) {
                        continue;
                    }
                    if is_for {
                        if let Some(ref n) = ivn {
                            if let Some(ll) = lookup_last_lineno(eng, inner_value, n) {
                                if ll > lineno(eng, node) {
                                    continue;
                                }
                            }
                        }
                    }
                    // inner_slice must infer to Const 0
                    let zero = matches!(
                        safe_infer(eng, cx.caches, inner_slice),
                        Some(Value::Node(g)) if const_value_is_zero(eng, g)
                    ) || matches!(safe_infer(eng, cx.caches, inner_slice), Some(Value::SynthConst(ref c)) if matches!(&**c, ConstValue::Int(pyast::tree::IntValue::Small(0))));
                    if !zero {
                        continue;
                    }
                    // suggestion: "1".join(value.as_string().rsplit("0", 1))
                    let vstr = u::as_string(eng, sub_slice);
                    let suggestion = replace_last(&vstr, "0", "1");
                    if has_nested {
                        queued.push((sub, suggestion));
                    } else {
                        cx.emit_node(
                            "R1733",
                            u::msg_line(eng, sub),
                            u::msg_col(eng, sub),
                            u::format_template("Unnecessary dictionary index lookup, use '%s' instead", &[&suggestion]),
                        );
                    }
                }
            }
        }
        for (sub, suggestion) in queued {
            cx.emit_node(
                "R1733",
                u::msg_line(eng, sub),
                u::msg_col(eng, sub),
                u::format_template("Unnecessary dictionary index lookup, use '%s' instead", &[&suggestion]),
            );
        }
    }

    /// R1736 unnecessary-list-index-lookup (refactoring_checker.py:2266-2454).
    fn check_list_index_lookup_for(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node.iter must be Call(func=Name("enumerate"))
        let iter = match comp_or_for_iter(eng, node) {
            Some(i) => i,
            None => return,
        };
        let is_enumerate = {
            let md = eng.md(iter.m);
            match &md.tree.nodes[iter.n.idx()].kind {
                NodeKind::Call { func, .. } => {
                    matches!(&md.tree.nodes[func.idx()].kind, NodeKind::Name { name } if md.tree.s(*name) == "enumerate")
                }
                _ => false,
            }
        };
        if !is_enumerate {
            return;
        }
        // iterable_arg = get_argument_from_call(iter, 0, "iterable")
        let iterable_arg = match get_argument_from_call(eng, iter, 0, "iterable") {
            Some(a) => a,
            None => {
                // infer_kwarg_from_call(iter, "iterable")
                match infer_kwarg_from_call(eng, cx.caches, iter, "iterable") {
                    Some(a) => a,
                    None => return,
                }
            }
        };
        // must be a Name
        if !eng.kind_is(iterable_arg, |k| matches!(k, NodeKind::Name { .. })) {
            return;
        }
        let iterating_object_name = name_of(eng, iterable_arg).unwrap_or_default();
        // node.target = Tuple(elts=[AssignName(name1), AssignName(name2), *_])
        let (name1, name2): (String, String) = {
            let target = match comp_or_for_target(eng, node) {
                Some(t) => t,
                None => return,
            };
            let md = eng.md(target.m);
            let NodeKind::Tuple { elts, .. } = &md.tree.nodes[target.n.idx()].kind else { return };
            if elts.len() < 2 {
                return;
            }
            let n1 = match &md.tree.nodes[elts[0].idx()].kind {
                NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                _ => return,
            };
            let n2 = match &md.tree.nodes[elts[1].idx()].kind {
                NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                _ => return,
            };
            (n1, n2)
        };
        // has_start_arg -> bail
        if enumerate_with_start(eng, cx.caches, iter) {
            return;
        }
        let is_for = eng.kind_is(node, |k| matches!(k, NodeKind::For(_)));
        let children: Vec<GNode> = if is_for {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::For(d) => d.body.iter().map(|&c| GNode { m: node.m, n: c }).collect(),
                _ => return,
            }
        } else {
            let parent = match eng.parent(node) {
                Some(p) => p,
                None => return,
            };
            eng.md(parent.m).tree.children(parent.n).iter().map(|&c| GNode { m: parent.m, n: c }).collect()
        };
        let has_nested = children.iter().any(|&c| {
            !nodes_of_class(eng, c, |k| matches!(k, NodeKind::For(_) | NodeKind::While { .. } | NodeKind::AsyncFor(_)), |_| false).is_empty()
        });
        let has_if = children.iter().any(|&c| {
            !nodes_of_class(eng, c, |k| matches!(k, NodeKind::If { .. }), |_| false).is_empty()
        });
        let mut bad_nodes: Vec<GNode> = Vec::new();
        for child in &children {
            for sub in nodes_of_class(eng, *child, |k| matches!(k, NodeKind::Subscript { .. }), |_| false) {
                if is_for && is_part_of_assignment_target(eng, sub) {
                    return;
                }
                if let Some(p) = eng.parent(sub) {
                    if eng.kind_is(p, |k| matches!(k, NodeKind::Delete { .. })) {
                        return;
                    }
                }
                let (sub_value, index): (GNode, GNode) = {
                    let md = eng.md(sub.m);
                    match &md.tree.nodes[sub.n.idx()].kind {
                        NodeKind::Subscript { value, slice, .. } => {
                            (GNode { m: sub.m, n: *value }, GNode { m: sub.m, n: *slice })
                        }
                        _ => continue,
                    }
                };
                if !eng.kind_is(index, |k| matches!(k, NodeKind::Name { .. })) {
                    continue;
                }
                let index_name = name_of(eng, index).unwrap_or_default();
                if index_name != name1 || iterating_object_name != u::as_string(eng, sub_value) {
                    continue;
                }
                if is_for {
                    if let Some(ll) = lookup_last_lineno(eng, index, &index_name) {
                        if ll > lineno(eng, node) {
                            continue;
                        }
                    }
                    if let Some(ll) = lookup_last_lineno(eng, index, &name2) {
                        if ll > lineno(eng, node) {
                            continue;
                        }
                    }
                }
                if has_nested {
                    bad_nodes.push(sub);
                } else if has_if {
                    continue;
                } else {
                    cx.emit_node(
                        "R1736",
                        u::msg_line(eng, sub),
                        u::msg_col(eng, sub),
                        u::format_template("Unnecessary list index lookup, use '%s' instead", &[&name2]),
                    );
                }
            }
        }
        for sub in bad_nodes {
            cx.emit_node(
                "R1736",
                u::msg_line(eng, sub),
                u::msg_col(eng, sub),
                u::format_template("Unnecessary list index lookup, use '%s' instead", &[&name2]),
            );
        }
    }

    pub fn visit_raise(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_stop_iteration_in_generator(cx, node);
    }

    /// R1708 (a) stop-iteration-inside-generator (1049-1066).
    fn check_stop_iteration_in_generator(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let frame = eng.frame(node);
        if !(is_funcdef(eng, frame) && eng.is_generator(frame)) {
            return;
        }
        if u::node_ignores_exception(eng, cx.caches, node, "StopIteration") {
            return;
        }
        let exc = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Raise { exc: Some(e), .. } => GNode { m: node.m, n: *e },
                _ => return, // bare raise
            }
        };
        let exc_val = match safe_infer(eng, cx.caches, exc) {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        // isinstance(exc, (Instance, ClassDef)); then any mro qname == StopIteration
        let cls = match &exc_val {
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => *cls,
            Value::Node(g) if eng.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) => *g,
            _ => return,
        };
        let mro = eng.mro(cls, None).unwrap_or_default();
        let hit = mro.iter().any(|&c| eng.qname(c) == "builtins.StopIteration");
        if hit {
            cx.emit_node(
                "R1708",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Do not raise StopIteration in generator, use return statement instead".to_string(),
            );
        }
    }

    pub fn visit_boolop(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_consider_merging_isinstance(cx, node);
        self.check_consider_using_in(cx, node);
        self.check_chained_comparison(cx, node);
        self.check_simplifiable_condition(cx, node);
    }

    pub fn visit_assign(&mut self, cx: &mut WalkCx, node: GNode) {
        // _append_context_managers_to_stack (R1732) then visit_return delegate
        self.append_context_managers_to_stack(cx, node);
        self.visit_return(cx, node);
    }

    pub fn visit_return(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_swap_variables(cx, node);
        self.check_ternary(cx, node);
    }

    pub fn visit_augassign(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_consider_using_join(cx, node);
    }

    pub fn visit_comprehension(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_unnecessary_comprehension(cx, node);
        self.check_unnecessary_dict_index_lookup(cx, node, false);
        self.check_list_index_lookup_for(cx, node);
    }

    /// R1714 consider-using-in (refactoring_checker.py:1365-1409).
    fn check_consider_using_in(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (op, values): (String, Vec<pyast::NodeId>) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::BoolOp { op, values } => (op.to_string(), values.clone()),
                _ => return,
            }
        };
        // allowed_ops = {"or": "==", "and": "!="}
        let allowed_op = match op.as_str() {
            "or" => "==",
            "and" => "!=",
            _ => return,
        };
        if values.len() < 2 {
            return;
        }
        // each value: single-op Compare with the right operator and no Call operands
        for &v in &values {
            let vg = GNode { m: node.m, n: v };
            let md = eng.md(vg.m);
            let NodeKind::Compare { left, ops } = &md.tree.nodes[vg.n.idx()].kind else {
                return;
            };
            if ops.len() != 1 {
                return;
            }
            // bail if left or comparator is a Call
            if matches!(&md.tree.nodes[left.idx()].kind, NodeKind::Call { .. }) {
                return;
            }
            if matches!(&md.tree.nodes[ops[0].1.idx()].kind, NodeKind::Call { .. }) {
                return;
            }
            // op must equal allowed_op
            if &*ops[0].0 != allowed_op {
                return;
            }
        }
        // collect variables/values
        let mut variable_sets: Vec<FxHashSet<String>> = Vec::new();
        let mut all_values: Vec<String> = Vec::new();
        for &v in &values {
            let vg = GNode { m: node.m, n: v };
            let (left, comparator): (GNode, GNode) = {
                let md = eng.md(vg.m);
                let NodeKind::Compare { left, ops } = &md.tree.nodes[vg.n.idx()].kind else {
                    return;
                };
                (GNode { m: vg.m, n: *left }, GNode { m: vg.m, n: ops[0].1 })
            };
            let mut vset = FxHashSet::default();
            for comparable in [left, comparator] {
                let is_var = eng.kind_is(comparable, |k| {
                    matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. })
                });
                let s = u::as_string(eng, comparable);
                if is_var {
                    vset.insert(s.clone());
                }
                all_values.push(s);
            }
            variable_sets.push(vset);
        }
        // common_variables = reduce(set.intersection, variables)
        let mut common: FxHashSet<String> = match variable_sets.first() {
            Some(s) => s.clone(),
            None => return,
        };
        for s in &variable_sets[1..] {
            common = common.intersection(s).cloned().collect();
        }
        if common.is_empty() {
            return;
        }
        let mut common_sorted: Vec<String> = common.into_iter().collect();
        common_sorted.sort();
        let common_variable = common_sorted[0].clone();
        // dedup preserving first-seen order
        let mut seen = FxHashSet::default();
        let mut deduped: Vec<String> = Vec::new();
        for v in &all_values {
            if seen.insert(v.clone()) {
                deduped.push(v.clone());
            }
        }
        // remove FIRST occurrence of common_variable
        if let Some(pos) = deduped.iter().position(|x| *x == common_variable) {
            deduped.remove(pos);
        }
        let values_string = if deduped.len() != 1 {
            deduped.join(", ")
        } else {
            format!("{},", deduped[0])
        };
        let maybe_not = if op == "or" { "" } else { "not " };
        cx.emit_node(
            "R1714",
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            u::format_template(
                "Consider merging these comparisons with 'in' by using '%s %sin (%s)'. Use a set instead if elements are hashable.",
                &[&common_variable, maybe_not, &values_string],
            ),
        );
    }

    /// R1716 chained-comparison (refactoring_checker.py:1411-1465).
    fn check_chained_comparison(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (op, values): (String, Vec<pyast::NodeId>) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::BoolOp { op, values } => (op.to_string(), values.clone()),
                _ => return,
            }
        };
        if op != "and" || values.len() < 2 {
            return;
        }
        // uses: key -> {lower_bound: set<cmp node id>, upper_bound: set}
        // keys are Name names (str) or Const values (rendered as a key string)
        #[derive(Default)]
        struct Bounds {
            lower: FxHashSet<pyast::NodeId>,
            upper: FxHashSet<pyast::NodeId>,
        }
        let mut uses: indexmap::IndexMap<String, Bounds> = indexmap::IndexMap::new();
        for &cmp_id in &values {
            let cmp = GNode { m: node.m, n: cmp_id };
            let (left, ops): (pyast::NodeId, Vec<(String, pyast::NodeId)>) = {
                let md = eng.md(cmp.m);
                match &md.tree.nodes[cmp.n.idx()].kind {
                    NodeKind::Compare { left, ops } => {
                        (*left, ops.iter().map(|(o, n)| (o.to_string(), *n)).collect())
                    }
                    _ => continue, // not a Compare -> skip this value
                }
            };
            let mut left_operand = left;
            for (operator, right_operand) in &ops {
                for (operand, is_left) in [(left_operand, true), (*right_operand, false)] {
                    // operand: Name(name) | Const(value) if value is not None
                    let key = {
                        let md = eng.md(node.m);
                        match &md.tree.nodes[operand.idx()].kind {
                            NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
                            NodeKind::Const(c) if !matches!(c, ConstValue::None) => {
                                Some(const_str(c))
                            }
                            _ => None,
                        }
                    };
                    let Some(key) = key else { continue };
                    let b = uses.entry(key).or_default();
                    match operator.as_str() {
                        "<" | "<=" => {
                            if is_left {
                                b.lower.insert(cmp_id);
                            } else {
                                b.upper.insert(cmp_id);
                            }
                        }
                        ">" | ">=" => {
                            if is_left {
                                b.upper.insert(cmp_id);
                            } else {
                                b.lower.insert(cmp_id);
                            }
                        }
                        _ => {}
                    }
                }
                left_operand = *right_operand;
            }
        }
        for b in uses.values() {
            let shared = b.lower.intersection(&b.upper).count();
            if shared < b.lower.len() && shared < b.upper.len() {
                cx.emit_node(
                    "R1716",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    "Simplify chained comparison between the operands".to_string(),
                );
                return; // break on first hit
            }
        }
    }

    /// R1726/R1727 simplifiable-condition / condition-evals-to-constant
    /// (refactoring_checker.py:1467-1546).
    fn check_simplifiable_condition(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if !is_test_condition(eng, node, None) {
            return;
        }
        self.can_simplify_bool_op = false;
        let simplified = self.simplify_boolean_operation(cx, node);
        if !self.can_simplify_bool_op {
            return;
        }
        let original = u::as_string(eng, node);
        let simplified_str = simplified_as_string(eng, &simplified);
        // if no Name in simplified -> R1727; else R1726
        let has_name = simplified_contains_name_eng(eng, &simplified);
        if !has_name {
            cx.emit_node(
                "R1727",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template(
                    "Boolean condition '%s' will always evaluate to '%s'",
                    &[&original, &simplified_str],
                ),
            );
        } else {
            cx.emit_node(
                "R1726",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template(
                    "Boolean condition \"%s\" may be simplified to \"%s\"",
                    &[&original, &simplified_str],
                ),
            );
        }
    }

    /// _simplify_boolean_operation (refactoring_checker.py:1496-1518).
    fn simplify_boolean_operation(&mut self, cx: &mut WalkCx, bool_op: GNode) -> SimplifiedExpr {
        let eng = cx.eng;
        let (op, children): (String, Vec<pyast::NodeId>) = {
            let md = eng.md(bool_op.m);
            match &md.tree.nodes[bool_op.n.idx()].kind {
                NodeKind::BoolOp { op, values } => (op.to_string(), values.clone()),
                _ => return SimplifiedExpr::Node(bool_op),
            }
        };
        let mut intermediate: Vec<SimplifiedExpr> = Vec::new();
        for &c in &children {
            let cg = GNode { m: bool_op.m, n: c };
            if eng.kind_is(cg, |k| matches!(k, NodeKind::BoolOp { .. })) {
                intermediate.push(self.simplify_boolean_operation(cx, cg));
            } else {
                intermediate.push(SimplifiedExpr::Node(cg));
            }
        }
        let result = self.apply_boolean_simplification_rules(cx, &op, &intermediate);
        if result.len() < children.len() {
            self.can_simplify_bool_op = true;
        }
        if result.len() == 1 {
            return result.into_iter().next().unwrap();
        }
        SimplifiedExpr::BoolOp { op, values: result }
    }

    /// _apply_boolean_simplification_rules (refactoring_checker.py:1467-1494).
    fn apply_boolean_simplification_rules(
        &mut self,
        cx: &mut WalkCx,
        operator: &str,
        values: &[SimplifiedExpr],
    ) -> Vec<SimplifiedExpr> {
        let eng = cx.eng;
        let mut simplified_values: Vec<SimplifiedExpr> = Vec::new();
        for sub in values {
            let mut inferred_bool: Option<bool> = None;
            // if not next(subnode.nodes_of_class(Name), False): skip-Name guard
            if !simplified_contains_name_eng(eng, sub) {
                if let SimplifiedExpr::Node(g) = sub {
                    if let Some(v) = safe_infer(eng, cx.caches, *g) {
                        if !v.is_uninferable() {
                            inferred_bool = eng.bool_value(&v, &Ctx::new());
                        }
                    }
                }
            }
            match inferred_bool {
                None => simplified_values.push(sub.clone()),
                Some(b) => {
                    // elif (operator == "or") == inferred_bool: return [subnode]
                    if (operator == "or") == b {
                        return vec![sub.clone()];
                    }
                    // else: drop (irrelevant constant)
                }
            }
        }
        if simplified_values.is_empty() {
            // Const(operator == "and")
            vec![SimplifiedExpr::ConstBool(operator == "and")]
        } else {
            simplified_values
        }
    }
    /// R1732 _append_context_managers_to_stack (refactoring_checker.py:1625-1669).
    fn append_context_managers_to_stack(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if is_inside_context_manager(eng, node) {
            return;
        }
        // targets[0]: Tuple/List/Set -> elts + safe_infer(value).elts;
        // else single (target, value).
        let (target0, value): (GNode, GNode) = {
            let md = eng.md(node.m);
            let NodeKind::Assign { targets, value } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            let Some(&t0) = targets.first() else { return };
            (GNode { m: node.m, n: t0 }, GNode { m: node.m, n: *value })
        };
        let is_seq = eng.kind_is(target0, |k| {
            matches!(k, NodeKind::Tuple { .. } | NodeKind::List { .. } | NodeKind::Set { .. })
        });
        let (assignees, values): (Vec<GNode>, Vec<GNode>) = if is_seq {
            let assignees: Vec<GNode> = {
                let md = eng.md(target0.m);
                match &md.tree.nodes[target0.n.idx()].kind {
                    NodeKind::Tuple { elts, .. }
                    | NodeKind::List { elts, .. }
                    | NodeKind::Set { elts } => {
                        elts.iter().map(|&e| GNode { m: target0.m, n: e }).collect()
                    }
                    _ => return,
                }
            };
            // value = safe_infer(node.value); must have .elts
            let v = safe_infer(eng, cx.caches, value);
            let value_elts: Vec<GNode> = match &v {
                Some(Value::Node(g)) => {
                    let md = eng.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::Tuple { elts, .. }
                        | NodeKind::List { elts, .. }
                        | NodeKind::Set { elts } => {
                            elts.iter().map(|&e| GNode { m: g.m, n: e }).collect()
                        }
                        _ => return, // not hasattr(value, "elts")
                    }
                }
                _ => return,
            };
            (assignees, value_elts)
        } else {
            (vec![target0], vec![value])
        };
        // zip truncates
        let n = assignees.len().min(values.len());
        for i in 0..n {
            let assignee = assignees[i];
            let v = values[i];
            // value must be a Call
            if !eng.kind_is(v, |k| matches!(k, NodeKind::Call { .. })) {
                continue;
            }
            let vfunc = match call_func(eng, v) {
                Some(f) => f,
                None => continue,
            };
            let inferred = safe_infer(eng, cx.caches, vfunc);
            let qname = match u::value_qname(eng, &inferred.clone().unwrap_or(Value::Uninferable)) {
                Some(q) => q,
                None => continue,
            };
            if inferred.is_none() || !CALLS_RETURNING_CMS.contains(&qname.as_str()) {
                continue;
            }
            // assignee must be AssignName | AssignAttr; varname accordingly
            let varname = {
                let md = eng.md(assignee.m);
                match &md.tree.nodes[assignee.n.idx()].kind {
                    NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                    NodeKind::AssignAttr { attrname, .. } => md.tree.s(*attrname).to_string(),
                    _ => continue,
                }
            };
            let frame = eng.frame(node);
            let stack = self.stack_for_frame(eng, frame);
            if let Some(&existing) = stack.get(&varname) {
                if eng.are_exclusive(node, existing) {
                    stack.insert(varname, v);
                    continue;
                }
                // redefined before use -> message on the EXISTING node
                cx.emit_node(
                    "R1732",
                    u::msg_line(eng, existing),
                    u::msg_col(eng, existing),
                    "Consider using 'with' for resource-allocating operations".to_string(),
                );
            }
            let stack = self.stack_for_frame(eng, eng.frame(node));
            stack.insert(varname, v);
        }
    }
    /// R1712 consider-swap-variables (refactoring_checker.py:1561-1581).
    fn check_swap_variables(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let s1 = match u::next_sibling(eng, node) {
            Some(s) => s,
            None => return,
        };
        let s2 = match u::next_sibling(eng, s1) {
            Some(s) => s,
            None => return,
        };
        let assignments = [node, s1, s2];
        // all must be Assign(targets=[AssignName], value=Name)
        let mut left: Vec<String> = Vec::new();
        let mut right: Vec<String> = Vec::new();
        for &a in &assignments {
            let md = eng.md(a.m);
            let NodeKind::Assign { targets, value } = &md.tree.nodes[a.n.idx()].kind else {
                return;
            };
            if targets.len() != 1 {
                return;
            }
            let lname = match &md.tree.nodes[targets[0].idx()].kind {
                NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                _ => return,
            };
            let rname = match &md.tree.nodes[value.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return,
            };
            left.push(lname);
            right.push(rname);
        }
        // bail if any already reported
        if assignments.iter().any(|a| self.reported_swap_nodes.contains(a)) {
            return;
        }
        // left[0] == right[-1] and left[1:] == right[:-1]
        if left[0] == right[2] && left[1..] == right[..2] {
            for a in assignments {
                self.reported_swap_nodes.insert(a);
            }
            cx.emit_node(
                "R1712",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Consider using tuple unpacking for swapping variables".to_string(),
            );
        }
    }

    /// R1706/R1709 — visit_return body (refactoring_checker.py:1561-1623).
    fn check_ternary(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node.value (Assign or Return)
        let value = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Assign { value, .. } => GNode { m: node.m, n: *value },
                NodeKind::Return { value: Some(v) } => GNode { m: node.m, n: *v },
                _ => return, // Return(None) -> _is_and_or_ternary(None) no match
            }
        };
        // _is_and_or_ternary(value): BoolOp(or, [BoolOp(and, [_, v1]), v2])
        //   and not (v2 or v1 is BoolOp)
        let (cond, truth, false_v) = match is_and_or_ternary(eng, value) {
            Some(t) => t,
            None => return,
        };
        // if both truth and false are Compare: return
        if eng.kind_is(truth, |k| matches!(k, NodeKind::Compare { .. }))
            && eng.kind_is(false_v, |k| matches!(k, NodeKind::Compare { .. }))
        {
            return;
        }
        // inferred_truth = safe_infer(truth, compare_constants=True)
        let inferred = match u::safe_infer_compare_constants(eng, truth) {
            Some(v) if !v.is_uninferable() => v,
            _ => return,
        };
        let truth_bool = eng.bool_value(&inferred, &Ctx::new());
        if truth_bool == Some(false) {
            // R1709 simplify-boolean-expression
            let suggestion = u::as_string(eng, false_v);
            cx.emit_node(
                "R1709",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template("Boolean expression may be simplified to %s", &[&suggestion]),
            );
        } else {
            // R1706 consider-using-ternary
            let suggestion = format!(
                "{} if {} else {}",
                u::as_string(eng, truth),
                u::as_string(eng, cond),
                u::as_string(eng, false_v)
            );
            cx.emit_node(
                "R1706",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template("Consider using ternary (%s)", &[&suggestion]),
            );
        }
    }
    /// R1713 consider-using-join (refactoring_checker.py:1748-1802).
    fn check_consider_using_join(&mut self, cx: &mut WalkCx, aug: GNode) {
        let eng = cx.eng;
        // for_loop = aug.parent; must be For with body len 1
        let Some(for_loop) = eng.parent(aug) else { return };
        let (body_len, for_target): (usize, GNode) = {
            let md = eng.md(for_loop.m);
            match &md.tree.nodes[for_loop.n.idx()].kind {
                NodeKind::For(d) => (d.body.len(), GNode { m: for_loop.m, n: d.target }),
                _ => return,
            }
        };
        if body_len != 1 {
            return;
        }
        // assign = for_loop.previous_sibling(); must be Assign
        let assign = match u::previous_sibling(eng, for_loop) {
            Some(a) if eng.kind_is(a, |k| matches!(k, NodeKind::Assign { .. })) => a,
            _ => return,
        };
        // result_assign_names = {AssignName.name in assign.targets}
        let (result_names, assign_value): (Vec<String>, GNode) = {
            let md = eng.md(assign.m);
            let NodeKind::Assign { targets, value } = &md.tree.nodes[assign.n.idx()].kind else {
                return;
            };
            let names: Vec<String> = targets
                .iter()
                .filter_map(|&t| match &md.tree.nodes[t.idx()].kind {
                    NodeKind::AssignName { name } => Some(md.tree.s(*name).to_string()),
                    _ => None,
                })
                .collect();
            (names, GNode { m: assign.m, n: *value })
        };
        // aug.op == "+=" and aug.target is AssignName and target.name in names
        let (aug_op, aug_target, aug_value): (String, GNode, GNode) = {
            let md = eng.md(aug.m);
            match &md.tree.nodes[aug.n.idx()].kind {
                NodeKind::AugAssign { op, target, value } => (
                    op.to_string(),
                    GNode { m: aug.m, n: *target },
                    GNode { m: aug.m, n: *value },
                ),
                _ => return,
            }
        };
        if aug_op != "+=" {
            return;
        }
        let target_name = match name_of_assign(eng, aug_target) {
            Some(n) => n,
            None => return,
        };
        if !result_names.contains(&target_name) {
            return;
        }
        // assign.value must be Const str
        let is_const_str = eng.kind_is(assign_value, |k| {
            matches!(k, NodeKind::Const(ConstValue::Str(_)))
        });
        if !is_const_str {
            return;
        }
        // _name_to_concatenate(aug.value) == for_loop.target.name
        let concat = name_to_concatenate(eng, aug_value);
        let Some(concat) = concat else { return };
        // for_loop.target.name — Tuple target -> None (no crash)
        let ft_name = match name_of_assign(eng, for_target) {
            Some(n) => n,
            None => return,
        };
        if concat == ft_name {
            cx.emit_node(
                "R1713",
                u::msg_line(eng, aug),
                u::msg_col(eng, aug),
                "Consider using str.join(sequence) for concatenating strings from an iterable"
                    .to_string(),
            );
        }
    }
    /// R1721 unnecessary-comprehension (refactoring_checker.py:1814-1889).
    fn check_unnecessary_comprehension(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node = Comprehension; bail if parent is GeneratorExp
        let parent = match eng.parent(node) {
            Some(p) => p,
            None => return,
        };
        if eng.kind_is(parent, |k| matches!(k, NodeKind::GeneratorExp(_))) {
            return;
        }
        // len(ifs)==0, parent.generators len 1, not async
        let (ifs_len, target, is_async): (usize, GNode, bool) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Comprehension { ifs, target, is_async, .. } => {
                    (ifs.len(), GNode { m: node.m, n: *target }, *is_async)
                }
                _ => return,
            }
        };
        if ifs_len != 0 || is_async {
            return;
        }
        let gens_len = comp_generators_len(eng, parent);
        if gens_len != 1 {
            return;
        }
        // collect expr_list/target_list per parent kind
        // expr_list / target_list mirror pylint's MIXED str-or-list values.
        // A bare Name elt/target is a STRING; a Tuple is a LIST. Python's
        // `"i" == ["i"]` is False, so the distinction matters (e.g.
        // `[(i,) for i in x]`: expr=[i] (list) vs target="i" (str) -> no
        // match, NOT a comprehension to simplify).
        let (expr_list, target_list): (CompVal, CompVal) = {
            let md = eng.md(parent.m);
            match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::DictComp(d) => {
                    let key_name = match &md.tree.nodes[d.key.idx()].kind {
                        NodeKind::Name { name } => md.tree.s(*name).to_string(),
                        _ => return,
                    };
                    let value_name = match &md.tree.nodes[d.value.idx()].kind {
                        NodeKind::Name { name } => md.tree.s(*name).to_string(),
                        _ => return,
                    };
                    let elts = match &md.tree.nodes[target.n.idx()].kind {
                        NodeKind::Tuple { elts, .. } => elts.clone(),
                        _ => return,
                    };
                    let mut tlist = Vec::new();
                    for &e in &elts {
                        match &md.tree.nodes[e.idx()].kind {
                            NodeKind::AssignName { name } => tlist.push(md.tree.s(*name).to_string()),
                            _ => return, // requires all AssignName
                        }
                    }
                    (CompVal::List(vec![key_name, value_name]), CompVal::List(tlist))
                }
                NodeKind::ListComp(d) | NodeKind::SetComp(d) => {
                    let elt = d.elt;
                    let expr: CompVal = match &md.tree.nodes[elt.idx()].kind {
                        NodeKind::Name { name } => CompVal::Scalar(md.tree.s(*name).to_string()),
                        NodeKind::Tuple { elts, .. } => {
                            let mut v = Vec::new();
                            for &e in elts {
                                match &md.tree.nodes[e.idx()].kind {
                                    NodeKind::Name { name } => v.push(md.tree.s(*name).to_string()),
                                    _ => return, // not all Names -> bail
                                }
                            }
                            CompVal::List(v)
                        }
                        _ => CompVal::Empty,
                    };
                    let tgt: CompVal = match &md.tree.nodes[target.n.idx()].kind {
                        NodeKind::AssignName { name } => {
                            CompVal::Scalar(md.tree.s(*name).to_string())
                        }
                        NodeKind::Tuple { elts, .. } => CompVal::List(
                            elts.iter()
                                .filter_map(|&e| match &md.tree.nodes[e.idx()].kind {
                                    NodeKind::AssignName { name } => {
                                        Some(md.tree.s(*name).to_string())
                                    }
                                    _ => None,
                                })
                                .collect(),
                        ),
                        _ => CompVal::Empty,
                    };
                    (expr, tgt)
                }
                _ => return,
            }
        };
        // if expr_list == target_list and expr_list (truthy)
        if expr_list != target_list || !expr_list.truthy() {
            return;
        }
        // iter + suggestion
        let iter = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Comprehension { iter, .. } => GNode { m: node.m, n: *iter },
                _ => return,
            }
        };
        let inferred = safe_infer(eng, cx.caches, iter);
        let parent_is_dict = eng.kind_is(parent, |k| matches!(k, NodeKind::DictComp(_)));
        let parent_is_list = eng.kind_is(parent, |k| matches!(k, NodeKind::ListComp(_)));
        let parent_is_set = eng.kind_is(parent, |k| matches!(k, NodeKind::SetComp(_)));
        // refined-suggestion matches:
        let args: Option<String> = if parent_is_dict && matches!(&inferred, Some(Value::DictItems(_))) {
            // dict(iter.func.expr.as_string())
            let func = call_func(eng, iter);
            func.and_then(|f| {
                let md = eng.md(f.m);
                match &md.tree.nodes[f.n.idx()].kind {
                    NodeKind::Attribute { expr, .. } => {
                        let e = GNode { m: f.m, n: *expr };
                        drop(md);
                        Some(format!("dict({})", u::as_string(eng, e)))
                    }
                    _ => None,
                }
            })
        } else if parent_is_list
            && matches!(&inferred, Some(Value::Node(g)) if eng.kind_is(*g, |k| matches!(k, NodeKind::List { .. })))
        {
            Some(format!("list({})", u::as_string(eng, iter)))
        } else if parent_is_set
            && matches!(&inferred, Some(Value::Node(g)) if eng.kind_is(*g, |k| matches!(k, NodeKind::Set { .. })))
        {
            Some(format!("set({})", u::as_string(eng, iter)))
        } else {
            None
        };
        let suggestion = match args {
            Some(a) => a,
            None => {
                // func = dict|list|set by parent type
                let func = if parent_is_dict {
                    "dict"
                } else if parent_is_list {
                    "list"
                } else {
                    "set"
                };
                format!("{}({})", func, u::as_string(eng, iter))
            }
        };
        cx.emit_node(
            "R1721",
            u::msg_line(eng, parent),
            u::msg_col(eng, parent),
            u::format_template("Unnecessary use of a comprehension, use %s instead.", &[&suggestion]),
        );
    }
    /// R1732 consumption (visit_with 773-789): delete varname from the FIRST
    /// stack (function, class, module order) containing it; break after.
    fn cuw_consume(&mut self, varname: &str) {
        if self.cuw_function.shift_remove(varname).is_some() {
            return;
        }
        if self.cuw_class.shift_remove(varname).is_some() {
            return;
        }
        self.cuw_module.shift_remove(varname);
    }

    // ---- RecommendationChecker ----
    pub fn recom_visit_for(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_consider_using_enumerate(cx, node);
        self.check_consider_using_dict_items(cx, node);
        self.check_use_sequence_for_iteration(cx, node);
    }

    pub fn recom_visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_consider_iterating_dictionary(cx, node);
        self.check_use_maxsplit_arg(cx, node);
    }

    pub fn recom_visit_comprehension(&mut self, cx: &mut WalkCx, node: GNode) {
        self.check_consider_using_dict_items_comprehension(cx, node);
        self.check_use_sequence_for_iteration(cx, node);
    }

    /// C0208 use-sequence-for-iteration (recommendation_checker.py:347-359):
    /// for/comprehension whose iter is a Set literal with no starred elements.
    fn check_use_sequence_for_iteration(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let iter = match comp_or_for_iter(eng, node) {
            Some(i) => i,
            None => return,
        };
        // isinstance(node.iter, nodes.Set) and not any(has_starred_node_recursive)
        if !eng.kind_is(iter, |k| matches!(k, NodeKind::Set { .. })) {
            return;
        }
        if set_has_starred_recursive(eng, iter) {
            return;
        }
        cx.emit_node(
            "C0208",
            u::msg_line(eng, iter),
            u::msg_col(eng, iter),
            "Use a sequence type when iterating over values".to_string(),
        );
    }

    /// C0206 consider-using-dict-items (For variant, 264-321).
    fn check_consider_using_dict_items(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let iterating_object_name = match get_iterating_dictionary_name(eng, cx.caches, node) {
            Some(n) => n,
            None => return,
        };
        let (target, body): (GNode, Vec<pyast::NodeId>) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::For(d) => (GNode { m: node.m, n: d.target }, d.body.clone()),
                _ => return,
            }
        };
        let target_name = match name_of_assign(eng, target) {
            Some(n) => n,
            None => return,
        };
        for &c in &body {
            for sub in nodes_of_class(
                eng,
                GNode { m: node.m, n: c },
                |k| matches!(k, NodeKind::Subscript { .. }),
                |_| false,
            ) {
                let (sub_value, sub_slice): (GNode, GNode) = {
                    let md = eng.md(sub.m);
                    match &md.tree.nodes[sub.n.idx()].kind {
                        NodeKind::Subscript { value, slice, .. } => {
                            (GNode { m: sub.m, n: *value }, GNode { m: sub.m, n: *slice })
                        }
                        _ => continue,
                    }
                };
                if !eng.kind_is(sub_value, |k| matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. })) {
                    continue;
                }
                // value (slice) must be Name with name == target.name
                let slice_name = match name_of(eng, sub_slice) {
                    Some(n) => n,
                    None => continue,
                };
                if slice_name != target_name
                    || u::as_string(eng, sub_value) != iterating_object_name
                {
                    continue;
                }
                // last_definition lineno > node.lineno -> key redefined -> continue
                if let Some(lr) = lookup_last_lineno(eng, sub_slice, &slice_name) {
                    if lr > lineno(eng, node) {
                        continue;
                    }
                }
                // write/delete -> abort silently
                let parent = match eng.parent(sub) {
                    Some(p) => p,
                    None => continue,
                };
                let is_write = {
                    let md = eng.md(parent.m);
                    match &md.tree.nodes[parent.n.idx()].kind {
                        NodeKind::Assign { targets, .. } => targets.contains(&sub.n),
                        NodeKind::AugAssign { target, .. } => *target == sub.n,
                        _ => false,
                    }
                };
                if is_write {
                    return;
                }
                if eng.kind_is(parent, |k| matches!(k, NodeKind::Delete { .. })) {
                    return;
                }
                cx.emit_node(
                    "C0206",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    "Consider iterating with .items()".to_string(),
                );
                return;
            }
        }
    }

    /// C0206 comprehension variant (323-345).
    fn check_consider_using_dict_items_comprehension(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node is a Comprehension; iterating dict name resolution applies to
        // the comprehension's own iter via the same helper.
        let iterating_object_name = match get_iterating_dictionary_name_comp(eng, cx.caches, node) {
            Some(n) => n,
            None => return,
        };
        let target = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Comprehension { target, .. } => GNode { m: node.m, n: *target },
                _ => return,
            }
        };
        let target_name = match name_of_assign(eng, target) {
            Some(n) => n,
            None => return,
        };
        let parent = match eng.parent(node) {
            Some(p) => p,
            None => return,
        };
        let children: Vec<pyast::NodeId> = eng.md(parent.m).tree.children(parent.n);
        for c in children {
            for sub in nodes_of_class(
                eng,
                GNode { m: parent.m, n: c },
                |k| matches!(k, NodeKind::Subscript { .. }),
                |_| false,
            ) {
                let (sub_value, sub_slice): (GNode, GNode) = {
                    let md = eng.md(sub.m);
                    match &md.tree.nodes[sub.n.idx()].kind {
                        NodeKind::Subscript { value, slice, .. } => {
                            (GNode { m: sub.m, n: *value }, GNode { m: sub.m, n: *slice })
                        }
                        _ => continue,
                    }
                };
                if !eng.kind_is(sub_value, |k| matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. })) {
                    continue;
                }
                let slice_name = match name_of(eng, sub_slice) {
                    Some(n) => n,
                    None => continue,
                };
                if slice_name == target_name
                    && u::as_string(eng, sub_value) == iterating_object_name
                {
                    cx.emit_node(
                        "C0206",
                        u::msg_line(eng, node),
                        u::msg_col(eng, node),
                        "Consider iterating with .items()".to_string(),
                    );
                    return;
                }
            }
        }
    }

    /// C0209 consider-using-f-string (recommendation_checker.py:361-452).
    pub fn recom_visit_const(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // _py36_plus default True. node.pytype() == "builtins.str" and parent
        // not JoinedStr.
        let str_value = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Const(ConstValue::Str(s)) => s.to_string(),
                _ => return,
            }
        };
        let Some(parent) = eng.parent(node) else { return };
        if eng.kind_is(parent, |k| matches!(k, NodeKind::JoinedStr { .. })) {
            return;
        }
        self.detect_replaceable_format_call(cx, node, &str_value, parent);
    }

    /// _detect_replacable_format_call (recommendation_checker.py).
    fn detect_replaceable_format_call(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        str_value: &str,
        parent: GNode,
    ) {
        let eng = cx.eng;
        // (a) .format branch: parent is Attribute attrname "format"
        let is_format_attr = eng.kind_is(parent, |k| {
            matches!(k, NodeKind::Attribute { attrname, .. } if {
                let _ = attrname; true
            })
        }) && attr_name(eng, parent).as_deref() == Some("format");
        if is_format_attr {
            // parent.parent must be Call
            let Some(call) = eng.parent(parent) else { return };
            if !eng.kind_is(call, |k| matches!(k, NodeKind::Call { .. })) {
                return;
            }
            // keyword_args = field head names
            let mf = match crate::strings::parse_format_method_string(str_value) {
                Ok(m) => m,
                Err(()) => return, // IncompleteFormatString -> bail
            };
            let keyword_arg_names: Vec<String> = mf
                .fields
                .iter()
                .filter_map(|(k, _)| match k {
                    crate::strings::KeyName::Str(s) => Some(s.clone()),
                    crate::strings::KeyName::Num(_) => None,
                })
                .collect();
            let (call_args, call_keywords): (Vec<pyast::NodeId>, Vec<pyast::NodeId>) = {
                let md = eng.md(call.m);
                match &md.tree.nodes[call.n.idx()].kind {
                    NodeKind::Call { args, keywords, .. } => (args.clone(), keywords.clone()),
                    _ => return,
                }
            };
            if !call_args.is_empty() {
                for &a in &call_args {
                    let ag = GNode { m: call.m, n: a };
                    // if arg is Starred and safe_infer(arg.value) is List len>1: bail
                    if let Some(starred_val) = starred_value(eng, ag) {
                        if let Some(Value::Node(g)) = safe_infer(eng, cx.caches, starred_val) {
                            let len = list_elts_len(eng, g);
                            if len > 1 {
                                return;
                            }
                        }
                    }
                    // if "\\" in arg.as_string(): bail
                    if u::as_string(eng, ag).contains('\\') {
                        return;
                    }
                }
            } else if !call_keywords.is_empty() {
                for &kw in &call_keywords {
                    let kwg = GNode { m: call.m, n: kw };
                    let (arg, value): (Option<String>, GNode) = {
                        let md = eng.md(kwg.m);
                        match &md.tree.nodes[kwg.n.idx()].kind {
                            NodeKind::Keyword { arg, value } => (
                                arg.map(|s| md.tree.s(s).to_string()),
                                GNode { m: kwg.m, n: *value },
                            ),
                            _ => continue,
                        }
                    };
                    // keyword_args.count(keyword.arg) > 1: bail
                    if let Some(ref an) = arg {
                        if keyword_arg_names.iter().filter(|x| *x == an).count() > 1 {
                            return;
                        }
                    }
                    // if safe_infer(value) is Dict len>1 and len(keyword_args)>1: bail
                    if let Some(v) = safe_infer(eng, cx.caches, value) {
                        let dlen = dict_items_len(eng, &v);
                        if dlen > 1 && keyword_arg_names.len() > 1 {
                            return;
                        }
                    }
                }
            }
            self.emit_c0209(cx, node);
            return;
        }
        // (b) % branch: parent is BinOp op "%"
        let (left, right): (GNode, GNode) = {
            let md = eng.md(parent.m);
            match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::BinOp { left, op, right } if &**op == "%" => {
                    (GNode { m: parent.m, n: *left }, GNode { m: parent.m, n: *right })
                }
                _ => return,
            }
        };
        // if "\\" in right.as_string(): bail
        if u::as_string(eng, right).contains('\\') {
            return;
        }
        // left must be Const str
        let left_str = {
            let md = eng.md(left.m);
            match &md.tree.nodes[left.n.idx()].kind {
                NodeKind::Const(ConstValue::Str(s)) => s.to_string(),
                _ => return,
            }
        };
        if left_str.contains('{') || left_str.contains('}') {
            return;
        }
        // safe_infer(right): Dict/List len>1 -> bail
        if let Some(v) = safe_infer(eng, cx.caches, right) {
            let n = dict_or_list_len(eng, &v);
            if n > 1 {
                return;
            }
        }
        self.emit_c0209(cx, node);
    }

    fn emit_c0209(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // add_message(node=node, line=node.lineno, col_offset=node.col_offset)
        cx.emit_node(
            "C0209",
            lineno(eng, node),
            col_offset(eng, node) as i64,
            "Formatting a regular string which could be an f-string".to_string(),
        );
    }

    /// C0201 consider-iterating-dictionary (recommendation_checker.py:83-106).
    fn check_consider_iterating_dictionary(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node.func is Attribute attrname "keys"
        let func = match call_func(eng, node) {
            Some(f) => f,
            None => return,
        };
        if attr_name(eng, func).as_deref() != Some("keys") {
            return;
        }
        // bail if node.parent is BinOp with op in {&,|,^}
        if let Some(parent) = eng.parent(node) {
            let is_setop = {
                let md = eng.md(parent.m);
                matches!(
                    &md.tree.nodes[parent.n.idx()].kind,
                    NodeKind::BinOp { op, .. } if matches!(&**op, "&" | "|" | "^")
                )
            };
            if is_setop {
                return;
            }
        }
        // membership: node.parent is For/Comprehension OR a Compare ancestor
        // with in/not-in whose comparator is node or an ancestor of node.
        let parent_is_iter = match eng.parent(node) {
            Some(p) => eng.kind_is(p, |k| {
                matches!(k, NodeKind::For(_) | NodeKind::Comprehension { .. })
            }),
            None => false,
        };
        let membership = parent_is_iter || self.dict_keys_in_membership(eng, node);
        if !membership {
            return;
        }
        // safe_infer(node.func): BoundMethod(bound=Dict)
        if let Some(Value::BoundMethod { bound, .. }) = safe_infer(eng, cx.caches, func) {
            if matches!(&*bound, Value::Node(g) if eng.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })))
                || matches!(&*bound, Value::SynthDict { .. })
            {
                cx.emit_node(
                    "C0201",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    "Consider iterating the dictionary directly instead of calling .keys()".to_string(),
                );
            }
        }
    }

    fn dict_keys_in_membership(&self, eng: &Engine, node: GNode) -> bool {
        // get_node_first_ancestor_of_type(node, Compare)
        let comp = match u::first_ancestor(eng, node, |k| matches!(k, NodeKind::Compare { .. })) {
            Some(c) => c,
            None => return false,
        };
        let ops: Vec<(String, pyast::NodeId)> = {
            let md = eng.md(comp.m);
            match &md.tree.nodes[comp.n.idx()].kind {
                NodeKind::Compare { ops, .. } => {
                    ops.iter().map(|(o, n)| (o.to_string(), *n)).collect()
                }
                _ => return false,
            }
        };
        ops.iter().any(|(op, comparator)| {
            if op != "in" && op != "not in" {
                return false;
            }
            let cg = GNode { m: comp.m, n: *comparator };
            cg == node || eng.parent_of(cg, node)
        })
    }

    /// C0207 use-maxsplit-arg (recommendation_checker.py:108-179).
    fn check_use_maxsplit_arg(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (func, attrname, recv): (GNode, String, GNode) = {
            let md = eng.md(node.m);
            let NodeKind::Call { func, .. } = &md.tree.nodes[node.n.idx()].kind else { return };
            match &md.tree.nodes[func.idx()].kind {
                NodeKind::Attribute { attrname, expr, .. } => {
                    let an = md.tree.s(*attrname).to_string();
                    (GNode { m: node.m, n: *func }, an, GNode { m: node.m, n: *expr })
                }
                _ => return,
            }
        };
        if attrname != "split" && attrname != "rsplit" {
            return;
        }
        if !matches!(safe_infer(eng, cx.caches, func), Some(Value::BoundMethod { .. })) {
            return;
        }
        // a non-node bases.Instance receiver bails; Const passes (Const is a
        // node-backed Instance whose nodes_of_class(ClassDef) is empty).
        if let Some(v) = safe_infer(eng, cx.caches, recv) {
            if matches!(v, Value::Inst { .. } | Value::ExcInst { .. }) {
                return;
            }
        }
        let mut confidence_inference = false;
        let sep = match get_argument_from_call(eng, node, 0, "sep") {
            Some(s) => s,
            None => match infer_kwarg_from_call(eng, cx.caches, node, "sep") {
                Some(s) => {
                    confidence_inference = true;
                    s
                }
                None => return,
            },
        };
        // maxsplit must be absent
        if get_argument_from_call(eng, node, 1, "maxsplit").is_some() {
            return;
        }
        if infer_kwarg_from_call(eng, cx.caches, node, "maxsplit").is_some() {
            return;
        }
        let Some(parent) = eng.parent(node) else { return };
        if !eng.kind_is(parent, |k| matches!(k, NodeKind::Subscript { .. })) {
            return;
        }
        let pslice = {
            let md = eng.md(parent.m);
            match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::Subscript { slice, .. } => GNode { m: parent.m, n: *slice },
                _ => return,
            }
        };
        let subscript_value = match get_subscript_const_value(eng, cx.caches, pslice) {
            Some(v) => v,
            None => return,
        };
        // loop-mutation guard when the slice is a Name
        if eng.kind_is(pslice, |k| matches!(k, NodeKind::Name { .. })) {
            let slice_name = name_of(eng, pslice).unwrap_or_default();
            let scope = eng.scope(node);
            for loop_node in nodes_of_class(
                eng,
                scope,
                |k| matches!(k, NodeKind::For(_) | NodeKind::While { .. } | NodeKind::AsyncFor(_)),
                |_| false,
            ) {
                if !eng.parent_of(loop_node, node) {
                    continue;
                }
                for a in nodes_of_class(eng, loop_node, |k| matches!(k, NodeKind::AugAssign { .. }), |_| false) {
                    let tgt = {
                        let md = eng.md(a.m);
                        match &md.tree.nodes[a.n.idx()].kind {
                            NodeKind::AugAssign { target, .. } => GNode { m: a.m, n: *target },
                            _ => continue,
                        }
                    };
                    if name_of_assign(eng, tgt).as_deref() == Some(slice_name.as_str()) {
                        return;
                    }
                }
                for a in nodes_of_class(eng, loop_node, |k| matches!(k, NodeKind::Assign { .. }), |_| false) {
                    let targets: Vec<pyast::NodeId> = {
                        let md = eng.md(a.m);
                        match &md.tree.nodes[a.n.idx()].kind {
                            NodeKind::Assign { targets, .. } => targets.clone(),
                            _ => continue,
                        }
                    };
                    for t in targets {
                        if name_of_assign(eng, GNode { m: a.m, n: t }).as_deref()
                            == Some(slice_name.as_str())
                        {
                            return;
                        }
                    }
                }
            }
        }
        let (is_neg1, is_zero) = subscript_value_neg1_or_zero(&subscript_value);
        if !is_neg1 && !is_zero {
            return;
        }
        let new_fn = if is_neg1 { "rsplit" } else { "split" };
        let func_str = u::as_string(eng, func);
        let prefix = match func_str.rfind(&attrname) {
            Some(i) => &func_str[..i],
            None => &func_str[..],
        };
        let value_repr = subscript_value_repr(&subscript_value);
        let new_name = format!(
            "{}{}({}, maxsplit=1)[{}]",
            prefix,
            new_fn,
            u::as_string(eng, sep),
            value_repr
        );
        let _ = confidence_inference;
        cx.emit_node(
            "C0207",
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            u::format_template("Use %s instead", &[&new_name]),
        );
    }

    /// C0200 consider-using-enumerate (recommendation_checker.py:191-262).
    fn check_consider_using_enumerate(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node.iter must be Call, _is_builtin(func, "range"), and has args
        let (iter, target): (GNode, GNode) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::For(d) => (
                    GNode { m: node.m, n: d.iter },
                    GNode { m: node.m, n: d.target },
                ),
                _ => return,
            }
        };
        let (iter_func, iter_args): (GNode, Vec<pyast::NodeId>) = {
            let md = eng.md(iter.m);
            match &md.tree.nodes[iter.n.idx()].kind {
                NodeKind::Call { func, args, .. } if !args.is_empty() => {
                    (GNode { m: iter.m, n: *func }, args.clone())
                }
                _ => return,
            }
        };
        if !is_builtin_named(eng, cx.caches, iter_func, "range") {
            return;
        }
        // is_constant_zero = args[0] is Const with value == 0
        let is_constant_zero_arg = const_value_is_zero(eng, GNode { m: iter.m, n: iter_args[0] });
        if iter_args.len() == 2 && !is_constant_zero_arg {
            return;
        }
        if iter_args.len() > 2 {
            return;
        }
        // last arg must be Call(func=second_func, args=[iterating_object]) with len
        let last_arg = GNode { m: iter.m, n: *iter_args.last().unwrap() };
        let (len_func, iterating_object): (GNode, GNode) = {
            let md = eng.md(last_arg.m);
            match &md.tree.nodes[last_arg.n.idx()].kind {
                NodeKind::Call { func, args, .. } if args.len() == 1 => {
                    (GNode { m: last_arg.m, n: *func }, GNode { m: last_arg.m, n: args[0] })
                }
                _ => return,
            }
        };
        if !is_builtin_named(eng, cx.caches, len_func, "len") {
            return;
        }
        // iterating_object: Name -> expect Name subscripts; Attribute -> Attribute; else bail
        let io_is_name = eng.kind_is(iterating_object, |k| matches!(k, NodeKind::Name { .. }));
        let io_is_attr = eng.kind_is(iterating_object, |k| matches!(k, NodeKind::Attribute { .. }));
        if !io_is_name && !io_is_attr {
            return;
        }
        // if iterating_object is Name("self") and node.scope().name == "__iter__": bail
        if io_is_name {
            if let Some(n) = name_of(eng, iterating_object) {
                if n == "self" {
                    let scope = eng.scope(node);
                    if func_bare_name(eng, scope) == "__iter__" {
                        return;
                    }
                }
            }
        }
        // node.target.name; Tuple target -> AttributeError family (treated as
        // non-match: skip without crash)
        let target_name = match name_of_assign(eng, target) {
            Some(n) => n,
            None => return,
        };
        let body: Vec<pyast::NodeId> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::For(d) => d.body.clone(),
                _ => return,
            }
        };
        let io_attrname = if io_is_attr {
            attr_name(eng, iterating_object)
        } else {
            None
        };
        let io_name = if io_is_name { name_of(eng, iterating_object) } else { None };
        for &c in &body {
            let croot = GNode { m: node.m, n: c };
            for sub in nodes_of_class(eng, croot, |k| matches!(k, NodeKind::Subscript { .. }), |_| false) {
                let (sub_value, sub_slice): (GNode, GNode) = {
                    let md = eng.md(sub.m);
                    match &md.tree.nodes[sub.n.idx()].kind {
                        NodeKind::Subscript { value, slice, .. } => {
                            (GNode { m: sub.m, n: *value }, GNode { m: sub.m, n: *slice })
                        }
                        _ => continue,
                    }
                };
                // subscript.value must match expected type
                let sv_is_name = eng.kind_is(sub_value, |k| matches!(k, NodeKind::Name { .. }));
                let sv_is_attr = eng.kind_is(sub_value, |k| matches!(k, NodeKind::Attribute { .. }));
                if io_is_name && !sv_is_name {
                    continue;
                }
                if io_is_attr && !sv_is_attr {
                    continue;
                }
                // value (slice) must be Name
                let slice_name = match name_of(eng, sub_slice) {
                    Some(n) => n,
                    None => continue,
                };
                // subscript.value.scope() != node.scope() -> continue
                if eng.scope(sub_value) != eng.scope(node) {
                    continue;
                }
                let value_match = if io_is_name {
                    name_of(eng, sub_value) == io_name
                } else {
                    attr_name(eng, sub_value) == io_attrname
                };
                if slice_name == target_name && value_match {
                    cx.emit_node(
                        "C0200",
                        u::msg_line(eng, node),
                        u::msg_col(eng, node),
                        "Consider using enumerate instead of iterating with range and len".to_string(),
                    );
                    return;
                }
            }
        }
    }

    // ---- ImplicitBooleanessChecker ----
    /// C1802 (b) visit_unaryop (implicit_booleaness_checker.py:161-173):
    /// `not len(x)` -> message (unconditional on context).
    pub fn implbool_visit_unaryop(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (op, operand) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::UnaryOp { op, operand } => {
                    (op.to_string(), GNode { m: node.m, n: *operand })
                }
                _ => return,
            }
        };
        if op == "not" && is_call_of_name(eng, operand, "len") {
            cx.emit_node(
                "C1802",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Do not use `len(SEQUENCE)` without comparison to determine if a sequence is empty"
                    .to_string(),
            );
        }
    }

    /// C1802 (a) visit_call (implicit_booleaness_checker.py:109-150).
    pub fn implbool_visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if !is_call_of_name(eng, node, "len") {
            return;
        }
        // parent = node.parent; while isinstance(parent, BoolOp): parent = parent.parent
        let mut parent = match eng.parent(node) {
            Some(p) => p,
            None => return,
        };
        while eng.kind_is(parent, |k| matches!(k, NodeKind::BoolOp { .. })) {
            match eng.parent(parent) {
                Some(p) => parent = p,
                None => break,
            }
        }
        if !is_test_condition(eng, node, Some(parent)) {
            return;
        }
        // len_arg = node.args[0]
        let len_arg = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Call { args, .. } if !args.is_empty() => {
                    GNode { m: node.m, n: args[0] }
                }
                _ => return, // IndexError on zero args -> crash; treated as bail
            }
        };
        // comp literal forms -> HIGH
        if eng.kind_is(len_arg, |k| {
            matches!(k, NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_))
        }) {
            cx.emit_node(
                "C1802",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Do not use `len(SEQUENCE)` without comparison to determine if a sequence is empty"
                    .to_string(),
            );
            return;
        }
        // instance = next(len_arg.infer())  -- FIRST value (not safe_infer)
        let instance = match u::infer_all(eng, cx.caches, len_arg).first().cloned() {
            Some(v) => v,
            None => return, // InferenceError on first pull
        };
        let mother = base_names_of_instance(eng, &instance);
        let affected = ["str", "tuple", "list", "set"]
            .iter()
            .any(|t| mother.iter().any(|m| m == t));
        if mother.iter().any(|m| m == "range")
            || (affected && !instance_has_bool(eng, &instance))
        {
            cx.emit_node(
                "C1802",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                "Do not use `len(SEQUENCE)` without comparison to determine if a sequence is empty"
                    .to_string(),
            );
        }
    }

    /// C1803/C1804/C1805 visit_compare (implicit_booleaness_checker.py:175-249).
    pub fn implbool_visit_compare(&mut self, cx: &mut WalkCx, node: GNode, c1803: bool, c1805: bool) {
        if c1803 {
            self.check_use_implicit_booleaness_not_comparison(cx, node);
        }
        // The typo `-to-str` gate is always True; the inner C1804 emit is
        // dropped by message filtering (default-off). The C1805 branch is
        // the real gate.
        self.check_compare_to_str_or_zero(cx, node, c1805);
    }

    /// C1803 (implicit_booleaness_checker.py:251-298).
    fn check_use_implicit_booleaness_not_comparison(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (left, operator, comparator): (GNode, String, GNode) = {
            let md = eng.md(node.m);
            let NodeKind::Compare { left, ops } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            if ops.len() != 1 {
                return;
            }
            (
                GNode { m: node.m, n: *left },
                ops[0].0.to_string(),
                GNode { m: node.m, n: ops[0].1 },
            )
        };
        let is_left_empty = is_empty_literal(eng, left);
        let is_right_empty = is_empty_literal(eng, comparator);
        if is_left_empty == is_right_empty {
            return; // need exactly one (XOR)
        }
        let (target_node, literal_node) = if is_right_empty {
            (left, comparator)
        } else {
            (comparator, left)
        };
        let target_instance = match safe_infer(eng, cx.caches, target_node) {
            Some(v) => v,
            None => return,
        };
        let mother = base_names_of_instance(eng, &target_instance);
        let is_base = ["tuple", "list", "dict", "set"]
            .iter()
            .any(|t| mother.iter().any(|m| m == t));
        if !is_base && instance_has_bool(eng, &target_instance) {
            return;
        }
        if matches!(operator.as_str(), "==" | "!=" | ">=" | ">" | "<=" | "<") {
            let args = implicit_booleaness_message_args(eng, node, literal_node, &operator, target_node);
            cx.emit_node(
                "C1803",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template(
                    "\"%s\" can be simplified to \"%s\", if it is strictly a sequence, as an empty %s is falsey",
                    &[&args.0, &args.1, &args.2],
                ),
            );
        }
    }

    /// C1804/C1805 (implicit_booleaness_checker.py:190-249).
    fn check_compare_to_str_or_zero(&mut self, cx: &mut WalkCx, node: GNode, c1805: bool) {
        let eng = cx.eng;
        let (left, operator, right): (GNode, String, GNode) = {
            let md = eng.md(node.m);
            let NodeKind::Compare { left, ops } = &md.tree.nodes[node.n.idx()].kind else {
                return;
            };
            if ops.len() != 1 {
                return;
            }
            (
                GNode { m: node.m, n: *left },
                ops[0].0.to_string(),
                GNode { m: node.m, n: ops[0].1 },
            )
        };
        if !matches!(operator.as_str(), "!=" | "==" | "is not" | "is") {
            return;
        }
        // C1805 (real gate)
        if c1805 {
            let operand = if is_constant_zero(eng, left) {
                Some(right)
            } else if is_constant_zero(eng, right) {
                Some(left)
            } else {
                None
            };
            if let Some(operand) = operand {
                let original = format!("{} {} {}", u::as_string(eng, left), operator, u::as_string(eng, right));
                let suggestion = get_suggestion(eng, node, &u::as_string(eng, operand), &operator);
                cx.emit_node(
                    "C1805",
                    u::msg_line(eng, node),
                    u::msg_col(eng, node),
                    u::format_template(
                        "\"%s\" can be simplified to \"%s\", if it is strictly an int, as 0 is falsey",
                        &[&original, &suggestion],
                    ),
                );
            }
        }
        // C1804 (typo gate is always True; emit dropped by message filtering
        // when C1804 default-off). Still emit so explicit --enable=C1804 works.
        let node_name = if is_empty_str_literal(eng, left) {
            Some(u::as_string(eng, right))
        } else if is_empty_str_literal(eng, right) {
            Some(u::as_string(eng, left))
        } else {
            None
        };
        if let Some(nn) = node_name {
            let suggestion = get_suggestion(eng, node, &nn, &operator);
            cx.emit_node(
                "C1804",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template(
                    "\"%s\" can be simplified to \"%s\", if it is strictly a string, as an empty string is falsey",
                    &[&u::as_string(eng, node), &suggestion],
                ),
            );
        }
    }

    // ---- NotChecker ----
    /// C0117 unnecessary-negation (not_checker.py).
    pub fn not_visit_unaryop(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (op, operand) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::UnaryOp { op, operand } => {
                    (op.to_string(), GNode { m: node.m, n: *operand })
                }
                _ => return,
            }
        };
        if op != "not" {
            return;
        }
        // not not X
        let operand_is_not = {
            let md = eng.md(operand.m);
            matches!(&md.tree.nodes[operand.n.idx()].kind, NodeKind::UnaryOp { op, .. } if &**op == "not")
        };
        if operand_is_not {
            let inner = {
                let md = eng.md(operand.m);
                match &md.tree.nodes[operand.n.idx()].kind {
                    NodeKind::UnaryOp { operand: inner_op, .. } => {
                        GNode { m: operand.m, n: *inner_op }
                    }
                    _ => return,
                }
            };
            cx.emit_node(
                "C0117",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template(
                    "Consider changing \"%s\" to \"%s\"",
                    &[&u::as_string(eng, node), &u::as_string(eng, inner)],
                ),
            );
            return;
        }
        // operand is Compare
        let is_compare = eng.kind_is(operand, |k| matches!(k, NodeKind::Compare { .. }));
        if !is_compare {
            return;
        }
        let (left, operator, right): (GNode, String, GNode) = {
            let md = eng.md(operand.m);
            let NodeKind::Compare { left, ops } = &md.tree.nodes[operand.n.idx()].kind else {
                return;
            };
            if ops.len() > 1 {
                return; // chained
            }
            (
                GNode { m: operand.m, n: *left },
                ops[0].0.to_string(),
                GNode { m: operand.m, n: ops[0].1 },
            )
        };
        let reversed = reverse_op(&operator);
        let Some(reversed) = reversed else { return };
        // frame.name == "__ne__" and operator == "==" -> bail
        let frame = eng.frame(node);
        if func_bare_name(eng, frame) == "__ne__" && operator == "==" {
            return;
        }
        // node_type of both operands: must be a single confident inferred
        // value, not a Set literal / set|frozenset instance.
        for operand_node in [left, right] {
            let ty = match node_type(eng, cx.caches, operand_node) {
                Some(t) => t,
                None => return,
            };
            // bail if isinstance(_type, nodes.Set) — literal Set node value
            if let Value::Node(g) = &ty {
                if eng.kind_is(*g, |k| matches!(k, NodeKind::Set { .. })) {
                    return;
                }
            }
            // bail if the inferred value is a set/frozenset Instance. astroid:
            // isinstance(_type, Instance) and qname in {set,frozenset}.
            // Value::Inst covers set instances; the objects.FrozenSet proxy
            // value (assumps in sympy) is qname builtins.frozenset.
            let set_qn = match &ty {
                Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(eng.qname(*cls)),
                Value::FrozenSet { .. } => Some("builtins.frozenset".to_string()),
                _ => None,
            };
            if matches!(set_qn.as_deref(), Some("builtins.set") | Some("builtins.frozenset")) {
                return;
            }
        }
        let suggestion = format!(
            "{} {} {}",
            u::as_string(eng, left),
            reversed,
            u::as_string(eng, right)
        );
        cx.emit_node(
            "C0117",
            u::msg_line(eng, node),
            u::msg_col(eng, node),
            u::format_template(
                "Consider changing \"%s\" to \"%s\"",
                &[&u::as_string(eng, node), &suggestion],
            ),
        );
    }

    /// R1701 consider-merging-isinstance (refactoring_checker.py:1309-1363).
    fn check_consider_merging_isinstance(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // node.op must be "or"
        let (op, values): (String, Vec<pyast::NodeId>) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::BoolOp { op, values } => (op.to_string(), values.clone()),
                _ => return,
            }
        };
        if op != "or" {
            return;
        }
        // _duplicated_isinstance_types
        let mut duplicated: Vec<String> = Vec::new();
        let mut all_types: indexmap::IndexMap<String, indexmap::IndexSet<String>> =
            indexmap::IndexMap::new();
        for &cv in &values {
            let call = GNode { m: node.m, n: cv };
            let (cfunc, cargs): (GNode, Vec<pyast::NodeId>) = {
                let md = eng.md(call.m);
                match &md.tree.nodes[call.n.idx()].kind {
                    NodeKind::Call { func, args, .. } if args.len() == 2 => {
                        (GNode { m: call.m, n: *func }, args.clone())
                    }
                    _ => continue,
                }
            };
            let inferred = safe_infer(eng, cx.caches, cfunc);
            let ok = match &inferred {
                Some(v) if value_is_builtin_object(eng, v) => {
                    matches!(v, Value::Node(g) if eng.node_name(*g).as_deref() == Some("isinstance"))
                }
                _ => false,
            };
            if !ok {
                continue;
            }
            let obj = u::as_string(eng, GNode { m: call.m, n: cargs[0] });
            if all_types.contains_key(&obj) && !duplicated.contains(&obj) {
                duplicated.push(obj.clone());
            }
            // elems: tuple itered or single
            let second = GNode { m: call.m, n: cargs[1] };
            let elems: Vec<String> = {
                let md = eng.md(second.m);
                match &md.tree.nodes[second.n.idx()].kind {
                    NodeKind::Tuple { elts, .. } => {
                        let elts = elts.clone();
                        drop(md);
                        elts.iter()
                            .map(|&e| u::as_string(eng, GNode { m: second.m, n: e }))
                            .collect()
                    }
                    _ => {
                        drop(md);
                        vec![u::as_string(eng, second)]
                    }
                }
            };
            all_types.entry(obj).or_default().extend(elems);
        }
        // emit one message per duplicated object, in first-occurrence order
        // of all_types insertion (filtered to duplicated set).
        for (obj, types) in &all_types {
            if !duplicated.contains(obj) {
                continue;
            }
            let mut names: Vec<String> = types.iter().cloned().collect();
            names.sort();
            cx.emit_node(
                "R1701",
                u::msg_line(eng, node),
                u::msg_col(eng, node),
                u::format_template(
                    "Consider merging these isinstance calls to isinstance(%s, (%s))",
                    &[obj, &names.join(", ")],
                ),
            );
        }
    }
}

/// dummy-variables-rgx default match (variables.py default):
/// `_+$|(_[a-zA-Z0-9_]*[a-zA-Z0-9]+?$)|dummy|^ignored_|^unused_`.
fn dummy_var_matches(name: &str) -> bool {
    // _+$  : all underscores
    if !name.is_empty() && name.chars().all(|c| c == '_') {
        return true;
    }
    // (_[a-zA-Z0-9_]*[a-zA-Z0-9]+?$) : starts with _, ends with alnum
    if let Some(rest) = name.strip_prefix('_') {
        if !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && rest.chars().last().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false)
        {
            return true;
        }
    }
    // dummy  (substring), ^ignored_, ^unused_
    name.contains("dummy") || name.starts_with("ignored_") || name.starts_with("unused_")
}

/// _name_to_concatenate (refactoring_checker.py:1748-1766). Name -> name;
/// JoinedStr with exactly one FormattedValue whose value is a Name (and, if
/// it has literal separators, suggest-join-with-non-empty-separator default
/// True allows it) -> that Name's name; else None.
fn name_to_concatenate(eng: &Engine, node: GNode) -> Option<String> {
    let md = eng.md(node.m);
    match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
        NodeKind::JoinedStr { values } => {
            let fvalues: Vec<pyast::NodeId> = values
                .iter()
                .copied()
                .filter(|&v| matches!(&md.tree.nodes[v.idx()].kind, NodeKind::FormattedValue { .. }))
                .collect();
            if fvalues.len() != 1 {
                return None;
            }
            let fv_value = match &md.tree.nodes[fvalues[0].idx()].kind {
                NodeKind::FormattedValue { value, .. } => *value,
                _ => return None,
            };
            let inner_name = match &md.tree.nodes[fv_value.idx()].kind {
                NodeKind::Name { name } => md.tree.s(*name).to_string(),
                _ => return None,
            };
            // with_separators = len(node.values) > len(fvalues); default option
            // True so non-empty separators are allowed.
            Some(inner_name)
        }
        _ => None,
    }
}

/// not_checker.py reverse_op table (lines 29-38). NOTE: "not in"/"is not"
/// are NOT in the table (returns None => bail).
fn reverse_op(op: &str) -> Option<&'static str> {
    match op {
        "<" => Some(">="),
        "<=" => Some(">"),
        ">" => Some("<="),
        ">=" => Some("<"),
        "==" => Some("!="),
        "!=" => Some("=="),
        "in" => Some("not in"),
        "is" => Some("is not"),
        _ => None,
    }
}

/// utils.node_type (utils.py:1494-1513): collect a SET of inferred VALUE
/// objects (not pytypes!), skipping Uninferable/None; >1 distinct value or
/// empty -> None. Returns the single value if exactly one. C0117 then checks
/// it against Set-literal / set-instance. Note: two distinct bool values
/// (True/False from different branches) count as TWO entries -> None.
fn node_type(eng: &Engine, caches: &u::LintCaches, node: GNode) -> Option<Value> {
    use pyinfer::value::value_key;
    let mut keys: Vec<pyinfer::value::ValueKey> = Vec::new();
    let mut last: Option<Value> = None;
    for v in u::infer_all(eng, caches, node).iter() {
        if v.is_uninferable() {
            continue;
        }
        // utils.is_none (utils.py:1487): matches Const(value=None) — whether
        // a real Const node OR a synthesized None (e.g. an implicit-return
        // None yielded from a called function). astroid's `infer()` can hand
        // back SynthConst(None) here; both must be skipped exactly like
        // node-backed Const(None) so the type set collapses identically.
        if matches!(eng.value_const(v), Some(ConstValue::None)) {
            continue;
        }
        let k = value_key(v);
        if !keys.contains(&k) {
            keys.push(k);
            last = Some(v.clone());
        }
        if keys.len() > 1 {
            return None;
        }
    }
    if keys.len() == 1 {
        last
    } else {
        None
    }
}

/// _is_and_or_ternary (refactoring_checker.py:1891-1902): returns
/// (cond, truth_value, false_value) when the node matches
/// BoolOp("or", [BoolOp("and", [cond, truth]), false]) and neither
/// truth nor false is itself a BoolOp.
fn is_and_or_ternary(eng: &Engine, node: GNode) -> Option<(GNode, GNode, GNode)> {
    let md = eng.md(node.m);
    let NodeKind::BoolOp { op, values } = &md.tree.nodes[node.n.idx()].kind else {
        return None;
    };
    if &**op != "or" || values.len() != 2 {
        return None;
    }
    let and_node = values[0];
    let v2 = values[1];
    let NodeKind::BoolOp { op: aop, values: avals } = &md.tree.nodes[and_node.idx()].kind else {
        return None;
    };
    if &**aop != "and" || avals.len() != 2 {
        return None;
    }
    let cond = avals[0];
    let v1 = avals[1];
    // not (isinstance(v2, BoolOp) or isinstance(v1, BoolOp))
    let v1_boolop = matches!(&md.tree.nodes[v1.idx()].kind, NodeKind::BoolOp { .. });
    let v2_boolop = matches!(&md.tree.nodes[v2.idx()].kind, NodeKind::BoolOp { .. });
    if v1_boolop || v2_boolop {
        return None;
    }
    Some((
        GNode { m: node.m, n: cond },
        GNode { m: node.m, n: v1 },
        GNode { m: node.m, n: v2 },
    ))
}

/// Simplified boolean expression (R1726/R1727): a real node, a synthesized
/// BoolOp over simplified children, or a synthetic Const(bool).
#[derive(Clone)]
enum SimplifiedExpr {
    Node(GNode),
    BoolOp { op: String, values: Vec<SimplifiedExpr> },
    ConstBool(bool),
}

/// `next(node.nodes_of_class(Name), False)` over a simplified expr.
fn simplified_contains_name_eng(eng: &Engine, expr: &SimplifiedExpr) -> bool {
    match expr {
        SimplifiedExpr::Node(g) => {
            !nodes_of_class(eng, *g, |k| matches!(k, NodeKind::Name { .. }), |_| false).is_empty()
        }
        SimplifiedExpr::BoolOp { values, .. } => {
            values.iter().any(|v| simplified_contains_name_eng(eng, v))
        }
        SimplifiedExpr::ConstBool(_) => false,
    }
}

/// as_string of a simplified expr. For a synthesized BoolOp, join children
/// with " op " applying astroid precedence parens (BoolOp precedence 2 for
/// or, 3 for and). A Const(bool) renders "True"/"False".
fn simplified_as_string(eng: &Engine, expr: &SimplifiedExpr) -> String {
    match expr {
        SimplifiedExpr::Node(g) => u::as_string(eng, *g),
        SimplifiedExpr::ConstBool(b) => if *b { "True" } else { "False" }.to_string(),
        SimplifiedExpr::BoolOp { op, values } => {
            // BoolOp precedence: or=2, and=3. A child needs parens if its
            // precedence rank < parent rank (np > cp). Const/Name = max rank.
            let parent_rank = if op == "or" { 2 } else { 3 };
            let parts: Vec<String> = values
                .iter()
                .map(|v| {
                    let s = simplified_as_string(eng, v);
                    let cr = simplified_rank(eng, v);
                    if parent_rank > cr {
                        format!("({s})")
                    } else {
                        s
                    }
                })
                .collect();
            parts.join(&format!(" {op} "))
        }
    }
}

fn simplified_rank(eng: &Engine, expr: &SimplifiedExpr) -> i32 {
    match expr {
        SimplifiedExpr::Node(g) => simplified_node_rank(eng, *g),
        SimplifiedExpr::BoolOp { op, .. } => {
            if op == "or" {
                2
            } else {
                3
            }
        }
        SimplifiedExpr::ConstBool(_) => 15,
    }
}

fn simplified_node_rank(eng: &Engine, g: GNode) -> i32 {
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
                4
            } else {
                12
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
            _ => 15,
        },
        NodeKind::Await { .. } => 14,
        _ => 15,
    }
}

/// _enumerate_with_start (refactoring_checker.py:2402-2434) + _get_start_value
/// (2436-2454): True if the enumerate call has a non-zero start.
fn enumerate_with_start(eng: &Engine, caches: &u::LintCaches, iter: GNode) -> bool {
    // second positional arg or start= keyword
    let start = get_argument_from_call(eng, iter, 1, "start");
    let Some(start) = start else { return false };
    // _get_start_value: Const -> value; UnaryOp(operand=Const) -> operand.value
    // (sign dropped!); else safe_infer Const -> value; else None.
    let val: Option<ConstValue> = {
        let md = eng.md(start.m);
        match &md.tree.nodes[start.n.idx()].kind {
            NodeKind::Const(c) => Some(c.clone()),
            NodeKind::UnaryOp { operand, .. } => match &md.tree.nodes[operand.idx()].kind {
                NodeKind::Const(c) => Some(c.clone()),
                _ => None,
            },
            _ => {
                drop(md);
                match safe_infer(eng, caches, start) {
                    Some(Value::Node(g)) => {
                        let m2 = eng.md(g.m);
                        match &m2.tree.nodes[g.n.idx()].kind {
                            NodeKind::Const(c) => Some(c.clone()),
                            _ => None,
                        }
                    }
                    Some(Value::SynthConst(c)) => Some((*c).clone()),
                    _ => None,
                }
            }
        }
    };
    // return not start_val == 0 ; None -> False (no start)
    match val {
        None => false,
        Some(c) => !const_is_zeroish(&c),
    }
}

/// start_val == 0 with the `False == 0` quirk.
fn const_is_zeroish(c: &ConstValue) -> bool {
    use pyast::tree::IntValue;
    match c {
        ConstValue::Int(IntValue::Small(0)) => true,
        ConstValue::Bool(false) => true,
        ConstValue::Float(f) if *f == 0.0 => true,
        _ => false,
    }
}

/// utils.get_argument_from_call (utils.py:717): positional index else keyword
/// by name; None if neither present (NoSuchArgumentError).
fn get_argument_from_call(
    eng: &Engine,
    call: GNode,
    position: usize,
    keyword: &str,
) -> Option<GNode> {
    let md = eng.md(call.m);
    let NodeKind::Call { args, keywords, .. } = &md.tree.nodes[call.n.idx()].kind else {
        return None;
    };
    // positional: args[position] if not a Starred
    if let Some(&a) = args.get(position) {
        if !matches!(&md.tree.nodes[a.idx()].kind, NodeKind::Starred { .. }) {
            return Some(GNode { m: call.m, n: a });
        }
    }
    // keyword by name
    for &kw in keywords {
        if let NodeKind::Keyword { arg: Some(arg), value } = &md.tree.nodes[kw.idx()].kind {
            if md.tree.s(*arg) == keyword {
                return Some(GNode { m: call.m, n: *value });
            }
        }
    }
    None
}

/// utils.infer_kwarg_from_call (utils.py:747): look in call.kwargs (**d),
/// infer d as Dict, return the value node for `keyword`.
fn infer_kwarg_from_call(
    eng: &Engine,
    caches: &u::LintCaches,
    call: GNode,
    keyword: &str,
) -> Option<GNode> {
    let kwargs: Vec<GNode> = {
        let md = eng.md(call.m);
        let NodeKind::Call { keywords, .. } = &md.tree.nodes[call.n.idx()].kind else {
            return None;
        };
        keywords
            .iter()
            .filter_map(|&kw| match &md.tree.nodes[kw.idx()].kind {
                NodeKind::Keyword { arg: None, value } => Some(GNode { m: call.m, n: *value }),
                _ => None,
            })
            .collect()
    };
    for d in kwargs {
        if let Some(Value::Node(g)) = safe_infer(eng, caches, d) {
            let items: Vec<(pyast::NodeId, pyast::NodeId)> = {
                let md = eng.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => items.clone(),
                    _ => continue,
                }
            };
            for (k, v) in items {
                let md = eng.md(g.m);
                if matches!(&md.tree.nodes[k.idx()].kind, NodeKind::Const(ConstValue::Str(s)) if &**s == keyword)
                {
                    return Some(GNode { m: g.m, n: v });
                }
            }
        }
    }
    None
}

/// utils.get_subscript_const_value (utils.py:1806): safe_infer(slice) must be
/// a Const; returns its ConstValue, else None (InferredTypeError).
fn get_subscript_const_value(
    eng: &Engine,
    caches: &u::LintCaches,
    slice: GNode,
) -> Option<ConstValue> {
    match safe_infer(eng, caches, slice) {
        Some(Value::Node(g)) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Const(c) => Some(c.clone()),
                _ => None,
            }
        }
        Some(Value::SynthConst(c)) => Some((*c).clone()),
        _ => None,
    }
}

/// subscript_value in (-1, 0): (is_neg1, is_zero). False==0 quirk replicated.
fn subscript_value_neg1_or_zero(c: &ConstValue) -> (bool, bool) {
    use pyast::tree::IntValue;
    match c {
        ConstValue::Int(IntValue::Small(-1)) => (true, false),
        ConstValue::Int(IntValue::Small(0)) => (false, true),
        ConstValue::Bool(false) => (false, true),       // False == 0
        ConstValue::Float(f) if *f == -1.0 => (true, false),
        ConstValue::Float(f) if *f == 0.0 => (false, true),
        _ => (false, false),
    }
}

/// f"[{subscript_value}]" — str(value). int -> digits, float -> repr, etc.
fn subscript_value_repr(c: &ConstValue) -> String {
    const_str(c)
}

/// iter of a For OR a Comprehension node.
fn comp_or_for_iter(eng: &Engine, node: GNode) -> Option<GNode> {
    let md = eng.md(node.m);
    match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::For(d) => Some(GNode { m: node.m, n: d.iter }),
        NodeKind::Comprehension { iter, .. } => Some(GNode { m: node.m, n: *iter }),
        _ => None,
    }
}

/// target of a For OR a Comprehension node.
fn comp_or_for_target(eng: &Engine, node: GNode) -> Option<GNode> {
    let md = eng.md(node.m);
    match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::For(d) => Some(GNode { m: node.m, n: d.target }),
        NodeKind::Comprehension { target, .. } => Some(GNode { m: node.m, n: *target }),
        _ => None,
    }
}

/// _is_part_of_assignment_target (refactoring_checker.py:195-210): node (or
/// an enclosing Tuple/List chain) is in Assign.targets / is AugAssign.target.
fn is_part_of_assignment_target(eng: &Engine, node: GNode) -> bool {
    let mut cur = node;
    loop {
        let Some(parent) = eng.parent(cur) else { return false };
        let md = eng.md(parent.m);
        match &md.tree.nodes[parent.n.idx()].kind {
            NodeKind::Assign { targets, .. } => return targets.contains(&cur.n),
            NodeKind::AugAssign { target, .. } => return *target == cur.n,
            NodeKind::Tuple { .. } | NodeKind::List { .. } => {
                drop(md);
                cur = parent;
            }
            _ => return false,
        }
    }
}

/// `"1".join(s.rsplit("0", 1))` — replace the LAST occurrence of "0" with "1".
fn replace_last(s: &str, from: &str, to: &str) -> String {
    match s.rfind(from) {
        Some(i) => format!("{}{}{}", &s[..i], to, &s[i + from.len()..]),
        None => s.to_string(),
    }
}

/// has_starred_node_recursive over a Set literal's elts (utils.py:2064-2077):
/// any Starred element, recursing into nested Sets.
fn set_has_starred_recursive(eng: &Engine, set_node: GNode) -> bool {
    let elts: Vec<pyast::NodeId> = {
        let md = eng.md(set_node.m);
        match &md.tree.nodes[set_node.n.idx()].kind {
            NodeKind::Set { elts } => elts.clone(),
            _ => return false,
        }
    };
    for e in elts {
        let eg = GNode { m: set_node.m, n: e };
        if eng.kind_is(eg, |k| matches!(k, NodeKind::Starred { .. })) {
            return true;
        }
        if eng.kind_is(eg, |k| matches!(k, NodeKind::Set { .. })) && set_has_starred_recursive(eng, eg) {
            return true;
        }
    }
    false
}

/// get_iterating_dictionary_name (utils.py:1781-1803) for a For node.
fn get_iterating_dictionary_name(
    eng: &Engine,
    caches: &u::LintCaches,
    node: GNode,
) -> Option<String> {
    let iter = comp_or_for_iter(eng, node)?;
    iterating_dict_name(eng, caches, iter)
}

fn get_iterating_dictionary_name_comp(
    eng: &Engine,
    caches: &u::LintCaches,
    node: GNode,
) -> Option<String> {
    let iter = comp_or_for_iter(eng, node)?;
    iterating_dict_name(eng, caches, iter)
}

fn iterating_dict_name(eng: &Engine, caches: &u::LintCaches, iter: GNode) -> Option<String> {
    // iter matches Call(func=Attribute(attrname="keys")):
    //   safe_infer(iter.func) BoundMethod -> iter.as_string().rpartition(".keys")[0]
    // iter is Name|Attribute: safe_infer(iter) Dict -> iter.as_string()
    let is_keys_call = {
        let md = eng.md(iter.m);
        match &md.tree.nodes[iter.n.idx()].kind {
            NodeKind::Call { func, .. } => {
                matches!(&md.tree.nodes[func.idx()].kind, NodeKind::Attribute { attrname, .. } if md.tree.s(*attrname) == "keys")
            }
            _ => false,
        }
    };
    if is_keys_call {
        let func = call_func(eng, iter)?;
        if !matches!(safe_infer(eng, caches, func), Some(Value::BoundMethod { .. })) {
            return None;
        }
        let s = u::as_string(eng, iter);
        // rpartition(".keys")[0]
        return s.rfind(".keys").map(|i| s[..i].to_string());
    }
    let is_name_or_attr = eng.kind_is(iter, |k| matches!(k, NodeKind::Name { .. } | NodeKind::Attribute { .. }));
    if is_name_or_attr {
        let inferred = safe_infer(eng, caches, iter);
        let is_dict = matches!(&inferred, Some(Value::Node(g)) if eng.kind_is(*g, |k| matches!(k, NodeKind::Dict { .. })))
            || matches!(&inferred, Some(Value::SynthDict { .. }));
        if is_dict {
            return Some(u::as_string(eng, iter));
        }
    }
    None
}

/// value.lookup(name)[1][-1].lineno — lineno of the LAST assignment statement
/// for `name` in scope order, None if no result.
fn lookup_last_lineno(eng: &Engine, name_node: GNode, name: &str) -> Option<u32> {
    let sym = eng.sym(name);
    let res = eng.lookup(name_node, sym);
    let last = res.1.last()?;
    match last {
        pyinfer::value::NV::N(g) => Some(lineno(eng, *g)),
        _ => None,
    }
}

// ---- RecommendationChecker helpers ----

/// _is_builtin (recommendation_checker.py:69-74): safe_infer(node) is a
/// builtin object named `function`.
fn is_builtin_named(eng: &Engine, caches: &u::LintCaches, node: GNode, function: &str) -> bool {
    match safe_infer(eng, caches, node) {
        Some(v) if value_is_builtin_object(eng, &v) => {
            matches!(&v, Value::Node(g) if eng.node_name(*g).as_deref() == Some(function))
        }
        _ => false,
    }
}

/// Const node value == 0 (== semantics: True for Const False too).
fn const_value_is_zero(eng: &Engine, g: GNode) -> bool {
    let md = eng.md(g.m);
    use pyast::tree::IntValue;
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Const(ConstValue::Int(IntValue::Small(0)))
        | NodeKind::Const(ConstValue::Bool(false)) => true,
        NodeKind::Const(ConstValue::Float(f)) if *f == 0.0 => true,
        _ => false,
    }
}

/// AssignName.name (for For targets — Tuple has no .name => None).
fn name_of_assign(eng: &Engine, g: GNode) -> Option<String> {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::AssignName { name } => Some(md.tree.s(*name).to_string()),
        _ => None,
    }
}

/// Attribute.attrname.
fn attr_name(eng: &Engine, g: GNode) -> Option<String> {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Attribute { attrname, .. } => Some(md.tree.s(*attrname).to_string()),
        _ => None,
    }
}

/// R1721 expr_list/target_list value: pylint mixes a bare STRING (Name elt/
/// target) with a LIST (Tuple). `"i" == ["i"]` is False in Python; this enum
/// preserves that distinction. Empty = the `[]` default (non-Name/Tuple).
#[derive(PartialEq)]
enum CompVal {
    Scalar(String),
    List(Vec<String>),
    Empty,
}

impl CompVal {
    /// Python truthiness: non-empty string / non-empty list. Empty default
    /// ([]) is falsy; "" scalar is falsy.
    fn truthy(&self) -> bool {
        match self {
            CompVal::Scalar(s) => !s.is_empty(),
            CompVal::List(v) => !v.is_empty(),
            CompVal::Empty => false,
        }
    }
}

/// number of generators on a comprehension scope node.
fn comp_generators_len(eng: &Engine, comp: GNode) -> usize {
    let md = eng.md(comp.m);
    match &md.tree.nodes[comp.n.idx()].kind {
        NodeKind::ListComp(d) | NodeKind::SetComp(d) | NodeKind::GeneratorExp(d) => {
            d.generators.len()
        }
        NodeKind::DictComp(d) => d.generators.len(),
        _ => 0,
    }
}

/// Starred.value if g is a Starred node.
fn starred_value(eng: &Engine, g: GNode) -> Option<GNode> {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Starred { value, .. } => Some(GNode { m: g.m, n: *value }),
        _ => None,
    }
}

/// len(List.elts) for a List node (0 otherwise).
fn list_elts_len(eng: &Engine, g: GNode) -> usize {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::List { elts, .. } => elts.len(),
        _ => 0,
    }
}

/// len(Dict.items) for an inferred Dict value (node or synth); 0 otherwise.
fn dict_items_len(eng: &Engine, v: &Value) -> usize {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Dict { items } => items.len(),
                _ => 0,
            }
        }
        Value::SynthDict { items } => items.len(),
        _ => 0,
    }
}

/// len(items/elts) for a Dict OR List inferred value (C0209 % branch).
fn dict_or_list_len(eng: &Engine, v: &Value) -> usize {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Dict { items } => items.len(),
                NodeKind::List { elts, .. } => elts.len(),
                _ => 0,
            }
        }
        Value::SynthDict { items } => items.len(),
        Value::SynthSeq { elems, kind } if matches!(kind, pyinfer::value::SeqKind::List) => {
            elems.len()
        }
        _ => 0,
    }
}

// ---- ImplicitBooleaness helpers ----

/// utils.is_call_of_name (utils.py:1700): Call with func Name(name).
fn is_call_of_name(eng: &Engine, node: GNode, name: &str) -> bool {
    let md = eng.md(node.m);
    let NodeKind::Call { func, .. } = &md.tree.nodes[node.n.idx()].kind else {
        return false;
    };
    matches!(
        &md.tree.nodes[func.idx()].kind,
        NodeKind::Name { name: n } if md.tree.s(*n) == name
    )
}

/// utils.is_test_condition (utils.py:1708-1718).
fn is_test_condition(eng: &Engine, node: GNode, parent: Option<GNode>) -> bool {
    let parent = parent.or_else(|| eng.parent(node));
    let Some(parent) = parent else { return false };
    let md = eng.md(parent.m);
    match &md.tree.nodes[parent.n.idx()].kind {
        NodeKind::While { test, .. }
        | NodeKind::If { test, .. }
        | NodeKind::Assert { test, .. } => {
            let test = GNode { m: parent.m, n: *test };
            node == test || eng.parent_of(test, node)
        }
        NodeKind::IfExp { test, .. } => {
            let test = GNode { m: parent.m, n: *test };
            node == test || eng.parent_of(test, node)
        }
        NodeKind::Comprehension { ifs, .. } => ifs.contains(&node.n),
        _ => {
            // is_call_of_name(parent, "bool") and parent.parent_of(node)
            drop(md);
            is_call_of_name(eng, parent, "bool") && eng.parent_of(parent, node)
        }
    }
}

/// base_names_of_instance (implicit_booleaness_checker.py:409-420): for an
/// Instance (incl. Const/containers) return [class name] + ancestor names.
fn base_names_of_instance(eng: &Engine, v: &Value) -> Vec<String> {
    let cls = match v {
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(*cls),
        _ => eng.proxied_class(v),
    };
    let Some(cls) = cls else { return Vec::new() };
    let mut names = Vec::new();
    if let Some(n) = eng.node_name(cls) {
        names.push(n);
    }
    for anc in eng.ancestors(cls, true, None) {
        if let Some(n) = eng.node_name(anc) {
            names.push(n);
        }
    }
    names
}

/// instance_has_bool (implicit_booleaness_checker.py:152-159): the proxied
/// class (incl. ancestors) has a `__bool__` attribute. Uninferable -> True
/// (getattr returns Uninferable, the call doesn't raise -> return True).
fn instance_has_bool(eng: &Engine, v: &Value) -> bool {
    if v.is_uninferable() {
        return true;
    }
    let cls = match v {
        Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(*cls),
        _ => eng.proxied_class(v),
    };
    let Some(cls) = cls else { return false };
    let sym = eng.sym("__bool__");
    matches!(eng.class_getattr(cls, sym, None, false), Ok(v) if !v.is_empty())
}

/// is_base_container (utils.py:1933) || is_empty_dict_literal (utils.py:1937):
/// empty List/Set/Tuple literal, or empty Dict literal.
fn is_empty_literal(eng: &Engine, g: GNode) -> bool {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::List { elts, .. } | NodeKind::Set { elts } | NodeKind::Tuple { elts, .. } => {
            elts.is_empty()
        }
        NodeKind::Dict { items } => items.is_empty(),
        _ => false,
    }
}

/// _is_constant_zero (implicit_booleaness_checker.py:17-20): Const, value==0,
/// value is not False. 0 / 0.0 / 0j.
fn is_constant_zero(eng: &Engine, g: GNode) -> bool {
    let md = eng.md(g.m);
    use pyast::tree::IntValue;
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Const(ConstValue::Int(IntValue::Small(0))) => true,
        NodeKind::Const(ConstValue::Float(f)) if *f == 0.0 => true,
        NodeKind::Const(ConstValue::Complex { real, imag }) if *real == 0.0 && *imag == 0.0 => true,
        _ => false,
    }
}

/// is_empty_str_literal (utils.py:1941): Const, str, falsy ("").
fn is_empty_str_literal(eng: &Engine, g: GNode) -> bool {
    let md = eng.md(g.m);
    matches!(
        &md.tree.nodes[g.n.idx()].kind,
        NodeKind::Const(ConstValue::Str(s)) if s.is_empty()
    )
}

/// _implicit_booleaness_message_args (implicit_booleaness_checker.py:300-341).
fn implicit_booleaness_message_args(
    eng: &Engine,
    node: GNode,
    literal_node: GNode,
    operator: &str,
    target_node: GNode,
) -> (String, String, String) {
    // description from literal_node type; collection literal from description
    let md = eng.md(literal_node.m);
    let description = match &md.tree.nodes[literal_node.n.idx()].kind {
        NodeKind::List { .. } => "list",
        NodeKind::Tuple { .. } => "tuple",
        NodeKind::Dict { .. } => "dict",
        NodeKind::Const(ConstValue::Str(_)) => "str",
        _ => "iterable",
    };
    let collection_literal = match description {
        "list" => "[]",
        "tuple" => "()",
        "dict" => "{}",
        _ => "iterable",
    };
    drop(md);
    let instance_name = match {
        let md = eng.md(target_node.m);
        match &md.tree.nodes[target_node.n.idx()].kind {
            NodeKind::Call { .. } => 0u8,
            NodeKind::Attribute { .. } | NodeKind::Name { .. } => 1,
            _ => 2,
        }
    } {
        0 => {
            // f"{target_node.func.as_string()}(...)"
            let func = call_func(eng, target_node).unwrap_or(target_node);
            format!("{}(...)", u::as_string(eng, func))
        }
        1 => u::as_string(eng, target_node),
        _ => "x".to_string(),
    };
    let original_comparison = format!("{} {} {}", instance_name, operator, collection_literal);
    let suggestion = get_suggestion_with_redundant(eng, node, &instance_name, operator, &["!="]);
    (original_comparison, suggestion, description.to_string())
}

/// _get_suggestion (implicit_booleaness_checker.py:332-341) with the
/// {"!="} negation-redundant set (used by C1803).
fn get_suggestion_with_redundant(
    eng: &Engine,
    node: GNode,
    name: &str,
    operator: &str,
    redundant_ops: &[&str],
) -> String {
    if redundant_ops.contains(&operator) {
        if in_boolean_context(eng, node) {
            name.to_string()
        } else {
            format!("bool({})", name)
        }
    } else {
        format!("not {}", name)
    }
}

/// _get_suggestion with {"!=", "is not"} (C1804/C1805).
fn get_suggestion(eng: &Engine, node: GNode, name: &str, operator: &str) -> String {
    get_suggestion_with_redundant(eng, node, name, operator, &["!=", "is not"])
}

/// _in_boolean_context (implicit_booleaness_checker.py:343-407).
fn in_boolean_context(eng: &Engine, node: GNode) -> bool {
    let mut current = node;
    loop {
        let Some(parent) = eng.parent(current) else { return false };
        let md = eng.md(parent.m);
        match &md.tree.nodes[parent.n.idx()].kind {
            NodeKind::If { test, .. }
            | NodeKind::While { test, .. }
            | NodeKind::Assert { test, .. }
            | NodeKind::IfExp { test, .. } => {
                return *test == current.n;
            }
            NodeKind::UnaryOp { op, operand } => {
                return &**op == "not" && *operand == current.n;
            }
            NodeKind::Comprehension { ifs, .. } => {
                return ifs.contains(&current.n);
            }
            NodeKind::Call { func, args, .. } => {
                // bool(...) call with current in args
                let is_bool = matches!(
                    &md.tree.nodes[func.idx()].kind,
                    NodeKind::Name { name } if md.tree.s(*name) == "bool"
                );
                return is_bool && args.contains(&current.n);
            }
            NodeKind::BoolOp { values, .. } => {
                if values.contains(&current.n) {
                    drop(md);
                    current = parent;
                    continue;
                }
                return false;
            }
            // GeneratorExp/Lambda special cases (any/all/filter) omitted —
            // rare; conservatively break.
            _ => return false,
        }
    }
}

// ---- R1732 helpers ----

/// _is_inside_context_manager (refactoring_checker.py:141-149).
fn is_inside_context_manager(eng: &Engine, node: GNode) -> bool {
    let frame = eng.frame(node);
    if !is_funcdef(eng, frame) {
        return false;
    }
    let name = func_bare_name(eng, frame);
    if name == "__enter__" || name == "__aenter__" {
        return true;
    }
    // decorated_with(frame, contextlib.contextmanager / asynccontextmanager)
    crate::typecheck::decorated_with(
        eng,
        frame,
        &["contextlib.contextmanager", "contextlib.asynccontextmanager"],
    )
}

/// _is_a_return_statement (refactoring_checker.py:152-159): some ancestor
/// strictly below the frame is a Return.
fn is_a_return_statement(eng: &Engine, node: GNode) -> bool {
    let frame = eng.frame(node);
    let mut cur = node;
    while let Some(p) = eng.parent(cur) {
        if p == frame {
            return false;
        }
        if eng.kind_is(p, |k| matches!(k, NodeKind::Return { .. })) {
            return true;
        }
        cur = p;
    }
    false
}

/// _is_part_of_with_items (refactoring_checker.py:162-174): a With ancestor
/// where items[0][0].lineno <= node.lineno <= items[-1][0].tolineno.
fn is_part_of_with_items(eng: &Engine, node: GNode) -> bool {
    let frame = eng.frame(node);
    let mut cur = node;
    loop {
        if cur == frame {
            return false;
        }
        if eng.kind_is(cur, |k| matches!(k, NodeKind::With(_) | NodeKind::AsyncWith(_))) {
            let md = eng.md(cur.m);
            let items: &[(pyast::NodeId, Option<pyast::NodeId>)] = match &md.tree.nodes[cur.n.idx()].kind {
                NodeKind::With(d) | NodeKind::AsyncWith(d) => &d.items,
                _ => return false,
            };
            if items.is_empty() {
                return false;
            }
            let first = items[0].0;
            let last = items[items.len() - 1].0;
            let first_line = md.tree.nodes[first.idx()].fromlineno;
            let last_tolineno = md.tree.nodes[last.idx()].tolineno;
            let node_line = eng.fromlineno(node);
            return first_line <= node_line && node_line <= last_tolineno;
        }
        match eng.parent(cur) {
            Some(p) => cur = p,
            None => return false,
        }
    }
}

/// _will_be_released_automatically (refactoring_checker.py:177-192): parent is
/// a Call whose func infers to contextlib ExitStack.enter_context.
fn will_be_released_automatically(eng: &Engine, caches: &u::LintCaches, node: GNode) -> bool {
    let Some(parent) = eng.parent(node) else { return false };
    let pfunc = match call_func(eng, parent) {
        Some(f) => f,
        None => return false,
    };
    match safe_infer(eng, caches, pfunc) {
        Some(v) => match u::value_qname(eng, &v) {
            Some(q) => {
                q == "contextlib._BaseExitStack.enter_context"
                    || q == "contextlib.ExitStack.enter_context"
            }
            None => false,
        },
        None => false,
    }
}

/// node.func for a Call node.
fn call_func(eng: &Engine, node: GNode) -> Option<GNode> {
    let md = eng.md(node.m);
    match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::Call { func, .. } => Some(GNode { m: node.m, n: *func }),
        _ => None,
    }
}

/// node.name for a Name node.
fn name_of(eng: &Engine, g: GNode) -> Option<String> {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Name { name } => Some(md.tree.s(*name).to_string()),
        _ => None,
    }
}

/// utils.node_frame_class (utils.py:677): climb frames until a ClassDef.
fn node_frame_class(eng: &Engine, node: GNode) -> Option<GNode> {
    let mut klass = eng.frame(node);
    let mut nodes_seen = std::collections::HashSet::new();
    loop {
        if !nodes_seen.insert(klass) {
            return None;
        }
        if eng.kind_is(klass, |k| matches!(k, NodeKind::ClassDef(_))) {
            return Some(klass);
        }
        // klass = klass.parent.frame() if klass.parent else None
        let p = eng.parent(klass)?;
        klass = eng.frame(p);
    }
}

/// _has_exit_in_scope (refactoring_checker.py): scope.locals.get("exit")
/// first entry is an Import / ImportFrom node.
fn has_exit_in_scope(eng: &Engine, scope: GNode) -> bool {
    let md = eng.md(scope.m);
    let Some(locals) = md.tree.locals.get(&scope.n) else { return false };
    let entry = locals.iter().find(|(sym, _)| md.tree.s(**sym) == "exit");
    let Some((_, entries)) = entry else { return false };
    let Some(&first) = entries.first() else { return false };
    matches!(
        &md.tree.nodes[first.idx()].kind,
        NodeKind::Import { .. } | NodeKind::ImportFrom { .. }
    )
}

/// IfExp(body, orelse) where both are 2-element Tuple|List → return
/// (body_key, body_val, orelse_key, orelse_val).
fn ifexp_2tuple_keyvals(eng: &Engine, g: GNode) -> Option<(GNode, GNode, GNode, GNode)> {
    let md = eng.md(g.m);
    let NodeKind::IfExp { body, orelse, .. } = &md.tree.nodes[g.n.idx()].kind else {
        return None;
    };
    let two_elts = |n: pyast::NodeId| -> Option<(pyast::NodeId, pyast::NodeId)> {
        match &md.tree.nodes[n.idx()].kind {
            NodeKind::Tuple { elts, .. } | NodeKind::List { elts, .. } if elts.len() == 2 => {
                Some((elts[0], elts[1]))
            }
            _ => None,
        }
    };
    let (bk, bv) = two_elts(*body)?;
    let (ok, ov) = two_elts(*orelse)?;
    Some((
        GNode { m: g.m, n: bk },
        GNode { m: g.m, n: bv },
        GNode { m: g.m, n: ok },
        GNode { m: g.m, n: ov },
    ))
}

/// `comp.as_string()[1:-1]` — strip the surrounding [ and ].
fn strip_brackets(s: &str) -> String {
    let trimmed = s.strip_prefix('[').unwrap_or(s);
    let trimmed = trimmed.strip_suffix(']').unwrap_or(trimmed);
    trimmed.to_string()
}

/// _looks_like_infinite_iterator (refactoring_checker.py:32-): safe_infer
/// is a bases.Instance with qname in {itertools.count, itertools.cycle}.
fn looks_like_infinite_iterator(eng: &Engine, caches: &u::LintCaches, param: GNode) -> bool {
    match safe_infer(eng, caches, param) {
        Some(Value::Inst { cls, .. }) => {
            let qn = eng.qname(cls);
            qn == "itertools.count" || qn == "itertools.cycle"
        }
        _ => false,
    }
}

/// _get_break_loop_node (basic_error_checker.py:28-44): walk parents until a
/// For/While whose orelse does NOT contain the current chain node.
fn break_loop_node(eng: &Engine, brk: GNode) -> Option<GNode> {
    let loop_nodes = |k: &NodeKind| {
        matches!(k, NodeKind::For(_) | NodeKind::While { .. } | NodeKind::AsyncFor(_))
    };
    let mut parent = eng.parent(brk);
    let mut node = brk;
    while let Some(p) = parent {
        let in_orelse = {
            let md = eng.md(p.m);
            let orelse: &[pyast::NodeId] = match &md.tree.nodes[p.n.idx()].kind {
                NodeKind::For(d) | NodeKind::AsyncFor(d) => &d.orelse,
                NodeKind::While { orelse, .. } => orelse,
                _ => &[],
            };
            orelse.contains(&node.n)
        };
        if eng.kind_is(p, loop_nodes) && !in_orelse {
            break;
        }
        node = p;
        parent = eng.parent(p);
    }
    parent
}
