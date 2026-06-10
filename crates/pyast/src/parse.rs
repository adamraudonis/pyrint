//! Parse entry point: decode -> ruff parse -> build astroid-equivalent tree.

use ruff_python_parser::{Mode, ParseOptions};
use ruff_text_size::{Ranged, TextSize};

use crate::build::{BuildOptions, Builder};
use crate::source::SourceFile;
use crate::tree::Tree;

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
    let tree = Builder::build(src, module, &def_class, &asyncs, &all_tokens, &opts);
    ParseOutcome {
        tree: Some(tree),
        error: None,
        tokens: tok_events,
    }
}
