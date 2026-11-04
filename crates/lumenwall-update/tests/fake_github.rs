//! End-to-end: the updater against a fake GitHub on 127.0.0.1 serving releases whose
//! `SHA256SUMS` is signed with the TEST key (`tests/keys`): up to date, new version
//! installed, missing or invalid signature, checksum mismatch, network down, rate
//! limit, pre-releases, redirects.

mod support;

use std::path::Path;
use std::sync::atomic::AtomicBool;

use lumenwall_update::{
    BUNDLE_MARKER, Channel, Check, Error, Layout, SUMS_NAME, Source, State, Version, check,
    download, install,
};
use support::{
    FakeGitHub, FakeRelease, program_bytes, sha256_hex, sign, stranger_key, sums_signature_name,
    target,
};

const NOW: u64 = 1_800_000_000;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn available(gh: &FakeGitHub, current: &str, channel: Channel) -> lumenwall_update::Release {
    let mut state = State::default();
    match check(
        &gh.source(),
        &v(current),
        target(),
        channel,
        &mut state,
        NOW,
    ) {
        Ok(Check::Available(r)) => *r,
        other => panic!("expected an update, got {other:?}"),
    }
}

fn fetch(
    gh: &FakeGitHub,
    release: &lumenwall_update::Release,
    dir: &Path,
) -> Result<lumenwall_update::Verified, Error> {
    download(
        &gh.source(),
        &v("0.1.0"),
        release,
        dir,
        &AtomicBool::new(false),
        |_, _| {},
    )
}

/// The downloads folder holds nothing but (at most) the verified archive.
fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn up_to_date_then_conditional_request() {
    let gh = FakeGitHub::start(vec![FakeRelease::signed("0.1.0")]);
    let mut state = State::default();
    let r = check(
        &gh.source(),
        &v("0.1.0"),
        target(),
        Channel::Stable,
        &mut state,
        NOW,
    );
    assert_eq!(r, Ok(Check::UpToDate));
    assert_eq!(state.next_check, Some(NOW + 12 * 3600));
    assert!(!state.due(NOW + 60), "no new check before 12 h");
    assert!(state.due(NOW + 12 * 3600));

    // Second call: If-None-Match with the stored ETag, 304, same answer.
    let r = check(
        &gh.source(),
        &v("0.1.0"),
        target(),
        Channel::Stable,
        &mut state,
        NOW + 1,
    );
    assert_eq!(r, Ok(Check::UpToDate));
    let reqs = gh.requests();
    assert_eq!(reqs.len(), 2);
    assert!(
        reqs[0].ends_with(" -"),
        "first request is unconditional: {reqs:?}"
    );
    assert!(
        reqs[1].contains('"'),
        "second request sends the ETag: {reqs:?}"
    );

    // A newer local version is never "downgraded".
    let r = check(
        &gh.source(),
        &v("0.2.0"),
        target(),
        Channel::Stable,
        &mut state,
        NOW + 2,
    );
    assert_eq!(r, Ok(Check::UpToDate));
}

#[test]
fn new_version_is_downloaded_verified_and_installed() {
    let gh = FakeGitHub::start(vec![
        FakeRelease::signed("0.1.0"),
        FakeRelease::signed("0.2.0"),
    ]);
    let mut state = State::default();
    let release = match check(
        &gh.source(),
        &v("0.1.0"),
        target(),
        Channel::Stable,
        &mut state,
        NOW,
    ) {
        Ok(Check::Available(r)) => *r,
        other => panic!("{other:?}"),
    };
    assert_eq!(release.version, v("0.2.0"));
    assert_eq!(state.available.as_ref(), Some(&release));
    assert!(release.notes.contains("Faster scene loading"));
    assert_eq!(release.published.as_deref(), Some("2026-11-03"));

    // Download: progress goes up to the size, the archive is kept under its name.
    let downloads = tempfile::tempdir().unwrap();
    let mut seen = Vec::new();
    let verified = download(
        &gh.source(),
        &v("0.1.0"),
        &release,
        downloads.path(),
        &AtomicBool::new(false),
        |done, total| seen.push((done, total)),
    )
    .expect("verified download");
    let size = release.archive.size;
    assert_eq!(seen.last(), Some(&(size, size)));
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0));
    assert_eq!(
        leftovers(downloads.path()),
        std::slice::from_ref(&release.archive.name)
    );
    assert_eq!(
        verified.sha256,
        sha256_hex(&std::fs::read(&verified.archive).unwrap())
    );

    // Install into a bundle folder with the old programs and a user file.
    let app = tempfile::tempdir().unwrap();
    let exe = std::env::consts::EXE_SUFFIX;
    let old = v("0.1.0");
    for p in ["lumenwall", "lumenwall-ui"] {
        std::fs::write(app.path().join(format!("{p}{exe}")), program_bytes(p, &old)).unwrap();
    }
    std::fs::write(app.path().join(BUNDLE_MARKER), "old").unwrap();
    std::fs::write(app.path().join("notes.txt"), "user file").unwrap();
    let layout = Layout::of_dir(app.path());
    assert!(layout.bundle);
    let installed = install(&verified, &layout).expect("installed");
    assert_eq!(installed.version, v("0.2.0"));
    for p in ["lumenwall", "lumenwall-ui"] {
        assert_eq!(
            std::fs::read(layout.program(p)).unwrap(),
            program_bytes(p, &v("0.2.0")),
            "{p} replaced"
        );
    }
    assert!(
        app.path().join("LICENSE-MIT").is_file(),
        "bundle files added"
    );
    assert_eq!(
        std::fs::read_to_string(app.path().join("notes.txt")).unwrap(),
        "user file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(layout.program("lumenwall-ui"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "programs stay executable");
    }
    // No staging folder, no backups (Linux), no archive left.
    let mut names = leftovers(app.path());
    names.sort();
    #[cfg(unix)]
    assert_eq!(
        names,
        [
            "LICENSE-MIT",
            "lumenwall",
            "lumenwall-ui",
            BUNDLE_MARKER,
            "notes.txt"
        ]
    );
    assert!(!names.iter().any(|n| n.starts_with(".lumenwall-update")));
    assert!(leftovers(downloads.path()).is_empty());
}

#[test]
fn programs_outside_a_bundle_only_replace_what_is_there() {
    let gh = FakeGitHub::start(vec![FakeRelease::signed("0.2.0")]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    let downloads = tempfile::tempdir().unwrap();
    let verified = fetch(&gh, &release, downloads.path()).unwrap();
    // Like ~/.local/bin: the two programs among other things, no bundle marker.
    let bin = tempfile::tempdir().unwrap();
    let exe = std::env::consts::EXE_SUFFIX;
    for p in ["lumenwall", "lumenwall-ui", "something-else"] {
        std::fs::write(bin.path().join(format!("{p}{exe}")), b"old").unwrap();
    }
    let installed = install(&verified, &Layout::of_dir(bin.path())).unwrap();
    assert_eq!(installed.files.len(), 2);
    let mut names = leftovers(bin.path());
    names.sort();
    // Sorted the same way: with ".exe", "lumenwall-ui.exe" comes before "lumenwall.exe".
    let mut want = [
        format!("lumenwall{exe}"),
        format!("lumenwall-ui{exe}"),
        format!("something-else{exe}"),
    ];
    want.sort();
    assert_eq!(names, want);
}

#[test]
fn only_the_signed_list_and_the_archive_are_downloaded() {
    let r = FakeRelease::signed("0.2.0");
    let name = r.archive_name();
    let mut published: Vec<&str> = r.assets.keys().map(String::as_str).collect();
    published.sort_unstable();
    let mut want = vec![
        name.as_str(),
        "Lumenwall-Setup.exe",
        "SHA256SUMS",
        "SHA256SUMS.minisig",
        "install.sh",
    ];
    want.sort_unstable();
    assert_eq!(published, want, "no signature per file");

    let gh = FakeGitHub::start(vec![r]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    let dir = tempfile::tempdir().unwrap();
    fetch(&gh, &release, dir.path()).expect("verified with SHA256SUMS.minisig alone");
    let downloads: Vec<String> = gh
        .requests()
        .into_iter()
        .filter_map(|r| {
            let path = r.split(' ').next()?;
            path.strip_prefix("/simnJS/lumenwall/releases/download/v0.2.0/")
                .map(str::to_owned)
        })
        .collect();
    // The signature first, then the list, then the archive: nothing else.
    assert_eq!(downloads, [sums_signature_name(), SUMS_NAME.into(), name]);
}

#[test]
fn missing_sums_signature_is_refused() {
    // Not offered at all.
    let mut r = FakeRelease::signed("0.2.0");
    r.assets.remove(&sums_signature_name());
    let gh = FakeGitHub::start(vec![r]);
    let mut state = State::default();
    let found = check(
        &gh.source(),
        &v("0.1.0"),
        target(),
        Channel::Stable,
        &mut state,
        NOW,
    );
    assert_eq!(found, Ok(Check::UpToDate));

    // Removed after the check (the release kept from it): the download stops before
    // the archive.
    let gh = FakeGitHub::start(vec![FakeRelease::signed("0.2.0")]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    gh.behaviour.lock().unwrap().releases[0]
        .assets
        .remove(&sums_signature_name());
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(fetch(&gh, &release, dir.path()), Err(Error::Status(404)));
    assert!(leftovers(dir.path()).is_empty());
    let archive_path = format!("/{}", release.archive.name);
    assert!(
        !gh.requests().iter().any(|r| r.contains(&archive_path)),
        "the archive is never fetched"
    );
}

#[test]
fn invalid_signature_is_refused() {
    let sk = support::test_secret_key();
    let dir = tempfile::tempdir().unwrap();

    // SHA256SUMS signed by a key the updater does not know.
    let mut bad_sums = FakeRelease::signed("0.2.0");
    let sums = bad_sums.assets[SUMS_NAME].clone();
    bad_sums.set_sums(sums, &stranger_key());
    let gh = FakeGitHub::start(vec![bad_sums]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    assert!(matches!(
        fetch(&gh, &release, dir.path()),
        Err(Error::Signature(_))
    ));
    assert!(leftovers(dir.path()).is_empty(), "nothing kept");

    // The archive was replaced and its line in SHA256SUMS rewritten to match, but the
    // signature is the original one: the list no longer matches it.
    let mut tampered = FakeRelease::signed("0.2.0");
    let name = tampered.archive_name();
    let mut bytes = tampered.assets[&name].clone();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x55;
    let sums = format!("{}  {name}\n", sha256_hex(&bytes)).into_bytes();
    tampered.assets.insert(SUMS_NAME.into(), sums);
    tampered.assets.insert(name, bytes);
    let gh = FakeGitHub::start(vec![tampered]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    assert!(matches!(
        fetch(&gh, &release, dir.path()),
        Err(Error::Signature(_))
    ));
    assert!(leftovers(dir.path()).is_empty());

    // A valid signature by the release key, made for another file (replayed as the
    // list's signature).
    let mut replay = FakeRelease::signed("0.2.0");
    let sig = sign(&sk, &replay.assets[SUMS_NAME], "lumenwall-0.1.0-old.tar.gz");
    replay.assets.insert(sums_signature_name(), sig);
    let gh = FakeGitHub::start(vec![replay]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    assert!(matches!(
        fetch(&gh, &release, dir.path()),
        Err(Error::Signature(_))
    ));

    // An older release's list and signature, both genuine, served with a newer
    // archive: the newer archive is not in that list.
    let old = FakeRelease::signed("0.1.5");
    let mut mixed = FakeRelease::signed("0.2.0");
    mixed
        .assets
        .insert(SUMS_NAME.into(), old.assets[SUMS_NAME].clone());
    mixed.assets.insert(
        sums_signature_name(),
        old.assets[&sums_signature_name()].clone(),
    );
    let gh = FakeGitHub::start(vec![mixed]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    assert!(matches!(
        fetch(&gh, &release, dir.path()),
        Err(Error::Checksum(_))
    ));

    // Not a signature at all.
    let mut garbage = FakeRelease::signed("0.2.0");
    garbage
        .assets
        .insert(sums_signature_name(), b"not a minisign signature".to_vec());
    let gh = FakeGitHub::start(vec![garbage]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    assert!(matches!(
        fetch(&gh, &release, dir.path()),
        Err(Error::Signature(_))
    ));
    assert!(leftovers(dir.path()).is_empty());
}

#[test]
fn checksum_mismatch_is_refused() {
    // SHA256SUMS correctly signed, but its hash for the archive is wrong.
    let mut r = FakeRelease::signed("0.2.0");
    let name = r.archive_name();
    let sums = format!("{}  {name}\n", "0".repeat(64)).into_bytes();
    r.set_sums(sums, &support::test_secret_key());
    let gh = FakeGitHub::start(vec![r]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        fetch(&gh, &release, dir.path()),
        Err(Error::Checksum(_))
    ));
    assert!(leftovers(dir.path()).is_empty());

    // The archive modified after signing, the signed list left as it was.
    let mut r = FakeRelease::signed("0.2.0");
    let name = r.archive_name();
    r.assets.get_mut(&name).unwrap()[20] ^= 1;
    let gh = FakeGitHub::start(vec![r]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    assert!(matches!(
        fetch(&gh, &release, dir.path()),
        Err(Error::Checksum(_))
    ));
    assert!(leftovers(dir.path()).is_empty());

    // The archive missing from SHA256SUMS.
    let mut r = FakeRelease::signed("0.2.0");
    r.set_sums(Vec::new(), &support::test_secret_key());
    let gh = FakeGitHub::start(vec![r]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    assert!(matches!(
        fetch(&gh, &release, dir.path()),
        Err(Error::Checksum(_))
    ));
}

#[test]
fn larger_than_announced_is_refused() {
    let mut r = FakeRelease::signed("0.2.0");
    let name = r.archive_name();
    let real = r.assets[&name].len() as u64;
    r.announced.insert(name, real - 10);
    let gh = FakeGitHub::start(vec![r]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(fetch(&gh, &release, dir.path()), Err(Error::TooLarge));
    assert!(leftovers(dir.path()).is_empty());
}

#[test]
fn network_down() {
    // A port nothing listens on.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let source = Source::new(
        &format!("http://127.0.0.1:{port}"),
        lumenwall_update::REPOSITORY,
        support::TEST_PUBLIC_KEY,
    )
    .unwrap();
    let mut state = State::default();
    let r = check(
        &source,
        &v("0.1.0"),
        target(),
        Channel::Stable,
        &mut state,
        NOW,
    );
    assert_eq!(r, Err(Error::Offline));
    assert_eq!(state.last_check, Some(NOW));
    // Retried after an hour, not in a loop.
    assert_eq!(state.next_check, Some(NOW + 3600));
    assert!(!state.due(NOW + 60));
    assert!(state.due(NOW + 3600));
}

#[test]
fn rate_limit_is_respected() {
    let gh = FakeGitHub::start(vec![FakeRelease::signed("0.2.0")]);
    gh.behaviour.lock().unwrap().rate_limited = true;
    let mut state = State::default();
    let now = lumenwall_update::now();
    let r = check(
        &gh.source(),
        &v("0.1.0"),
        target(),
        Channel::Stable,
        &mut state,
        now,
    );
    let Err(Error::RateLimited { retry_at }) = r else {
        panic!("{r:?}");
    };
    assert!(retry_at > now && retry_at <= now + 601);
    assert_eq!(state.not_before, Some(retry_at));
    assert!(!state.due(now + 1));
    assert_eq!(gh.api_requests(), 1);

    // Even an explicit check sends nothing until the deadline.
    let r = check(
        &gh.source(),
        &v("0.1.0"),
        target(),
        Channel::Stable,
        &mut state,
        now + 5,
    );
    assert_eq!(r, Err(Error::RateLimited { retry_at }));
    assert_eq!(gh.api_requests(), 1);

    // After it, requests go out again.
    gh.behaviour.lock().unwrap().rate_limited = false;
    let r = check(
        &gh.source(),
        &v("0.1.0"),
        target(),
        Channel::Stable,
        &mut state,
        retry_at,
    );
    assert!(matches!(r, Ok(Check::Available(_))));
    assert_eq!(state.not_before, None);
}

#[test]
fn pre_releases_only_on_request() {
    let gh = FakeGitHub::start(vec![
        FakeRelease::signed("0.2.0"),
        FakeRelease::signed("0.3.0-beta.1"),
    ]);
    assert_eq!(available(&gh, "0.1.0", Channel::Stable).version, v("0.2.0"));
    let pre = available(&gh, "0.1.0", Channel::Prerelease);
    assert_eq!(pre.version, v("0.3.0-beta.1"));
    assert!(pre.prerelease);
    // A pre-release user moves on to the final release when it is newer.
    let gh = FakeGitHub::start(vec![
        FakeRelease::signed("0.3.0-beta.1"),
        FakeRelease::signed("0.3.0"),
    ]);
    assert_eq!(
        available(&gh, "0.3.0-beta.1", Channel::Prerelease).version,
        v("0.3.0")
    );
}

#[test]
fn downloads_never_leave_the_allowed_hosts() {
    let gh = FakeGitHub::start(vec![FakeRelease::signed("0.2.0")]);
    let release = available(&gh, "0.1.0", Channel::Stable);
    gh.behaviour.lock().unwrap().foreign_redirect = true;
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        fetch(&gh, &release, dir.path()),
        Err(Error::Refused(_))
    ));
}

#[test]
fn no_release_yet() {
    let gh = FakeGitHub::start(vec![]);
    let mut state = State::default();
    let r = check(
        &gh.source(),
        &v("0.1.0"),
        target(),
        Channel::Stable,
        &mut state,
        NOW,
    );
    assert_eq!(r, Ok(Check::UpToDate));
}

#[test]
fn release_files_on_disk_verify_like_downloads() {
    let r = FakeRelease::signed("0.2.0");
    let dir = tempfile::tempdir().unwrap();
    for (name, data) in &r.assets {
        std::fs::write(dir.path().join(name), data).unwrap();
    }
    let source = Source::new(
        lumenwall_update::API_BASE,
        lumenwall_update::REPOSITORY,
        support::TEST_PUBLIC_KEY,
    )
    .unwrap();
    let name = r.archive_name();
    let local = || lumenwall_update::verify_local(&source, dir.path(), &v("0.2.0"), target());
    let ok = local().unwrap();
    assert_eq!(ok.sha256, sha256_hex(&r.assets[&name]));

    // Archive modified: its hash is not the signed one.
    let mut bytes = r.assets[&name].clone();
    bytes[20] ^= 1;
    std::fs::write(dir.path().join(&name), bytes).unwrap();
    assert!(matches!(local(), Err(Error::Checksum(_))));
    std::fs::write(dir.path().join(&name), &r.assets[&name]).unwrap();

    // SHA256SUMS signed by another key.
    let sig = dir.path().join(sums_signature_name());
    std::fs::write(&sig, sign(&stranger_key(), &r.assets[SUMS_NAME], SUMS_NAME)).unwrap();
    assert!(matches!(local(), Err(Error::Signature(_))));

    // No SHA256SUMS.minisig.
    std::fs::remove_file(&sig).unwrap();
    assert!(matches!(local(), Err(Error::Io(_))));
}

#[test]
fn the_test_key_is_not_the_release_key() {
    let test_key = support::TEST_PUBLIC_KEY.lines().nth(1).unwrap().trim();
    assert_ne!(lumenwall_update::RELEASE_PUBLIC_KEY, Some(test_key));
}
