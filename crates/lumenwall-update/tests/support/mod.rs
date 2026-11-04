//! A fake GitHub for tests and development: the Releases API endpoints the updater
//! calls, and release assets signed with the TEST key in `tests/keys` (never the
//! release key). Plain HTTP on 127.0.0.1, one thread per connection, `Connection:
//! close` on every answer.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lumenwall_update::{SIGNATURE_SUFFIX, SUMS_NAME, Version, archive_name};
use serde_json::{Value, json};

pub const TEST_PUBLIC_KEY: &str = include_str!("../keys/TEST-ONLY-lumenwall-update.pub");
const TEST_SECRET_KEY: &str = include_str!("../keys/TEST-ONLY-lumenwall-update.key");
/// The Windows installer's name in every release (ignored by the updater).
pub const SETUP_NAME: &str = "Lumenwall-Setup.exe";

/// `SHA256SUMS.minisig`, the one signature of a release.
pub fn sums_signature_name() -> String {
    format!("{SUMS_NAME}{SIGNATURE_SUFFIX}")
}

pub fn target() -> &'static str {
    lumenwall_update::TARGET.unwrap_or("x86_64-unknown-linux-gnu")
}

pub fn test_secret_key() -> minisign::SecretKey {
    minisign::SecretKeyBox::from_string(TEST_SECRET_KEY)
        .and_then(minisign::SecretKeyBox::into_unencrypted_secret_key)
        .expect("test secret key")
}

/// Another key, unknown to the updater.
pub fn stranger_key() -> minisign::SecretKey {
    minisign::KeyPair::generate_unencrypted_keypair()
        .expect("key pair")
        .sk
}

/// A `.minisig` file for `data`, whose trusted comment names `file` (like the release
/// workflow's signatures).
pub fn sign(sk: &minisign::SecretKey, data: &[u8], file: &str) -> Vec<u8> {
    let comment = format!("timestamp:1800000000\tfile:{file}\tlumenwall test release");
    minisign::sign(
        None,
        sk,
        data,
        Some(&comment),
        Some("signature from the lumenwall-update TEST key"),
    )
    .expect("sign")
    .into_string()
    .into_bytes()
}

pub fn sha256_hex(data: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Content of the fake programs in the release of `version`.
pub fn program_bytes(name: &str, version: &Version) -> Vec<u8> {
    format!("#!/bin/sh\n# fake {name} {version}\nexit 0\n").into_bytes()
}

/// Files of a release archive besides the programs, the bundle marker and the licence,
/// the same in every version, some in folders: on Windows the web host, FFmpeg's DLLs
/// and the CEF runtime with its `locales` folder (packaging/release/windows-runtime.toml),
/// on Linux the desktop files under `share/`.
pub fn runtime_files() -> Vec<(&'static str, &'static [u8])> {
    if target().contains("windows") {
        vec![
            ("lumenwall-web-host.exe", b"MZ fake web host"),
            ("avcodec-62.dll", b"MZ fake avcodec"),
            ("libcef.dll", b"MZ fake libcef"),
            ("locales/en-US.pak", b"fake pak"),
            ("licenses/FFmpeg-LGPL-3.0.txt", b"LGPL-3.0 text\n"),
        ]
    } else {
        vec![(
            "share/applications/lumenwall-ui.desktop",
            b"[Desktop Entry]\n",
        )]
    }
}

/// A release archive in this platform's format: one top folder with the programs,
/// the bundle marker, a licence file and [`runtime_files`] (plus `padding` bytes of
/// filler, to watch the progress bar in the app).
pub fn release_archive(version: &Version, padding: usize) -> Vec<u8> {
    let exe = std::env::consts::EXE_SUFFIX;
    let top = format!("lumenwall-{version}-{}", target());
    let mut files: Vec<(String, Vec<u8>, u32)> = vec![
        (
            format!("lumenwall{exe}"),
            program_bytes("lumenwall", version),
            0o755,
        ),
        (
            format!("lumenwall-ui{exe}"),
            program_bytes("lumenwall-ui", version),
            0o755,
        ),
        (
            "lumenwall.bundle".into(),
            format!("{} {version}\n", target()).into_bytes(),
            0o644,
        ),
        ("LICENSE-MIT".into(), b"MIT licence text\n".to_vec(), 0o644),
    ];
    for (name, data) in runtime_files() {
        let mode = if name.ends_with(".exe") { 0o755 } else { 0o644 };
        files.push((name.into(), data.to_vec(), mode));
    }
    if padding > 0 {
        // Incompressible, so the download really is that large.
        let mut x: u32 = 0x9e37_79b9;
        let filler = (0..padding)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        files.push(("share/padding.bin".into(), filler, 0o644));
    }
    if target().contains("windows") {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        z.add_directory(format!("{top}/"), opts).unwrap();
        for (name, data, mode) in &files {
            z.start_file(format!("{top}/{name}"), opts.unix_permissions(*mode))
                .unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        out.into_inner()
    } else {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        let mut dir = tar::Header::new_gnu();
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_mode(0o755);
        dir.set_size(0);
        b.append_data(&mut dir, format!("{top}/"), std::io::empty())
            .unwrap();
        for (name, data, mode) in &files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(*mode);
            b.append_data(&mut h, format!("{top}/{name}"), data.as_slice())
                .unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }
}

/// One release as the fake server publishes it.
pub struct FakeRelease {
    pub version: Version,
    pub prerelease: bool,
    pub notes: String,
    /// Asset name → content.
    pub assets: HashMap<String, Vec<u8>>,
    /// Asset name → size announced by the API (default: the real size).
    pub announced: HashMap<String, u64>,
}

impl FakeRelease {
    /// A correct release, as the release workflow publishes it: the archive, the
    /// installer, `install.sh`, `SHA256SUMS` listing them all, and `SHA256SUMS.minisig`
    /// (the only signature).
    pub fn signed(version: &str) -> Self {
        Self::signed_padded(version, 0)
    }

    /// Same, with an archive of at least `padding` bytes.
    pub fn signed_padded(version: &str, padding: usize) -> Self {
        let version = Version::parse(version).unwrap();
        let name = archive_name(&version, target());
        let mut assets = HashMap::new();
        assets.insert(name, release_archive(&version, padding));
        assets.insert(SETUP_NAME.to_owned(), b"MZ fake installer".to_vec());
        assets.insert("install.sh".to_owned(), b"#!/bin/sh\n".to_vec());
        let mut release = Self {
            prerelease: !version.pre.is_empty(),
            version,
            notes: "## What's Changed\n* Faster scene loading in https://github.com/simnJS/lumenwall/pull/12\n".into(),
            assets,
            announced: HashMap::new(),
        };
        release.sign_sums();
        release
    }

    /// Writes `SHA256SUMS` for every asset (except the list and its signature) and
    /// signs it with the test key, like `release_sign sign`.
    pub fn sign_sums(&mut self) {
        let mut names: Vec<&String> = self
            .assets
            .keys()
            .filter(|n| !n.ends_with(SIGNATURE_SUFFIX) && n.as_str() != SUMS_NAME)
            .collect();
        names.sort();
        let sums: String = names
            .into_iter()
            .map(|n| format!("{}  {n}\n", sha256_hex(&self.assets[n])))
            .collect();
        self.set_sums(sums.into_bytes(), &test_secret_key());
    }

    /// Replaces `SHA256SUMS` with `sums`, signed by `sk`.
    pub fn set_sums(&mut self, sums: Vec<u8>, sk: &minisign::SecretKey) {
        self.assets
            .insert(sums_signature_name(), sign(sk, &sums, SUMS_NAME));
        self.assets.insert(SUMS_NAME.to_owned(), sums);
    }

    pub fn archive_name(&self) -> String {
        archive_name(&self.version, target())
    }

    fn tag(&self) -> String {
        format!("v{}", self.version)
    }

    fn json(&self, base: &str) -> Value {
        let tag = self.tag();
        let mut names: Vec<&String> = self.assets.keys().collect();
        names.sort();
        let assets: Vec<Value> = names
            .into_iter()
            .map(|name| {
                let size = self
                    .announced
                    .get(name)
                    .copied()
                    .unwrap_or(self.assets[name].len() as u64);
                json!({
                    "name": name,
                    "state": "uploaded",
                    "size": size,
                    "browser_download_url":
                        format!("{base}/simnJS/lumenwall/releases/download/{tag}/{name}"),
                })
            })
            .collect();
        json!({
            "tag_name": tag,
            "name": format!("Lumenwall {}", self.version),
            "draft": false,
            "prerelease": self.prerelease,
            "body": self.notes,
            "html_url": format!("{base}/simnJS/lumenwall/releases/tag/{tag}"),
            "published_at": "2026-11-03T12:00:00Z",
            "assets": assets,
        })
    }
}

#[derive(Default)]
pub struct Behaviour {
    pub releases: Vec<FakeRelease>,
    /// Answer API calls with GitHub's "rate limit exceeded".
    pub rate_limited: bool,
    /// Send downloads to another origin (must be refused).
    pub foreign_redirect: bool,
    /// Serve release files at about 2 MB/s.
    pub slow: bool,
}

/// Every request seen: `"<path> <if-none-match or ->"`.
pub type Log = Arc<Mutex<Vec<String>>>;

pub struct FakeGitHub {
    pub base: String,
    pub log: Log,
    pub behaviour: Arc<Mutex<Behaviour>>,
    stop: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
}

impl FakeGitHub {
    pub fn start(releases: Vec<FakeRelease>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let behaviour = Arc::new(Mutex::new(Behaviour {
            releases,
            ..Behaviour::default()
        }));
        let log: Log = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (b, l, s, base2) = (
            Arc::clone(&behaviour),
            Arc::clone(&log),
            Arc::clone(&stop),
            base.clone(),
        );
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let (b, l, base) = (Arc::clone(&b), Arc::clone(&l), base2.clone());
                std::thread::spawn(move || {
                    let _ = serve(conn, &b, &l, &base);
                });
            }
        });
        Self {
            base,
            log,
            behaviour,
            stop,
            addr,
        }
    }

    pub fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn api_requests(&self) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.starts_with("/repos/"))
            .count()
    }

    pub fn source(&self) -> lumenwall_update::Source {
        lumenwall_update::Source::new(&self.base, lumenwall_update::REPOSITORY, TEST_PUBLIC_KEY)
            .expect("source")
    }
}

impl Drop for FakeGitHub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }
}

fn serve(conn: TcpStream, b: &Mutex<Behaviour>, log: &Log, base: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let mut if_none_match = None;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.trim().eq_ignore_ascii_case("if-none-match")
        {
            if_none_match = Some(v.trim().to_owned());
        }
    }
    log.lock().unwrap().push(format!(
        "{path} {}",
        if_none_match.as_deref().unwrap_or("-")
    ));
    let b = b.lock().unwrap();
    let mut out = conn;
    let api = "/repos/simnJS/lumenwall/releases";
    if path.starts_with(api) && b.rate_limited {
        let reset = lumenwall_update::now() + 600;
        let body = br#"{"message":"API rate limit exceeded"}"#;
        return respond(
            &mut out,
            "403 Forbidden",
            &[
                ("x-ratelimit-remaining", "0".into()),
                ("x-ratelimit-reset", reset.to_string()),
            ],
            body,
        );
    }
    if path == format!("{api}/latest") || path.starts_with(&format!("{api}?")) {
        let list = !path.ends_with("/latest");
        let body = if list {
            let mut all: Vec<&FakeRelease> = b.releases.iter().collect();
            all.sort_by(|x, y| y.version.cmp(&x.version));
            Value::Array(all.iter().map(|r| r.json(base)).collect())
        } else {
            match b
                .releases
                .iter()
                .filter(|r| !r.prerelease)
                .max_by(|x, y| x.version.cmp(&y.version))
            {
                Some(r) => r.json(base),
                None => return respond(&mut out, "404 Not Found", &[], b"{}"),
            }
        };
        let body = serde_json::to_vec(&body).unwrap();
        let etag = format!("\"{}\"", &sha256_hex(&body)[..16]);
        if if_none_match.as_deref() == Some(etag.as_str()) {
            return respond(&mut out, "304 Not Modified", &[("etag", etag)], b"");
        }
        return respond(&mut out, "200 OK", &[("etag", etag)], &body);
    }
    // github.com/<repo>/releases/download/<tag>/<name> → storage, like GitHub.
    if let Some(rest) = path.strip_prefix("/simnJS/lumenwall/releases/download/") {
        let to = if b.foreign_redirect {
            format!("http://127.0.0.2:9/storage/{rest}")
        } else {
            format!("{base}/storage/{rest}?sig=opaque")
        };
        return respond(&mut out, "302 Found", &[("location", to)], b"");
    }
    if let Some(rest) = path.strip_prefix("/storage/") {
        let rest = rest.split('?').next().unwrap_or_default();
        if let Some((tag, name)) = rest.split_once('/')
            && let Some(r) = b.releases.iter().find(|r| r.tag() == tag)
            && let Some(data) = r.assets.get(name)
        {
            if b.slow {
                let data = data.clone();
                drop(b);
                return respond_slowly(&mut out, &data);
            }
            return respond(&mut out, "200 OK", &[], data);
        }
    }
    respond(&mut out, "404 Not Found", &[], b"")
}

fn respond_slowly(out: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    out.write_all(head.as_bytes())?;
    for chunk in body.chunks(64 << 10) {
        out.write_all(chunk)?;
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    out.flush()
}

fn respond(
    out: &mut TcpStream,
    status: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    out.write_all(body)?;
    out.flush()?;
    // Let the client read everything before the socket closes.
    let _ = out.shutdown(std::net::Shutdown::Write);
    let mut sink = [0u8; 256];
    let _ = out.read(&mut sink);
    Ok(())
}
