//! Python-style repr formatting (needed for message texts and dumps).

/// Python repr() of a float (shortest round-trip, Python formatting rules).
pub fn repr_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    // Rust's {} is shortest round-trip like Python's repr, but differs in
    // exponent formatting. Python: 1e+16 -> '1e+16', 1e-05 -> '1e-05',
    // integral floats -> '1.0'.
    let mut s = format!("{f}");
    if let Some(epos) = s.find(['e', 'E']) {
        // normalize exponent: Python uses e+NN / e-NN with at least 2 digits
        let (mant, exp) = s.split_at(epos);
        let exp = &exp[1..];
        let (sign, digits) = match exp.strip_prefix('-') {
            Some(d) => ("-", d),
            None => ("+", exp.strip_prefix('+').unwrap_or(exp)),
        };
        let digits = if digits.len() < 2 {
            format!("0{digits}")
        } else {
            digits.to_string()
        };
        s = format!("{mant}e{sign}{digits}");
    } else if !s.contains('.') {
        s.push_str(".0");
    }
    // Python switches to exponent form for large/small magnitudes; Rust never
    // does. Match Python: repr uses 'r' format -> exponent when exp < -4 or
    // >= 16.
    if !s.contains('e') {
        let abs = f.abs();
        if abs != 0.0 && (abs >= 1e16 || abs < 1e-4) {
            // reformat via exponent notation matching Python
            let formatted = format!("{f:e}");
            let (mant, exp) = formatted.split_once('e').unwrap();
            let exp_n: i32 = exp.parse().unwrap_or(0);
            let mant = if mant.contains('.') {
                mant.trim_end_matches('0').trim_end_matches('.')
            } else {
                mant
            };
            let sign = if exp_n < 0 { "-" } else { "+" };
            s = format!("{}e{}{:02}", mant, sign, exp_n.abs());
        }
    }
    s
}

/// Python repr() of a str.
pub fn repr_str(s: &str) -> String {
    let has_single = s.contains('\'');
    let has_double = s.contains('"');
    let (quote, escape_quote) = if has_single && !has_double {
        ('"', '"')
    } else {
        ('\'', '\'')
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == escape_quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                // Python repr keeps printable unicode; escapes non-printable.
                if is_py_printable(c) {
                    out.push(c);
                } else {
                    let v = c as u32;
                    if v <= 0xff {
                        out.push_str(&format!("\\x{v:02x}"));
                    } else if v <= 0xffff {
                        out.push_str(&format!("\\u{v:04x}"));
                    } else {
                        out.push_str(&format!("\\U{v:08x}"));
                    }
                }
            }
        }
    }
    out.push(quote);
    out
}

/// Approximation of Python str.isprintable() for repr purposes: excludes
/// separators and "other" categories. Covers the common cases; refine on
/// diff evidence.
fn is_py_printable(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    !matches!(
        get_general_category(c),
        G::Control
            | G::Format
            | G::Surrogate
            | G::PrivateUse
            | G::Unassigned
            | G::LineSeparator
            | G::ParagraphSeparator
            | G::SpaceSeparator
    ) || c == ' '
}
