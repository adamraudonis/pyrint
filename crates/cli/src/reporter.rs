//! TextReporter port (pylint/reporters/text.py): module headers printed once
//! per module NAME (global set), default line template, streamed to stdout
//! through a buffer.

use std::io::Write;

use rustc_hash::FxHashSet;

/// MSG_TYPES_STATUS (pylint/constants.py:43).
pub fn status_bit(category: char) -> i32 {
    match category {
        'I' => 0,
        'C' => 16,
        'R' => 8,
        'W' => 4,
        'E' => 2,
        'F' => 1,
        _ => 0,
    }
}

/// One displayed message, fully formatted.
pub struct OutMsg {
    /// msg.module (FileItem.name for node-less messages)
    pub module: String,
    /// cwd-stripped path
    pub path: String,
    /// already `line or 1`
    pub line: i64,
    /// already `col_offset or 0`
    pub col: i64,
    pub msgid: &'static str,
    pub symbol: &'static str,
    pub text: String,
}

pub struct Reporter<W: Write> {
    out: W,
    modules_seen: FxHashSet<String>,
    pub msg_status: i32,
}

impl<W: Write> Reporter<W> {
    pub fn new(out: W) -> Self {
        Reporter { out, modules_seen: FxHashSet::default(), msg_status: 0 }
    }

    /// TextReporter.handle_message (text.py:156-161).
    pub fn handle(&mut self, m: &OutMsg) {
        if !self.modules_seen.contains(&m.module) {
            // make_header (text.py:105-106): 13 asterisks
            let _ = writeln!(self.out, "************* Module {}", m.module);
            self.modules_seen.insert(m.module.clone());
        }
        // line_format = "{path}:{line}:{column}: {msg_id}: {msg} ({symbol})"
        let _ = writeln!(
            self.out,
            "{}:{}:{}: {}: {} ({})",
            m.path, m.line, m.col, m.msgid, m.text, m.symbol
        );
        self.msg_status |= status_bit(m.msgid.chars().next().unwrap_or('I'));
    }

    pub fn flush(&mut self) {
        let _ = self.out.flush();
    }
}
