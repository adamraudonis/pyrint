//! Parse entry point: decode -> ruff parse -> build astroid-equivalent tree.

use ruff_python_parser::{Mode, ParseOptions};
use ruff_text_size::{Ranged, TextSize};

use crate::build::{BuildOptions, Builder};
use crate::source::SourceFile;
use crate::tree::Tree;

pub struct ParseOutcome {
    pub tree: Option<Tree>,
    pub error: Option<ParseFailure>,
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
    }
}
