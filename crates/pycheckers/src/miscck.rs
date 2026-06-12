//! Port of pylint/checkers/misc.py EncodingChecker (name "miscellaneous"):
//! W0511 fixme, token phase. Default config only (notes = FIXME/XXX/TODO,
//! notes-rgx empty, check-fixme-in-docstring off).
//!
//! `process_module` (the raw side) is a deliberate no-op: its encoding
//! sweeps are dead in this pipeline (a file with an unknown/undecodable
//! coding declaration fails the AST build first — spec §19.2).
//!
//! ByIdManagedMessagesChecker (I0023) is default-OFF (default_enabled:
//! False) and dropped by prepare_checkers under every target profile; not
//! ported.

use pyast::pytok::{PyTokKind, PyTokens};

pub struct FixmeMsg {
    pub line: u32,
    /// comment start col + 1 (the misc.py quirk), CHARACTER columns
    pub col: i64,
    /// the matched `msg` group (note tag through end of comment)
    pub text: String,
}

/// `#\s*(?P<msg>(FIXME|XXX|TODO)(?=(:|\s|\Z)).*?$)`, re.IGNORECASE,
/// `.match`-anchored at the comment start (misc.py:104-123, 150-166).
fn fixme_match(comment: &str) -> Option<usize> {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"^#\s*((?i:FIXME|XXX|TODO))").unwrap());
    let caps = re.captures(comment)?;
    let tag = caps.get(1).unwrap();
    // lookahead (?=(:|\s|\Z)): tag must be followed by ':', whitespace or end
    let rest = &comment[tag.end()..];
    match rest.chars().next() {
        None => Some(tag.start()),
        Some(c) if c == ':' || c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}') => {
            Some(tag.start())
        }
        _ => None,
    }
}

/// EncodingChecker.process_tokens (misc.py:150-180), comment path only
/// (docstring path is behind check-fixme-in-docstring, default off).
pub fn process_tokens(text: &str, tk: &PyTokens, emit: &mut dyn FnMut(FixmeMsg)) {
    for (i, t) in tk.toks.iter().enumerate() {
        if t.kind != PyTokKind::Comment {
            continue;
        }
        let s = tk.tok_str(text, i);
        if let Some(msg_start) = fixme_match(s) {
            emit(FixmeMsg {
                line: t.row,
                col: tk.tok_col(text, i) as i64 + 1,
                text: s[msg_start..].to_string(),
            });
        }
    }
}
