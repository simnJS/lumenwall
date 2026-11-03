//! `.pkg` archive reader (spec 01 §1).
//!
//! The archive is memory-mapped (or held in memory) and parsed once at open time:
//! one pass over the entry table, then a hash index is sorted so lookups allocate
//! nothing. Entry contents are slices borrowed from the mapping; nothing is copied.
//!
//! Layout: `sstr` header starting with `PKGV`, `u32` entry count N, N entries of
//! (`sstr` name, `u32` offset, `u32` size), then the data area. Offsets are relative
//! to `base`, the first byte after the table.
//!
//! Entry bounds are checked when an entry is read, never at open time: an entry
//! that runs past the end of the file fails alone with `E_PKG_ENTREE_HORS_LIMITES`
//! and every other entry stays readable (spec 01 §1.4, amendment A.4).

use std::fs::File;
use std::io;
use std::path::Path;

use thiserror::Error;

use crate::reader::{Reader, UnexpectedEof, escape_bytes};

/// Errors raised while opening an archive or reading one of its entries. All of them
/// are in the spec's `Invalide` category (the file exists but cannot be used).
#[derive(Debug, Error)]
pub enum PkgError {
    /// The header `sstr` does not start with `PKGV` (spec code `E_PKG_ENTETE`).
    #[error("E_PKG_ENTETE: .pkg header \"{found}\" does not start with \"PKGV\"")]
    BadHeader {
        /// The header bytes, escaped (at most 32 bytes shown).
        found: String,
    },
    /// The header or the table runs past the end of the file (`FinDeFluxInattendue`).
    #[error(transparent)]
    UnexpectedEof(#[from] UnexpectedEof),
    /// The entry being read ends past the end of the file
    /// (`E_PKG_ENTREE_HORS_LIMITES`, raised by [`PkgEntry::data`] only).
    #[error(
        "E_PKG_ENTREE_HORS_LIMITES: entry \"{name}\" (offset {offset}, size {size}) ends at byte {end}, \
         past the end of the file ({file_len} bytes, data area starts at {base})"
    )]
    EntryOutOfBounds {
        /// Entry name, escaped.
        name: String,
        /// Offset relative to the data area.
        offset: u32,
        /// Declared size.
        size: u32,
        /// Absolute end position, `base + offset + size`.
        end: u64,
        /// Start of the data area.
        base: u64,
        /// File length.
        file_len: u64,
    },
    /// Reading or mapping the file failed.
    #[error("cannot read .pkg file: {0}")]
    Io(#[from] io::Error),
}

impl PkgError {
    /// Spec error code, for logs and tests.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadHeader { .. } => "E_PKG_ENTETE",
            Self::UnexpectedEof(_) => "FinDeFluxInattendue",
            Self::EntryOutOfBounds { .. } => "E_PKG_ENTREE_HORS_LIMITES",
            Self::Io(_) => "E_IO",
        }
    }
}

/// How [`PkgArchive::open_with`] loads the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenMode {
    /// Memory-map the file (default): no copy, pages are read on demand.
    #[default]
    Map,
    /// Read the whole file into memory and close it immediately. Useful on Windows,
    /// where an open mapping locks the file and blocks Steam updates.
    Read,
}

enum Backing {
    Map(memmap2::Mmap),
    Owned(Box<[u8]>),
    Static(&'static [u8]),
}

impl Backing {
    #[inline]
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Map(map) => map,
            Self::Owned(bytes) => bytes,
            Self::Static(bytes) => bytes,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RawEntry {
    name_start: usize,
    name_len: u32,
    offset: u32,
    size: u32,
}

/// One archive entry, borrowed from the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PkgEntry<'a> {
    /// Position of the entry in the table.
    pub index: usize,
    /// Raw name bytes (`/`-separated relative path, normally UTF-8).
    pub name: &'a [u8],
    /// Offset relative to the data area.
    pub offset: u32,
    /// Declared size.
    pub size: u32,
    archive: &'a [u8],
    base: usize,
}

impl<'a> PkgEntry<'a> {
    /// Name as UTF-8, if it is valid UTF-8.
    pub fn name_str(&self) -> Option<&'a str> {
        std::str::from_utf8(self.name).ok()
    }

    /// Entry contents, borrowed from the mapping. Fails with
    /// `E_PKG_ENTREE_HORS_LIMITES` when `base + offset + size` passes the end of the
    /// file (64-bit arithmetic); only this entry is affected (A.4, QO-37).
    pub fn data(&self) -> Result<&'a [u8], PkgError> {
        let start = self.base as u64 + u64::from(self.offset);
        let end = start + u64::from(self.size);
        let file_len = self.archive.len() as u64;
        if end > file_len {
            return Err(PkgError::EntryOutOfBounds {
                name: escape_bytes(self.name),
                offset: self.offset,
                size: self.size,
                end,
                base: self.base as u64,
                file_len,
            });
        }
        // Both bounds are at most `file_len`, which fits in `usize`.
        Ok(&self.archive[start as usize..end as usize])
    }

    /// Whether the entry lies entirely inside the file.
    pub fn is_in_bounds(&self) -> bool {
        self.data().is_ok()
    }
}

/// An opened `.pkg` archive.
pub struct PkgArchive {
    backing: Backing,
    header: (usize, usize),
    base: usize,
    entries: Box<[RawEntry]>,
    /// `(hash(name), entry index)`, sorted. Equal hashes keep table order, which
    /// implements the "first entry wins" rule (spec 01 §1.5).
    index: Box<[(u64, u32)]>,
}

impl std::fmt::Debug for PkgArchive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkgArchive")
            .field("header", &escape_bytes(self.header()))
            .field("entries", &self.entries.len())
            .field("base", &self.base)
            .field("mapped", &self.is_mapped())
            .finish()
    }
}

impl PkgArchive {
    /// Memory-maps and parses the archive at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PkgError> {
        Self::open_with(path, OpenMode::Map)
    }

    /// Opens the archive at `path` with an explicit loading strategy.
    pub fn open_with(path: impl AsRef<Path>, mode: OpenMode) -> Result<Self, PkgError> {
        let path = path.as_ref();
        let backing = match mode {
            OpenMode::Read => Backing::Owned(std::fs::read(path)?.into_boxed_slice()),
            OpenMode::Map => {
                let file = File::open(path)?;
                if file.metadata()?.len() == 0 {
                    // Mapping an empty file fails on some platforms; it is invalid anyway.
                    Backing::Owned(Box::default())
                } else {
                    // SAFETY: the mapping is read-only and only ever exposed as `&[u8]`
                    // borrowed from `self`. The remaining hazard, inherent to every mmap
                    // reader, is another process truncating the file while it is mapped;
                    // wallpaper archives are not modified in place while in use, and
                    // `release_mapping` exists for platforms where that matters.
                    let map = unsafe { memmap2::Mmap::map(&file)? };
                    Backing::Map(map)
                }
            }
        };
        Self::from_backing(backing)
    }

    /// Parses an archive held in memory.
    pub fn from_vec(bytes: Vec<u8>) -> Result<Self, PkgError> {
        Self::from_backing(Backing::Owned(bytes.into_boxed_slice()))
    }

    /// Parses an archive from static data (embedded assets, tests).
    pub fn from_static(bytes: &'static [u8]) -> Result<Self, PkgError> {
        Self::from_backing(Backing::Static(bytes))
    }

    fn from_backing(backing: Backing) -> Result<Self, PkgError> {
        let parsed = parse_table(backing.bytes())?;
        Ok(Self {
            backing,
            header: parsed.header,
            base: parsed.base,
            entries: parsed.entries,
            index: parsed.index,
        })
    }

    /// Replaces the memory mapping with an in-memory copy, which releases the file.
    ///
    /// On Windows a live mapping keeps the file locked (Steam cannot update the
    /// wallpaper). Call this once the archive is loaded if it must stay usable;
    /// otherwise simply drop the archive after the assets are uploaded. No-op when
    /// the archive is not mapped.
    pub fn release_mapping(&mut self) {
        if let Backing::Map(map) = &self.backing {
            let owned: Box<[u8]> = map[..].into();
            self.backing = Backing::Owned(owned);
        }
    }

    /// Whether the archive is backed by a live memory mapping.
    pub fn is_mapped(&self) -> bool {
        matches!(self.backing, Backing::Map(_))
    }

    /// The whole archive file.
    pub fn as_bytes(&self) -> &[u8] {
        self.backing.bytes()
    }

    /// Header bytes (for example `PKGV0019`); only the `PKGV` prefix is validated.
    pub fn header(&self) -> &[u8] {
        let (start, len) = self.header;
        &self.as_bytes()[start..start + len]
    }

    /// Start of the data area (spec 01 §1.3).
    pub fn base(&self) -> usize {
        self.base
    }

    /// Number of entries in the table, duplicates included.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entry at position `index` in the table.
    pub fn entry(&self, index: usize) -> Option<PkgEntry<'_>> {
        let raw = self.entries.get(index)?;
        Some(self.make_entry(index, raw))
    }

    /// Entries in table order, duplicates included.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = PkgEntry<'_>> + '_ {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, raw)| self.make_entry(index, raw))
    }

    /// Exact, case-sensitive lookup by name. With duplicate names the first entry in
    /// table order wins. Does not allocate.
    pub fn get(&self, name: &str) -> Option<PkgEntry<'_>> {
        self.get_bytes(name.as_bytes())
    }

    /// [`get`](Self::get) for raw name bytes.
    pub fn get_bytes(&self, name: &[u8]) -> Option<PkgEntry<'_>> {
        let index = self.find(name)?;
        Some(self.make_entry(index, &self.entries[index]))
    }

    /// Whether an entry named `name` exists, even if it is out of bounds (A.4).
    pub fn contains(&self, name: &str) -> bool {
        self.find(name.as_bytes()).is_some()
    }

    /// Contents of the entry named `name`, borrowed from the archive: `None` when
    /// there is no such entry, `Some(Err(_))` when it is out of bounds.
    pub fn read(&self, name: &str) -> Option<Result<&[u8], PkgError>> {
        self.get(name).map(|entry| entry.data())
    }

    fn find(&self, name: &[u8]) -> Option<usize> {
        let hash = hash_name(name);
        let start = self.index.partition_point(|&(h, _)| h < hash);
        let bytes = self.as_bytes();
        self.index[start..]
            .iter()
            .take_while(|&&(h, _)| h == hash)
            .map(|&(_, i)| i as usize)
            .find(|&i| entry_name(bytes, &self.entries[i]) == name)
    }

    #[inline]
    fn make_entry(&self, index: usize, raw: &RawEntry) -> PkgEntry<'_> {
        let bytes = self.as_bytes();
        PkgEntry {
            index,
            name: entry_name(bytes, raw),
            offset: raw.offset,
            size: raw.size,
            archive: bytes,
            base: self.base,
        }
    }
}

#[inline]
fn entry_name<'a>(bytes: &'a [u8], raw: &RawEntry) -> &'a [u8] {
    &bytes[raw.name_start..raw.name_start + raw.name_len as usize]
}

struct ParsedTable {
    header: (usize, usize),
    base: usize,
    entries: Box<[RawEntry]>,
    index: Box<[(u64, u32)]>,
}

/// Smallest table entry: empty name `sstr` (4) + offset (4) + size (4).
const MIN_ENTRY_SIZE: u64 = 12;

fn parse_table(bytes: &[u8]) -> Result<ParsedTable, PkgError> {
    let mut r = Reader::new(bytes);

    // Spec 01 §1.4 step 1. A length past the end of the file is a Lumen check (QO-02).
    let header_start = r.pos() + 4;
    let header = r.sstr()?;
    if !header.starts_with(b"PKGV") {
        let shown = &header[..header.len().min(32)];
        return Err(PkgError::BadHeader {
            found: escape_bytes(shown),
        });
    }

    let count = r.u32()?;
    // Refuse impossible counts before allocating (QO-02).
    r.ensure(u64::from(count) * MIN_ENTRY_SIZE)?;

    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name_len = r.u32()?;
        let name_start = r.pos();
        r.take(name_len as usize)?;
        let offset = r.u32()?;
        let size = r.u32()?;
        entries.push(RawEntry {
            name_start,
            name_len,
            offset,
            size,
        });
    }
    // Entry bounds are not checked here: only reading an entry checks its own (A.4).
    let base = r.pos();

    let mut index: Vec<(u64, u32)> = entries
        .iter()
        .enumerate()
        .map(|(i, raw)| (hash_name(entry_name(bytes, raw)), i as u32))
        .collect();
    // Sorting the (hash, position) pairs keeps duplicates in table order.
    index.sort_unstable();

    Ok(ParsedTable {
        header: (header_start, header.len()),
        base,
        entries: entries.into_boxed_slice(),
        index: index.into_boxed_slice(),
    })
}

/// Fast non-cryptographic hash for entry names (multiply-rotate over 8-byte words).
/// Collisions only cost an extra name comparison.
#[inline]
fn hash_name(bytes: &[u8]) -> u64 {
    const K: u64 = 0xf135_7aea_2e62_a9c5;
    let mut h = (bytes.len() as u64).wrapping_mul(K);
    let (words, rest) = bytes.as_chunks::<8>();
    for &word in words {
        h = (h ^ u64::from_le_bytes(word))
            .wrapping_mul(K)
            .rotate_left(26);
    }
    if !rest.is_empty() {
        let mut word = [0u8; 8];
        word[..rest.len()].copy_from_slice(rest);
        h = (h ^ u64::from_le_bytes(word))
            .wrapping_mul(K)
            .rotate_left(26);
    }
    h ^= h >> 32;
    h = h.wrapping_mul(K);
    h ^ (h >> 29)
}

/// Builds a valid `.pkg` image from `(name, data)` pairs, stored contiguously in
/// order. Meant for tests, benchmarks and tools.
pub fn build_pkg<'a>(
    version: &str,
    files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Vec<u8> {
    let files: Vec<(&str, &[u8])> = files.into_iter().collect();
    let mut out = Vec::new();
    out.extend_from_slice(&(version.len() as u32).to_le_bytes());
    out.extend_from_slice(version.as_bytes());
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    let mut offset = 0u32;
    for (name, data) in &files {
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &files {
        out.extend_from_slice(data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_duplicate_wins() {
        let bytes = build_pkg("PKGV0001", [("a", &b"1"[..]), ("b", b"2"), ("a", b"3")]);
        let pkg = PkgArchive::from_vec(bytes).unwrap();
        assert_eq!(pkg.len(), 3);
        assert_eq!(pkg.read("a").unwrap().unwrap(), b"1");
        assert_eq!(pkg.get("a").unwrap().index, 0);
        assert_eq!(pkg.entry(2).unwrap().data().unwrap(), b"3");
    }

    #[test]
    fn many_entries_all_found() {
        let names: Vec<String> = (0..2000).map(|i| format!("materials/x{i}.tex")).collect();
        let data: Vec<Vec<u8>> = (0..2000u32).map(|i| i.to_le_bytes().to_vec()).collect();
        let bytes = build_pkg(
            "PKGV0019",
            names
                .iter()
                .map(String::as_str)
                .zip(data.iter().map(Vec::as_slice)),
        );
        let pkg = PkgArchive::from_vec(bytes).unwrap();
        for (i, name) in names.iter().enumerate() {
            assert_eq!(pkg.read(name).unwrap().unwrap(), (i as u32).to_le_bytes());
        }
        assert!(pkg.get("materials/x2000.tex").is_none());
    }

    #[test]
    fn huge_count_is_rejected_without_allocating() {
        let mut bytes = build_pkg("PKGV0001", []);
        let n = bytes.len() - 4;
        bytes[n..].copy_from_slice(&u32::MAX.to_le_bytes());
        let err = PkgArchive::from_vec(bytes).unwrap_err();
        assert_eq!(err.code(), "FinDeFluxInattendue");
    }
}
