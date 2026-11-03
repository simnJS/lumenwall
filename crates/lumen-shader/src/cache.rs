//! On-disk cache of compiled programs: one file per key, containing the two
//! naga modules and the reflection, serialized with postcard and protected by
//! a blake3 checksum. Writes are atomic (temporary file + rename).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::backend::CompiledProgram;

const MAGIC: &[u8; 8] = b"LUMSHD01";
/// Bumped whenever the translation or the serialized layout changes.
pub const FORMAT_VERSION: u32 = 1;

pub struct DiskCache {
    dir: PathBuf,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl DiskCache {
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(DiskCache { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: &blake3::Hash) -> PathBuf {
        self.dir.join(format!("{}.lshader", key.to_hex()))
    }

    /// Loads an entry; a missing, truncated or corrupt file is a miss.
    pub fn load(&self, key: &blake3::Hash) -> Option<CompiledProgram> {
        let data = fs::read(self.path(key)).ok()?;
        if data.len() < 8 + 32 || &data[..8] != MAGIC {
            return None;
        }
        let (sum, payload) = data[8..].split_at(32);
        if blake3::hash(payload).as_bytes() != sum {
            tracing::warn!("corrupt shader cache entry {}", key.to_hex());
            return None;
        }
        postcard::from_bytes(payload).ok()
    }

    pub fn store(&self, key: &blake3::Hash, program: &CompiledProgram) -> io::Result<()> {
        let payload = postcard::to_allocvec(program).map_err(|e| io::Error::other(e.to_string()))?;
        let mut data = Vec::with_capacity(payload.len() + 40);
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(blake3::hash(&payload).as_bytes());
        data.extend_from_slice(&payload);
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join(format!(".{}.{}.{}.tmp", key.to_hex(), std::process::id(), n));
        fs::write(&tmp, &data)?;
        let r = fs::rename(&tmp, self.path(key));
        if r.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        r
    }
}
