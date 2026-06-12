//! CPython-3.12-`tokenize`-equivalent token stream for the token-layer
//! checkers (FormatChecker, EncodingChecker fixme). Built from ruff's lexer
//! over the UNNORMALIZED decoded source text — pylint tokenizes the raw byte
//! stream (`tokenize.tokenize(stream.readline)`), so line endings are
//! preserved here, unlike the AST build's universal-newline text.
//!
//! Divergences from ruff's raw stream patched to match CPython (probed
//! against the pinned 3.12 venv, /tmp/tokdump.py transcripts):
//! - synthetic ENCODING token at (0,0) prepended;
//! - synthetic ENDMARKER at (nrows+1, 0) appended;
//! - DEDENT tokens at EOF re-rowed to nrows+1 (CPython puts them one row
//!   past the last physical line even when the file lacks a final newline);
//! - a trailing NL with empty string is synthesized when the file does not
//!   end in a newline and its final partial row carries no real token
//!   (whitespace-only last line) or ends in a COMMENT — CPython emits
//!   `NL ''` there, ruff emits nothing.
//!
//! Known divergence (accepted): lone `\r` mid-file. CPython 3.12 lexes
//! `a\rb` as one row (the `\r` folds into an ERRORTOKEN-ish OP), while ruff
//! emits a Newline token for the `\r`. Rows still match (our row model
//! splits at `\n` only); only the extra NEWLINE token differs.

use ruff_python_parser::{Mode, ParseOptions};
use ruff_text_size::Ranged;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PyTokKind {
    Encoding,
    Name,
    Number,
    Str,
    FStringStart,
    FStringMiddle,
    FStringEnd,
    Op,
    /// tokenize.NEWLINE (logical line end)
    Newline,
    /// tokenize.NL
    Nl,
    Indent,
    Dedent,
    Comment,
    EndMarker,
}

#[derive(Clone, Copy, Debug)]
pub struct PyTok {
    pub kind: PyTokKind,
    /// 1-based start row (0 for the ENCODING token)
    pub row: u32,
    /// byte offsets into the raw text (start == end for synthetic tokens)
    pub start: u32,
    pub end: u32,
}

pub struct PyTokens {
    pub toks: Vec<PyTok>,
    /// byte offset of each physical row start. Rows split at `\n` ONLY
    /// (CPython's tokenize consumes lines via readline: `\r\n` stays inside
    /// the row and a lone `\r` is NOT a row boundary).
    pub row_starts: Vec<u32>,
    /// number of physical rows; a final partial line counts (0 for "")
    pub nrows: u32,
}

impl PyTokens {
    pub fn row_of(&self, off: u32) -> u32 {
        match self.row_starts.binary_search(&off) {
            Ok(i) => i as u32 + 1,
            Err(i) => i as u32, // i >= 1 since row_starts[0] == 0
        }
    }

    /// Physical row text INCLUDING its newline; "" for rows past the end.
    pub fn row_text<'a>(&self, text: &'a str, row: u32) -> &'a str {
        if row == 0 || row > self.nrows {
            return "";
        }
        let start = self.row_starts[row as usize - 1] as usize;
        let end = if (row as usize) < self.row_starts.len() {
            self.row_starts[row as usize] as usize
        } else {
            text.len()
        };
        &text[start..end]
    }

    /// token.string
    pub fn tok_str<'a>(&self, text: &'a str, i: usize) -> &'a str {
        let t = &self.toks[i];
        if t.kind == PyTokKind::Encoding {
            return "utf-8"; // value unused by the checkers
        }
        &text[t.start as usize..t.end as usize]
    }

    /// token.line: the physical row(s) spanned by the token, with line
    /// terminators ("" for ENCODING/ENDMARKER/EOF DEDENTs).
    pub fn tok_line<'a>(&self, text: &'a str, i: usize) -> &'a str {
        let t = &self.toks[i];
        if t.row == 0 || t.row > self.nrows {
            return "";
        }
        let start_row = t.row;
        let end_row = if t.end > t.start {
            self.row_of(t.end - 1)
        } else {
            start_row
        };
        let start = self.row_starts[start_row as usize - 1] as usize;
        let end = if (end_row as usize) < self.row_starts.len() {
            self.row_starts[end_row as usize] as usize
        } else {
            text.len()
        };
        &text[start..end]
    }

    /// token start column in CHARACTERS (CPython tokenize columns).
    pub fn tok_col(&self, text: &str, i: usize) -> u32 {
        let t = &self.toks[i];
        if t.row == 0 || t.row > self.nrows {
            return 0;
        }
        let line_start = self.row_starts[t.row as usize - 1] as usize;
        text[line_start..t.start as usize].chars().count() as u32
    }
}

/// Tokenize raw (line-ending-preserved) decoded source for the token-layer
/// checkers.
pub fn tokenize_for_checkers(text: &str) -> PyTokens {
    use ruff_python_ast::token::TokenKind as TK;

    let mut row_starts = vec![0u32];
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' && i + 1 < bytes.len() {
            row_starts.push(i as u32 + 1);
        }
    }
    let nrows = if text.is_empty() { 0 } else { row_starts.len() as u32 };

    let mut tk = PyTokens { toks: Vec::new(), row_starts, nrows };
    tk.toks.push(PyTok { kind: PyTokKind::Encoding, row: 0, start: 0, end: 0 });

    let options = ParseOptions::from(Mode::Module)
        .with_target_version(ruff_python_ast::PythonVersion::PY312);
    let parsed = ruff_python_parser::parse_unchecked(text, options);
    for t in parsed.tokens() {
        let kind = match t.kind() {
            TK::Comment => PyTokKind::Comment,
            TK::Newline => PyTokKind::Newline,
            TK::NonLogicalNewline => PyTokKind::Nl,
            TK::Indent => PyTokKind::Indent,
            TK::Dedent => PyTokKind::Dedent,
            TK::String => PyTokKind::Str,
            TK::FStringStart | TK::TStringStart => PyTokKind::FStringStart,
            TK::FStringMiddle | TK::TStringMiddle => PyTokKind::FStringMiddle,
            TK::FStringEnd | TK::TStringEnd => PyTokKind::FStringEnd,
            TK::Int | TK::Float | TK::Complex => PyTokKind::Number,
            TK::EndOfFile => continue,
            k if k.is_keyword() || k == TK::Name => PyTokKind::Name,
            _ => PyTokKind::Op,
        };
        let mut start = t.range().start().to_u32();
        let end = t.range().end().to_u32();
        let mut row = if kind == PyTokKind::Dedent && start as usize == text.len() {
            nrows + 1
        } else {
            tk.row_of(start)
        };
        if kind == PyTokKind::Indent && end > start {
            // ruff's Indent token spans backslash-continuation lines
            // ("\\\n    "); CPython's INDENT is the FINAL row's leading
            // whitespace only, positioned at (final_row, 0).
            let end_row = tk.row_of(end - 1);
            if end_row != row {
                row = end_row;
                start = tk.row_starts[end_row as usize - 1];
            }
        }
        tk.toks.push(PyTok { kind, row, start, end });
    }

    // Trailing-NL synthesis: CPython emits `NL ''` at EOF-without-newline
    // when the last lexical token is a COMMENT, or when the final partial
    // row carries no token at all (whitespace-only last line). Ruff emits
    // nothing in those cases (probed).
    if !text.is_empty() && !text.ends_with('\n') && !text.ends_with('\r') {
        // insertion point: after the last non-Dedent token
        let mut ins = tk.toks.len();
        while ins > 1 && tk.toks[ins - 1].kind == PyTokKind::Dedent {
            ins -= 1;
        }
        let last_real = &tk.toks[ins - 1];
        let final_row_has_token = tk.toks[1..]
            .iter()
            .any(|t| t.row == nrows && t.kind != PyTokKind::Dedent);
        if last_real.kind == PyTokKind::Comment || !final_row_has_token {
            let len = text.len() as u32;
            tk.toks.insert(
                ins,
                PyTok { kind: PyTokKind::Nl, row: nrows, start: len, end: len },
            );
        }
    }

    tk.toks.push(PyTok {
        kind: PyTokKind::EndMarker,
        row: nrows + 1,
        start: text.len() as u32,
        end: text.len() as u32,
    });
    tk
}
