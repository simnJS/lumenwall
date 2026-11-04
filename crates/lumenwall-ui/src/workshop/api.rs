//! Minimal client for the public Steam Web API and Steam's image CDN.
//!
//! Privacy and safety rules (AGENTS.md):
//! - requests go only to `api.steampowered.com`, `steamcommunity.com` and Steam's
//!   image CDNs (checked for every URL, including images returned by the API);
//! - the User-Agent is `Lumenwall/<version>`, nothing about the user;
//! - the API key is sent only to `api.steampowered.com`, never logged, never part of
//!   a cache file name or of an error message.
//!
//! Every call is blocking; callers run them on worker threads. Responses and images
//! are cached on disk with a time-to-live, so revisiting the page costs no request.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::{Item, Query};

const API_HOST: &str = "https://api.steampowered.com";
const PER_PAGE: u32 = 30;
const MAX_JSON: u64 = 8 << 20;
const MAX_IMAGE: u64 = 16 << 20;
pub const TTL_QUERY: Duration = Duration::from_secs(15 * 60);
pub const TTL_DETAILS: Duration = Duration::from_secs(6 * 3600);
pub const TTL_NAMES: Duration = Duration::from_secs(24 * 3600);
pub const TTL_IMAGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// Hosts images may come from (suffix match on a dot boundary).
const IMAGE_HOSTS: [&str; 5] = [
    "steamusercontent.com",
    "steamstatic.com",
    "steampowered.com",
    "steamcommunity.com",
    "steamuserimages-a.akamaihd.net",
];

/// Network requests made since start (for the measurements in docs/UI.md).
pub static REQUESTS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// No connection, DNS failure or timeout.
    Offline,
    /// HTTP 429: Steam's rate limit.
    Quota,
    /// HTTP 401/403: missing or rejected API key.
    Forbidden,
    /// Another HTTP status.
    Status(u16),
    /// The response was not what the documented API returns.
    BadResponse,
    /// A URL outside Steam's hosts was refused.
    Refused,
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::Offline => f.write_str("offline"),
            NetError::Quota => f.write_str("rate limited"),
            NetError::Forbidden => f.write_str("forbidden"),
            NetError::Status(s) => write!(f, "HTTP {s}"),
            NetError::BadResponse => f.write_str("unexpected response"),
            NetError::Refused => f.write_str("host not allowed"),
        }
    }
}

fn map_err(e: ureq::Error) -> NetError {
    // Only the kind is kept: ureq messages could contain the URL (and the key).
    match e {
        ureq::Error::StatusCode(429) => NetError::Quota,
        ureq::Error::StatusCode(401 | 403) => NetError::Forbidden,
        ureq::Error::StatusCode(s) => NetError::Status(s),
        ureq::Error::BodyExceedsLimit(_) | ureq::Error::Protocol(_) => NetError::BadResponse,
        _ => NetError::Offline,
    }
}

/// Test and demo override: an `http://127.0.0.1:<port>` mock standing in for every
/// Steam host. Set once, before the first request.
static BASE: OnceLock<String> = OnceLock::new();

pub fn use_mock_server(base: String) {
    let _ = BASE.set(base);
}

fn base() -> &'static str {
    BASE.get().map_or(API_HOST, String::as_str)
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .user_agent(concat!("Lumenwall/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(6)))
            .timeout_global(Some(Duration::from_secs(20)))
            .https_only(BASE.get().is_none())
            .max_redirects(2)
            .tls_config(tls)
            .build();
        ureq::Agent::new_with_config(config)
    })
}

/// Is `url` one we may fetch? HTTPS on Steam's hosts, or the configured mock.
pub fn allowed(url: &str) -> bool {
    allowed_with(url, BASE.get().map(String::as_str))
}

/// [`allowed`] with the mock base passed in, so tests do not depend on the process-wide
/// [`BASE`] another test may set at any time.
fn allowed_with(url: &str, mock: Option<&str>) -> bool {
    if let Some(mock) = mock {
        return url.starts_with(mock) && url[mock.len()..].starts_with('/');
    }
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    if uri.scheme_str() != Some("https") || uri.port().is_some() {
        return false;
    }
    let Some(host) = uri.host() else { return false };
    let host = host.to_ascii_lowercase();
    IMAGE_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

static CACHE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Tests: keep the response and image cache out of the user's cache directory.
#[cfg(test)]
pub fn use_cache_root(dir: PathBuf) {
    let _ = CACHE_ROOT.set(dir);
}

fn cache_root() -> Option<PathBuf> {
    CACHE_ROOT
        .get()
        .cloned()
        .or_else(|| crate::config::cache_dir().map(|d| d.join("workshop")))
}

fn fnv(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.as_bytes().iter().chain(std::iter::once(&0u8)) {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn fresh(path: &Path, ttl: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < ttl)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("part{}", std::process::id()));
    std::fs::File::create(&tmp)?.write_all(bytes)?;
    std::fs::rename(tmp, path)
}

/// JSON from the disk cache when fresh, else from `fetch` (then cached).
/// `cache_key` must not contain the API key.
fn cached_json(
    cache_key: &[&str],
    ttl: Duration,
    fetch: impl FnOnce() -> Result<Vec<u8>, NetError>,
) -> Result<Value, NetError> {
    let file = cache_root().map(|d| d.join("api").join(format!("{:016x}.json", fnv(cache_key))));
    if let Some(f) = &file
        && fresh(f, ttl)
        && let Ok(bytes) = std::fs::read(f)
        && let Ok(v) = serde_json::from_slice(&bytes)
    {
        return Ok(v);
    }
    let bytes = fetch()?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| NetError::BadResponse)?;
    if let Some(f) = &file
        && let Err(e) = write_atomic(f, &bytes)
    {
        tracing::debug!("workshop cache not written: {e}");
    }
    Ok(v)
}

fn get(path: &str, params: &[(String, String)], key: Option<&str>) -> Result<Vec<u8>, NetError> {
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    let mut req = agent().get(format!("{}{path}", base()));
    for (k, v) in params {
        req = req.query(k, v);
    }
    if let Some(k) = key {
        req = req.query("key", k);
    }
    tracing::debug!(endpoint = path, "Steam Web API request");
    let mut resp = req.call().map_err(map_err)?;
    resp.body_mut()
        .with_config()
        .limit(MAX_JSON)
        .read_to_vec()
        .map_err(map_err)
}

/// One page of `IPublishedFileService/QueryFiles` (needs the user's key).
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub items: Vec<Item>,
    pub next_cursor: Option<String>,
    pub total: u64,
}

pub fn query_files(query: &Query, cursor: &str, key: &str) -> Result<Page, NetError> {
    let params = query.params(cursor, PER_PAGE);
    let flat: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let mut cache_key: Vec<&str> = vec!["QueryFiles/v1"];
    cache_key.extend(flat.iter().map(String::as_str));
    let v = cached_json(&cache_key, TTL_QUERY, || {
        get("/IPublishedFileService/QueryFiles/v1/", &params, Some(key))
    })?;
    let r = v.get("response").ok_or(NetError::BadResponse)?;
    let items = r
        .get("publishedfiledetails")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Item::from_json).collect())
        .unwrap_or_default();
    let next = r
        .get("next_cursor")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty() && *c != cursor)
        .map(str::to_owned);
    let total = r.get("total").and_then(Value::as_u64).unwrap_or(0);
    Ok(Page {
        items,
        next_cursor: next,
        total,
    })
}

/// `ISteamRemoteStorage/GetPublishedFileDetails` (POST, no key): details of
/// installed items. At most 100 ids per call.
pub fn file_details(ids: &[String]) -> Result<Vec<Item>, NetError> {
    let mut out = Vec::new();
    for chunk in ids.chunks(100) {
        let mut sorted: Vec<&str> = chunk.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        let mut cache_key = vec!["GetPublishedFileDetails/v1"];
        cache_key.extend(sorted);
        let v = cached_json(&cache_key, TTL_DETAILS, || {
            REQUESTS.fetch_add(1, Ordering::Relaxed);
            let mut form: Vec<(String, String)> =
                vec![("itemcount".into(), chunk.len().to_string())];
            form.extend(
                chunk
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (format!("publishedfileids[{i}]"), id.clone())),
            );
            let mut resp = agent()
                .post(format!(
                    "{}/ISteamRemoteStorage/GetPublishedFileDetails/v1/",
                    base()
                ))
                .send_form(form.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .map_err(map_err)?;
            resp.body_mut()
                .with_config()
                .limit(MAX_JSON)
                .read_to_vec()
                .map_err(map_err)
        })?;
        let list = v
            .get("response")
            .and_then(|r| r.get("publishedfiledetails"))
            .and_then(Value::as_array);
        out.extend(list.into_iter().flatten().filter_map(Item::from_json));
    }
    Ok(out)
}

/// Persona names for SteamID64s, through `ISteamUser/GetPlayerSummaries` (key).
pub fn player_names(ids: &[String], key: &str) -> Result<HashMap<String, String>, NetError> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(100) {
        let mut sorted: Vec<&str> = chunk.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        sorted.dedup();
        let joined = sorted.join(",");
        let v = cached_json(&["GetPlayerSummaries/v2", &joined], TTL_NAMES, || {
            get(
                "/ISteamUser/GetPlayerSummaries/v2/",
                &[("steamids".into(), joined.clone())],
                Some(key),
            )
        })?;
        for p in v
            .get("response")
            .and_then(|r| r.get("players"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(id), Some(name)) = (
                p.get("steamid").and_then(Value::as_str),
                p.get("personaname").and_then(Value::as_str),
            ) {
                out.insert(id.to_owned(), name.to_owned());
            }
        }
    }
    Ok(out)
}

/// Local file for an image URL (whether or not it is downloaded yet).
pub fn image_path(url: &str) -> Option<PathBuf> {
    let ext = url
        .rsplit('/')
        .next()
        .and_then(|name| name.split('?').next())
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| matches!(e.as_str(), "gif" | "jpg" | "jpeg" | "png" | "webp"))
        .unwrap_or_else(|| "img".into());
    cache_root().map(|d| d.join("img").join(format!("{:016x}.{ext}", fnv(&[url]))))
}

/// Downloads `url` into `dest` unless a fresh copy is there.
pub fn download(url: &str, dest: &Path) -> Result<(), NetError> {
    if fresh(dest, TTL_IMAGE) {
        return Ok(());
    }
    if !allowed(url) {
        return Err(NetError::Refused);
    }
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    let mut resp = agent().get(url).call().map_err(map_err)?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(MAX_IMAGE)
        .read_to_vec()
        .map_err(map_err)?;
    write_atomic(dest, &bytes).map_err(|_| NetError::BadResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_steam_hosts_over_https() {
        // Checked without the process-wide mock base, which the mock server test sets
        // concurrently.
        let allowed = |url| allowed_with(url, None);
        assert!(allowed("https://images.steamusercontent.com/ugc/1/abc/"));
        assert!(allowed("https://shared.akamai.steamstatic.com/x.jpg"));
        assert!(allowed(
            "https://steamuserimages-a.akamaihd.net/ugc/1/x.gif"
        ));
        assert!(allowed("https://api.steampowered.com/x"));
        assert!(!allowed("http://images.steamusercontent.com/x.jpg"));
        assert!(!allowed("https://evil.com/steamusercontent.com/x"));
        assert!(!allowed("https://steamusercontent.com.evil.com/x"));
        assert!(!allowed("https://other.akamaihd.net/x"));
        assert!(!allowed("https://images.steamusercontent.com:8443/x"));
        assert!(!allowed("file:///etc/passwd"));
    }

    #[test]
    fn only_the_mock_when_one_is_set() {
        let mock = Some("http://127.0.0.1:21000");
        assert!(allowed_with("http://127.0.0.1:21000/x.gif", mock));
        assert!(!allowed_with("http://127.0.0.1:210001/x.gif", mock));
        assert!(!allowed_with("http://127.0.0.1:21000", mock));
        assert!(!allowed_with(
            "https://images.steamusercontent.com/ugc/1/abc/",
            mock
        ));
    }

    #[test]
    fn image_cache_names() {
        let a = image_path("https://images.steamusercontent.com/ugc/1/x.GIF?imw=5").unwrap();
        assert!(a.to_string_lossy().ends_with(".gif"));
        let b = image_path("https://images.steamusercontent.com/ugc/1/").unwrap();
        assert!(b.to_string_lossy().ends_with(".img"));
        assert_ne!(a, b);
    }
}
