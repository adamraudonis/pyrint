//! Port of pylint/checkers/unicode.py (UnicodeChecker, raw-bytes checker):
//! E2501 invalid-unicode-codec, E2502 bidirectional-unicode,
//! C2503 bad-file-encoding (out of -E scope but still emitted through the
//! normal suppression machinery), E2510-E2515 invalid-character-*.
//!
//! Bug-for-bug quirks preserved:
//! - `_map_positions_to_result` uses `while pos > 0` (unicode.py:182): a bad
//!   char whose FIRST occurrence on a line is at column 0 is never reported
//!   (and neither are later occurrences of that same char on that line).
//! - reported col_offset is the 0-based column PLUS ONE (unicode.py:488).
//! - only the final `\r` of a `\r\n` line ending is exempted (unicode.py:175).
//! - the per-line result dict is keyed by column: a second bad char at the
//!   same column overwrites the first IN PLACE (insertion order kept).
//! - one E2502 per line max (break after first BIDI hit, unicode.py:516).

use pyast::codecs_gen::{self, CodecKind};
use pyast::source::detect_encoding_lines;

/// BAD_CHARS (unicode.py:80-137), in table order (drives emission order).
/// (unescaped char, msgid)
const BAD_CHARS: &[(char, &str)] = &[
    ('\u{8}', "E2510"),  // backspace
    ('\r', "E2511"),     // carriage-return
    ('\u{1a}', "E2512"), // sub
    ('\u{1b}', "E2513"), // esc
    ('\0', "E2514"),     // nul
    ('\u{200b}', "E2515"), // zero-width-space
];

/// BIDI_UNICODE (unicode.py:37-55). U+200E deliberately excluded.
const BIDI_UNICODE: &[char] = &[
    '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}',
    '\u{2068}', '\u{2069}', '\u{200f}',
];

/// A message emitted by the raw checker. `col` is already pylint's reported
/// column (the +1 quirk applied for E251x; 0 for E2501/E2502/C2503).
pub struct UnicodeMsg {
    pub msgid: &'static str,
    pub line: u32,
    pub col: u32,
}

/// `_normalize_codec_name` (unicode.py:213-215):
/// re.sub("utf[ -]?(8|16|32)[ -]?(le|be|)?(sig)?", r"utf-\1\2", codec, IGNORECASE).lower()
fn normalize_codec_name(codec: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?i)utf[ -]?(8|16|32)[ -]?(le|be|)?(?:sig)?").unwrap()
    });
    re.replace_all(codec, "utf-${1}${2}").to_lowercase()
}

/// UNICODE_BOMS (unicode.py:193-201). codecs.BOM_UTF16/BOM_UTF32 are the
/// native-endian (little-endian on the pinned platforms) variants.
fn bom_for(codec: &str) -> Option<&'static [u8]> {
    match codec {
        "utf-8" => Some(b"\xef\xbb\xbf"),
        "utf-16" | "utf-16le" => Some(b"\xff\xfe"),
        "utf-16be" => Some(b"\xfe\xff"),
        "utf-32" | "utf-32le" => Some(b"\xff\xfe\x00\x00"),
        "utf-32be" => Some(b"\x00\x00\xfe\xff"),
        _ => None,
    }
}

/// `extract_codec_from_bom` (unicode.py:279-297): BOM_SORTED_TO_CODEC order
/// (utf-32le, utf-32be, utf-8, utf-16le, utf-16be — longest first).
fn extract_codec_from_bom(first_line: &[u8]) -> Option<&'static str> {
    const ORDER: &[(&[u8], &str)] = &[
        (b"\xff\xfe\x00\x00", "utf-32le"),
        (b"\x00\x00\xfe\xff", "utf-32be"),
        (b"\xef\xbb\xbf", "utf-8"),
        (b"\xff\xfe", "utf-16le"),
        (b"\xfe\xff", "utf-16be"),
    ];
    ORDER
        .iter()
        .find(|(bom, _)| first_line.starts_with(bom))
        .map(|(_, c)| *c)
}

fn remove_bom<'a>(encoded: &'a [u8], codec: &str) -> &'a [u8] {
    if let Some(bom) = bom_for(codec) {
        if encoded.starts_with(bom) {
            return &encoded[bom.len()..];
        }
    }
    encoded
}

/// `_encode_without_bom` for the codecs we can encode natively (utf family;
/// all the search chars are BMP). None for codecs we cannot encode — pylint
/// would raise UnicodeEncodeError there (an unhandled crash path) but it is
/// unreachable for files astroid already decoded.
fn encode_without_bom(c: char, codec: &str) -> Option<Vec<u8>> {
    let cp = c as u32;
    if codec == "utf-8" {
        let mut buf = [0u8; 4];
        return Some(c.encode_utf8(&mut buf).as_bytes().to_vec());
    }
    if codec.starts_with("utf-16") {
        debug_assert!(cp < 0x10000);
        let b = (cp as u16).to_le_bytes();
        return Some(if codec == "utf-16be" {
            vec![b[1], b[0]]
        } else {
            vec![b[0], b[1]]
        });
    }
    if codec.starts_with("utf-32") {
        let b = cp.to_le_bytes();
        return Some(if codec == "utf-32be" {
            vec![b[3], b[2], b[1], b[0]]
        } else {
            b.to_vec()
        });
    }
    // single-byte codecs: the BAD_CHARS below U+0080 map to themselves in
    // every codec in the corpus; U+200B is not encodable.
    if cp < 0x80 {
        return Some(vec![cp as u8]);
    }
    None
}

fn byte_to_str_length(codec: &str) -> usize {
    if codec.starts_with("utf-32") {
        4
    } else if codec.starts_with("utf-16") {
        2
    } else {
        1
    }
}

/// Decode a raw line strictly with the (normalized) codec, for char-accurate
/// columns. Falls back to Err for undecodable/unsupported codecs (pylint's
/// UnicodeDecodeError byte-search path).
fn decode_strict(line: &[u8], codec: &str) -> Result<Vec<char>, ()> {
    if codec == "utf-8" {
        return std::str::from_utf8(line)
            .map(|s| s.chars().collect())
            .map_err(|_| ());
    }
    let entry = codecs_gen::lookup(&codecs_gen::normalize_encoding(codec)).ok_or(())?;
    match entry.kind {
        CodecKind::Utf8 => std::str::from_utf8(line)
            .map(|s| s.chars().collect())
            .map_err(|_| ()),
        CodecKind::Ascii => {
            if line.is_ascii() {
                Ok(line.iter().map(|&b| b as char).collect())
            } else {
                Err(())
            }
        }
        CodecKind::Latin1 => Ok(line.iter().map(|&b| b as char).collect()),
        CodecKind::Table(table) => {
            let mut out = Vec::with_capacity(line.len());
            for &b in line {
                let v = table[b as usize];
                if v == 0xFFFE {
                    return Err(());
                }
                out.push(char::from_u32(v).ok_or(())?);
            }
            Ok(out)
        }
        _ => Err(()),
    }
}

/// `_map_positions_to_result` (unicode.py:156-190) — str variant.
/// Returns (col, msgid) in dict insertion order with same-col overwrite.
fn map_positions_str(chars: &[char]) -> Vec<(usize, &'static str)> {
    let mut result: Vec<(usize, &'static str)> = Vec::new();
    for &(ch, msgid) in BAD_CHARS {
        if !chars.contains(&ch) {
            continue;
        }
        // Special handling for Windows '\r\n' (new_line == "\n")
        let ignore_pos: Option<usize> = if ch == '\r' && chars.last() == Some(&'\n') {
            chars.len().checked_sub(2)
        } else {
            None
        };
        // start = 0; pos = line.find(...); while pos > 0: ...
        let mut start = 0usize;
        let mut pos = chars[start..].iter().position(|&c| c == ch).map(|p| p + start);
        while let Some(p) = pos {
            if p == 0 {
                break; // `while pos > 0` — col-0 hit aborts this char entirely
            }
            if Some(p) != ignore_pos {
                match result.iter_mut().find(|(c, _)| *c == p) {
                    Some(slot) => slot.1 = msgid, // dict overwrite keeps position
                    None => result.push((p, msgid)),
                }
            }
            start = p + 1;
            pos = if start < chars.len() {
                chars[start..].iter().position(|&c| c == ch).map(|q| q + start)
            } else {
                None
            };
        }
    }
    result
}

/// Byte-search fallback (`_find_line_matches` except branch, unicode.py:398-416).
fn map_positions_bytes(line: &[u8], codec: &str) -> Vec<(usize, &'static str)> {
    let new_line = match encode_without_bom('\n', codec) {
        Some(n) => n,
        None => return Vec::new(),
    };
    let bsl = byte_to_str_length(codec);
    let mut result: Vec<(usize, &'static str)> = Vec::new();
    for &(ch, msgid) in BAD_CHARS {
        let Some(needle) = encode_without_bom(ch, codec) else { continue };
        let find = |from: usize| -> Option<usize> {
            if from > line.len() {
                return None;
            }
            line[from..]
                .windows(needle.len())
                .position(|w| w == needle.as_slice())
                .map(|p| p + from)
        };
        if find(0).is_none() {
            continue;
        }
        let ignore_pos: Option<usize> = if ch == '\r' && line.ends_with(new_line.as_slice()) {
            line.len().checked_sub(2 * bsl)
        } else {
            None
        };
        let mut pos = find(0);
        while let Some(p) = pos {
            if p == 0 {
                break;
            }
            if Some(p) != ignore_pos {
                let col = p / bsl;
                match result.iter_mut().find(|(c, _)| *c == col) {
                    Some(slot) => slot.1 = msgid,
                    None => result.push((col, msgid)),
                }
            }
            pos = find(p + 1);
        }
    }
    result
}

/// Split the byte stream into lines like Python's binary readlines
/// (split on b'\n', newline kept), or — for utf-16/32 — on the encoded
/// newline (`_fix_utf16_32_line_stream`, unicode.py:249-276).
fn line_stream<'a>(bytes: &'a [u8], codec: &str) -> Vec<&'a [u8]> {
    let sep: Vec<u8> = if codec.starts_with("utf-16") || codec.starts_with("utf-32") {
        encode_without_bom('\n', codec).unwrap_or_else(|| vec![b'\n'])
    } else {
        vec![b'\n']
    };
    let mut out = Vec::new();
    let mut start = 0usize;
    while start <= bytes.len().saturating_sub(1) {
        let pos = bytes[start..]
            .windows(sep.len())
            .position(|w| w == sep.as_slice())
            .map(|p| p + start);
        match pos {
            Some(p) => {
                out.push(&bytes[start..p + sep.len()]);
                start = p + sep.len();
            }
            None => {
                if start < bytes.len() {
                    out.push(&bytes[start..]);
                }
                break;
            }
        }
    }
    out
}

/// `UnicodeChecker.process_module` (unicode.py:518-533).
pub fn process_module(bytes: &[u8], emit: &mut dyn FnMut(UnicodeMsg)) {
    // _determine_codec (unicode.py:419-458)
    let (name, codec_line) = match detect_encoding_lines(bytes, "") {
        Ok(v) => v,
        Err(_) => {
            // SyntaxError fallback: check for UTF-16/32 BOMs manually
            let mut pos = 0usize;
            let first = read_first_line(bytes, &mut pos);
            match extract_codec_from_bom(first) {
                Some(c) => (c.to_string(), 1),
                None => return, // re-raise: unreachable (astroid decode succeeded)
            }
        }
    };
    let codec = normalize_codec_name(&name);

    // _check_codec (unicode.py:460-475)
    if codec != "utf-8" {
        let msgid = if codec.starts_with("utf-16") || codec.starts_with("utf-32") {
            "E2501"
        } else {
            "C2503"
        };
        emit(UnicodeMsg { msgid, line: codec_line, col: 0 });
    }

    for (idx, raw_line) in line_stream(bytes, &codec).into_iter().enumerate() {
        let lineno = idx as u32 + 1;
        let line: &[u8] = if lineno == 1 {
            remove_bom(raw_line, &codec)
        } else {
            raw_line
        };
        // _check_bidi_chars (unicode.py:492-516)
        if codec.starts_with("utf") {
            for &d in BIDI_UNICODE {
                if let Some(needle) = encode_without_bom(d, &codec) {
                    let found = line
                        .windows(needle.len())
                        .any(|w| w == needle.as_slice());
                    if found {
                        emit(UnicodeMsg { msgid: "E2502", line: lineno, col: 0 });
                        break; // once per line
                    }
                }
            }
        }
        // _check_invalid_chars (unicode.py:477-490) via _find_line_matches
        let matches = match decode_strict(line, &codec) {
            Ok(chars) => map_positions_str(&chars),
            Err(()) => map_positions_bytes(line, &codec),
        };
        for (col, msgid) in matches {
            emit(UnicodeMsg {
                msgid,
                line: lineno,
                col: col as u32 + 1, // the intentional +1 (unicode.py:488)
            });
        }
    }
}

fn read_first_line<'a>(bytes: &'a [u8], pos: &mut usize) -> &'a [u8] {
    let start = *pos;
    let end = match bytes[start..].iter().position(|&b| b == b'\n') {
        Some(i) => start + i + 1,
        None => bytes.len(),
    };
    *pos = end;
    &bytes[start..end]
}
