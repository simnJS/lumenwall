//! The GitHub Releases API: one request per check, conditional on the last ETag.
//!
//! - Stable channel: `GET /repos/<repo>/releases/latest` (newest non-draft,
//!   non-prerelease release).
//! - Pre-releases: `GET /repos/<repo>/releases?per_page=20`, highest version wins.
//!
//! A release counts only if it has, for this platform, the archive, `SHA256SUMS` and
//! `SHA256SUMS.minisig`; anything else is ignored here and refused at download time
//! anyway.

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{Http, Kind, header};
use crate::state::{Cached, State};
use crate::{Channel, Error, SIGNATURE_SUFFIX, SUMS_NAME, Source};

/// Largest API answer read (a list of 20 releases with long notes is ~200 kB).
const MAX_JSON: u64 = 4 << 20;

/// A downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// A release that can update this platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// Release title (falls back to the tag).
    pub name: String,
    /// Release notes, Markdown as written on GitHub (see [`crate::notes`]).
    pub notes: String,
    /// The release page on GitHub.
    pub page: String,
    /// `YYYY-MM-DD`.
    pub published: Option<String>,
    pub prerelease: bool,
    pub archive: Asset,
    pub sums: Asset,
    pub sums_signature: Asset,
}

/// Result of a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Nothing newer than the running version for this platform and channel.
    UpToDate,
    Available(Box<Release>),
}

/// Asks GitHub for the newest release. Updates `state` (ETag cache, check times,
/// rate-limit deadline, `available`); the caller saves it.
///
/// Refuses to send anything while GitHub's rate limit is in force
/// ([`Error::RateLimited`]). Callers decide when to call: [`State::due`] for automatic
/// checks, any time for "Check now".
pub fn check(
    source: &Source,
    current: &Version,
    target: &str,
    channel: Channel,
    state: &mut State,
    now: u64,
) -> Result<Check, Error> {
    if state.rate_limited(now)
        && let Some(retry_at) = state.not_before
    {
        return Err(Error::RateLimited { retry_at });
    }
    let result = fetch(source, current, target, channel, state, now);
    state.last_check = Some(now);
    let retry = crate::RETRY_INTERVAL.as_secs();
    match &result {
        Ok(_) => {
            state.last_success = Some(now);
            state.next_check = Some(now + crate::CHECK_INTERVAL.as_secs());
            state.not_before = None;
        }
        Err(Error::RateLimited { retry_at }) => {
            state.not_before = Some(*retry_at);
            state.next_check = Some((*retry_at).max(now + retry));
        }
        Err(_) => state.next_check = Some(now + retry),
    }
    let best = result?;
    let newer = best.filter(|r| {
        &r.version > current && (channel == Channel::Prerelease || r.version.pre.is_empty())
    });
    state.available = newer.clone();
    Ok(match newer {
        Some(r) => Check::Available(Box::new(r)),
        None => Check::UpToDate,
    })
}

/// The best release the API offers for `target` (any version).
fn fetch(
    source: &Source,
    current: &Version,
    target: &str,
    channel: Channel,
    state: &mut State,
    now: u64,
) -> Result<Option<Release>, Error> {
    let url = match channel {
        Channel::Stable => source.api_url("releases/latest"),
        Channel::Prerelease => source.api_url("releases?per_page=20"),
    };
    let http = Http::new(source, &crate::user_agent(current), Kind::Api);
    let cached = state.cache.get(&url).cloned();
    let mut headers = vec![
        ("Accept", "application/vnd.github+json"),
        ("X-GitHub-Api-Version", "2022-11-28"),
    ];
    if let Some(c) = &cached {
        headers.push(("If-None-Match", c.etag.as_str()));
    }
    tracing::debug!(?channel, conditional = cached.is_some(), "update check");
    let mut resp = http.get(&url, &headers)?;
    let status = resp.status().as_u16();
    match status {
        304 => match cached {
            Some(c) => Ok(c.release),
            None => Err(Error::BadResponse("304 without a cached answer".into())),
        },
        200 => {
            let etag = header(&resp, "etag");
            let body = resp
                .body_mut()
                .with_config()
                .limit(MAX_JSON)
                .read_to_vec()
                .map_err(crate::http::map_err)?;
            let json: Value =
                serde_json::from_slice(&body).map_err(|_| Error::BadResponse("not JSON".into()))?;
            let best = match channel {
                Channel::Stable => release(&json, source, target),
                Channel::Prerelease => json
                    .as_array()
                    .ok_or_else(|| Error::BadResponse("expected a list of releases".into()))?
                    .iter()
                    .filter_map(|r| release(r, source, target))
                    .max_by(|a, b| a.version.cmp(&b.version)),
            };
            match etag {
                Some(etag) => {
                    state.cache.insert(
                        url,
                        Cached {
                            etag,
                            release: best.clone(),
                        },
                    );
                }
                None => {
                    state.cache.remove(&url);
                }
            }
            Ok(best)
        }
        // `releases/latest` of a repository without releases.
        404 => {
            state.cache.remove(&url);
            Ok(None)
        }
        403 | 429 => match rate_limit_deadline(&resp, now, status) {
            Some(retry_at) => Err(Error::RateLimited { retry_at }),
            None => Err(Error::Status(status)),
        },
        s => Err(Error::Status(s)),
    }
}

/// GitHub's rate-limit answer: `retry-after` (seconds), or `x-ratelimit-remaining: 0`
/// with `x-ratelimit-reset` (Unix seconds). A 429 without either waits an hour.
fn rate_limit_deadline(
    resp: &ureq::http::Response<ureq::Body>,
    now: u64,
    status: u16,
) -> Option<u64> {
    let num = |name: &str| header(resp, name).and_then(|v| v.parse::<u64>().ok());
    if let Some(after) = num("retry-after") {
        return Some(now + after.clamp(1, 24 * 3600));
    }
    if header(resp, "x-ratelimit-remaining").as_deref() == Some("0") {
        let reset = num("x-ratelimit-reset").unwrap_or(now + 3600);
        return Some(reset.clamp(now + 1, now + 24 * 3600));
    }
    (status == 429).then_some(now + 3600)
}

/// Reads one release object; `None` if it is a draft, its tag is not a version, or
/// it lacks a file this platform needs.
fn release(v: &Value, source: &Source, target: &str) -> Option<Release> {
    if v.get("draft").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let tag = v.get("tag_name")?.as_str()?.to_owned();
    let version = crate::parse_tag(&tag)?;
    let assets: Vec<Asset> = v
        .get("assets")?
        .as_array()?
        .iter()
        .filter_map(|a| asset(a, source))
        .collect();
    let find = |name: &str| assets.iter().find(|a| a.name == name).cloned();
    let archive_name = crate::archive_name(&version, target);
    let archive = find(&archive_name)?;
    let sums = find(SUMS_NAME)?;
    let sums_signature = find(&format!("{SUMS_NAME}{SIGNATURE_SUFFIX}"))?;
    let page = v
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|u| crate::starts_with_ignore_case(u, &source.page_prefix()))
        .map_or_else(|| source.releases_page(), str::to_owned);
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&tag)
        .to_owned();
    let published = v
        .get("published_at")
        .and_then(Value::as_str)
        .and_then(|d| d.get(..10))
        .filter(|d| {
            d.bytes().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
        })
        .map(str::to_owned);
    Some(Release {
        version,
        tag,
        name,
        notes: v
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        page,
        published,
        prerelease: v.get("prerelease").and_then(Value::as_bool) == Some(true),
        archive,
        sums,
        sums_signature,
    })
}

fn asset(a: &Value, source: &Source) -> Option<Asset> {
    if a.get("state")
        .and_then(Value::as_str)
        .is_some_and(|s| s != "uploaded")
    {
        return None;
    }
    let url = a.get("browser_download_url")?.as_str()?;
    if !crate::starts_with_ignore_case(url, &source.download_prefix()) {
        return None;
    }
    Some(Asset {
        name: a.get("name")?.as_str()?.to_owned(),
        url: url.to_owned(),
        size: a.get("size")?.as_u64()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const T: &str = "x86_64-unknown-linux-gnu";

    fn dl(name: &str) -> Value {
        json!({
            "name": name,
            "state": "uploaded",
            "size": 10,
            "browser_download_url":
                format!("https://github.com/simnJS/lumenwall/releases/download/v0.2.0/{name}"),
        })
    }

    fn full() -> Value {
        let a = "lumenwall-0.2.0-x86_64-unknown-linux-gnu.tar.gz";
        json!({
            "tag_name": "v0.2.0",
            "name": "Lumenwall 0.2.0",
            "draft": false,
            "prerelease": false,
            "body": "## What's Changed\n* Faster",
            "html_url": "https://github.com/simnJS/lumenwall/releases/tag/v0.2.0",
            "published_at": "2026-11-03T10:00:00Z",
            // What the release workflow publishes: no signature per file, one signed list.
            "assets": [
                dl("Lumenwall-Setup.exe"),
                dl(a),
                dl("lumenwall-0.2.0-amd64.deb"),
                dl("install.sh"),
                dl("SHA256SUMS"),
                dl("SHA256SUMS.minisig"),
            ],
        })
    }

    fn without(name: &str) -> Value {
        let mut r = full();
        r["assets"]
            .as_array_mut()
            .unwrap()
            .retain(|a| a["name"] != name);
        r
    }

    #[test]
    fn reads_a_complete_release() {
        let s = Source::new(crate::API_BASE, crate::REPOSITORY, KEY).unwrap();
        let r = release(&full(), &s, T).unwrap();
        assert_eq!(r.version, Version::new(0, 2, 0));
        assert_eq!(r.published.as_deref(), Some("2026-11-03"));
        assert_eq!(r.archive.size, 10);
        assert_eq!(
            r.archive.name,
            "lumenwall-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(r.sums.name, SUMS_NAME);
        assert_eq!(r.sums_signature.name, "SHA256SUMS.minisig");
        assert!(r.page.ends_with("/tag/v0.2.0"));
        // No Windows build in it.
        assert!(release(&full(), &s, "x86_64-pc-windows-msvc").is_none());
    }

    #[test]
    fn ignores_incomplete_or_foreign_releases() {
        let s = Source::new(crate::API_BASE, crate::REPOSITORY, KEY).unwrap();
        let mut draft = full();
        draft["draft"] = json!(true);
        assert!(release(&draft, &s, T).is_none());
        assert!(release(&without("SHA256SUMS.minisig"), &s, T).is_none());
        assert!(release(&without("SHA256SUMS"), &s, T).is_none());
        assert!(
            release(
                &without("lumenwall-0.2.0-x86_64-unknown-linux-gnu.tar.gz"),
                &s,
                T
            )
            .is_none()
        );
        // The installer, the .deb and install.sh are not needed by the updater.
        assert!(release(&without("Lumenwall-Setup.exe"), &s, T).is_some());
        let mut foreign = full();
        foreign["assets"][1]["browser_download_url"] =
            json!("https://evil.example/lumenwall-0.2.0-x86_64-unknown-linux-gnu.tar.gz");
        assert!(release(&foreign, &s, T).is_none());
        let mut foreign_sig = full();
        foreign_sig["assets"][5]["browser_download_url"] =
            json!("https://evil.example/SHA256SUMS.minisig");
        assert!(release(&foreign_sig, &s, T).is_none());
        let mut bad_tag = full();
        bad_tag["tag_name"] = json!("latest");
        assert!(release(&bad_tag, &s, T).is_none());
        let mut bad_page = full();
        bad_page["html_url"] = json!("https://evil.example/");
        assert_eq!(
            release(&bad_page, &s, T).unwrap().page,
            crate::RELEASES_PAGE
        );
    }
}
