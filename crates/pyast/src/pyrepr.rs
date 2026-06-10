//! Python-style repr formatting (needed for message texts and dumps).

/// Python repr() of a float (shortest round-trip, Python formatting rules).
///
/// Python's repr is the SHORTEST CORRECTLY-ROUNDED decimal that round-trips
/// (CPython _Py_dg_dtoa mode 0). Rust's `{}` is also shortest-round-trip but
/// resolves decimal ties differently (e.g. 2001599834386887.25: Python
/// '2001599834386887.2', Rust '2001599834386887.3'), so we generate digits
/// with correctly-rounded fixed-precision formatting instead.
pub fn repr_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let mut sci = String::new();
    for p in 0..=16 {
        sci = format!("{:.*e}", p, f);
        if sci.parse::<f64>().map(f64::to_bits) == Ok(f.to_bits()) {
            break;
        }
    }
    let neg = sci.starts_with('-');
    let body = if neg { &sci[1..] } else { sci.as_str() };
    let (mant, exp) = body.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let all_digits: String = mant.chars().filter(|c| *c != '.').collect();
    let trimmed = all_digits.trim_end_matches('0');
    let digits = if trimmed.is_empty() { "0" } else { trimmed };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    // Python repr ('r' format): positional for -4 <= exp < 16, else e+NN
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let e = exp as usize;
            if e + 1 >= digits.len() {
                out.push_str(digits);
                out.push_str(&"0".repeat(e + 1 - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..e + 1]);
                out.push('.');
                out.push_str(&digits[e + 1..]);
            }
        } else {
            out.push_str("0.");
            out.push_str(&"0".repeat((-exp - 1) as usize));
            out.push_str(digits);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    }
    out
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

/// Python repr() of a sequence of code points that may include lone
/// surrogates (which CPython renders as \\udXXX escapes).
pub fn repr_str_points(points: &[u32]) -> String {
    let has_single = points.contains(&0x27);
    let has_double = points.contains(&0x22);
    let (quote, escape_quote) = if has_single && !has_double {
        ('"', '"')
    } else {
        ('\'', '\'')
    };
    let mut out = String::with_capacity(points.len() + 2);
    out.push(quote);
    for &v in points {
        match char::from_u32(v) {
            None => {
                // lone surrogate
                out.push_str(&format!("\\u{v:04x}"));
            }
            Some(c) => match c {
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
                    if is_py_printable(c) {
                        out.push(c);
                    } else if v <= 0xff {
                        out.push_str(&format!("\\x{v:02x}"));
                    } else if v <= 0xffff {
                        out.push_str(&format!("\\u{v:04x}"));
                    } else {
                        out.push_str(&format!("\\U{v:08x}"));
                    }
                }
            },
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
