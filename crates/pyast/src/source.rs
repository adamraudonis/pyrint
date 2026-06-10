//! Source file reading with astroid's decoding semantics
//! (BOM + PEP 263 coding cookie detection, mirroring
//! `astroid.builder.open_source_file` / Python's `tokenize.detect_encoding`).

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
