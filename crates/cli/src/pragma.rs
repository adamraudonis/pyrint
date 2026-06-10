//! Port of pylint/utils/pragma_parser.py: the OPTION_PO comment regex, the
//! TOK_REGEX pragma tokenizer and the parse_pragma state machine —
//! regex-semantics-exact (Python `re` backtracking behavior replicated by
//! construction, see option_po_search).

/// MESSAGE_KEYWORDS (pragma_parser.py:36).
pub const MESSAGE_KEYWORDS: &[&str] = &["disable-next", "disable-msg", "enable-msg", "disable", "enable"];

/// ALL_KEYWORDS sorted by length descending (pragma_parser.py:41-44):
/// alternation order in the KEYWORD branch of TOK_REGEX.
const KEYWORDS_BY_LEN: &[&str] = &[
    "disable-next",
    "disable-all",
    "disable-msg",
    "enable-msg",
    "skip-file",
    "disable",
    "enable",
];

fn is_word(c: char) -> bool {
    // Python \w (unicode): alphanumerics + underscore
    c.is_alphanumeric() || c == '_'
}

/// OPTION_PO.search(content).group(2) for a COMMENT token's text
/// (pragma_parser.py:14-27). The pattern is
/// ```text
/// (?:^\s*\#.*|\s*|\s*\#.*(?=\#.*?\bpylint:))(\#.*?\bpylint:\s*([^;#\n]+))[;#]{0,1}
/// ```
/// Backtracking semantics replicated: at string start the first alternative's
/// greedy `.*` binds group 1 to the LAST '#' (strictly after the first one)
/// from which `\bpylint:` + a non-empty payload can be matched; failing that,
/// the `\s*` alternative binds group 1 to the first viable '#'.
pub fn option_po_search(content: &str) -> Option<String> {
    let hashes: Vec<usize> = content
        .char_indices()
        .filter(|&(_, c)| c == '#')
        .map(|(i, _)| i)
        .collect();
    if hashes.is_empty() {
        return None;
    }
    // alt 1 (`^\s*\#.*` prefix) requires only whitespace before the first '#'
    let leading_ws = content[..hashes[0]].chars().all(|c| c.is_whitespace());
    if leading_ws {
        for &q in hashes.iter().skip(1).rev() {
            if let Some(p) = viable_payload(content, q) {
                return Some(p);
            }
        }
    }
    // alt 2 / later search positions: earliest viable '#'
    for &q in &hashes {
        if let Some(p) = viable_payload(content, q) {
            return Some(p);
        }
    }
    None
}

/// Group 1 attempt anchored at the '#' at byte `q`: `\#.*?\bpylint:\s*([^;#\n]+)`.
/// `.*?` is lazy, so occurrences of `pylint:` after `q` are tried in order;
/// the payload `\s*([^;#\n]+)` needs at least one char before the next
/// `;`/`#`/newline (an all-whitespace region backtracks `\s*` to leave its
/// last char for the `+`).
fn viable_payload(content: &str, q: usize) -> Option<String> {
    let bytes = content.as_bytes();
    let mut from = q + 1;
    loop {
        let t = content[from..].find("pylint:").map(|p| p + from)?;
        from = t + 1;
        // \b before 'pylint'
        let prev = content[..t].chars().next_back();
        if let Some(pc) = prev {
            if is_word(pc) {
                continue;
            }
        }
        let after = t + "pylint:".len();
        // region = up to the first of ';', '#', '\n' (group 2 char class)
        let mut end = bytes.len();
        for (i, &b) in bytes.iter().enumerate().skip(after) {
            if b == b';' || b == b'#' || b == b'\n' {
                end = i;
                break;
            }
        }
        if end <= after {
            continue; // empty payload region: backtrack to the next pylint:
        }
        let region = &content[after..end];
        let trimmed_start = region
            .char_indices()
            .find(|&(_, c)| !c.is_whitespace())
            .map(|(i, _)| i);
        return Some(match trimmed_start {
            Some(i) => region[i..].to_string(),
            // all-whitespace region: `\s*` gives back one char to `[^;#\n]+`
            None => region.chars().next_back().unwrap().to_string(),
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    Keyword,
    MessageString,
    Assign,
    MessageNumber,
}

/// re.finditer(TOK_REGEX, payload): alternatives tried in order at each
/// position; unmatched characters silently skipped (pragma_parser.py:46-58).
pub fn lex_pragma(payload: &str) -> Vec<(TokKind, String)> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = payload.char_indices().collect();
    let n = chars.len();
    let mut i = 0usize;
    while i < n {
        let (byte_pos, c) = chars[i];
        // KEYWORD: \b(<alternation>)\b
        let prev_ok = i == 0 || !is_word(chars[i - 1].1);
        if prev_ok {
            let rest = &payload[byte_pos..];
            let mut matched = None;
            for kw in KEYWORDS_BY_LEN {
                if rest.starts_with(kw) {
                    let after = rest[kw.len()..].chars().next();
                    if after.map_or(true, |a| !is_word(a)) {
                        matched = Some(*kw);
                        break;
                    }
                }
            }
            if let Some(kw) = matched {
                out.push((TokKind::Keyword, kw.to_string()));
                i += kw.chars().count();
                continue;
            }
        }
        // MESSAGE_STRING: [0-9A-Za-z\-\_]{2,}
        let is_msg_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
        if is_msg_char(c) {
            let mut j = i;
            while j < n && is_msg_char(chars[j].1) {
                j += 1;
            }
            if j - i >= 2 {
                let end_byte = if j < n { chars[j].0 } else { payload.len() };
                out.push((TokKind::MessageString, payload[byte_pos..end_byte].to_string()));
                i = j;
                continue;
            }
        }
        // ASSIGN
        if c == '=' {
            out.push((TokKind::Assign, "=".to_string()));
            i += 1;
            continue;
        }
        // MESSAGE_NUMBER: [CREIWF]{1}\d*
        if matches!(c, 'C' | 'R' | 'E' | 'I' | 'W' | 'F') {
            let mut j = i + 1;
            while j < n && chars[j].1.is_ascii_digit() {
                j += 1;
            }
            let end_byte = if j < n { chars[j].0 } else { payload.len() };
            out.push((TokKind::MessageNumber, payload[byte_pos..end_byte].to_string()));
            i = j;
            continue;
        }
        i += 1; // no alternative matched: character skipped
    }
    out
}

#[derive(Debug)]
pub struct PragmaRepr {
    pub action: &'static str,
    pub messages: Vec<String>,
}

#[derive(Debug)]
pub enum PragmaError {
    /// UnRecognizedOptionError -> E0011 unrecognized-inline-option
    Unrecognized { token: String },
    /// InvalidPragmaError -> I0010 bad-inline-option
    Invalid { token: String },
}

/// `parse_pragma` (pragma_parser.py:89-135). The Python generator can raise
/// mid-iteration AFTER earlier representers were already consumed, so the
/// port returns (representers-yielded-so-far, optional error).
pub fn parse_pragma(payload: &str) -> (Vec<PragmaRepr>, Option<PragmaError>) {
    let mut out: Vec<PragmaRepr> = Vec::new();
    let mut action: Option<&'static str> = None;
    let mut messages: Vec<String> = Vec::new();
    let mut assignment_required = false;
    let mut previous_token = String::new();

    let flush = |action: &'static str, messages: Vec<String>, out: &mut Vec<PragmaRepr>| -> Option<PragmaError> {
        // emit_pragma_representer (pragma_parser.py:61-66)
        if messages.is_empty() && MESSAGE_KEYWORDS.contains(&action) {
            return Some(PragmaError::Invalid { token: action.to_string() });
        }
        out.push(PragmaRepr { action, messages });
        None
    };

    for (kind, value) in lex_pragma(payload) {
        match kind {
            TokKind::Assign => {
                if !assignment_required {
                    if let Some(a) = action {
                        // "The keyword doesn't support assignment"
                        return (out, Some(PragmaError::Unrecognized { token: a.to_string() }));
                    }
                    if !previous_token.is_empty() {
                        // "The keyword is unknown"
                        return (out, Some(PragmaError::Unrecognized { token: previous_token }));
                    }
                    // "Missing keyword before assignment"
                    return (out, Some(PragmaError::Invalid { token: String::new() }));
                }
                assignment_required = false;
            }
            _ if assignment_required => {
                // "The = sign is missing after the keyword"
                return (
                    out,
                    Some(PragmaError::Invalid { token: action.unwrap_or("").to_string() }),
                );
            }
            TokKind::Keyword => {
                if let Some(a) = action {
                    if let Some(e) = flush(a, std::mem::take(&mut messages), &mut out) {
                        return (out, Some(e));
                    }
                }
                action = Some(KEYWORDS_BY_LEN.iter().find(|k| **k == value).unwrap());
                messages = Vec::new();
                assignment_required = MESSAGE_KEYWORDS.contains(&value.as_str());
            }
            TokKind::MessageString | TokKind::MessageNumber => {
                messages.push(value.clone());
                assignment_required = false;
            }
        }
        previous_token = value;
    }

    if let Some(a) = action {
        if let Some(e) = flush(a, messages, &mut out) {
            return (out, Some(e));
        }
        (out, None)
    } else {
        (out, Some(PragmaError::Unrecognized { token: previous_token }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Differential-tested against the pinned pylint's OPTION_PO
    /// (`.venv-pylint`, pylint 4.0.5).
    #[test]
    fn option_po_matches_python_re() {
        let cases: &[(&str, Option<&str>)] = &[
            ("# pylint: disable=E0602", Some("disable=E0602")),
            ("#pylint:bouboule=1", Some("bouboule=1")),
            ("# pylint:bouboule=1", Some("bouboule=1")),
            ("# foo # pylint: disable=bar", Some("disable=bar")),
            ("## pylint: disable=foo", Some("disable=foo")),
            ("# pylint: disable=x # pylint: enable=y", Some("enable=y")),
            ("# pylint: disable=unused-import ; reason", Some("disable=unused-import ")),
            (
                "# pylint: disable=a-removed-option, bad-option-value # [unknown-option-value]",
                Some("disable=a-removed-option, bad-option-value "),
            ),
            ("# pylint:   ", Some(" ")),
            ("# pylint:", None),
            ("# nopylint: disable=X", None),
            ("# comment # pylint: disable=X", Some("disable=X")),
            ("# pylint: disable=x#comment", Some("disable=x")),
            ("#: pylint: disable=q", Some("disable=q")),
            ("# pylint: ;x", Some(" ")),
            ("# pylint: ; pylint: disable=ok", Some(" ")),
            ("# see pylint: for details # pylint: disable=z", Some("disable=z")),
            ("# pylint: disable=first # then pylint: disable=second", Some("disable=second")),
            ("# x pylint: disable=a # pylint:", Some("disable=a ")),
            ("#", None),
            ("# -*- pylint: disable=W0212 -*-", Some("disable=W0212 -*-")),
        ];
        for (content, want) in cases {
            let got = option_po_search(content);
            assert_eq!(got.as_deref(), *want, "content: {content:?}");
        }
    }

    /// Verified error matrix from notes/03 §5.4 (empirical, pinned build).
    #[test]
    fn parse_pragma_matrix() {
        let p = |s: &str| parse_pragma(s);
        let (r, e) = p("disable=E0602");
        assert!(e.is_none());
        assert_eq!(r[0].action, "disable");
        assert_eq!(r[0].messages, vec!["E0602"]);

        let (r, e) = p("disable = missing-docstring, C0103");
        assert!(e.is_none());
        assert_eq!(r[0].messages, vec!["missing-docstring", "C0103"]);

        let (r, e) = p("skip-file");
        assert!(e.is_none());
        assert_eq!(r[0].action, "skip-file");

        let (r, e) = p("disable=E");
        assert!(e.is_none());
        assert_eq!(r[0].messages, vec!["E"]);

        let (_, e) = p("disable");
        assert!(matches!(e, Some(PragmaError::Invalid { ref token }) if token == "disable"));

        let (_, e) = p("disable=");
        assert!(matches!(e, Some(PragmaError::Invalid { ref token }) if token == "disable"));

        let (_, e) = p("skip-file=foo");
        assert!(matches!(e, Some(PragmaError::Unrecognized { ref token }) if token == "skip-file"));

        let (_, e) = p("foo=bar");
        assert!(matches!(e, Some(PragmaError::Unrecognized { ref token }) if token == "foo"));

        let (_, e) = p("foobar");
        assert!(matches!(e, Some(PragmaError::Unrecognized { ref token }) if token == "foobar"));

        let (_, e) = p("=foo");
        assert!(matches!(e, Some(PragmaError::Invalid { ref token }) if token.is_empty()));

        let (_, e) = p("disable foo");
        assert!(matches!(e, Some(PragmaError::Invalid { ref token }) if token == "disable"));

        // `x` matches no token -> messages empty when `enable` flushes
        let (r, e) = p("disable=x enable=y");
        assert!(r.is_empty());
        assert!(matches!(e, Some(PragmaError::Invalid { ref token }) if token == "disable"));

        let (r, e) = p("disable=C0103 enable=E0602");
        assert!(e.is_none());
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].action, "enable");

        // bare token only: UnRecognizedOptionError with token "" when the
        // payload had no tokens at all
        let (_, e) = p(" ");
        assert!(matches!(e, Some(PragmaError::Unrecognized { ref token }) if token.is_empty()));
    }
}
