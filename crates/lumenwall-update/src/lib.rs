//! Signed self-update of Lumenwall from its GitHub Releases.
//!
//! The app (`lumenwall-ui`) drives these steps, each one blocking (run them off the UI
//! thread):
//!
//! 1. [`availability`]: may this copy update itself at all? Not when a package manager
//!    (Flatpak, Snap, a distribution, Scoop, winget…) installed it, when its folder is
//!    read-only, for development builds, or when the build has no release key.
//! 2. [`check`]: one request to the GitHub Releases API, with `If-None-Match` so an
//!    unchanged answer is an empty `304`. [`State::due`] limits automatic checks to one
//!    every [`CHECK_INTERVAL`]; nothing here runs on a timer.
//! 3. [`download`]: `SHA256SUMS` and its minisign signature, verified with the public
//!    key compiled into the binary ([`RELEASE_PUBLIC_KEY`]), then the archive for this
//!    platform, whose SHA-256 must equal its signed line. An archive that fails the
//!    check is deleted, never extracted.
//! 4. [`install`]: replaces the programs next to the running one atomically (written
//!    beside them, then renamed), with rollback if any step fails.
//! 5. [`restart_daemon`]: asks the running engine to stop over IPC, waits until it is
//!    gone, then starts the new one.
//!
//! Privacy: requests carry a generic `Lumenwall/<version>` User-Agent and nothing about
//! the user (no token, no identifier). GitHub sees the IP address, as for any download.

mod archive;
mod download;
mod error;
mod github;
mod http;
mod install;
mod managed;
pub mod notes;
mod restart;
mod state;
mod verify;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use download::{Verified, download, verify_local};
pub use error::Error;
pub use github::{Asset, Check, Release, check};
pub use install::{BUNDLE_MARKER, Installed, Layout, PROGRAMS, cleanup, install};
pub use managed::{Availability, Manager, PACKAGED_MARKER, availability, dev_override};
pub use restart::{DaemonRestart, restart_daemon};
pub use semver::Version;
pub use state::{Cached, InstalledNote, State};

/// `owner/name` of the GitHub repository releases come from.
pub const REPOSITORY: &str = "simnJS/lumenwall";
/// Base URL of the GitHub REST API.
pub const API_BASE: &str = "https://api.github.com";
/// Human-readable list of releases (shown when the app cannot update itself).
pub const RELEASES_PAGE: &str = "https://github.com/simnJS/lumenwall/releases";

/// Minimum time between two automatic checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 3600);
/// After a failed check (offline, server error), the next automatic one waits this long.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(3600);

/// The release signing key: the second line of the production `minisign.pub`
/// (base64, starts with `RW`).
///
/// `None` would disable the updater ([`Availability::NoKey`]): there would be nothing to verify
/// against. `docs/RELEASING.md` explains how the key pair was created and where its secret half
/// lives (repository secrets, never in git).
pub const RELEASE_PUBLIC_KEY: Option<&str> =
    Some("RWQt/7tWeHNqM8Z/BqlP0VmbIHU4DhM9iWA8dzM7rfrHXK5ZxtoIHMbY");

/// Rust target triple whose release archive this build installs, if releases exist
/// for this platform.
pub const TARGET: Option<&str> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("x86_64-unknown-linux-gnu")
} else if cfg!(all(windows, target_arch = "x86_64")) {
    // The release is built with MSVC; a MinGW build of the same code updates to it.
    Some("x86_64-pc-windows-msvc")
} else {
    None
};

/// Which releases count as updates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Releases only (the default).
    #[default]
    Stable,
    /// Releases and pre-releases (`0.3.0-beta.1`).
    Prerelease,
}

/// File name of the release archive of `version` for `target`.
pub fn archive_name(version: &Version, target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("lumenwall-{version}-{target}.{ext}")
}

/// Name of the signed checksum list published with every release.
pub const SUMS_NAME: &str = "SHA256SUMS";
/// Suffix of a minisign signature file.
pub const SIGNATURE_SUFFIX: &str = ".minisig";

/// The `Lumenwall/<version>` User-Agent (GitHub requires one; it says nothing about the user).
pub fn user_agent(version: &Version) -> String {
    format!("Lumenwall/{version}")
}

/// Where releases come from and the key that must have signed them.
#[derive(Debug, Clone)]
pub struct Source {
    api_base: String,
    repository: String,
    key: minisign_verify::PublicKey,
    /// `http://127.0.0.1:<port>`-style test server: every URL must stay on it.
    local: bool,
}

impl Source {
    /// The official repository on github.com, verified with [`RELEASE_PUBLIC_KEY`].
    /// `None` when this build has no release key.
    pub fn official() -> Option<Self> {
        let key = RELEASE_PUBLIC_KEY?;
        match Self::new(API_BASE, REPOSITORY, key) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("invalid release public key in this build: {e}");
                None
            }
        }
    }

    /// `api_base` is [`API_BASE`], or a loopback `http://127.0.0.1:<port>` server (tests,
    /// development). `public_key` is the base64 line of a `minisign.pub` file, or the
    /// whole file.
    pub fn new(api_base: &str, repository: &str, public_key: &str) -> Result<Self, Error> {
        let api_base = api_base.trim_end_matches('/').to_owned();
        let local = if api_base == API_BASE {
            false
        } else if is_loopback_http(&api_base) {
            true
        } else {
            return Err(Error::Refused(api_base));
        };
        let key = if public_key.trim().contains('\n') {
            minisign_verify::PublicKey::decode(public_key.trim())
        } else {
            minisign_verify::PublicKey::from_base64(public_key.trim())
        }
        .map_err(|e| Error::Signature(format!("unreadable public key: {e}")))?;
        let valid_repo = repository.split_once('/').is_some_and(|(owner, name)| {
            [owner, name].iter().all(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            })
        });
        if !valid_repo {
            return Err(Error::BadResponse(format!(
                "bad repository name {repository}"
            )));
        }
        Ok(Self {
            api_base,
            repository: repository.to_owned(),
            key,
            local,
        })
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// The page listing the releases (official source: [`RELEASES_PAGE`]).
    pub fn releases_page(&self) -> String {
        if self.local {
            format!("{}/{}/releases", self.api_base, self.repository)
        } else {
            format!("https://github.com/{}/releases", self.repository)
        }
    }

    pub(crate) fn api_url(&self, path: &str) -> String {
        format!("{}/repos/{}/{path}", self.api_base, self.repository)
    }

    /// Prefix every asset download URL must start with.
    pub(crate) fn download_prefix(&self) -> String {
        if self.local {
            format!("{}/", self.api_base)
        } else {
            format!("https://github.com/{}/releases/download/", self.repository)
        }
    }

    /// Prefix of release pages (`html_url`).
    pub(crate) fn page_prefix(&self) -> String {
        if self.local {
            format!("{}/", self.api_base)
        } else {
            format!("https://github.com/{}/releases/", self.repository)
        }
    }

    pub(crate) fn local_origin(&self) -> Option<&str> {
        self.local.then_some(self.api_base.as_str())
    }

    pub(crate) fn key(&self) -> &minisign_verify::PublicKey {
        &self.key
    }
}

fn is_loopback_http(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let host = rest.rsplit_once(':').map_or(rest, |(h, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) && !port.is_empty() {
            h
        } else {
            rest
        }
    });
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// Starts with `prefix`, comparing ASCII case-insensitively (GitHub owner and
/// repository names are case-insensitive in URLs).
pub(crate) fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Parses a release tag (`v1.2.3`, `1.2.3-rc.1`).
pub fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

    #[test]
    fn archive_names() {
        let v = Version::parse("1.2.3-rc.1").unwrap();
        assert_eq!(
            archive_name(&v, "x86_64-unknown-linux-gnu"),
            "lumenwall-1.2.3-rc.1-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            archive_name(&v, "x86_64-pc-windows-msvc"),
            "lumenwall-1.2.3-rc.1-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn tags() {
        assert_eq!(parse_tag("v0.2.0"), Some(Version::new(0, 2, 0)));
        assert_eq!(parse_tag("0.2.0"), Some(Version::new(0, 2, 0)));
        assert!(parse_tag("v0.2").is_none());
        assert!(parse_tag("nightly").is_none());
        assert!(parse_tag("v1.0.0-rc.1").unwrap() < parse_tag("v1.0.0").unwrap());
    }

    #[test]
    fn sources_only_github_or_loopback() {
        assert!(Source::new(API_BASE, REPOSITORY, KEY).is_ok());
        assert!(Source::new("http://127.0.0.1:8080", REPOSITORY, KEY).is_ok());
        assert!(Source::new("http://localhost:1", REPOSITORY, KEY).is_ok());
        assert!(Source::new("https://evil.example", REPOSITORY, KEY).is_err());
        assert!(Source::new("http://127.0.0.1.evil.example:80", REPOSITORY, KEY).is_err());
        assert!(Source::new("http://api.github.com", REPOSITORY, KEY).is_err());
        assert!(Source::new(API_BASE, "simnJS/../x", KEY).is_err());
        assert!(Source::new(API_BASE, REPOSITORY, "not a key").is_err());
        let full = format!("untrusted comment: minisign public key\n{KEY}\n");
        assert!(Source::new(API_BASE, REPOSITORY, &full).is_ok());
    }

    #[test]
    fn source_urls() {
        let s = Source::new(API_BASE, REPOSITORY, KEY).unwrap();
        assert_eq!(
            s.api_url("releases/latest"),
            "https://api.github.com/repos/simnJS/lumenwall/releases/latest"
        );
        assert_eq!(
            s.download_prefix(),
            "https://github.com/simnJS/lumenwall/releases/download/"
        );
        assert_eq!(s.releases_page(), RELEASES_PAGE);
        assert!(starts_with_ignore_case(
            "https://github.com/SimnJS/Lumenwall/releases/download/v1/x",
            &s.download_prefix()
        ));
    }
}
