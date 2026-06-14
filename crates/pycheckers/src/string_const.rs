//! StringConstantChecker (checkers/strings.py:639-1015).
//!
//! Token-phase (`process_tokens`): emits W1402 (anomalous-unicode-escape) and
//! W1401 (anomalous-backslash) via `process_string_token`, AND builds the
//! `string_tokens` map (token position -> str_eval value + next token) plus the
//! `_parenthesized_string_tokens` map consumed by the AST callbacks.
//!
//! AST-phase callbacks (run during the walk):
//!   - visit_call/list/set/tuple/assign -> check_for_concatenated_strings (W1404)
//!   - visit_const -> redundant-u-string-prefix (W1406)
//!
//! Default config: check_str_concat_over_line_jumps=False,
//! check_quote_consistency=False (so C0327 inconsistent-quotes is inert and the
//! parenthesized map only matters when the line-jump option is on — i.e. never
//! by default; we keep it for fidelity).

use pyast::pytok::{PyTokKind as K, PyTokens};
use pyast::tree::{ConstValue, NodeKind};
use pyinfer::value::GNode;
use rustc_hash::FxHashMap;

use crate::ckutils as u;
use crate::walker::WalkCx;

/// A non-raw string token's special-meaning escape sets (strings.py:707-711).
const ESCAPE_CHARACTERS: &str = "abfnrtvx\n\r\t\\'\"01234567";
const UNICODE_ESCAPE_CHARACTERS: &str = "uUN";

/// One W1401/W1402 message precomputed in phase 1.
#[derive(Clone)]
pub struct StrConstMsg {
    pub msgid: &'static str,
    pub line: u32,
    pub col: i64,
    pub text: String,
}

/// What the AST callbacks need about a string token at a given (row, byte_col).
#[derive(Clone)]
pub struct StrTokInfo {
    /// `str_eval(token)` — the decoded body of THIS single token.
    pub eval_value: String,
    /// the next non-(NL/NEWLINE/COMMENT) token is a STRING literal
    pub next_is_string: bool,
    /// start row of that next token (only meaningful when next_is_string)
    pub next_row: u32,
    /// `_parenthesized_string_tokens[start]`
    pub parenthesized: bool,
}

/// Result of process_tokens — replayed/consumed in later phases.
#[derive(Default)]
pub struct StrConstTokens {
    /// W1402/W1401 messages, in emission order.
    pub msgs: Vec<StrConstMsg>,
    /// (row, byte_col) -> info. byte_col matches astroid Const.col_offset.
    pub string_tokens: FxHashMap<(u32, u32), StrTokInfo>,
}

const PAREN_IGNORE: [K; 3] = [K::Newline, K::Nl, K::Comment];

/// `_find_prev_token` / `_find_next_token` with a custom ignore set.
fn find_prev(tk: &PyTokens, index: usize, ignore: &[K]) -> Option<usize> {
    let mut i = index as isize - 1;
    while i >= 0 && ignore.contains(&tk.toks[i as usize].kind) {
        i -= 1;
    }
    if i >= 0 {
        Some(i as usize)
    } else {
        None
    }
}
fn find_next(tk: &PyTokens, index: usize, ignore: &[K]) -> Option<usize> {
    let mut i = index + 1;
    while i < tk.toks.len() && ignore.contains(&tk.toks[i].kind) {
        i += 1;
    }
    if i < tk.toks.len() {
        Some(i)
    } else {
        None
    }
}

/// `_is_initial_string_token` (strings.py:757-766).
fn is_initial_string_token(tk: &PyTokens, index: usize) -> bool {
    if let Some(p) = find_prev(tk, index, &PAREN_IGNORE) {
        if tk.toks[p].kind == K::Str {
            return false;
        }
    }
    matches!(find_next(tk, index, &PAREN_IGNORE), Some(n) if tk.toks[n].kind == K::Str)
}

/// `_is_parenthesized` (strings.py:768-779). ignore = PAREN_IGNORE + STRING.
fn is_parenthesized(text: &str, tk: &PyTokens, index: usize) -> bool {
    let ignore = [K::Newline, K::Nl, K::Comment, K::Str];
    let prev_ok = find_prev(tk, index, &ignore).is_some_and(|p| {
        tk.toks[p].kind == K::Op && tok_text(text, tk, p) == "("
    });
    if !prev_ok {
        return false;
    }
    find_next(tk, index, &ignore).is_some_and(|n| {
        tk.toks[n].kind == K::Op && tok_text(text, tk, n) == ")"
    })
}

fn tok_text<'a>(text: &'a str, tk: &PyTokens, i: usize) -> &'a str {
    let t = &tk.toks[i];
    &text[t.start as usize..t.end as usize]
}

/// `process_tokens` (strings.py:724-755), default config (no consistency check).
/// `encoding_is_ascii`: pylint converts the start column to a byte count only
/// when the module encoding is not ascii; our token offsets are already byte
/// offsets, so the start col we record is always the byte col — which is what
/// astroid Const.col_offset reports. We therefore ignore the ascii branch.
pub fn process_tokens(text: &str, tk: &PyTokens) -> StrConstTokens {
    let mut out = StrConstTokens::default();
    let n = tk.toks.len();
    for i in 0..n {
        let tok = tk.toks[i];
        if tok.kind != K::Str {
            continue;
        }
        let raw = &text[tok.start as usize..tok.end as usize];
        // byte column of the token start within its row
        let row_start = tk.row_starts[tok.row as usize - 1];
        let start_col = tok.start - row_start;
        // process_string_token (W1401/W1402)
        process_string_token(raw, tok.row, start_col, text, tok.start, &mut out.msgs);
        // next token ignoring NL/NEWLINE/COMMENT
        let next = find_next(tk, i, &PAREN_IGNORE);
        let (next_is_string, next_row) = match next {
            Some(j) if tk.toks[j].kind == K::Str => (true, tk.toks[j].row),
            _ => (false, 0),
        };
        let parenthesized =
            is_initial_string_token(tk, i) && is_parenthesized(text, tk, i);
        out.string_tokens.insert(
            (tok.row, start_col),
            StrTokInfo {
                eval_value: str_eval(raw),
                next_is_string,
                next_row,
                parenthesized,
            },
        );
    }
    out
}

/// `process_string_token` (strings.py:913-937) + `process_non_raw_string_token`
/// (strings.py:939-999). `tok_byte_start` is the byte offset of the token start
/// in the whole file (used to compute per-line col offsets identically).
fn process_string_token(
    token: &str,
    start_row: u32,
    start_col: u32,
    _text: &str,
    _tok_byte_start: u32,
    msgs: &mut Vec<StrConstMsg>,
) {
    // find the first quote char; everything before is the (lowercased) prefix
    let chars: Vec<char> = token.chars().collect();
    let mut quote_idx = None;
    for (idx, &c) in chars.iter().enumerate() {
        if c == '\'' || c == '"' {
            quote_idx = Some(idx);
            break;
        }
    }
    let Some(qi) = quote_idx else { return };
    let quote_char = chars[qi];
    let prefix: String = chars[..qi].iter().collect::<String>().to_lowercase();
    let after_prefix: Vec<char> = chars[qi..].to_vec();
    let quote_length = if after_prefix.len() >= 6
        && after_prefix[..3] == [quote_char, quote_char, quote_char]
        && after_prefix[after_prefix.len() - 3..] == [quote_char, quote_char, quote_char]
    {
        3
    } else {
        1
    };
    let string_body: String = after_prefix
        [quote_length..after_prefix.len() - quote_length]
        .iter()
        .collect();
    if prefix.contains('r') {
        return;
    }
    // process_non_raw_string_token: string_start_col = start_col + len(prefix)
    // + quote_length. len(prefix) counts CHARACTERS (prefix is ascii markers).
    let string_start_col = start_col as i64 + prefix.chars().count() as i64 + quote_length as i64;
    process_non_raw_string_token(&prefix, &string_body, start_row, string_start_col, msgs);
}

fn process_non_raw_string_token(
    prefix: &str,
    string_body: &str,
    start_row: u32,
    string_start_col: i64,
    msgs: &mut Vec<StrConstMsg>,
) {
    let body: Vec<char> = string_body.chars().collect();
    let mut index = 0usize;
    loop {
        // index = string_body.find("\\", index)
        let mut found = None;
        let mut j = index;
        while j < body.len() {
            if body[j] == '\\' {
                found = Some(j);
                break;
            }
            j += 1;
        }
        let Some(bi) = found else { break };
        index = bi;
        // next_char = string_body[index+1]; match = string_body[index:index+2]
        let next_char = body.get(index + 1).copied().unwrap_or('\0');
        let matchs: String = body[index..(index + 2).min(body.len())].iter().collect();
        // last_newline = rfind("\n", 0, index)
        let last_newline = body[..index].iter().rposition(|&c| c == '\n');
        let (line, col_offset) = match last_newline {
            None => (start_row, index as i64 + string_start_col),
            Some(ln) => {
                let nl_count = body[..index].iter().filter(|&&c| c == '\n').count() as u32;
                (start_row + nl_count, (index - ln - 1) as i64)
            }
        };
        if UNICODE_ESCAPE_CHARACTERS.contains(next_char) {
            if prefix.contains('u') {
                // pass
            } else if !prefix.contains('b') {
                // unicode by default: pass
            } else {
                msgs.push(StrConstMsg {
                    msgid: "W1402",
                    line,
                    col: col_offset,
                    text: format!(
                        "Anomalous Unicode escape in byte string: '{}'. \
                         String constant might be missing an r or u prefix.",
                        matchs
                    ),
                });
            }
        } else if !ESCAPE_CHARACTERS.contains(next_char) {
            msgs.push(StrConstMsg {
                msgid: "W1401",
                line,
                col: col_offset,
                text: format!(
                    "Anomalous backslash in string: '{}'. \
                     String constant might be missing an r prefix.",
                    matchs
                ),
            });
        }
        index += 2;
    }
}

// ---------------------------------------------------------------------------
// AST-phase callbacks (W1404 implicit-str-concat, W1406 redundant-u-string).
// The per-module `string_tokens` map is carried on WalkCx.strconst_tokens.
// ---------------------------------------------------------------------------

/// visit_call -> check_for_concatenated_strings(node.args, "call")
pub fn visit_call(cx: &mut WalkCx, node: GNode) {
    let md = cx.eng.md(node.m);
    let NodeKind::Call { args, .. } = &md.tree.nodes[node.n.idx()].kind else { return };
    let elts: Vec<GNode> = args.iter().map(|&a| GNode { m: node.m, n: a }).collect();
    check_for_concatenated_strings(cx, &elts, "call");
}
pub fn visit_list(cx: &mut WalkCx, node: GNode) {
    let md = cx.eng.md(node.m);
    let NodeKind::List { elts, .. } = &md.tree.nodes[node.n.idx()].kind else { return };
    let elts: Vec<GNode> = elts.iter().map(|&a| GNode { m: node.m, n: a }).collect();
    check_for_concatenated_strings(cx, &elts, "list");
}
pub fn visit_set(cx: &mut WalkCx, node: GNode) {
    let md = cx.eng.md(node.m);
    let NodeKind::Set { elts } = &md.tree.nodes[node.n.idx()].kind else { return };
    let elts: Vec<GNode> = elts.iter().map(|&a| GNode { m: node.m, n: a }).collect();
    check_for_concatenated_strings(cx, &elts, "set");
}
pub fn visit_tuple(cx: &mut WalkCx, node: GNode) {
    let md = cx.eng.md(node.m);
    let NodeKind::Tuple { elts, .. } = &md.tree.nodes[node.n.idx()].kind else { return };
    let elts: Vec<GNode> = elts.iter().map(|&a| GNode { m: node.m, n: a }).collect();
    check_for_concatenated_strings(cx, &elts, "tuple");
}
/// visit_assign: only when node.value is Const(str).
pub fn visit_assign(cx: &mut WalkCx, node: GNode) {
    let md = cx.eng.md(node.m);
    let NodeKind::Assign { value, .. } = &md.tree.nodes[node.n.idx()].kind else { return };
    let value = *value;
    let is_const_str = matches!(
        &md.tree.nodes[value.idx()].kind,
        NodeKind::Const(ConstValue::Str(_) | ConstValue::StrSurrogate(_))
    );
    if is_const_str {
        let v = GNode { m: node.m, n: value };
        check_for_concatenated_strings(cx, &[v], "assignment");
    }
}

/// visit_const -> _detect_u_string_prefix (W1406). Only str (pytype
/// builtins.str) whose parent is NOT a JoinedStr and whose astroid kind == "u".
pub fn visit_const(cx: &mut WalkCx, node: GNode) {
    let eng = cx.eng;
    let md = eng.md(node.m);
    if !matches!(
        &md.tree.nodes[node.n.idx()].kind,
        NodeKind::Const(ConstValue::Str(_) | ConstValue::StrSurrogate(_))
    ) {
        return;
    }
    if !md.tree.u_string_consts.contains(&node.n) {
        return;
    }
    // parent not JoinedStr
    if let Some(p) = eng.parent(node) {
        if matches!(eng.md(p.m).tree.nodes[p.n.idx()].kind, NodeKind::JoinedStr { .. }) {
            return;
        }
    }
    // _detect_u_string_prefix emits with line/col only (NO node=), so pylint
    // attributes it to linter.current_name (the raw FileItem name) rather than
    // node.root().name. For a package __init__.py these differ ("X.__init__"
    // vs the astroid-stripped "X") — use emit_nodeless to match.
    cx.emit_nodeless(
        "W1406",
        u::lineno(eng, node),
        u::col_offset(eng, node) as i64,
        "The u prefix for strings is no longer necessary in Python >=3.0".to_string(),
    );
}

/// `check_for_concatenated_strings` (strings.py:874-911). Default config:
/// check_str_concat_over_line_jumps=False, so a concat only fires when the
/// next string token starts on the SAME line as the element.
fn check_for_concatenated_strings(cx: &mut WalkCx, elements: &[GNode], iterable_type: &str) {
    let eng = cx.eng;
    for &elt in elements {
        let md = eng.md(elt.m);
        // isinstance(elt, Const) and pytype in str types
        let elt_value: Option<String> = match &md.tree.nodes[elt.n.idx()].kind {
            NodeKind::Const(ConstValue::Str(s)) => Some(s.to_string()),
            NodeKind::Const(ConstValue::StrSurrogate(points)) => {
                Some(points.iter().filter_map(|&c| char::from_u32(c)).collect())
            }
            _ => None,
        };
        let Some(elt_value) = elt_value else { continue };
        let col = md.tree.nodes[elt.n.idx()].col_offset;
        let line = md.tree.nodes[elt.n.idx()].fromlineno;
        drop(md);
        // elt.col_offset < 0 -> escaped newlines, skip
        if col < 0 {
            continue;
        }
        let Some(info) = cx.strconst_tokens.get(&(line, col as u32)).cloned() else {
            // not in string_tokens (Latin1 / mismatch)
            continue;
        };
        // concatenation: AST Const value != the single token's eval value, and
        // the next token is a STRING.
        if info.eval_value != elt_value && info.next_is_string {
            // next_token.start[0] == elt.lineno OR (over_line_jumps && !paren).
            // Default over_line_jumps=False -> only the same-line case.
            if info.next_row == line {
                // add_message(line=elt.lineno) with NO col_offset -> col 0.
                cx.emit_node(
                    "W1404",
                    line,
                    0,
                    format!("Implicit string concatenation found in {}", iterable_type),
                );
            }
        }
    }
}

/// `str_eval` (strings.py:1023-1036): strip prefix markers and the quotes.
fn str_eval(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    let lower2: String = chars
        .iter()
        .take(2)
        .collect::<String>()
        .to_lowercase();
    let mut start = 0usize;
    if lower2 == "fr" || lower2 == "rf" {
        start = 2;
    } else {
        let lower1: String = chars.first().map(|c| c.to_lowercase().to_string()).unwrap_or_default();
        if lower1 == "r" || lower1 == "u" || lower1 == "f" {
            start = 1;
        }
    }
    let rest = &chars[start..];
    if rest.len() >= 3 {
        let head: String = rest[..3].iter().collect();
        if head == "\"\"\"" || head == "'''" {
            return rest[3..rest.len() - 3].iter().collect();
        }
    }
    if rest.len() >= 2 {
        rest[1..rest.len() - 1].iter().collect()
    } else {
        String::new()
    }
}
