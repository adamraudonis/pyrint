//! Parse entry point: decode -> ruff parse -> build astroid-equivalent tree.

use ruff_python_parser::{Mode, ParseOptions};
use ruff_text_size::{Ranged, TextSize};

use crate::build::{BuildOptions, Builder};
use crate::source::SourceFile;
use crate::tree::Tree;
use crate::NodeId;

pub struct ParseOutcome {
    pub tree: Option<Tree>,
    pub error: Option<ParseFailure>,
    /// Compacted token stream for pylint's `process_tokens` pragma pass
    /// (message_state_handler.py:347-444). Only populated on success.
    pub tokens: Vec<TokEvent>,
}

/// What pylint's pragma loop needs from the token stream: row transitions,
/// NL/NEWLINE tokens (`seen_newline` tracking) and COMMENT tokens.
/// Consecutive same-row "other" tokens are collapsed (only the first token of
/// each physical row matters for the `saw_newline` backslash-continuation
/// rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokEventKind {
    Other,
    /// tokenize.NL / tokenize.NEWLINE
    Nl,
    Comment,
}

#[derive(Debug, Clone, Copy)]
pub struct TokEvent {
    /// 1-based row of the token start
    pub row: u32,
    pub kind: TokEventKind,
    /// byte range into the decoded source text (comments only; 0..0 otherwise)
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone)]
pub struct ParseFailure {
    pub line: u32,
    /// 1-based column as CPython SyntaxError.offset (approximated by ruff;
    /// exact value comes from the CPython fallback pass)
    pub offset: u32,
    pub message: String,
}

/// Parse decoded source text into a Tree.
pub fn parse_module(src: &SourceFile, modname: &str, filepath: &str, package: bool) -> ParseOutcome {
    // Target the pinned CPython (3.12): astroid raises SyntaxError for
    // 3.13+/3.14 syntax (PEP 696 type-param defaults, PEP 758 unparenthesized
    // except tuples, ...), which ruff reports as unsupported-syntax errors.
    let options = ParseOptions::from(Mode::Module)
        .with_target_version(ruff_python_ast::PythonVersion::PY312);
    let parsed = ruff_python_parser::parse_unchecked(&src.text, options);
    if let Some(err) = parsed.errors().first() {
        let (line, col) = src.line_col(err.location.start().to_u32());
        return ParseOutcome {
            tree: None,
            error: Some(ParseFailure {
                line,
                offset: col + 1,
                message: format!("{}", err.error),
            }),
            tokens: Vec::new(),
        };
    }
    if let Some(err) = parsed.unsupported_syntax_errors().first() {
        let (line, col) = src.line_col(err.range.start().to_u32());
        return ParseOutcome {
            tree: None,
            error: Some(ParseFailure {
                line,
                offset: col + 1,
                message: format!("{err}"),
            }),
            tokens: Vec::new(),
        };
    }
    let module = match parsed.syntax() {
        ruff_python_ast::Mod::Module(m) => m,
        _ => unreachable!("Mode::Module"),
    };

    // def/class/async keyword token offsets for position fixups + triaged
    // token list for paren matching
    let mut def_class: Vec<(TextSize, bool)> = Vec::new();
    let mut asyncs: Vec<(TextSize, bool)> = Vec::new();
    let mut all_tokens: Vec<(u32, u32, u8)> = Vec::new();
    let mut tok_events: Vec<TokEvent> = Vec::new();
    let mut last_row: u32 = 0;
    for tok in parsed.tokens() {
        use ruff_python_ast::token::TokenKind;
        match tok.kind() {
            TokenKind::Def => def_class.push((tok.range().start(), true)),
            TokenKind::Class => def_class.push((tok.range().start(), false)),
            TokenKind::Async => asyncs.push((tok.range().start(), true)),
            _ => {}
        }
        let kind = match tok.kind() {
            TokenKind::Lpar => 1u8,
            TokenKind::Rpar => 2,
            TokenKind::Comment | TokenKind::NonLogicalNewline | TokenKind::Newline
            | TokenKind::Indent | TokenKind::Dedent => 3,
            _ => 0,
        };
        all_tokens.push((tok.range().start().to_u32(), tok.range().end().to_u32(), kind));

        // pragma-pass token events (Python tokenize equivalents: NL/NEWLINE
        // and COMMENT; plus the first token of every physical row for the
        // row-transition logic in message_state_handler.process_tokens)
        let ev_kind = match tok.kind() {
            TokenKind::Comment => TokEventKind::Comment,
            TokenKind::Newline | TokenKind::NonLogicalNewline => TokEventKind::Nl,
            _ => TokEventKind::Other,
        };
        let (row, _) = src.line_col(tok.range().start().to_u32());
        if ev_kind != TokEventKind::Other || row != last_row {
            let (s, e) = if ev_kind == TokEventKind::Comment {
                (tok.range().start().to_u32(), tok.range().end().to_u32())
            } else {
                (0, 0)
            };
            tok_events.push(TokEvent { row, kind: ev_kind, start: s, end: e });
        }
        last_row = row;
    }

    let opts = BuildOptions {
        modname: modname.to_string(),
        filepath: filepath.to_string(),
        package,
    };
    let mut tree = Builder::build(src, module, &def_class, &asyncs, &all_tokens, &opts);
    attach_type_comments(src, &tok_events, &mut tree);
    ParseOutcome {
        tree: Some(tree),
        error: None,
        tokens: tok_events,
    }
}

/// CPython `type_comments=True` association, approximated for the positions
/// the corpora use (astroid feeds them to VariablesChecker only):
/// - Assign / For / With (+async): `# type: T` at the end of the header's
///   logical line -> stmt.type_annotation
/// - FunctionDef: `# type: (...) -> T` on the header colon line or on its
///   own line between header and first body statement ->
///   type_comment_args/_returns
/// `# type: ignore...` is never an annotation.
fn attach_type_comments(src: &SourceFile, tok_events: &[TokEvent], tree: &mut Tree) {
    use crate::tree::NodeKind;
    // line -> payload (first `# type:` comment per line)
    let mut by_line: std::collections::BTreeMap<u32, &str> = std::collections::BTreeMap::new();
    for ev in tok_events {
        if ev.kind != TokEventKind::Comment {
            continue;
        }
        let text = &src.text[ev.start as usize..ev.end as usize];
        let Some(rest) = text.strip_prefix('#') else { continue };
        let rest = rest.trim_start_matches([' ', '\t']);
        let Some(payload) = rest.strip_prefix("type:") else { continue };
        let payload = payload.trim();
        // TYPE_IGNORE: "ignore" followed by end or a non-alphanumeric
        let is_ignore = payload.strip_prefix("ignore").map_or(false, |r| {
            r.chars().next().map_or(true, |c| !c.is_alphanumeric())
        });
        if payload.is_empty() || is_ignore {
            continue;
        }
        by_line.entry(ev.row).or_insert(payload);
    }
    if by_line.is_empty() {
        return;
    }
    let mut used: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let n = tree.nodes.len();
    // pass 1: Assign / For / With header-line comments (deepest-first by
    // iterating in node order; first consumer of a line wins)
    for idx in 0..n {
        let id = NodeId(idx as u32);
        let header_line: Option<u32> = match &tree.nodes[idx].kind {
            NodeKind::Assign { .. } => Some(tree.nodes[idx].tolineno),
            NodeKind::For(d) | NodeKind::AsyncFor(d) => {
                Some(tree.nodes[d.iter.idx()].tolineno)
            }
            NodeKind::With(d) | NodeKind::AsyncWith(d) => d
                .items
                .last()
                .map(|&(cm, opt)| tree.nodes[opt.unwrap_or(cm).idx()].tolineno),
            _ => None,
        };
        let Some(line) = header_line else { continue };
        if used.contains(&line) {
            continue;
        }
        if let Some(payload) = by_line.get(&line) {
            // For/With: the body must NOT start on the header line
            // (a one-liner puts the comment after the block, not in the
            // grammar's TYPE_COMMENT slot)
            let ok = match &tree.nodes[idx].kind {
                NodeKind::For(d) | NodeKind::AsyncFor(d) => d
                    .body
                    .first()
                    .map(|&b| tree.nodes[b.idx()].fromlineno > line)
                    .unwrap_or(false),
                NodeKind::With(d) | NodeKind::AsyncWith(d) => d
                    .body
                    .first()
                    .map(|&b| tree.nodes[b.idx()].fromlineno > line)
                    .unwrap_or(false),
                _ => true,
            };
            if ok {
                used.insert(line);
                tree.type_comments.push((id, false, (*payload).into()));
            }
        }
    }
    // pass 1b: per-argument comments (`a,  # type: T` inside the parens)
    // attached to the Arguments node; signature-form payloads (leading
    // '(') are left for pass 2
    for idx in 0..n {
        let (args_id, body0): (NodeId, Option<NodeId>) = match &tree.nodes[idx].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                (d.args, d.body.first().copied())
            }
            _ => continue,
        };
        let Some(body0) = body0 else { continue };
        let from = tree.nodes[idx].fromlineno;
        let body0_line = tree.nodes[body0.idx()].fromlineno;
        if body0_line <= from + 1 {
            continue; // header fits one line: no per-arg comment positions
        }
        let to = body0_line - 1;
        for l in from..=to {
            if used.contains(&l) {
                continue;
            }
            if let Some(payload) = by_line.get(&l) {
                if payload.trim_start().starts_with('(') {
                    continue; // signature form -> pass 2
                }
                used.insert(l);
                tree.type_comments.push((args_id, false, (*payload).into()));
            }
        }
    }
    // pass 2: function signature comments
    for idx in 0..n {
        let id = NodeId(idx as u32);
        let (args, returns, body0): (NodeId, Option<NodeId>, Option<NodeId>) =
            match &tree.nodes[idx].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    (d.args, d.returns, d.body.first().copied())
                }
                _ => continue,
            };
        let Some(body0) = body0 else { continue };
        let body0_line = tree.nodes[body0.idx()].fromlineno;
        let mut header_last = tree.nodes[idx].fromlineno;
        header_last = header_last.max(tree.nodes[args.idx()].tolineno);
        if let Some(r) = returns {
            header_last = header_last.max(tree.nodes[r.idx()].tolineno);
        }
        if body0_line <= header_last {
            continue; // one-liner: no TYPE_COMMENT slot
        }
        // same-line form, else first `# type:` line strictly between
        let mut hit: Option<u32> = if by_line.contains_key(&header_last)
            && !used.contains(&header_last)
        {
            Some(header_last)
        } else {
            None
        };
        if hit.is_none() {
            for l in (header_last + 1)..body0_line {
                if used.contains(&l) {
                    continue;
                }
                if by_line.contains_key(&l) {
                    hit = Some(l);
                    break;
                }
            }
        }
        if let Some(l) = hit {
            used.insert(l);
            tree.type_comments.push((id, true, (*by_line.get(&l).unwrap()).into()));
        }
    }
}
