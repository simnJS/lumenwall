//! Bounds-checked little-endian cursor over a byte slice (spec 01 §0).

use std::fmt;

/// A read went past the end of the available data (spec code `FinDeFluxInattendue`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnexpectedEof {
    /// Offset of the read that failed, from the start of the data.
    pub offset: u64,
    /// Number of bytes the read needed.
    pub needed: u64,
    /// Number of bytes left at `offset`.
    pub available: u64,
}

impl fmt::Display for UnexpectedEof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FinDeFluxInattendue: unexpected end of data at offset {} (needed {} bytes, {} available)",
            self.offset, self.needed, self.available
        )
    }
}

impl std::error::Error for UnexpectedEof {}

/// Cursor that borrows the data it reads: every slice it returns lives as long as the
/// input, so parsers built on it are zero-copy.
#[derive(Debug, Clone)]
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub(crate) fn eof(&self, needed: u64) -> UnexpectedEof {
        UnexpectedEof {
            offset: self.pos as u64,
            needed,
            available: self.remaining() as u64,
        }
    }

    /// Fails without consuming anything unless `needed` more bytes are available.
    pub(crate) fn ensure(&self, needed: u64) -> Result<(), UnexpectedEof> {
        if needed > self.remaining() as u64 {
            Err(self.eof(needed))
        } else {
            Ok(())
        }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], UnexpectedEof> {
        if n > self.remaining() {
            return Err(self.eof(n as u64));
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], UnexpectedEof> {
        let bytes = self.take(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, UnexpectedEof> {
        self.array().map(u32::from_le_bytes)
    }

    pub(crate) fn i32(&mut self) -> Result<i32, UnexpectedEof> {
        self.array().map(i32::from_le_bytes)
    }

    pub(crate) fn f32(&mut self) -> Result<f32, UnexpectedEof> {
        self.array().map(f32::from_le_bytes)
    }

    /// `sstr`: `u32` length then that many raw bytes.
    pub(crate) fn sstr(&mut self) -> Result<&'a [u8], UnexpectedEof> {
        let len = self.u32()?;
        self.take(len as usize)
    }

    /// `cstr`: bytes up to the first NUL. The NUL is consumed but not returned.
    /// A missing terminator is an end-of-data error (spec 01 QO-01).
    pub(crate) fn cstr(&mut self) -> Result<&'a [u8], UnexpectedEof> {
        let rest = &self.data[self.pos..];
        match rest.iter().position(|&b| b == 0) {
            Some(len) => {
                self.pos += len + 1;
                Ok(&rest[..len])
            }
            None => Err(self.eof(rest.len() as u64 + 1)),
        }
    }
}

/// Renders raw bytes (magics, headers) as escaped ASCII for error messages.
pub(crate) fn escape_bytes(bytes: &[u8]) -> String {
    bytes.escape_ascii().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_and_stops_at_end() {
        let data = [
            1, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0x80, 0x3F, b'h', b'i', 0,
        ];
        let mut r = Reader::new(&data);
        assert_eq!(r.u32().unwrap(), 1);
        assert_eq!(r.i32().unwrap(), -1);
        assert_eq!(r.f32().unwrap(), 1.0);
        assert_eq!(r.cstr().unwrap(), b"hi");
        assert_eq!(r.remaining(), 0);
        let err = r.u32().unwrap_err();
        assert_eq!(
            err,
            UnexpectedEof {
                offset: 15,
                needed: 4,
                available: 0
            }
        );
    }

    #[test]
    fn cstr_without_terminator_is_eof() {
        let mut r = Reader::new(b"abc");
        assert!(r.cstr().is_err());
        assert_eq!(r.pos(), 0);
    }

    #[test]
    fn sstr_longer_than_data_is_eof() {
        let mut r = Reader::new(&[0xFF, 0xFF, 0xFF, 0x7F, 1, 2]);
        let err = r.sstr().unwrap_err();
        assert_eq!(err.needed, 0x7FFF_FFFF);
        assert_eq!(err.available, 2);
    }
}
