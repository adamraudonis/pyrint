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
    let parsed = ruff_python_parser::parse_unchecked(&src.text, ParseOptions::from(Mode::Module));
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
    let module = match parsed.syntax() {
        ruff_python_ast::Mod::Module(m) => m,
        _ => unreachable!("Mode::Module"),
    };

    // def/class/async keyword token offsets for position fixups
    let mut def_class: Vec<(TextSize, bool)> = Vec::new();
    let mut asyncs: Vec<(TextSize, bool)> = Vec::new();
    for tok in parsed.tokens() {
        use ruff_python_ast::token::TokenKind;
        match tok.kind() {
            TokenKind::Def => def_class.push((tok.range().start(), true)),
            TokenKind::Class => def_class.push((tok.range().start(), false)),
            TokenKind::Async => asyncs.push((tok.range().start(), true)),
            _ => {}
        }
    }

    let opts = BuildOptions {
        modname: modname.to_string(),
        filepath: filepath.to_string(),
        package,
    };
    let tree = Builder::build(src, module, &def_class, &asyncs, &opts);
    ParseOutcome {
        tree: Some(tree),
        error: None,
    }
}
