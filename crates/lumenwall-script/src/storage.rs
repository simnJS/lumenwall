//! Persistent `localStorage` (WE `ILocalStorage`): one JSON file per store, written lazily.
//!
//! Values are kept as JSON text. Writes happen at most once per flush delay, on a
//! background thread, and synchronously when the last handle is dropped.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// WE quota: "up to 100 KB per wallpaper".
pub const STORAGE_QUOTA: usize = 100 * 1024;

/// A `localStorage` area (`LOCATION_SCREEN` or `LOCATION_GLOBAL`).
///
/// Cloning shares the same store: give every [`crate::ScriptEngine`] of a wallpaper the
/// same global store and a per-screen store of its own.
#[derive(Clone)]
pub struct StorageFile {
    inner: Arc<Mutex<StoreInner>>,
}

struct StoreInner {
    path: Option<PathBuf>,
    entries: BTreeMap<String, String>,
    bytes: usize,
    dirty_since: Option<Instant>,
    writer: Option<JoinHandle<()>>,
}

/// The store would exceed [`STORAGE_QUOTA`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuotaExceeded;

impl StorageFile {
    /// A store that is never written to disk (reference behaviour).
    pub fn in_memory() -> Self {
        Self::from_inner(None, BTreeMap::new())
    }

    /// A store persisted at `path`. An existing file is read now; a missing or corrupt file
    /// starts empty (logged).
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => parse_store(&text).unwrap_or_else(|| {
                tracing::warn!(target: "lumenwall_script", "ignoring corrupt localStorage file {}", path.display());
                BTreeMap::new()
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                tracing::warn!(target: "lumenwall_script", "cannot read localStorage file {}: {e}", path.display());
                BTreeMap::new()
            }
        };
        Self::from_inner(Some(path), entries)
    }

    fn from_inner(path: Option<PathBuf>, entries: BTreeMap<String, String>) -> Self {
        let bytes = entries.iter().map(|(k, v)| k.len() + v.len()).sum();
        StorageFile {
            inner: Arc::new(Mutex::new(StoreInner {
                path,
                entries,
                bytes,
                dirty_since: None,
                writer: None,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, StoreInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Writes pending changes now, synchronously.
    pub fn flush(&self) -> io::Result<()> {
        let mut inner = self.lock();
        inner.join_writer();
        inner.flush_sync()
    }

    /// Number of stored keys.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether changes are waiting to be written.
    pub fn is_dirty(&self) -> bool {
        self.lock().dirty_since.is_some()
    }

    pub(crate) fn get(&self, key: &str) -> Option<String> {
        self.lock().entries.get(key).cloned()
    }

    pub(crate) fn set(&self, key: &str, json: String) -> Result<(), QuotaExceeded> {
        let mut inner = self.lock();
        let old = inner.entries.get(key).map_or(0, |v| key.len() + v.len());
        let new_total = inner.bytes - old + key.len() + json.len();
        if new_total > STORAGE_QUOTA {
            return Err(QuotaExceeded);
        }
        if inner.entries.get(key) == Some(&json) {
            return Ok(());
        }
        inner.bytes = new_total;
        inner.entries.insert(key.to_owned(), json);
        inner.mark_dirty();
        Ok(())
    }

    pub(crate) fn delete(&self, key: &str) -> bool {
        let mut inner = self.lock();
        match inner.entries.remove(key) {
            Some(v) => {
                inner.bytes -= key.len() + v.len();
                inner.mark_dirty();
                true
            }
            None => false,
        }
    }

    pub(crate) fn clear(&self) {
        let mut inner = self.lock();
        if !inner.entries.is_empty() {
            inner.entries.clear();
            inner.bytes = 0;
            inner.mark_dirty();
        }
    }

    /// Starts a background write when changes are older than `delay`.
    pub(crate) fn flush_if_due(&self, now: Instant, delay: Duration) {
        let mut inner = self.lock();
        let Some(since) = inner.dirty_since else {
            return;
        };
        if now.saturating_duration_since(since) < delay {
            return;
        }
        let Some(path) = inner.path.clone() else {
            inner.dirty_since = None;
            return;
        };
        if inner.writer.as_ref().is_some_and(|w| !w.is_finished()) {
            return; // previous write still running; retry next tick
        }
        inner.join_writer();
        let text = serialize_store(&inner.entries);
        inner.dirty_since = None;
        inner.writer = Some(std::thread::spawn(move || {
            if let Err(e) = write_atomic(&path, &text) {
                tracing::warn!(target: "lumenwall_script", "cannot write localStorage file {}: {e}", path.display());
            }
        }));
    }
}

impl StoreInner {
    fn mark_dirty(&mut self) {
        if self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
        }
    }

    fn join_writer(&mut self) {
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }

    fn flush_sync(&mut self) -> io::Result<()> {
        if self.dirty_since.is_none() {
            return Ok(());
        }
        if let Some(path) = &self.path {
            write_atomic(path, &serialize_store(&self.entries))?;
        }
        self.dirty_since = None;
        Ok(())
    }
}

impl Drop for StoreInner {
    fn drop(&mut self) {
        self.join_writer();
        if let Err(e) = self.flush_sync() {
            tracing::warn!(target: "lumenwall_script", "cannot write localStorage file: {e}");
        }
    }
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn serialize_store(entries: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(
        entries
            .iter()
            .map(|(k, v)| k.len() + v.len() + 4)
            .sum::<usize>()
            + 2,
    );
    out.push('{');
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_json_string(&mut out, k);
        out.push(':');
        out.push_str(v);
    }
    out.push('}');
    out
}

/// Appends `s` as a JSON string literal.
pub(crate) fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parses `{"key": <json value>, …}` keeping each value as raw JSON text.
fn parse_store(text: &str) -> Option<BTreeMap<String, String>> {
    let b = text.as_bytes();
    let mut i = skip_ws(b, 0);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i = skip_ws(b, i + 1);
    let mut out = BTreeMap::new();
    if b.get(i) == Some(&b'}') {
        return Some(out);
    }
    loop {
        let (key, next) = parse_string(text, i)?;
        i = skip_ws(b, next);
        if b.get(i) != Some(&b':') {
            return None;
        }
        i = skip_ws(b, i + 1);
        let end = skip_value(b, i)?;
        out.insert(key, text[i..end].trim_end().to_owned());
        i = skip_ws(b, end);
        match b.get(i) {
            Some(b',') => i = skip_ws(b, i + 1),
            Some(b'}') => return Some(out),
            _ => return None,
        }
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
        i += 1;
    }
    i
}

fn parse_string(text: &str, start: usize) -> Option<(String, usize)> {
    let b = text.as_bytes();
    if b.get(start) != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = text[start + 1..].char_indices();
    while let Some((off, c)) = chars.next() {
        match c {
            '"' => return Some((out, start + 1 + off + 1)),
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex = |chars: &mut std::str::CharIndices<'_>| -> Option<u32> {
                            let mut v = 0;
                            for _ in 0..4 {
                                v = v * 16 + chars.next()?.1.to_digit(16)?;
                            }
                            Some(v)
                        };
                        let hi = hex(&mut chars)?;
                        let cp = if (0xD800..0xDC00).contains(&hi) {
                            let (_, bs) = chars.next()?;
                            let (_, u) = chars.next()?;
                            if bs != '\\' || u != 'u' {
                                return None;
                            }
                            let lo = hex(&mut chars)?;
                            0x10000 + ((hi - 0xD800) << 10) + (lo.checked_sub(0xDC00)?)
                        } else {
                            hi
                        };
                        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                    }
                    _ => return None,
                }
            }
            c => out.push(c),
        }
    }
    None
}

fn skip_value(b: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = start;
    loop {
        match *b.get(i)? {
            b'"' => {
                i += 1;
                loop {
                    match *b.get(i)? {
                        b'\\' => i += 2,
                        b'"' => break,
                        _ => i += 1,
                    }
                }
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                if depth == 0 {
                    return (i > start).then_some(i);
                }
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b',' if depth == 0 => return (i > start).then_some(i),
            _ => i += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_through_file_format() {
        let mut m = BTreeMap::new();
        m.insert(
            "a\"b\n".to_owned(),
            "{\"x\":[1,2,{\"y\":\"}\"}]}".to_owned(),
        );
        m.insert("n".to_owned(), "-1.5e3".to_owned());
        m.insert("s".to_owned(), "\"str,with}\"".to_owned());
        m.insert("é😀".to_owned(), "true".to_owned());
        let text = serialize_store(&m);
        assert_eq!(parse_store(&text), Some(m));
        assert_eq!(parse_store("{}"), Some(BTreeMap::new()));
        assert_eq!(
            parse_store(" { \"k\" : null } "),
            Some([("k".into(), "null".into())].into())
        );
        assert_eq!(
            parse_store("{\"\\ud83d\\ude00\":1}"),
            Some([("😀".into(), "1".into())].into())
        );
        assert_eq!(parse_store("{\"k\":}"), None);
        assert_eq!(parse_store("garbage"), None);
    }

    #[test]
    fn quota_and_dirty_tracking() {
        let s = StorageFile::in_memory();
        assert!(!s.is_dirty());
        s.set("k", "1".into()).unwrap();
        assert!(s.is_dirty());
        assert_eq!(s.get("k").as_deref(), Some("1"));
        assert!(s.set("big", "x".repeat(STORAGE_QUOTA)).is_err());
        assert_eq!(s.len(), 1);
        assert!(s.delete("k"));
        assert!(!s.delete("k"));
        s.flush().unwrap();
        assert!(!s.is_dirty());
    }

    #[test]
    fn persists_lazily_and_on_drop() {
        let dir =
            std::env::temp_dir().join(format!("lumenwall-script-storage-{}", std::process::id()));
        let path = dir.join("store.json");
        let _ = std::fs::remove_file(&path);
        {
            let s = StorageFile::open(&path);
            s.set("score", "42".into()).unwrap();
            s.flush_if_due(Instant::now(), Duration::from_secs(3600));
            assert!(!path.exists(), "write must be deferred");
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"score\":42}");
        let s = StorageFile::open(&path);
        assert_eq!(s.get("score").as_deref(), Some("42"));
        s.set("score", "43".into()).unwrap();
        s.flush_if_due(
            Instant::now() + Duration::from_secs(10),
            Duration::from_secs(1),
        );
        s.inner.lock().unwrap().join_writer();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"score\":43}");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
