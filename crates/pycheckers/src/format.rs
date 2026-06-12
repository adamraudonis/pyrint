//! Port of pylint/checkers/format.py (FormatChecker, pylint 4.0.5).
//!
//! Token side (`process_tokens`, format.py:380-469): C0301 line-too-long
//! (incl. pragma-text excision + checker_off quirk), C0302 too-many-lines
//! (incl. the run-global `_pragma_lineno` leak), C0303/C0304/C0305 with the
//! `specific_splitlines` buffer-drop quirk, W0301 unnecessary-semicolon,
//! W0311 bad-indentation, C0325 superfluous-parens, C0327/C0328 line
//! endings. AST side (`visit_default`, format.py:495-579): C0321
//! multiple-statements.
//!
//! Spec: reference/notes/09-format.md §1. All quirks replicated bug-for-bug
//! (dead code — `_lines` bookkeeping, the lowercase-l-suffix branch and
//! visit_default's discarded `lines` list — is omitted with comments).

use pyast::pytok::{PyTokKind as K, PyTokens};
use pyast::tree::NodeKind;
use pyinfer::graph::Engine;
use pyinfer::value::GNode;
use rustc_hash::FxHashMap;

use crate::ckutils as u;
use crate::pragma::{option_po_search_pos, parse_pragma};
use crate::walker::WalkCx;

/// A message from the token phase. `ignored_only` marks the C0301
/// checker_off path (linter.add_ignored_message — I0021 bookkeeping only,
/// no visible output).
pub struct FmtMsg {
    pub msgid: &'static str,
    pub line: u32,
    pub col: i64,
    pub text: String,
    pub ignored_only: bool,
}

const MAX_LINE_LENGTH: usize = 100; // max-line-length default
const MAX_MODULE_LINES: u32 = 1000; // max-module-lines default
const INDENT_STRING: &str = "    "; // indent-string default

/// _KEYWORD_TOKENS (format.py:34-50)
const KEYWORD_TOKENS: &[&str] = &[
    "assert", "del", "elif", "except", "for", "if", "in", "not", "raise", "return", "while",
    "yield", "with", "=", ":=",
];

/// Python str.isspace() characters (str.strip()/rstrip() default set).
fn py_is_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(py_is_space)
}

/// len(line) > n in CHARACTERS without counting the whole string.
fn char_len_gt(s: &str, n: usize) -> bool {
    s.chars().nth(n).is_some()
}

/// `specific_splitlines` (format.py:626-649): split like Python
/// str.splitlines(keepends=True) but re-merge breaks other than \n/\r/\r\n.
/// Net effect: split at \n / \r\n / \r; a FINAL chunk whose last char is one
/// of the "unsplit" break characters is silently DROPPED (the buffer is
/// never flushed — replicate).
fn specific_splitlines(lines: &str) -> Vec<&str> {
    const UNSPLIT_ENDS: &[char] = &[
        '\u{0b}', '\u{0c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}', '\u{2029}',
    ];
    let mut res = Vec::new();
    let bytes = lines.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                res.push(&lines[start..=i]);
                start = i + 1;
                i += 1;
            }
            b'\r' => {
                let end = if i + 1 < bytes.len() && bytes[i + 1] == b'\n' { i + 1 } else { i };
                res.push(&lines[start..=end]);
                start = end + 1;
                i = end + 1;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        let tail = &lines[start..];
        if let Some(last) = tail.chars().next_back() {
            if !UNSPLIT_ENDS.contains(&last) {
                res.push(tail);
            }
            // else: buffer-drop quirk — the chunk vanishes from all checks
        }
    }
    res
}

struct TokState<'a> {
    text: &'a str,
    tk: &'a PyTokens,
}

impl<'a> TokState<'a> {
    fn s(&self, i: usize) -> &'a str {
        self.tk.tok_str(self.text, i)
    }
    fn kind(&self, i: usize) -> K {
        self.tk.toks[i].kind
    }
    fn row(&self, i: usize) -> u32 {
        self.tk.toks[i].row
    }
    fn line(&self, i: usize) -> &'a str {
        self.tk.tok_line(self.text, i)
    }
}

/// FormatChecker.process_tokens (format.py:380-469).
pub fn process_tokens(
    text: &str,
    tk: &PyTokens,
    pragma_lineno: &FxHashMap<String, u32>,
    emit: &mut dyn FnMut(FmtMsg),
) {
    let ts = TokState { text, tk };
    let n = tk.toks.len();
    let mut indents: Vec<i64> = vec![0];
    let mut check_equal = false;
    let mut line_num: u32 = 0;
    let mut last_blank_line_num: u32 = 0;
    let mut last_line_ending: Option<&str> = None;

    for idx in 0..n {
        let tok = tk.toks[idx];
        if tok.row != line_num {
            line_num = tok.row;
            if tok.kind == K::Indent {
                new_line(&ts, idx.wrapping_sub(1), idx + 1, emit);
            } else {
                new_line(&ts, idx.wrapping_sub(1), idx, emit);
            }
        }
        match tok.kind {
            K::Newline => {
                check_equal = true;
                // _check_line_ending (format.py:471-493). C0328 needs the
                // non-default expected-line-ending-format option — inactive.
                let line_ending = ts.s(idx);
                if let Some(last) = last_line_ending {
                    if !line_ending.is_empty() && line_ending != last {
                        emit(FmtMsg {
                            msgid: "C0327",
                            line: line_num,
                            col: 0,
                            text: "Mixed line endings LF and CRLF".to_string(),
                            ignored_only: false,
                        });
                    }
                }
                last_line_ending = Some(line_ending);
            }
            K::Indent => {
                check_equal = false;
                check_indent_level(ts.s(idx), *indents.last().unwrap() + 1, line_num, emit);
                let top = *indents.last().unwrap();
                indents.push(top + 1);
            }
            K::Dedent => {
                check_equal = true;
                if indents.len() > 1 {
                    indents.pop();
                }
            }
            K::Nl => {
                let line = ts.line(idx);
                if line.trim_matches(['\r', '\n']).is_empty() {
                    last_blank_line_num = line_num;
                }
            }
            K::Comment | K::Encoding => {}
            _ => {
                if check_equal {
                    check_equal = false;
                    check_indent_level(ts.line(idx), *indents.last().unwrap(), line_num, emit);
                }
            }
        }
        // (dead lowercase-l-suffix branch omitted: unreachable on 3.12)
        if KEYWORD_TOKENS.contains(&ts.s(idx)) {
            check_keyword_parentheses(&ts, idx, emit);
        }
    }

    // format.py:448-469
    line_num = line_num.saturating_sub(1); // ENDMARKER row - 1 == wc -l
    if line_num > MAX_MODULE_LINES {
        // C0302 line attribution: linter._pragma_lineno — RUN-GLOBAL map of
        // raw pragma message strings -> last pragma row; leaks across files
        // (format.py:448-464, spec §1.11).
        let lineno = ["C0302", "too-many-lines"]
            .iter()
            .filter_map(|k| pragma_lineno.get(*k))
            .find(|&&l| l != 0)
            .copied()
            .unwrap_or(1);
        emit(FmtMsg {
            msgid: "C0302",
            line: lineno,
            col: 0,
            text: format!("Too many lines in module ({line_num}/{MAX_MODULE_LINES})"),
            ignored_only: false,
        });
    }
    if line_num == last_blank_line_num && line_num > 0 {
        emit(FmtMsg {
            msgid: "C0305",
            line: line_num,
            col: 0,
            text: "Trailing newlines".to_string(),
            ignored_only: false,
        });
    }
}

/// `_last_token_on_line_is` (format.py:117-122).
fn last_token_on_line_is(ts: &TokState, line_end: usize, token: &str) -> bool {
    if line_end == usize::MAX {
        return false; // idx-1 underflow can't happen: idx 0 is ENCODING row 0
    }
    (line_end > 0 && ts.s(line_end - 1) == token)
        || (line_end > 1 && ts.s(line_end - 2) == token && ts.kind(line_end - 1) == K::Comment)
}

/// `new_line` (format.py:261-270) minus the dead `_lines` bookkeeping.
fn new_line(ts: &TokState, line_end: usize, line_start: usize, emit: &mut dyn FnMut(FmtMsg)) {
    if last_token_on_line_is(ts, line_end, ";") {
        emit(FmtMsg {
            msgid: "W0301",
            line: ts.row(line_end),
            col: 0,
            text: "Unnecessary semicolon".to_string(),
            ignored_only: false,
        });
    }
    let line_num = ts.row(line_start);
    let line = ts.line(line_start);
    check_lines(ts, line_start, line, line_num, emit);
}

/// `check_lines` (format.py:651-705): C0304 + C0303 dispatch + C0301.
fn check_lines(
    ts: &TokState,
    line_start: usize,
    lines: &str,
    lineno: u32,
    emit: &mut dyn FnMut(FmtMsg),
) {
    let split_lines = specific_splitlines(lines);
    for (offset, line) in split_lines.iter().enumerate() {
        if !line.ends_with('\n') {
            emit(FmtMsg {
                msgid: "C0304",
                line: lineno + offset as u32,
                col: 0,
                text: "Final newline missing".to_string(),
                ignored_only: false,
            });
            continue;
        }
        if ts.kind(line_start) != K::Str {
            // check_trailing_whitespace_ending (format.py:581-591):
            // rstrip of "\t\n\r\v " only — \f (formfeed) NOT stripped.
            let stripped = line.trim_end_matches(['\t', '\n', '\r', '\u{b}', ' ']);
            let remainder = &line[stripped.len()..];
            if remainder != "\n" && remainder != "\r\n" {
                emit(FmtMsg {
                    msgid: "C0303",
                    line: lineno + offset as u32,
                    col: stripped.chars().count() as i64,
                    text: "Trailing whitespace".to_string(),
                    ignored_only: false,
                });
            }
        }
    }

    // rough pre-filter on the UNstripped lines (includes the newline chars)
    if !split_lines.iter().any(|l| char_len_gt(l, MAX_LINE_LENGTH)) {
        return;
    }

    let mut checker_off = false;
    let purged: String;
    let mut effective: &str = lines;
    if let Some(m) = option_po_search_pos(lines) {
        // is_line_length_check_activated (format.py:614-624)
        let (reprs, _err) = parse_pragma(&m.payload);
        for r in &reprs {
            if r.action == "disable" && r.messages.iter().any(|s| s == "line-too-long") {
                checker_off = true;
                break;
            }
        }
        // remove_pylint_option_from_lines (format.py:604-612)
        purged = format!("{}{}", py_rstrip(&lines[..m.g1_start]), &lines[m.g1_end..]);
        effective = &purged;
    }
    for (offset, line) in specific_splitlines(effective).iter().enumerate() {
        check_line_length(line, lineno + offset as u32, checker_off, emit);
    }
}

/// `check_line_length` (format.py:593-602).
fn check_line_length(line: &str, i: u32, checker_off: bool, emit: &mut dyn FnMut(FmtMsg)) {
    let line = py_rstrip(line);
    if !char_len_gt(line, MAX_LINE_LENGTH) {
        return;
    }
    if ignore_long_lines(line) {
        return;
    }
    let len = line.chars().count();
    emit(FmtMsg {
        msgid: "C0301",
        line: i,
        col: 0,
        text: format!("Line too long ({len}/{MAX_LINE_LENGTH})"),
        ignored_only: checker_off,
    });
}

/// ignore-long-lines default regex: `^\s*(# )?<?https?://\S+>?$`
/// (anchored, so search == fullmatch on the rstripped line).
fn ignore_long_lines(line: &str) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"^\s*(# )?<?https?://\S+>?$").unwrap());
    re.is_match(line)
}

/// `check_indent_level` (format.py:707-729). `string` is the INDENT token
/// text or the WHOLE raw line (check_equal path).
fn check_indent_level(string: &str, expected: i64, line_num: u32, emit: &mut dyn FnMut(FmtMsg)) {
    let unit_size = INDENT_STRING.len() as i64;
    let mut s = string;
    let mut level: i64 = 0;
    while s.starts_with(INDENT_STRING) {
        s = &s[INDENT_STRING.len()..];
        level += 1;
    }
    let mut suppl: i64 = 0;
    while let Some(c) = s.chars().next() {
        if c == ' ' || c == '\t' {
            suppl += 1;
            s = &s[1..];
        } else {
            break;
        }
    }
    if level != expected || suppl > 0 {
        let i_type = if INDENT_STRING.starts_with('\t') { "tabs" } else { "spaces" };
        emit(FmtMsg {
            msgid: "W0311",
            line: line_num,
            col: 0,
            text: format!(
                "Bad indentation. Found {} {}, expected {}",
                level * unit_size + suppl,
                i_type,
                expected * unit_size
            ),
            ignored_only: false,
        });
    }
}

/// `_check_keyword_parentheses` (format.py:276-378). Absolute-index port —
/// the recursive `tokens[i:]` slice call is equivalent to restarting at
/// absolute index i (same scan bound `len(tokens) - 1`).
fn check_keyword_parentheses(ts: &TokState, start: usize, emit: &mut dyn FnMut(FmtMsg)) {
    let n = ts.tk.toks.len();
    if start + 1 >= n || ts.s(start + 1) != "(" {
        return;
    }
    if ts.s(start) == "not" && start > 0 && ts.s(start - 1) == "is" {
        return; // `is not (...)` is a binary operator
    }
    let mut found_and_or = false;
    let mut contains_walrus_operator = false;
    let mut walrus_operator_depth: i64 = 0;
    let mut contains_double_parens: i64 = 0;
    let mut depth: i64 = 0;
    let keyword_token = ts.s(start);
    let line_num = ts.row(start);

    for i in start..n.saturating_sub(1) {
        let tok_s = ts.s(i);
        if ts.kind(i) == K::Nl {
            return; // parens assumed needed for line continuation
        }
        if tok_s == ":=" || (format!("{}{}", tok_s, ts.s(i + 1)) == ":=") {
            contains_walrus_operator = true;
            walrus_operator_depth = depth;
        }
        if tok_s == "(" {
            depth += 1;
            if ts.s(i + 1) == "(" {
                contains_double_parens = 1;
            }
        } else if tok_s == ")" {
            depth -= 1;
            if depth != 0 {
                if contains_double_parens != 0 && ts.s(i + 1) == ")" {
                    if matches!(keyword_token, "in" | "if" | "not") {
                        continue;
                    }
                    return;
                }
                contains_double_parens -= 1;
                continue;
            }
            // depth == 0: closing the outermost paren
            let next_s = ts.s(i + 1);
            let next_k = ts.kind(i + 1);
            if matches!(next_s, ":" | ")" | "]" | "}" | "in")
                || matches!(next_k, K::Newline | K::EndMarker | K::Comment)
            {
                if contains_walrus_operator && walrus_operator_depth - 1 == depth {
                    return;
                }
                if i == start + 2 {
                    return; // empty tuple `()`
                }
                if found_and_or {
                    return;
                }
                if keyword_token == "in" {
                    return; // PR #4948 churn-avoidance special case
                }
                emit(FmtMsg {
                    msgid: "C0325",
                    line: line_num,
                    col: 0,
                    text: u::format_template("Unnecessary parens after %r keyword", &[keyword_token]),
                    ignored_only: false,
                });
            }
            return; // always stop once depth hits 0
        } else if depth == 1 {
            match tok_s {
                "," => return,             // tuple
                "and" | "or" => found_and_or = true,
                "yield" => return,         // parens required
                "for" => return,           // generator expression
                "else" => {
                    // `if "(" in remaining tokens: recurse from the else`
                    if (i..n).any(|j| ts.s(j) == "(") {
                        check_keyword_parentheses(ts, i, emit);
                    }
                    return;
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AST side: C0321 multiple-statements (visit_default, format.py:495-579)
// ---------------------------------------------------------------------------

/// Per-module state for visit_default. In pylint `_visited_lines` is reset
/// by process_tokens; here it is rebuilt fresh for every module walk.
#[derive(Default)]
pub struct FormatWalkState {
    /// line -> 1 (seen) | 2 (C0321 already emitted on this line)
    visited_lines: FxHashMap<u32, u8>,
}

impl FormatWalkState {
    pub fn clear(&mut self) {
        self.visited_lines.clear();
    }

    /// FormatChecker.visit_default — registered for EVERY node class
    /// (ast_walker.py:64-69 bypasses the checks_msgs gate, so an inline
    /// `# pylint: enable=multiple-statements` CAN resurrect C0321).
    pub fn visit_default(&mut self, cx: &mut WalkCx, g: GNode) {
        let eng = cx.eng;
        if !is_statement(eng, g) {
            return;
        }
        // node.root().pure_python is always true in this pipeline
        let prev_line: u32 = if let Some(sib) = u::previous_sibling(eng, g) {
            u::lineno(eng, sib)
        } else if let Some(parent) = eng.parent(g) {
            let md = eng.md(parent.m);
            match &md.tree.nodes[parent.n.idx()].kind {
                NodeKind::Try(d) | NodeKind::TryStar(d)
                    if is_first_in_else_finally(&d.orelse, &d.finalbody, g.n) =>
                {
                    infer_else_finally_line(eng, g, &d.body, &d.handlers, &d.orelse, &d.finalbody)
                }
                NodeKind::Module(_) => 0,
                _ => statement_fromlineno(eng, parent),
            }
        } else {
            0
        };
        let line = u::lineno(eng, g);
        if prev_line == line && self.visited_lines.get(&line) != Some(&2) {
            self.check_multi_statement_line(cx, g, line);
            return;
        }
        if self.visited_lines.contains_key(&line) {
            return;
        }
        let tolineno = blockstart_tolineno(eng, g);
        for l in line..=tolineno {
            // unconditional overwrite, exactly like `_visited_lines[line] = 1`
            self.visited_lines.insert(l, 1);
        }
        // (the `lines` list built from self._lines is dead code — omitted)
    }

    /// `_check_multi_statement_line` (format.py:555-579).
    fn check_multi_statement_line(&mut self, cx: &mut WalkCx, g: GNode, line: u32) {
        let eng = cx.eng;
        {
            let md = eng.md(g.m);
            let kind = &md.tree.nodes[g.n.idx()].kind;
            // 1. node is a With itself -> nested context managers, exempt
            if matches!(kind, NodeKind::With(_) | NodeKind::AsyncWith(_)) {
                return;
            }
            // 2./3. single-line-if-stmt / single-line-class-stmt opt-ins are
            //       default-False -> no exemption.
            // 4. Ellipsis stubs: Expr(Const(Ellipsis)) directly in a
            //    FunctionDef/ClassDef body.
            if let NodeKind::Expr { value } = kind {
                if matches!(
                    &md.tree.nodes[value.idx()].kind,
                    NodeKind::Const(pyast::tree::ConstValue::Ellipsis)
                ) {
                    if let Some(p) = eng.parent(g) {
                        if matches!(
                            &md.tree.nodes[p.n.idx()].kind,
                            NodeKind::FunctionDef(_)
                                | NodeKind::AsyncFunctionDef(_)
                                | NodeKind::ClassDef(_)
                        ) {
                            return;
                        }
                    }
                }
            }
        }
        cx.emit_node(
            "C0321",
            u::msg_line(cx.eng, g),
            u::msg_col(cx.eng, g),
            "More than one statement on a single line".to_string(),
        );
        self.visited_lines.insert(line, 2);
    }
}

/// astroid `is_statement` (Statement subclasses).
fn is_statement(eng: &Engine, g: GNode) -> bool {
    matches!(
        &eng.md(g.m).tree.nodes[g.n.idx()].kind,
        NodeKind::FunctionDef(_)
            | NodeKind::AsyncFunctionDef(_)
            | NodeKind::ClassDef(_)
            | NodeKind::Return { .. }
            | NodeKind::Delete { .. }
            | NodeKind::Assign { .. }
            | NodeKind::AugAssign { .. }
            | NodeKind::AnnAssign { .. }
            | NodeKind::TypeAlias { .. }
            | NodeKind::For(_)
            | NodeKind::AsyncFor(_)
            | NodeKind::While { .. }
            | NodeKind::If { .. }
            | NodeKind::With(_)
            | NodeKind::AsyncWith(_)
            | NodeKind::Match { .. }
            | NodeKind::Raise { .. }
            | NodeKind::Try(_)
            | NodeKind::TryStar(_)
            | NodeKind::Assert { .. }
            | NodeKind::Import { .. }
            | NodeKind::ImportFrom { .. }
            | NodeKind::Global { .. }
            | NodeKind::Nonlocal { .. }
            | NodeKind::Expr { .. }
            | NodeKind::Pass
            | NodeKind::Break
            | NodeKind::Continue
            | NodeKind::ExceptHandler { .. }
    )
}

fn is_first_in_else_finally(
    orelse: &[pyast::NodeId],
    finalbody: &[pyast::NodeId],
    n: pyast::NodeId,
) -> bool {
    orelse.first() == Some(&n) || finalbody.first() == Some(&n)
}

/// `_infer_else_finally_line_number` (format.py:542-553).
fn infer_else_finally_line(
    eng: &Engine,
    node: GNode,
    body: &[pyast::NodeId],
    handlers: &[pyast::NodeId],
    orelse: &[pyast::NodeId],
    finalbody: &[pyast::NodeId],
) -> u32 {
    let md = eng.md(node.m);
    let tol = |n: pyast::NodeId| md.tree.node(n).tolineno;
    let last_handler_body_last: Option<pyast::NodeId> = handlers.last().and_then(|&h| {
        match &md.tree.nodes[h.idx()].kind {
            NodeKind::ExceptHandler { body: hbody, .. } => hbody.last().copied(),
            _ => None,
        }
    });
    let mut last_line_of_prev_block = 0u32;
    if finalbody.contains(&node.n) && !orelse.is_empty() {
        last_line_of_prev_block = tol(*orelse.last().unwrap());
    } else if let Some(last) = last_handler_body_last {
        last_line_of_prev_block = tol(last);
    } else if let Some(&last) = body.last() {
        last_line_of_prev_block = tol(last);
    }
    if last_line_of_prev_block != 0 {
        last_line_of_prev_block + 1
    } else {
        0
    }
}

/// node.parent.statement().fromlineno — first Statement ancestor (or self).
fn statement_fromlineno(eng: &Engine, mut g: GNode) -> u32 {
    loop {
        if is_statement(eng, g) {
            return u::lineno(eng, g);
        }
        match eng.parent(g) {
            Some(p) => g = p,
            None => return u::lineno(eng, g), // Module.statement() raises in
                                              // astroid 4; unreachable here
        }
    }
}

/// `node.blockstart_tolineno` where defined, else node.tolineno
/// (visit_default's try/except AttributeError).
fn blockstart_tolineno(eng: &Engine, g: GNode) -> u32 {
    let md = eng.md(g.m);
    let tree = &md.tree;
    let tol = |n: pyast::NodeId| tree.node(n).tolineno;
    match &tree.nodes[g.n.idx()].kind {
        // If/While -> test.tolineno
        NodeKind::If { test, .. } | NodeKind::While { test, .. } => tol(*test),
        // For -> iter.tolineno
        NodeKind::For(d) | NodeKind::AsyncFor(d) => tol(d.iter),
        // With -> items[-1][0].tolineno (last context-manager expression)
        NodeKind::With(d) | NodeKind::AsyncWith(d) => d
            .items
            .last()
            .map(|it| tol(it.0))
            .unwrap_or_else(|| tree.node(g.n).fromlineno),
        // Try/TryStar (MultiLineWithElseBlockNode default) -> lineno
        NodeKind::Try(_) | NodeKind::TryStar(_) => tree.node(g.n).fromlineno,
        // ExceptHandler -> name.tolineno / type.tolineno / lineno
        NodeKind::ExceptHandler { type_, name, .. } => {
            if let Some(nm) = name {
                tol(*nm)
            } else if let Some(t) = type_ {
                tol(*t)
            } else {
                tree.node(g.n).fromlineno
            }
        }
        // FunctionDef -> returns.tolineno if returns else args.tolineno
        NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
            if let Some(r) = d.returns {
                tol(r)
            } else {
                tol(d.args)
            }
        }
        // ClassDef -> bases[-1].tolineno if bases else fromlineno
        NodeKind::ClassDef(d) => {
            if let Some(&b) = d.bases.last() {
                tol(b)
            } else {
                tree.node(g.n).fromlineno
            }
        }
        // no blockstart_tolineno attribute -> tolineno
        _ => tree.node(g.n).tolineno,
    }
}
