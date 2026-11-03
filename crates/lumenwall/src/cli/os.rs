//! Splitting `OsStr` arguments at ASCII boundaries without lossy conversion, so that
//! non-UTF-8 paths given as `--bg=<path>` or `-b<path>` survive intact.

use std::ffi::{OsStr, OsString};

/// Splits `--name=value` at the first `=`; returns `(whole, None)` without `=`.
pub(super) fn split_at_eq(arg: &OsStr) -> (OsString, Option<OsString>) {
    match arg.as_encoded_bytes().iter().position(|&b| b == b'=') {
        Some(index) => (slice(arg, 0, index), Some(suffix(arg, index + 1))),
        None => (arg.to_owned(), None),
    }
}

/// The part of `arg` after its first `start` bytes; `start` must follow an ASCII byte.
pub(super) fn suffix(arg: &OsStr, start: usize) -> OsString {
    slice(arg, start, arg.as_encoded_bytes().len())
}

/// `arg[start..end]` in encoded bytes; both bounds must be next to ASCII bytes.
#[cfg(unix)]
fn slice(arg: &OsStr, start: usize, end: usize) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(&arg.as_bytes()[start..end]).to_owned()
}

// On Windows the encoded bytes are WTF-8. Convert the byte bounds into UTF-16 unit bounds:
// every sequence is one unit except 4-byte ones (supplementary planes), which are two.
#[cfg(windows)]
fn slice(arg: &OsStr, start: usize, end: usize) -> OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    fn units(wtf8: &[u8]) -> usize {
        wtf8.iter()
            .filter(|&&b| b & 0xC0 != 0x80)
            .map(|&b| if b >= 0xF0 { 2 } else { 1 })
            .sum()
    }
    let bytes = arg.as_encoded_bytes();
    let (from, to) = (units(&bytes[..start]), units(&bytes[..end]));
    let wide: Vec<u16> = arg.encode_wide().collect();
    OsString::from_wide(&wide[from..to])
}

#[cfg(not(any(unix, windows)))]
fn slice(arg: &OsStr, start: usize, end: usize) -> OsString {
    String::from_utf8_lossy(&arg.as_encoded_bytes()[start..end])
        .into_owned()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_first_equal_sign() {
        let (name, value) = split_at_eq(OsStr::new("--set-property=a=b"));
        assert_eq!(name, "--set-property");
        assert_eq!(value.unwrap(), "a=b");
        assert_eq!(split_at_eq(OsStr::new("--bg")), ("--bg".into(), None));
        assert_eq!(split_at_eq(OsStr::new("--bg=")).1.unwrap(), "");
        assert_eq!(suffix(OsStr::new("-bfoo"), 2), "foo");
        assert_eq!(suffix(OsStr::new("-bé😀"), 2), "é😀");
    }

    #[cfg(unix)]
    #[test]
    fn keeps_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let arg = OsStr::from_bytes(b"--bg=/tmp/\xff\xfe");
        let (_, value) = split_at_eq(arg);
        assert_eq!(value.unwrap().as_bytes(), b"/tmp/\xff\xfe");
    }
}
