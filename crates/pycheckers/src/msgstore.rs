//! Message lookup tables: the Rust counterpart of pylint's
//! `MessageIdStore.get_active_msgids` (message_id_store.py:121-160), the
//! deleted/moved-message tables (`pylint/message/_deleted_message_ids.py`)
//! and `_MessageStateHandler._get_messages_to_set`
//! (message_state_handler.py:82-140).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::msgs::{MessageDef, MESSAGES};

pub type MsgIdx = u16;

pub fn def(i: MsgIdx) -> &'static MessageDef {
    &MESSAGES[i as usize]
}

pub struct Store {
    pub by_msgid: HashMap<&'static str, MsgIdx>,
    pub by_symbol: HashMap<&'static str, MsgIdx>,
    /// old msgid -> indexes of the NEW messages carrying it
    /// (MessageIdStore.__old_names; e.g. E0012 -> [W0012, R0022])
    pub old_msgid_to_new: HashMap<&'static str, Vec<MsgIdx>>,
    /// old msgid registered into __msgid_to_symbol (maps to the OLD symbol)
    pub old_msgid_to_symbol: HashMap<&'static str, &'static str>,
    /// old symbol registered into __symbol_to_msgid (maps to the OLD msgid)
    pub old_symbol_to_msgid: HashMap<&'static str, &'static str>,
}

pub fn store() -> &'static Store {
    static STORE: OnceLock<Store> = OnceLock::new();
    STORE.get_or_init(|| {
        let mut s = Store {
            by_msgid: HashMap::new(),
            by_symbol: HashMap::new(),
            old_msgid_to_new: HashMap::new(),
            old_msgid_to_symbol: HashMap::new(),
            old_symbol_to_msgid: HashMap::new(),
        };
        for (i, m) in MESSAGES.iter().enumerate() {
            let i = i as MsgIdx;
            s.by_msgid.insert(m.msgid, i);
            s.by_symbol.insert(m.symbol, i);
            for (old_id, old_sym) in m.old_names {
                s.old_msgid_to_new.entry(old_id).or_default().push(i);
                s.old_msgid_to_symbol.insert(old_id, old_sym);
                s.old_symbol_to_msgid.insert(old_sym, old_id);
            }
        }
        s
    })
}

/// Resolution failure from `get_active_msgids`.
#[derive(Debug, Clone)]
pub enum ResolveError {
    /// DeletedMessageError -- payload is str(exception):
    /// `'{token}' was removed from pylint, see {url}.`
    Deleted(String),
    /// MessageBecameExtensionError -- payload is str(exception):
    /// `'{token}' was moved to an optional extension, see {url}.`
    Moved(String),
    /// UnknownMessageError
    Unknown,
}

// pylint/message/_deleted_message_ids.py — DELETED_MESSAGES_IDS flattened
// (msgid OR symbol -> removal-explanation URL), old_names included.
const PR4942: &str = "https://github.com/pylint-dev/pylint/pull/4942";
const PR3578: &str = "https://github.com/pylint-dev/pylint/pull/3578";
const PR3577: &str = "https://github.com/pylint-dev/pylint/pull/3577";
const PR3571: &str = "https://github.com/pylint-dev/pylint/pull/3571";
const WN143: &str =
    "https://pylint.readthedocs.io/en/latest/whatsnew/1/1.4.html#what-s-new-in-pylint-1-4-3";
const ISS2409: &str = "https://github.com/pylint-dev/pylint/issues/2409";
const PR6421: &str = "https://github.com/pylint-dev/pylint/pull/6421";

static DELETED: &[(&str, &str, &str)] = &[
    // (msgid, symbol, url); old_names appear as extra rows with the same url
    ("W1601", "apply-builtin", PR4942),
    ("E1601", "print-statement", PR4942),
    ("E1602", "parameter-unpacking", PR4942),
    ("E1603", "unpacking-in-except", PR4942),
    ("W0712", "old-unpacking-in-except", PR4942),
    ("E1604", "old-raise-syntax", PR4942),
    ("W0121", "old-old-raise-syntax", PR4942),
    ("E1605", "backtick", PR4942),
    ("W0333", "old-backtick", PR4942),
    ("E1609", "import-star-module-level", PR4942),
    ("W1602", "basestring-builtin", PR4942),
    ("W1603", "buffer-builtin", PR4942),
    ("W1604", "cmp-builtin", PR4942),
    ("W1605", "coerce-builtin", PR4942),
    ("W1606", "execfile-builtin", PR4942),
    ("W1607", "file-builtin", PR4942),
    ("W1608", "long-builtin", PR4942),
    ("W1609", "raw_input-builtin", PR4942),
    ("W1610", "reduce-builtin", PR4942),
    ("W1611", "standarderror-builtin", PR4942),
    ("W1612", "unicode-builtin", PR4942),
    ("W1613", "xrange-builtin", PR4942),
    ("W1614", "coerce-method", PR4942),
    ("W1615", "delslice-method", PR4942),
    ("W1616", "getslice-method", PR4942),
    ("W1617", "setslice-method", PR4942),
    ("W1618", "no-absolute-import", PR4942),
    ("W1619", "old-division", PR4942),
    ("W1620", "dict-iter-method", PR4942),
    ("W1621", "dict-view-method", PR4942),
    ("W1622", "next-method-called", PR4942),
    ("W1623", "metaclass-assignment", PR4942),
    ("W1624", "indexing-exception", PR4942),
    ("W0713", "old-indexing-exception", PR4942),
    ("W1625", "raising-string", PR4942),
    ("W0701", "old-raising-string", PR4942),
    ("W1626", "reload-builtin", PR4942),
    ("W1627", "oct-method", PR4942),
    ("W1628", "hex-method", PR4942),
    ("W1629", "nonzero-method", PR4942),
    ("W1630", "cmp-method", PR4942),
    ("W1632", "input-builtin", PR4942),
    ("W1633", "round-builtin", PR4942),
    ("W1634", "intern-builtin", PR4942),
    ("W1635", "unichr-builtin", PR4942),
    ("W1636", "map-builtin-not-iterating", PR4942),
    ("W1631", "implicit-map-evaluation", PR4942),
    ("W1637", "zip-builtin-not-iterating", PR4942),
    ("W1638", "range-builtin-not-iterating", PR4942),
    ("W1639", "filter-builtin-not-iterating", PR4942),
    ("W1640", "using-cmp-argument", PR4942),
    ("W1642", "div-method", PR4942),
    ("W1643", "idiv-method", PR4942),
    ("W1644", "rdiv-method", PR4942),
    ("W1645", "exception-message-attribute", PR4942),
    ("W1646", "invalid-str-codec", PR4942),
    ("W1647", "sys-max-int", PR4942),
    ("W1648", "bad-python3-import", PR4942),
    ("W1649", "deprecated-string-function", PR4942),
    ("W1650", "deprecated-str-translate-call", PR4942),
    ("W1651", "deprecated-itertools-function", PR4942),
    ("W1652", "deprecated-types-field", PR4942),
    ("W1653", "next-method-defined", PR4942),
    ("W1654", "dict-items-not-iterating", PR4942),
    ("W1655", "dict-keys-not-iterating", PR4942),
    ("W1656", "dict-values-not-iterating", PR4942),
    ("W1657", "deprecated-operator-function", PR4942),
    ("W1658", "deprecated-urllib-function", PR4942),
    ("W1659", "xreadlines-attribute", PR4942),
    ("W1660", "deprecated-sys-function", PR4942),
    ("W1661", "exception-escape", PR4942),
    ("W1662", "comprehension-escape", PR4942),
    ("W0312", "mixed-indentation", PR3578),
    ("C0326", "bad-whitespace", PR3577),
    ("C0323", "no-space-after-operator", PR3577),
    ("C0324", "no-space-after-comma", PR3577),
    ("C0322", "no-space-before-operator", PR3577),
    ("C0330", "bad-continuation", PR3571),
    ("R0921", "abstract-class-not-used", WN143),
    ("R0922", "abstract-class-little-used", WN143),
    ("W0142", "star-args", WN143),
    ("W0232", "no-init", ISS2409),
    ("W0111", "assign-to-new-keyword", PR6421),
];

static MOVED: &[(&str, &str, &str)] = &[(
    "R0201",
    "no-self-use",
    "https://pylint.readthedocs.io/en/latest/whatsnew/2/2.14/summary.html#removed-checkers",
)];

fn is_deleted_msgid(msgid: &str) -> Option<&'static str> {
    DELETED.iter().find(|(m, _, _)| *m == msgid).map(|(_, _, u)| *u)
}
fn is_deleted_symbol(symbol: &str) -> Option<&'static str> {
    DELETED.iter().find(|(_, s, _)| *s == symbol).map(|(_, _, u)| *u)
}
fn is_moved_msgid(msgid: &str) -> Option<&'static str> {
    MOVED.iter().find(|(m, _, _)| *m == msgid).map(|(_, _, u)| *u)
}
fn is_moved_symbol(symbol: &str) -> Option<&'static str> {
    MOVED.iter().find(|(_, s, _)| *s == symbol).map(|(_, _, u)| *u)
}

/// `MessageIdStore.get_active_msgids` (message_id_store.py:121-160).
pub fn get_active_msgids(token: &str) -> Result<Vec<MsgIdx>, ResolveError> {
    let s = store();
    // "Only msgid can have a digit as second letter": token[1:].isdigit()
    // ("".isdigit() is False, so 1-char tokens take the symbol branch).
    let tail_digits = token.len() > 1 && token[1..].chars().all(|c| c.is_ascii_digit());
    let (msgid, found): (String, Option<MsgIdx>);
    let mut deletion: Option<&str> = None;
    let mut moved: Option<&str> = None;
    let mut old_hit: Option<&'static str> = None; // old msgid hit
    if tail_digits {
        msgid = token.to_uppercase();
        found = s.by_msgid.get(msgid.as_str()).copied();
        if found.is_none() {
            if s.old_msgid_to_symbol.contains_key(msgid.as_str()) {
                old_hit = s.old_msgid_to_symbol.get_key_value(msgid.as_str()).map(|(k, _)| *k);
            } else {
                deletion = is_deleted_msgid(&msgid);
                if deletion.is_none() {
                    moved = is_moved_msgid(&msgid);
                }
            }
        }
    } else {
        // symbol branch
        if let Some(&i) = s.by_symbol.get(token) {
            msgid = MESSAGES[i as usize].msgid.to_string();
            found = Some(i);
        } else if let Some(old_id) = s.old_symbol_to_msgid.get(token) {
            msgid = (*old_id).to_string();
            found = None;
            old_hit = Some(*old_id);
        } else {
            msgid = String::new();
            found = None;
            deletion = is_deleted_symbol(token);
            if deletion.is_none() {
                moved = is_moved_symbol(token);
            }
        }
    }
    if found.is_none() && old_hit.is_none() {
        if let Some(url) = deletion {
            return Err(ResolveError::Deleted(format!(
                "'{token}' was removed from pylint, see {url}."
            )));
        }
        if let Some(url) = moved {
            return Err(ResolveError::Moved(format!(
                "'{token}' was moved to an optional extension, see {url}."
            )));
        }
        return Err(ResolveError::Unknown);
    }
    // ids = self.__old_names.get(msgid, [msgid])
    if let Some(new_ids) = s.old_msgid_to_new.get(msgid.as_str()) {
        return Ok(new_ids.clone());
    }
    Ok(vec![found.expect("current msgid resolved")])
}

/// MSG_TYPES literal order (constants.py:33-40) — drives disable("all").
pub const MSG_CATEGORIES: &[char] = &['I', 'C', 'R', 'W', 'E', 'F'];

/// pylint's own default-enabled main-checker messages, exempt from
/// `disable=all` (message_state_handler.py:115-120 + default_enabled_messages).
pub const DEFAULT_ENABLED_MAIN: &[&str] = &[
    "F0001", "F0002", "F0010", "F0011", "E0001", "E0011", "W0012", "R0022", "E0013", "E0014",
    "E0015",
];

/// Disabled-by-default (W/C/R/I) msgids our ported checkers nevertheless
/// COMPUTE — an inline `# pylint: enable=` can resurrect these exactly.
/// Enables naming anything else (and not enabled under the flags) get a
/// stderr warning: the resurrected message would be a false negative.
pub const EMITTED_DISABLED_MSGIDS: &[&str] = &[
    // pragma machinery (cli)
    "I0010", "I0011", "I0013", "I0020", "I0021", "I0022", "R0022", "W0012",
    // basic / basic_error
    "W0101", "W0122", "W0123", "W0134", "W0136", "W0137",
    // variables
    "W0611", "W0614", "W0632", "W0642", "W0644",
    // exceptions
    "W0702", "W0705", "W0706", "W0707", "W0711", "W0715", "W0718", "W0719",
    // logging
    "W1201", "W1202", "W1203",
    // strings
    "W1300", "W1301", "W1302", "W1303", "W1304", "W1305", "W1306", "W1307",
    "W1308", "W1310",
    // stdlib
    "W1501", "W1503", "W1506", "W1507", "W1508", "W1509", "W1510", "W1514",
    "W1515", "W1518",
    // method_args / modified_iterating / match
    "W3101", "W4701", "R1906",
    // imports
    "C0411", "C0412", "C0413",
];

/// Registered checker name -> msgids (`linter._checkers`), dumped from the
/// pinned pylint 4.0.5 (`PyLinter.load_default_plugins`). Used by the
/// checker-name branch of `_get_messages_to_set`.
pub static CHECKER_MSGS: &[(&str, &[&str])] = &[
    ("main", &["E0001", "E0011", "E0013", "E0014", "E0015", "F0001", "F0002", "F0010", "F0011", "I0001", "I0010", "I0011", "I0013", "I0020", "I0021", "I0022", "R0022", "W0012"]),
    ("dataclass", &["E3701"]),
    ("logging", &["E1200", "E1201", "E1205", "E1206", "W1201", "W1202", "W1203"]),
    ("spelling", &["C0401", "C0402", "C0403"]),
    ("miscellaneous", &["I0023", "W0511"]),
    ("async", &["E1700", "E1701"]),
    ("unnecessary-dunder-call", &["C2801"]),
    ("similarities", &["R0801"]),
    ("typecheck", &["E1101", "E1102", "E1111", "E1120", "E1121", "E1123", "E1124", "E1125", "E1126", "E1127", "E1128", "E1129", "E1130", "E1131", "E1132", "E1133", "E1134", "E1135", "E1136", "E1137", "E1138", "E1139", "E1141", "E1142", "E1143", "E1144", "E1145", "I1101", "W1113", "W1114", "W1115", "W1116", "W1117"]),
    ("unicode_checker", &["C2503", "E2501", "E2502", "E2510", "E2511", "E2512", "E2513", "E2514", "E2515"]),
    ("modified_iteration", &["E4702", "E4703", "W4701"]),
    ("classes", &["C0202", "C0203", "C0204", "C0205", "E0202", "E0203", "E0211", "E0213", "E0236", "E0237", "E0238", "E0239", "E0240", "E0241", "E0242", "E0243", "E0244", "E0245", "E0301", "E0302", "E0303", "E0304", "E0305", "E0306", "E0307", "E0308", "E0309", "E0310", "E0311", "E0312", "E0313", "F0202", "R0202", "R0203", "R0205", "R0206", "W0201", "W0211", "W0212", "W0213", "W0221", "W0222", "W0223", "W0231", "W0233", "W0236", "W0237", "W0238", "W0239", "W0240", "W0244", "W0245", "W0246"]),
    ("lambda-expressions", &["C3001", "C3002"]),
    ("variables", &["E0601", "E0602", "E0603", "E0604", "E0605", "E0606", "E0611", "E0633", "E0643", "W0601", "W0602", "W0603", "W0604", "W0611", "W0612", "W0613", "W0614", "W0621", "W0622", "W0631", "W0632", "W0640", "W0641", "W0642", "W0644"]),
    ("unsupported_version", &["W2601", "W2602", "W2603", "W2604", "W2605", "W2606"]),
    ("unnecessary_ellipsis", &["W2301"]),
    ("nonascii-checker", &["C2401", "C2403", "W2402"]),
    ("format", &["C0301", "C0302", "C0303", "C0304", "C0305", "C0321", "C0325", "C0327", "C0328", "W0301", "W0311"]),
    ("imports", &["C0410", "C0411", "C0412", "C0413", "C0414", "C0415", "E0401", "E0402", "R0401", "R0402", "W0401", "W0404", "W0406", "W0407", "W0410", "W0416", "W4901"]),
    ("method_args", &["E3102", "W3101"]),
    ("match_statements", &["E1901", "E1902", "E1903", "E1904", "R1905", "R1906"]),
    ("threading", &["W2101"]),
    ("metrics", &[]),
    ("newstyle", &["E1003"]),
    ("exceptions", &["E0701", "E0702", "E0704", "E0705", "E0710", "E0711", "E0712", "W0702", "W0705", "W0706", "W0707", "W0711", "W0715", "W0716", "W0718", "W0719"]),
    ("stdlib", &["E1507", "E1519", "E1520", "W1501", "W1502", "W1503", "W1506", "W1507", "W1508", "W1509", "W1510", "W1514", "W1515", "W1518", "W4902", "W4903", "W4904", "W4905", "W4906"]),
    ("refactoring", &["C0117", "C0200", "C0201", "C0206", "C0207", "C0208", "C0209", "C1802", "C1803", "C1804", "C1805", "R1701", "R1702", "R1703", "R1704", "R1705", "R1706", "R1707", "R1708", "R1709", "R1710", "R1711", "R1712", "R1713", "R1714", "R1715", "R1716", "R1717", "R1718", "R1719", "R1720", "R1721", "R1722", "R1723", "R1724", "R1725", "R1726", "R1727", "R1728", "R1729", "R1730", "R1731", "R1732", "R1733", "R1734", "R1735", "R1736", "R1737"]),
    ("design", &["R0901", "R0902", "R0903", "R0904", "R0911", "R0912", "R0913", "R0914", "R0915", "R0916", "R0917"]),
    ("string", &["E1300", "E1301", "E1302", "E1303", "E1304", "E1305", "E1306", "E1307", "E1310", "W1300", "W1301", "W1302", "W1303", "W1304", "W1305", "W1306", "W1307", "W1308", "W1309", "W1310", "W1401", "W1402", "W1404", "W1405", "W1406"]),
    ("basic", &["C0103", "C0104", "C0105", "C0112", "C0114", "C0115", "C0116", "C0121", "C0123", "C0131", "C0132", "E0100", "E0101", "E0102", "E0103", "E0104", "E0105", "E0106", "E0107", "E0108", "E0110", "E0111", "E0112", "E0113", "E0114", "E0115", "E0117", "E0118", "E0119", "R0123", "R0124", "R0133", "W0101", "W0102", "W0104", "W0105", "W0106", "W0107", "W0108", "W0109", "W0120", "W0122", "W0123", "W0124", "W0125", "W0126", "W0127", "W0128", "W0129", "W0130", "W0131", "W0133", "W0134", "W0135", "W0136", "W0137", "W0143", "W0150", "W0177", "W0199"]),
    ("nested_min_max", &["W3301"]),
    ("bad-chained-comparison", &["W3601"]),
];

/// `_MessageStateHandler._get_messages_to_set` (message_state_handler.py:82-140).
pub fn get_messages_to_set(token: &str, enable: bool) -> Result<Vec<MsgIdx>, ResolveError> {
    let s = store();
    // 1. "all"
    if token == "all" {
        let mut out = Vec::new();
        for &cat in MSG_CATEGORIES {
            out.extend(get_messages_to_set(&cat.to_string(), enable)?);
        }
        if !enable {
            // "all" should not disable pylint's own default-enabled warnings
            out.retain(|&i| !DEFAULT_ENABLED_MAIN.contains(&MESSAGES[i as usize].msgid));
        }
        return Ok(out);
    }
    // 2. category letter (msgid.upper() in MSG_TYPES; long names never match
    //    in 4.0.5 -- MSG_TYPES_LONG bug, message_state_handler.py:97-104)
    if token.len() == 1 {
        let upper = token.to_uppercase().chars().next().unwrap();
        if MSG_CATEGORIES.contains(&upper) {
            let mut out = Vec::new();
            for (i, m) in MESSAGES.iter().enumerate() {
                if m.msgid.starts_with(upper) {
                    out.push(i as MsgIdx);
                }
            }
            return Ok(out);
        }
    }
    // 3. checker name (lowercased lookup into linter._checkers)
    let lower = token.to_lowercase();
    if let Some((_, ids)) = CHECKER_MSGS.iter().find(|(n, _)| *n == lower) {
        let mut out = Vec::new();
        for id in *ids {
            out.extend(get_messages_to_set(id, enable)?);
        }
        return Ok(out);
    }
    // 4. report id
    if lower.starts_with("rp") {
        return Ok(Vec::new());
    }
    // 5. plain msgid / symbol (with old-name resolution)
    let _ = s;
    get_active_msgids(token)
}
