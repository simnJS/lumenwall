//! Signs, then checks, a release before it is published (`.github/workflows/release.yml`,
//! `docs/RELEASING.md`):
//!
//! ```sh
//! release_sign sign   --version 0.2.0 dist/   # SHA256SUMS of every file (lumenwall-0.2.0-*,
//!                                             # Lumenwall-Setup.exe, install.sh) and
//!                                             # SHA256SUMS.minisig, the only signature
//! release_sign verify --version 0.2.0 dist/   # each archive exactly as the app checks it
//! ```
//!
//! `sign` reads the secret key from `MINISIGN_SECRET_KEY` (the whole `minisign.key`
//! file) and its password from `MINISIGN_PASSWORD` (empty for a key made with
//! `minisign -G -W`). It refuses a key whose public half is not the
//! `RELEASE_PUBLIC_KEY` compiled into this release: the app could not verify it.
//! The signed list binds each file's name to its SHA-256, so it vouches for every
//! file at once; a `.minisig` per file would add nothing and is refused.
//!
//! `verify` checks every archive with `RELEASE_PUBLIC_KEY` through the app's own code
//! (`lumenwall_update::verify_local`), then installs this platform's archive into a
//! temporary folder with `lumenwall_update::install`.
//!
//! `--public-key <base64>` replaces `RELEASE_PUBLIC_KEY` (tests with the test key).
//! The secret key is never printed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lumenwall_update::{
    API_BASE, Layout, REPOSITORY, SIGNATURE_SUFFIX, SUMS_NAME, Source, Version, archive_name,
};

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"];
/// Assets with the same name in every release (`releases/latest/download/<name>`).
const UNVERSIONED: [&str; 2] = ["Lumenwall-Setup.exe", "install.sh"];

struct Args {
    command: String,
    version: Version,
    dir: PathBuf,
    public_key: Option<String>,
    targets: Vec<String>,
}

fn main() -> ExitCode {
    match parse().and_then(|a| match a.command.as_str() {
        "sign" => sign(&a),
        "verify" => verify(&a),
        other => Err(format!("unknown command {other} (sign or verify)")),
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("release_sign: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it
        .next()
        .ok_or("usage: release_sign sign|verify --version V DIR")?;
    let (mut version, mut dir, mut public_key, mut targets) = (None, None, None, Vec::new());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" => version = it.next(),
            "--public-key" => public_key = it.next(),
            "--target" => targets.extend(it.next()),
            _ if !a.starts_with("--") => dir = Some(PathBuf::from(a)),
            _ => return Err(format!("unknown option {a}")),
        }
    }
    let version = version.ok_or("--version is required")?;
    let version = Version::parse(version.trim_start_matches('v'))
        .map_err(|e| format!("--version {version}: {e}"))?;
    if targets.is_empty() {
        targets = TARGETS.iter().map(|t| (*t).to_owned()).collect();
    }
    Ok(Args {
        command,
        version,
        dir: dir.ok_or("the release folder is required")?,
        public_key,
        targets,
    })
}

/// The public key releases must verify with.
fn release_key(a: &Args) -> Result<String, String> {
    a.public_key
        .clone()
        .or_else(|| lumenwall_update::RELEASE_PUBLIC_KEY.map(str::to_owned))
        .ok_or_else(|| {
            "RELEASE_PUBLIC_KEY is not set in crates/lumenwall-update/src/lib.rs: \
             the app of this release could not verify updates (docs/RELEASING.md)"
                .to_owned()
        })
}

fn sign(a: &Args) -> Result<(), String> {
    let text = std::env::var("MINISIGN_SECRET_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or("MINISIGN_SECRET_KEY is empty: add the repository secret (docs/RELEASING.md)")?;
    let password = std::env::var("MINISIGN_PASSWORD").unwrap_or_default();
    let boxed = minisign::SecretKeyBox::from_string(text.trim())
        .map_err(|_| "MINISIGN_SECRET_KEY is not a minisign secret key file")?;
    let sk = if password.is_empty() {
        minisign::SecretKey::from_unencrypted_box(boxed)
            .map_err(|_| "the secret key is encrypted: set the MINISIGN_PASSWORD secret")?
    } else {
        minisign::SecretKey::from_box(boxed, Some(password))
            .map_err(|_| "cannot decrypt the secret key: check MINISIGN_PASSWORD")?
    };
    let pk = minisign::PublicKey::from_secret_key(&sk)
        .map_err(|_| "cannot derive the public key")?
        .to_base64();
    let expected = release_key(a)?;
    if pk != expected.trim() {
        return Err(format!(
            "the signing key's public half ({pk}) is not RELEASE_PUBLIC_KEY ({}): \
             users could not verify this release",
            expected.trim()
        ));
    }

    let mut files = release_files(&a.dir)?;
    files.sort();
    let prefix = format!("lumenwall-{}-", a.version);
    if let Some(bad) = files
        .iter()
        .find(|f| !f.starts_with(&prefix) && !UNVERSIONED.contains(&f.as_str()))
    {
        return Err(format!("{bad} does not belong to release {}", a.version));
    }
    let mut sums = String::new();
    for f in &files {
        let data = std::fs::read(a.dir.join(f)).map_err(|e| format!("{f}: {e}"))?;
        let digest = ring::digest::digest(&ring::digest::SHA256, &data);
        let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        sums.push_str(&format!("{hex}  {f}\n"));
    }
    std::fs::write(a.dir.join(SUMS_NAME), &sums).map_err(|e| format!("{SUMS_NAME}: {e}"))?;
    for f in &files {
        println!("listed {f}");
    }

    // The app requires `file:SHA256SUMS`: a signature cannot be reused for another file.
    let trusted = format!(
        "timestamp:{}\tfile:{SUMS_NAME}\tlumenwall {}",
        lumenwall_update::now(),
        a.version
    );
    let sig = minisign::sign(
        None,
        &sk,
        sums.as_bytes(),
        Some(&trusted),
        Some("signature from the Lumenwall release key"),
    )
    .map_err(|e| format!("signing {SUMS_NAME}: {e}"))?;
    let out = a.dir.join(format!("{SUMS_NAME}{SIGNATURE_SUFFIX}"));
    std::fs::write(&out, sig.into_string()).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("signed {SUMS_NAME}");
    Ok(())
}

/// Release files to list in `SHA256SUMS`: everything except the list and its signature.
/// Any other `.minisig` is refused: releases are signed through `SHA256SUMS` only.
fn release_files(dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let sums_sig = format!("{SUMS_NAME}{SIGNATURE_SUFFIX}");
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !e.file_type().is_ok_and(|t| t.is_file()) || name == SUMS_NAME || name == sums_sig {
            continue;
        }
        if name.ends_with(SIGNATURE_SUFFIX) {
            return Err(format!(
                "{name}: releases have no signature per file, only {sums_sig}; remove it"
            ));
        }
        out.push(name);
    }
    if out.is_empty() {
        return Err(format!("nothing to sign in {}", dir.display()));
    }
    Ok(out)
}

fn verify(a: &Args) -> Result<(), String> {
    let source = Source::new(API_BASE, REPOSITORY, &release_key(a)?).map_err(|e| e.to_string())?;
    for target in &a.targets {
        let name = archive_name(&a.version, target);
        let v = lumenwall_update::verify_local(&source, &a.dir, &a.version, target)
            .map_err(|e| format!("{name}: {e}"))?;
        println!("verified {name} (sha256 {})", v.sha256);
        if Some(target.as_str()) == lumenwall_update::TARGET {
            try_install(&v).map_err(|e| format!("{name}: {e}"))?;
            println!("installed {name} into a temporary folder");
        }
    }
    Ok(())
}

/// Installs a copy of the archive into a temporary bundle folder, as the app would.
fn try_install(v: &lumenwall_update::Verified) -> Result<(), String> {
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let copy = scratch
        .path()
        .join(v.archive.file_name().ok_or("bad archive path")?);
    std::fs::copy(&v.archive, &copy).map_err(|e| e.to_string())?;
    let app = scratch.path().join("app");
    std::fs::create_dir(&app).map_err(|e| e.to_string())?;
    std::fs::write(app.join(lumenwall_update::BUNDLE_MARKER), "").map_err(|e| e.to_string())?;
    let verified = lumenwall_update::Verified {
        archive: copy,
        ..v.clone()
    };
    let installed =
        lumenwall_update::install(&verified, &Layout::of_dir(&app)).map_err(|e| e.to_string())?;
    for p in ["lumenwall", "lumenwall-ui"] {
        let path = installed.layout.program(p);
        if !path.is_file() {
            return Err(format!("{} missing after installation", path.display()));
        }
    }
    Ok(())
}
