//! Compiled shader bytecode on disk: one file per program, `<key>.dxbc` in the folder the engine
//! gives (`lumenwall_engine::cache_dir()` + `dxbc`: `%LOCALAPPDATA%\Lumenwall\cache\dxbc`).
//!
//! File layout: magic `LUMDXBC1`, format version (u32 LE), blake3 of the payload (32 bytes),
//! payload = vertex bytecode length (u32 LE), vertex bytecode, pixel bytecode. A missing,
//! truncated, foreign or corrupt file is a miss (the program is compiled again and the file
//! rewritten); writes go to a temporary file renamed into place, so a crash or a second process
//! never leaves half a file under a valid name.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"LUMDXBC1";
/// Bumped whenever the file layout changes.
pub const FORMAT_VERSION: u32 = 1;
const HEADER: usize = 8 + 4 + 32;

/// Bytecode of one program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bytecode {
    pub vs: Vec<u8>,
    pub ps: Vec<u8>,
}

/// What `D3DCompile` is asked for; part of the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompileOptions {
    /// `D3DCOMPILE_*` flags (`D3DCOMPILE_OPTIMIZATION_LEVEL3` by default).
    pub flags: u32,
}

impl CompileOptions {
    /// `D3DCOMPILE_OPTIMIZATION_LEVEL3` (Microsoft documentation, "D3DCOMPILE Constants").
    pub const OPTIMIZATION_LEVEL3: u32 = 1 << 15;
    pub const VERTEX_PROFILE: &'static str = "vs_5_0";
    pub const PIXEL_PROFILE: &'static str = "ps_5_0";
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            flags: Self::OPTIMIZATION_LEVEL3,
        }
    }
}

/// Identity of a cache entry: blake3 over a domain tag, the format and generator versions, the
/// profiles and flags, then the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey([u8; 32]);

impl CacheKey {
    fn hasher(options: CompileOptions) -> blake3::Hasher {
        let mut h = blake3::Hasher::new();
        h.update(b"lumenwall-d3d11/dxbc\0");
        h.update(&FORMAT_VERSION.to_le_bytes());
        h.update(&crate::hlsl::GENERATOR_VERSION.to_le_bytes());
        h.update(CompileOptions::VERTEX_PROFILE.as_bytes());
        h.update(CompileOptions::PIXEL_PROFILE.as_bytes());
        h.update(&options.flags.to_le_bytes());
        h
    }

    /// From the HLSL itself (source and entry points): any change upstream (naga version,
    /// translation, transforms) changes the key. Costs the naga → HLSL step on every load
    /// (2–4 ms per scene, tools/d3d11-probe).
    #[must_use]
    pub fn for_hlsl(
        source: &str,
        vertex_entry: &str,
        fragment_entry: &str,
        options: CompileOptions,
    ) -> Self {
        let mut h = Self::hasher(options);
        h.update(b"hlsl\0");
        for part in [source, vertex_entry, fragment_entry] {
            h.update(&(part.len() as u64).to_le_bytes());
            h.update(part.as_bytes());
        }
        Self(*h.finalize().as_bytes())
    }

    /// From an identity the caller vouches for (lumenwall-shader's program key plus the version
    /// of every transform applied after it, naga's version included): a hit skips naga → HLSL.
    #[must_use]
    pub fn for_identity(identity: &[u8], options: CompileOptions) -> Self {
        let mut h = Self::hasher(options);
        h.update(b"identity\0");
        h.update(&(identity.len() as u64).to_le_bytes());
        h.update(identity);
        Self(*h.finalize().as_bytes())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn file_name(&self) -> String {
        let mut s = String::with_capacity(64 + 5);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s.push_str(".dxbc");
        s
    }
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The bytecode cache folder.
#[derive(Debug)]
pub struct BytecodeCache {
    dir: PathBuf,
}

impl BytecodeCache {
    /// Opens (and creates) the folder.
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: &CacheKey) -> PathBuf {
        self.dir.join(key.file_name())
    }

    /// The entry of `key`, `None` on any miss.
    #[must_use]
    pub fn load(&self, key: &CacheKey) -> Option<Bytecode> {
        let data = fs::read(self.path(key)).ok()?;
        let parsed = decode(&data);
        if parsed.is_none() {
            tracing::warn!("discarding invalid shader cache entry {}", key.file_name());
        }
        parsed
    }

    /// Writes the entry of `key` (atomically).
    pub fn store(&self, key: &CacheKey, code: &Bytecode) -> io::Result<()> {
        let data = encode(code)?;
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join(format!(
            ".{}.{}.{n}.tmp",
            key.file_name(),
            std::process::id()
        ));
        fs::write(&tmp, &data)?;
        let r = fs::rename(&tmp, self.path(key));
        if r.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        r
    }

    /// Removes every entry (measurements of cold starts).
    pub fn clear(&self) -> io::Result<()> {
        for e in fs::read_dir(&self.dir)? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "dxbc") {
                fs::remove_file(p)?;
            }
        }
        Ok(())
    }
}

fn encode(code: &Bytecode) -> io::Result<Vec<u8>> {
    let vs_len =
        u32::try_from(code.vs.len()).map_err(|_| io::Error::other("bytecode too large"))?;
    let mut payload = Vec::with_capacity(4 + code.vs.len() + code.ps.len());
    payload.extend_from_slice(&vs_len.to_le_bytes());
    payload.extend_from_slice(&code.vs);
    payload.extend_from_slice(&code.ps);
    let mut data = Vec::with_capacity(HEADER + payload.len());
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    data.extend_from_slice(blake3::hash(&payload).as_bytes());
    data.extend_from_slice(&payload);
    Ok(data)
}

fn decode(data: &[u8]) -> Option<Bytecode> {
    if data.len() < HEADER + 4 || &data[..8] != MAGIC {
        return None;
    }
    if u32::from_le_bytes(data[8..12].try_into().ok()?) != FORMAT_VERSION {
        return None;
    }
    let (sum, payload) = data[12..].split_at(32);
    if blake3::hash(payload).as_bytes() != sum {
        return None;
    }
    let n = u32::from_le_bytes(payload[..4].try_into().ok()?) as usize;
    let vs = payload.get(4..4 + n)?.to_vec();
    let ps = payload.get(4 + n..)?.to_vec();
    if vs.is_empty() || ps.is_empty() {
        return None;
    }
    Some(Bytecode { vs, ps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code() -> Bytecode {
        Bytecode {
            vs: b"DXBC-vertex".to_vec(),
            ps: b"DXBC-pixel-shader".to_vec(),
        }
    }

    #[test]
    fn round_trip_and_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BytecodeCache::new(dir.path().join("dxbc")).unwrap();
        let key = CacheKey::for_hlsl(
            "float4 main() : SV_Target { return 0; }",
            "vs",
            "ps",
            CompileOptions::default(),
        );
        assert_eq!(cache.load(&key), None);
        cache.store(&key, &code()).unwrap();
        assert_eq!(cache.load(&key), Some(code()));
        // No temporary file left behind.
        let names: Vec<_> = fs::read_dir(cache.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec![key.file_name()]);
        cache.clear().unwrap();
        assert_eq!(cache.load(&key), None);
    }

    #[test]
    fn corrupt_truncated_or_foreign_files_are_misses() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BytecodeCache::new(dir.path()).unwrap();
        let key = CacheKey::for_identity(b"program", CompileOptions::default());
        cache.store(&key, &code()).unwrap();
        let path = cache.path(&key);
        let good = fs::read(&path).unwrap();

        let mut flipped = good.clone();
        *flipped.last_mut().unwrap() ^= 1;
        fs::write(&path, &flipped).unwrap();
        assert_eq!(cache.load(&key), None, "checksum");

        fs::write(&path, &good[..good.len() - 3]).unwrap();
        assert_eq!(cache.load(&key), None, "truncated");

        let mut other_version = good.clone();
        other_version[8] = other_version[8].wrapping_add(1);
        fs::write(&path, &other_version).unwrap();
        assert_eq!(cache.load(&key), None, "format version");

        fs::write(&path, b"LUMSHD01 not ours").unwrap();
        assert_eq!(cache.load(&key), None, "foreign");

        cache.store(&key, &code()).unwrap();
        assert_eq!(cache.load(&key), Some(code()), "rewritten after a miss");
    }

    #[test]
    fn keys_change_with_every_input() {
        let o = CompileOptions::default();
        let base = CacheKey::for_hlsl("src", "vs", "ps", o);
        assert_eq!(base, CacheKey::for_hlsl("src", "vs", "ps", o));
        assert_ne!(base, CacheKey::for_hlsl("src2", "vs", "ps", o));
        assert_ne!(base, CacheKey::for_hlsl("src", "vs2", "ps", o));
        assert_ne!(base, CacheKey::for_hlsl("src", "vs", "ps2", o));
        // Length prefixes: moving bytes between parts changes the key.
        assert_ne!(
            CacheKey::for_hlsl("ab", "c", "d", o),
            CacheKey::for_hlsl("a", "bc", "d", o)
        );
        assert_ne!(
            base,
            CacheKey::for_hlsl("src", "vs", "ps", CompileOptions { flags: 0 })
        );
        assert_ne!(
            CacheKey::for_identity(b"src", o),
            CacheKey::for_hlsl("src", "", "", o)
        );
        assert_eq!(base.file_name().len(), 64 + 5);
    }
}
