//! What the updater remembers between runs: when it last asked, the ETag of each
//! answer (so an unchanged one costs no request quota), GitHub's rate-limit deadline,
//! and the update just installed. A small JSON file in the cache directory; deleting
//! it only means the next start checks again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{CHECK_INTERVAL, Release};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Last request to the API (Unix seconds), successful or not.
    pub last_check: Option<u64>,
    /// Last successful answer.
    pub last_success: Option<u64>,
    /// Earliest next automatic check.
    pub next_check: Option<u64>,
    /// GitHub's rate limit: no request at all before this time.
    pub not_before: Option<u64>,
    /// Per API URL: the ETag of the last answer and what it contained.
    pub cache: BTreeMap<String, Cached>,
    /// Newest release found by the last check, if newer than the running version.
    pub available: Option<Release>,
    /// Written just before the restart into a new version; the new app reads it once
    /// to say "updated".
    pub installed: Option<InstalledNote>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cached {
    pub etag: String,
    /// The best release that answer offered for this platform (any version).
    pub release: Option<Release>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledNote {
    pub from: String,
    pub to: String,
    pub at: u64,
}

impl State {
    /// Default location: `<cache dir>/lumenwall/update.json`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::cache_dir().map(|d| d.join("lumenwall").join("update.json"))
    }

    /// Reads the file; a missing or unreadable file gives the default state.
    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("{}: {e}; starting over", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes the file atomically (temporary file, then rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    }

    /// Whether an automatic check may run at `now` (Unix seconds): never before
    /// GitHub's rate-limit deadline, and at most once per [`CHECK_INTERVAL`] (an hour
    /// after a failure). A clock that went back is not trusted to delay checks for days.
    pub fn due(&self, now: u64) -> bool {
        if self.rate_limited(now) {
            return false;
        }
        let interval = CHECK_INTERVAL.as_secs();
        match (self.next_check, self.last_check) {
            (Some(next), _) => now >= next || next > now.saturating_add(2 * interval),
            (None, Some(last)) => now >= last.saturating_add(interval) || last > now + interval,
            (None, None) => true,
        }
    }

    /// GitHub asked to wait until `not_before`.
    pub fn rate_limited(&self, now: u64) -> bool {
        self.not_before
            .is_some_and(|t| now < t && t <= now + 24 * 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;

    #[test]
    fn checks_at_most_every_twelve_hours() {
        let t0 = 1_800_000_000;
        let mut s = State::default();
        assert!(s.due(t0), "never checked");
        s.last_check = Some(t0);
        s.next_check = Some(t0 + 12 * H);
        assert!(!s.due(t0 + 1));
        assert!(!s.due(t0 + 12 * H - 1));
        assert!(s.due(t0 + 12 * H));
    }

    #[test]
    fn rate_limit_blocks_checks() {
        let t0 = 1_800_000_000;
        let s = State {
            next_check: Some(t0),
            not_before: Some(t0 + 600),
            ..State::default()
        };
        assert!(!s.due(t0 + 10));
        assert!(s.due(t0 + 600));
        // A deadline absurdly far away (bad header, clock change) is ignored.
        let far = State {
            not_before: Some(t0 + 30 * 24 * H),
            ..State::default()
        };
        assert!(far.due(t0));
    }

    #[test]
    fn clock_going_back_does_not_block_for_days() {
        let t0 = 1_800_000_000;
        let s = State {
            last_check: Some(t0 + 10 * 24 * H),
            next_check: Some(t0 + 10 * 24 * H + 12 * H),
            ..State::default()
        };
        assert!(s.due(t0));
    }

    #[test]
    fn round_trip_and_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/update.json");
        let s = State {
            last_check: Some(5),
            installed: Some(InstalledNote {
                from: "0.1.0".into(),
                to: "0.2.0".into(),
                at: 6,
            }),
            ..State::default()
        };
        s.save(&path).unwrap();
        assert_eq!(State::load(&path), s);
        std::fs::write(&path, br#"{"last_check": 7, "unknown": true}"#).unwrap();
        assert_eq!(State::load(&path).last_check, Some(7));
        std::fs::write(&path, b"garbage").unwrap();
        assert_eq!(State::load(&path), State::default());
    }
}
