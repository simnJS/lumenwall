//! Number, vector and colour strings (spec 02 §0.5–§0.7) and C-style number formatting
//! (§4.3, §15.1).

use std::borrow::Cow;
use std::fmt::Write as _;

use glam::Vec4;

/// Whitespace as C `isspace` sees it in the "C" locale.
#[inline]
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn starts_with_ci(s: &[u8], prefix: &[u8]) -> bool {
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Component layout of a vector string (§0.6): `count` in 1..=4 and the byte offset where each
/// component starts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Components {
    pub count: usize,
    pub starts: [usize; 4],
}

/// Counts components the way the reference does: only U+0020 separates, at most the first three
/// spaces are looked at, and component `i` starts right after the `i`-th space.
// parity: "1  2" has 3 components (1, 2, 2), " 1 2" has 3 (1, 1, 2), "3 " has 2 (3, 0) (Q4).
pub(crate) fn components(s: &str) -> Components {
    let mut starts = [0usize; 4];
    let mut count = 1;
    for (i, b) in s.bytes().enumerate() {
        if b == b' ' {
            starts[count] = i + 1;
            count += 1;
            if count == 4 {
                break;
            }
        }
    }
    Components { count, starts }
}

/// Reads the longest prefix C `strtof` accepts (leading whitespace skipped; decimal, exponent,
/// `inf`, `infinity`, `nan`, `nan(...)`, hexadecimal floats). Returns the value and the number of
/// bytes consumed including the skipped whitespace, or `None` when nothing converts.
pub(crate) fn scan_f32(s: &str) -> Option<(f32, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && is_c_space(b[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let sign = |v: f32| if neg { -v } else { v };
    let rest = &b[i..];
    if starts_with_ci(rest, b"inf") {
        let len = if starts_with_ci(rest, b"infinity") {
            8
        } else {
            3
        };
        return Some((sign(f32::INFINITY), i + len));
    }
    if starts_with_ci(rest, b"nan") {
        let mut len = 3;
        if rest.get(3) == Some(&b'(') {
            let mut j = 4;
            while j < rest.len() && (rest[j].is_ascii_alphanumeric() || rest[j] == b'_') {
                j += 1;
            }
            if rest.get(j) == Some(&b')') {
                len = j + 1;
            }
        }
        return Some((sign(f32::NAN), i + len));
    }
    if rest.len() >= 2 && rest[0] == b'0' && (rest[1] | 0x20) == b'x' {
        return Some(match scan_hex_float(&rest[2..]) {
            Some((v, len)) => (sign(v), i + 2 + len),
            // "0x" without hexadecimal digits: only the "0" converts.
            None => (sign(0.0), i + 1),
        });
    }
    let mut j = 0;
    let mut digits = 0;
    while j < rest.len() && rest[j].is_ascii_digit() {
        j += 1;
        digits += 1;
    }
    if j < rest.len() && rest[j] == b'.' {
        j += 1;
        while j < rest.len() && rest[j].is_ascii_digit() {
            j += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if j < rest.len() && (rest[j] | 0x20) == b'e' {
        let mut k = j + 1;
        if k < rest.len() && (rest[k] == b'+' || rest[k] == b'-') {
            k += 1;
        }
        let exp_start = k;
        while k < rest.len() && rest[k].is_ascii_digit() {
            k += 1;
        }
        if k > exp_start {
            j = k;
        }
    }
    // The slice is ASCII digits, '.', 'e' and a sign only, so it is valid UTF-8 and a valid Rust
    // float literal ("5." and ".5" included).
    let text = std::str::from_utf8(&rest[..j]).ok()?;
    let mag: f32 = text.parse().ok()?;
    Some((sign(mag), i + j))
}

fn scan_hex_float(b: &[u8]) -> Option<(f32, usize)> {
    let mut j = 0;
    let mut mant: u64 = 0;
    let mut exp: i64 = 0;
    let mut any = false;
    let hex = |c: u8| (c as char).to_digit(16).map(u64::from);
    while let Some(d) = b.get(j).copied().and_then(hex) {
        any = true;
        if mant < (1 << 56) {
            mant = mant * 16 + d;
        } else {
            exp += 4;
        }
        j += 1;
    }
    if b.get(j) == Some(&b'.') {
        let mut k = j + 1;
        let mut frac_any = false;
        while let Some(d) = b.get(k).copied().and_then(hex) {
            frac_any = true;
            if mant < (1 << 56) {
                mant = mant * 16 + d;
                exp -= 4;
            }
            k += 1;
        }
        if any || frac_any {
            any = true;
            j = k;
        }
    }
    if !any {
        return None;
    }
    if j < b.len() && (b[j] | 0x20) == b'p' {
        let mut k = j + 1;
        let mut eneg = false;
        if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
            eneg = b[k] == b'-';
            k += 1;
        }
        let start = k;
        let mut e: i64 = 0;
        while k < b.len() && b[k].is_ascii_digit() {
            e = (e * 10 + i64::from(b[k] - b'0')).min(100_000);
            k += 1;
        }
        if k > start {
            exp += if eneg { -e } else { e };
            j = k;
        }
    }
    let e = exp.clamp(-2000, 2000) as i32;
    let v = (mant as f64) * 2f64.powi(e);
    Some((v as f32, j))
}

/// Whole-string float (§5.2, one component): leading whitespace tolerated, everything else must
/// be consumed.
pub(crate) fn parse_full_f32(s: &str) -> Option<f32> {
    match scan_f32(s) {
        Some((v, n)) if n == s.len() => Some(v),
        _ => None,
    }
}

/// Float component starting at `start` (§0.6-4): no valid prefix reads as 0.
pub(crate) fn component_f32(s: &str, start: usize) -> f32 {
    s.get(start..).and_then(scan_f32).map_or(0.0, |(v, _)| v)
}

/// Integer component (§0.6-5): whitespace, optional sign, decimal digits; no digit or overflow of
/// the 32-bit range fails.
pub(crate) fn component_i32(s: &str, start: usize) -> Option<i32> {
    let b = s.as_bytes().get(start..)?;
    let mut i = 0;
    while i < b.len() && is_c_space(b[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let start_digits = i;
    let mut v: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        v = v * 10 + i64::from(b[i] - b'0');
        if v > i64::from(i32::MAX) + 1 {
            return None;
        }
        i += 1;
    }
    if i == start_digits {
        return None;
    }
    let v = if neg { -v } else { v };
    i32::try_from(v).ok()
}

/// Reads `n` float components of a vector string without checking the component count.
pub(crate) fn vector_components(s: &str) -> (usize, [f32; 4]) {
    let c = components(s);
    let mut out = [0.0; 4];
    for (i, slot) in out.iter_mut().enumerate().take(c.count) {
        *slot = component_f32(s, c.starts[i]);
    }
    (c.count, out)
}

/// Why a colour string was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ColorError {
    /// `#` followed by a digit count other than 3, 4, 6 or 8.
    #[error("hexadecimal colour must have 3, 4, 6 or 8 digits, found {0}")]
    HexLength(usize),
    /// `#` followed by no hexadecimal digit.
    #[error("hexadecimal colour has no hexadecimal digit")]
    HexDigits,
    /// Not 3 or 4 components.
    #[error("colour must have 3 or 4 components, found {0}")]
    Components(usize),
    /// Integer path: a component is not a 32-bit integer.
    #[error("colour component is not an integer")]
    Integer,
}

/// Colour strings (§0.7). Always returns RGBA with an implicit alpha of 1; no clamping.
pub(crate) fn parse_color(input: &str) -> Result<Vec4, ColorError> {
    let s: Cow<'_, str> = if input.contains(',') {
        // parity: commas become spaces without merging, so "1, 0, 0" has 4 components (Q3).
        Cow::Owned(input.replace(',', " "))
    } else {
        Cow::Borrowed(input)
    };
    if let Some(hex) = s.strip_prefix('#') {
        let n = hex.chars().count();
        let expanded: Cow<'_, str> = match n {
            3 | 4 => {
                let mut out = String::with_capacity(8);
                for c in hex.chars() {
                    out.push(c);
                    out.push(c);
                }
                if n == 3 {
                    out.push_str("ff");
                }
                Cow::Owned(out)
            }
            6 | 8 => Cow::Borrowed(hex),
            other => return Err(ColorError::HexLength(other)),
        };
        // Lumen reads the digits as an unsigned 32-bit number: 8-digit colours whose first byte
        // is >= 0x80 load instead of failing (Q2, fatal error fixed).
        let mut v: u32 = 0;
        let mut any = false;
        for c in expanded.chars() {
            let Some(d) = c.to_digit(16) else { break };
            any = true;
            v = (v << 4) | d;
        }
        if !any {
            return Err(ColorError::HexDigits);
        }
        // parity: "#RRGGBB" is read as 0x00RRGGBB, giving R=0, G=RR, B=GG, A=BB (Q2).
        let byte = |shift: u32| ((v >> shift) & 0xff) as f32 / 255.0;
        return Ok(Vec4::new(byte(24), byte(16), byte(8), byte(0)));
    }
    let c = components(&s);
    if c.count != 3 && c.count != 4 {
        return Err(ColorError::Components(c.count));
    }
    let mut out = [0.0f32, 0.0, 0.0, 1.0];
    if !s.contains('.') {
        // parity: integer path, 0-255 per channel; "1 1 1" is almost black (Q1).
        let mut ints = [0i32, 0, 0, 255];
        for (i, slot) in ints.iter_mut().enumerate().take(c.count) {
            *slot = component_i32(&s, c.starts[i]).ok_or(ColorError::Integer)?;
        }
        for (o, i) in out.iter_mut().zip(ints) {
            *o = i as f32 / 255.0;
        }
    } else {
        for (i, slot) in out.iter_mut().enumerate().take(c.count) {
            *slot = component_f32(&s, c.starts[i]);
        }
    }
    Ok(Vec4::from_array(out))
}

/// `printf("%f")`: fixed notation with 6 decimals, C spelling of non-finite values.
pub(crate) fn fmt_fixed6(out: &mut String, v: f32) {
    if v.is_nan() {
        out.push_str(if v.is_sign_negative() { "-nan" } else { "nan" });
    } else if v.is_infinite() {
        out.push_str(if v < 0.0 { "-inf" } else { "inf" });
    } else {
        let _ = write!(out, "{:.6}", f64::from(v));
    }
}

/// Default C++ stream formatting of a float (`%g` with 6 significant digits).
pub(crate) fn fmt_g6(v: f32) -> String {
    let v = f64::from(v);
    if v.is_nan() {
        return if v.is_sign_negative() {
            "-nan".into()
        } else {
            "nan".into()
        };
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf".into() } else { "inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    const P: i32 = 6;
    let sci = format!("{:.*e}", (P - 1) as usize, v);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..P).contains(&exp) {
        let mantissa = trim_fraction(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    } else {
        let decimals = (P - 1 - exp).max(0) as usize;
        trim_fraction(&format!("{v:.decimals$}")).to_owned()
    }
}

fn trim_fraction(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec(s: &str) -> Vec<f32> {
        let (n, v) = vector_components(s);
        v[..n].to_vec()
    }

    #[test]
    fn vector_component_counting_matches_reference() {
        assert_eq!(vec("1 2 3"), [1.0, 2.0, 3.0]);
        assert_eq!(vec("1  2"), [1.0, 2.0, 2.0]);
        assert_eq!(vec(" 1 2"), [1.0, 1.0, 2.0]);
        assert_eq!(vec("3 "), [3.0, 0.0]);
        assert_eq!(vec("1 2 3 4 5"), [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(vec("1\t2"), [1.0]);
        assert_eq!(vec("a b"), [0.0, 0.0]);
    }

    #[test]
    fn strtof_prefixes() {
        assert_eq!(scan_f32("0.5abc"), Some((0.5, 3)));
        assert_eq!(scan_f32("  -2e3x"), Some((-2000.0, 6)));
        assert_eq!(scan_f32("1e"), Some((1.0, 1)));
        assert_eq!(scan_f32("5."), Some((5.0, 2)));
        assert_eq!(scan_f32(".5"), Some((0.5, 2)));
        assert_eq!(scan_f32("inf"), Some((f32::INFINITY, 3)));
        assert_eq!(scan_f32("-Infinity"), Some((f32::NEG_INFINITY, 9)));
        assert!(scan_f32("nan(123)").is_some_and(|(v, n)| v.is_nan() && n == 8));
        assert_eq!(scan_f32("0x1.8p1"), Some((3.0, 7)));
        assert_eq!(scan_f32("0xg"), Some((0.0, 1)));
        assert_eq!(scan_f32("abc"), None);
        assert_eq!(scan_f32(""), None);
        assert_eq!(scan_f32("."), None);
        assert_eq!(parse_full_f32("\t5"), Some(5.0));
        assert_eq!(parse_full_f32("5 "), None);
        assert_eq!(parse_full_f32("5px"), None);
    }

    #[test]
    fn integer_components() {
        assert_eq!(component_i32("255", 0), Some(255));
        assert_eq!(component_i32(" -3x", 0), Some(-3));
        assert_eq!(component_i32("x", 0), None);
        assert_eq!(component_i32("2147483648", 0), None);
        assert_eq!(component_i32("-2147483648", 0), Some(i32::MIN));
    }

    #[test]
    fn colors() {
        let c = parse_color("255 128 0").unwrap();
        assert_eq!(c, Vec4::new(1.0, 128.0 / 255.0, 0.0, 1.0));
        let c = parse_color("1 1 1").unwrap();
        assert_eq!(c, Vec4::new(1.0 / 255.0, 1.0 / 255.0, 1.0 / 255.0, 1.0));
        let c = parse_color("0.2 0.4 0.8").unwrap();
        assert_eq!(c, Vec4::new(0.2, 0.4, 0.8, 1.0));
        let c = parse_color("0.5 0.5 0.5 0.25").unwrap();
        assert_eq!(c.w, 0.25);
        // Q3: commas double the spaces.
        let c = parse_color("1, 0, 0").unwrap();
        assert_eq!(c, Vec4::new(1.0 / 255.0, 0.0, 0.0, 0.0));
        // Q2: #RRGGBB shifted by one byte.
        let c = parse_color("#ff8000").unwrap();
        assert_eq!(c, Vec4::new(0.0, 1.0, 128.0 / 255.0, 0.0));
        let c = parse_color("#f00").unwrap();
        assert_eq!(c, Vec4::new(1.0, 0.0, 0.0, 1.0));
        let c = parse_color("#80ff00ff").unwrap();
        assert_eq!(c.x, 128.0 / 255.0);
        assert_eq!(parse_color("#12345"), Err(ColorError::HexLength(5)));
        assert_eq!(parse_color("1 2"), Err(ColorError::Components(2)));
        assert_eq!(parse_color("a b c"), Err(ColorError::Integer));
    }

    #[test]
    fn formatting() {
        let mut s = String::new();
        fmt_fixed6(&mut s, 1.5);
        assert_eq!(s, "1.500000");
        assert_eq!(fmt_g6(0.0), "0");
        assert_eq!(fmt_g6(10.0), "10");
        assert_eq!(fmt_g6(0.1), "0.1");
        assert_eq!(fmt_g6(1234567.0), "1.23457e+06");
        assert_eq!(fmt_g6(0.00001), "1e-05");
        assert_eq!(fmt_g6(-2.5), "-2.5");
        assert_eq!(fmt_g6(100000.0), "100000");
    }
}
