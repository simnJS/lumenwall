//! HTTP client limited to GitHub: the REST API, and release downloads with their
//! redirects to GitHub's storage hosts, each hop checked against an allow-list.
//! A loopback test server replaces all of them in tests.

use std::time::Duration;

use ureq::Body;
use ureq::http::{Response, Uri};

use crate::{Error, Source};

const API_HOST: &str = "api.github.com";
/// Where `github.com/<repo>/releases/download/...` redirects to.
const STORAGE_HOSTS: [&str; 4] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];
const MAX_HOPS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Api,
    Download,
}

pub(crate) struct Http<'a> {
    agent: ureq::Agent,
    source: &'a Source,
    kind: Kind,
}

impl<'a> Http<'a> {
    pub fn new(source: &'a Source, user_agent: &str, kind: Kind) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .user_agent(user_agent)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(match kind {
                Kind::Api => Duration::from_secs(30),
                // Archives of a few hundred MB on a slow link.
                Kind::Download => Duration::from_secs(3600),
            }))
            .https_only(source.local_origin().is_none())
            // Redirects are followed below, after checking where they go.
            .max_redirects(0)
            // 304, 403 and 429 carry headers we need.
            .http_status_as_error(false)
            .tls_config(tls)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            source,
            kind,
        }
    }

    /// GET `url`, following at most five redirects to allowed hosts. Any status is
    /// returned as is, except redirects.
    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Response<Body>, Error> {
        let mut url = url.to_owned();
        for hop in 0..=MAX_HOPS {
            self.allow(&url, hop)?;
            let mut req = self.agent.get(&url);
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            let resp = req.call().map_err(map_err)?;
            if !matches!(resp.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Ok(resp);
            }
            let location = header(&resp, "location")
                .ok_or_else(|| Error::BadResponse("redirect without a location".into()))?;
            url = resolve(&url, &location)?;
        }
        Err(Error::BadResponse("too many redirects".into()))
    }

    fn allow(&self, url: &str, hop: usize) -> Result<(), Error> {
        if let Some(origin) = self.source.local_origin() {
            return if url.starts_with(origin) && url[origin.len()..].starts_with('/') {
                Ok(())
            } else {
                Err(Error::Refused(host_of(url)))
            };
        }
        let uri: Uri = url
            .parse()
            .map_err(|_| Error::Refused("an invalid URL".into()))?;
        let host = uri.host().unwrap_or_default().to_ascii_lowercase();
        if uri.scheme_str() != Some("https") || uri.port().is_some() {
            return Err(Error::Refused(host));
        }
        let ok = match self.kind {
            // Same host for API redirects (renamed repositories).
            Kind::Api => host == API_HOST,
            // The first URL must be one of this repository's release assets.
            Kind::Download if hop == 0 => {
                crate::starts_with_ignore_case(url, &self.source.download_prefix())
            }
            Kind::Download => STORAGE_HOSTS.contains(&host.as_str()),
        };
        if ok {
            Ok(())
        } else {
            Err(Error::Refused(host))
        }
    }
}

/// A response header as text.
pub(crate) fn header(resp: &Response<Body>, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned())
}

fn resolve(base: &str, location: &str) -> Result<String, Error> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_owned());
    }
    if location.starts_with('/') && !location.starts_with("//") {
        let uri: Uri = base
            .parse()
            .map_err(|_| Error::BadResponse("bad redirect".into()))?;
        let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
            return Err(Error::BadResponse("bad redirect".into()));
        };
        return Ok(format!("{scheme}://{authority}{location}"));
    }
    Err(Error::BadResponse("bad redirect".into()))
}

fn host_of(url: &str) -> String {
    url.parse::<Uri>()
        .ok()
        .and_then(|u| u.host().map(str::to_owned))
        .unwrap_or_else(|| "an invalid URL".into())
}

/// Only the kind of failure is kept: ureq's messages may contain URLs.
pub(crate) fn map_err(e: ureq::Error) -> Error {
    match e {
        ureq::Error::BodyExceedsLimit(_) => Error::TooLarge,
        ureq::Error::Http(_)
        | ureq::Error::BadUri(_)
        | ureq::Error::Protocol(_)
        | ureq::Error::LargeResponseHeader(..)
        | ureq::Error::Decompress(..) => Error::BadResponse("malformed HTTP response".into()),
        ureq::Error::RequireHttpsOnly(_) | ureq::Error::TlsRequired => {
            Error::Refused("a plain HTTP URL".into())
        }
        _ => Error::Offline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

    #[test]
    fn download_hops_are_checked() {
        let s = Source::new(crate::API_BASE, crate::REPOSITORY, KEY).unwrap();
        let h = Http::new(&s, "Lumenwall/0", Kind::Download);
        let asset = "https://github.com/simnJS/lumenwall/releases/download/v1.0.0/x.tar.gz";
        assert!(h.allow(asset, 0).is_ok());
        assert!(
            h.allow("https://github.com/other/repo/releases/download/v1/x", 0)
                .is_err()
        );
        assert!(
            h.allow(
                "https://github.com/simnJS/lumenwall-evil/releases/download/v1/x",
                0
            )
            .is_err()
        );
        assert!(
            h.allow(
                "https://release-assets.githubusercontent.com/github-production-release-asset/1",
                1
            )
            .is_ok()
        );
        assert!(h.allow("https://evil.example/x", 1).is_err());
        assert!(
            h.allow("http://objects.githubusercontent.com/x", 1)
                .is_err()
        );
        assert!(
            h.allow("https://objects.githubusercontent.com:444/x", 1)
                .is_err()
        );
        let api = Http::new(&s, "Lumenwall/0", Kind::Api);
        assert!(
            api.allow(
                "https://api.github.com/repos/simnJS/lumenwall/releases/latest",
                0
            )
            .is_ok()
        );
        assert!(api.allow("https://github.com/x", 1).is_err());
    }

    #[test]
    fn local_source_stays_local() {
        let s = Source::new("http://127.0.0.1:9", crate::REPOSITORY, KEY).unwrap();
        let h = Http::new(&s, "Lumenwall/0", Kind::Download);
        assert!(h.allow("http://127.0.0.1:9/dl/x", 0).is_ok());
        assert!(h.allow("http://127.0.0.1:99/dl/x", 0).is_err());
        assert!(h.allow("https://github.com/x", 1).is_err());
    }

    #[test]
    fn relative_redirects() {
        assert_eq!(
            resolve("https://github.com/a/b", "/c/d").unwrap(),
            "https://github.com/c/d"
        );
        assert_eq!(
            resolve("https://github.com/a", "https://x.example/y").unwrap(),
            "https://x.example/y"
        );
        assert!(resolve("https://github.com/a", "//evil.example/x").is_err());
        assert!(resolve("https://github.com/a", "x").is_err());
    }
}
