//! Source file reading with astroid's decoding semantics
//! (BOM + PEP 263 coding cookie detection, mirroring
//! `astroid.builder.open_source_file` / Python's `tokenize.detect_encoding`).

use crate::codecs_gen::{lookup, normalize_encoding, CodecEntry, CodecKind};
use crate::pyrepr::repr_str;

/// A decoded source file.
pub struct SourceFile {
    /// Decoded text (what CPython's parser would see).
    pub text: String,
    /// The encoding that was detected (lowercased), e.g. "utf-8".
    pub encoding: String,
    /// Byte offset of each line start in `text` (UTF-8 bytes).
    pub line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn from_text(text: String, encoding: String) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        SourceFile {
            text,
            encoding,
            line_starts,
        }
    }

    /// 1-based line, 0-based byte column for a byte offset (CPython style).
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        (line_idx as u32 + 1, offset - self.line_starts[line_idx])
    }
}

/// How decoding a source file can fail, mirroring the exception mapping in
/// astroid.builder.AstroidBuilder.file_build (astroid/builder.py:117-139).
pub enum DecodeError {
    /// tokenize.detect_encoding raised SyntaxError -> astroid
    /// AstroidSyntaxError; the SyntaxError has lineno/offset = None.
    Syntax(String),
    /// open() raised LookupError for a non-text codec -> also
    /// AstroidSyntaxError (builder.py:127), but LookupError has no
    /// lineno/offset attributes at all.
    Lookup(String),
    /// Decoding with the detected encoding raised UnicodeError ->
    /// astroid AstroidBuildingError ("Wrong or no encoding specified").
    Unicode,
}

const BOM_UTF8: &[u8] = b"\xef\xbb\xbf";

/// One binary-mode readline: everything up to and including the first b'\n'.
fn readline<'a>(bytes: &'a [u8], pos: &mut usize) -> &'a [u8] {
    let start = *pos;
    let end = match memchr::memchr(b'\n', &bytes[start..]) {
        Some(i) => start + i + 1,
        None => bytes.len(),
    };
    *pos = end;
    &bytes[start..end]
}

/// tokenize.blank_re: `^[ \t\f]*(?:[#\r\n]|$)` over the raw line bytes.
fn is_blank_line(line: &[u8]) -> bool {
    let mut i = 0;
    while i < line.len() && matches!(line[i], b' ' | b'\t' | b'\x0c') {
        i += 1;
    }
    i == line.len() || matches!(line[i], b'#' | b'\r' | b'\n')
}

/// tokenize.cookie_re: `^[ \t\f]*#.*?coding[:=][ \t]*([-\w.]+)` (re.ASCII).
/// Returns the raw charset group. `.` never matches '\n', so the cookie must
/// start before the line's first newline byte.
fn find_cookie_charset(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches([' ', '\t', '\x0c']);
    let rest = rest.strip_prefix('#')?;
    let limit = rest.find('\n').unwrap_or(rest.len());
    let mut start = 0;
    while let Some(i) = rest[start..limit].find("coding") {
        let i = start + i;
        start = i + 1; // regex backtracking tries every later occurrence
        let after = &rest[i + 6..];
        let mut chars = after.chars();
        if !matches!(chars.next(), Some(':' | '=')) {
            continue;
        }
        let after = chars.as_str().trim_start_matches([' ', '\t']);
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
            .unwrap_or(after.len());
        if end > 0 {
            return Some(&after[..end]);
        }
    }
    None
}

/// tokenize._get_normal_name: only the first 12 chars matter for the
/// utf-8 / latin-1 family check; otherwise the ORIGINAL spelling is kept.
fn get_normal_name(orig: &str) -> String {
    let enc: String = orig
        .chars()
        .take(12)
        .flat_map(char::to_lowercase)
        .map(|c| if c == '_' { '-' } else { c })
        .collect();
    if enc == "utf-8" || enc.starts_with("utf-8-") {
        return "utf-8".to_string();
    }
    if matches!(enc.as_str(), "latin-1" | "iso-8859-1" | "iso-latin-1")
        || enc.starts_with("latin-1-")
        || enc.starts_with("iso-8859-1-")
        || enc.starts_with("iso-latin-1-")
    {
        return "iso-8859-1".to_string();
    }
    orig.to_string()
}

struct Cookie {
    entry: &'static CodecEntry,
    /// What detect_encoding returns: the _get_normal_name spelling.
    name: String,
}

/// tokenize.detect_encoding's find_cookie. `filename` is the (absolute) path
/// that appears in the SyntaxError messages.
fn find_cookie(
    line: &[u8],
    filename: &str,
    bom_found: bool,
) -> Result<Option<Cookie>, DecodeError> {
    let line_str = std::str::from_utf8(line).map_err(|_| {
        DecodeError::Syntax(format!(
            "invalid or missing encoding declaration for {}",
            repr_str(filename)
        ))
    })?;
    let Some(charset) = find_cookie_charset(line_str) else {
        return Ok(None);
    };
    let mut name = get_normal_name(charset);
    let Some(entry) = lookup(&normalize_encoding(&name)) else {
        return Err(DecodeError::Syntax(format!(
            "unknown encoding for {}: {}",
            repr_str(filename),
            name
        )));
    };
    if bom_found {
        if name != "utf-8" {
            return Err(DecodeError::Syntax(format!(
                "encoding problem for {}: utf-8",
                repr_str(filename)
            )));
        }
        name.push_str("-sig");
    }
    Ok(Some(Cookie { entry, name }))
}

/// Decode raw file bytes exactly as astroid.builder.open_source_file does:
/// tokenize.detect_encoding over the first two lines, then a text-mode read
/// with that encoding and universal newlines. `filename` should be the
/// absolute path (os.path.abspath via modutils.get_source_file) so error
/// messages match astroid's.
pub fn decode_source(bytes: &[u8], filename: &str) -> Result<SourceFile, DecodeError> {
    let mut pos = 0usize;
    let mut first = readline(bytes, &mut pos);
    let mut bom_found = false;
    if first.starts_with(BOM_UTF8) {
        bom_found = true;
        first = &first[3..];
    }
    let default = || Cookie {
        entry: lookup("utf_8").expect("utf-8 codec in registry"),
        name: if bom_found { "utf-8-sig" } else { "utf-8" }.to_string(),
    };
    let cookie = if first.is_empty() {
        default()
    } else if let Some(c) = find_cookie(first, filename, bom_found)? {
        c
    } else if !is_blank_line(first) {
        default()
    } else {
        let second = readline(bytes, &mut pos);
        if second.is_empty() {
            default()
        } else if let Some(c) = find_cookie(second, filename, bom_found)? {
            c
        } else {
            default()
        }
    };

    // open(filename, newline=None, encoding=encoding): utf-8-sig strips a
    // leading BOM; universal newlines translate \r\n and \r to \n.
    let data = if cookie.name.ends_with("-sig") && bytes.starts_with(BOM_UTF8) {
        &bytes[3..]
    } else {
        bytes
    };
    let text = match cookie.entry.kind {
        CodecKind::Utf8 => std::str::from_utf8(data)
            .map_err(|_| DecodeError::Unicode)?
            .to_string(),
        CodecKind::Ascii => {
            if !data.is_ascii() {
                return Err(DecodeError::Unicode);
            }
            std::str::from_utf8(data)
                .map_err(|_| DecodeError::Unicode)?
                .to_string()
        }
        CodecKind::Latin1 => data.iter().map(|&b| b as char).collect(),
        CodecKind::Table(table) => {
            let mut s = String::with_capacity(data.len());
            for &b in data {
                let v = table[b as usize];
                if v == 0xFFFE {
                    return Err(DecodeError::Unicode);
                }
                s.push(char::from_u32(v).ok_or(DecodeError::Unicode)?);
            }
            s
        }
        // Real text codec we do not decode natively (utf-16, CJK multi-byte,
        // ...). No corpus file uses these; fall back to latin-1 so a tree is
        // still produced (astroid would parse the properly decoded text).
        CodecKind::OtherText => data.iter().map(|&b| b as char).collect(),
        // encodings.undefined raises UnicodeError on any decode.
        CodecKind::Undefined => return Err(DecodeError::Unicode),
        // bytes<->bytes codec: TextIOWrapper raises LookupError, which
        // astroid maps to AstroidSyntaxError (builder.py:127).
        CodecKind::NotText => {
            return Err(DecodeError::Lookup(format!(
                "{} is not a text encoding; use codecs.open() to handle arbitrary codecs",
                repr_str(&cookie.name)
            )))
        }
    };
    let text = if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    };
    Ok(SourceFile::from_text(text, cookie.name))
}
