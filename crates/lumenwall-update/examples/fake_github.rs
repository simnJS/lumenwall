//! A fake GitHub serving one release signed with the TEST key (`tests/keys`), to try
//! the update flow in a debug build of the app without publishing anything:
//!
//! ```sh
//! cargo run -p lumenwall-update --example fake_github -- 0.2.0 [--slow] [--bad-signature|--bad-checksum|--rate-limit]
//! # then, in another terminal, with the three variables it prints:
//! LUMENWALL_UPDATE_SERVER=… LUMENWALL_UPDATE_KEY=… LUMENWALL_UPDATE_DIR=… cargo run -p lumenwall-ui
//! ```
//!
//! The app then installs into `LUMENWALL_UPDATE_DIR` (a scratch folder with fake
//! programs), never over itself, and does not restart anything.

#[path = "../tests/support/mod.rs"]
mod support;

use lumenwall_update::{SIGNATURE_SUFFIX, SUMS_NAME};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let version = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "9.9.9".into());
    let mut release = support::FakeRelease::signed_padded(&version, 24 << 20);
    release.notes = NOTES.replace("{v}", &version);
    let name = release.archive_name();
    if flag("--bad-signature") {
        let sig = support::sign(&support::stranger_key(), &release.assets[&name], &name);
        release
            .assets
            .insert(format!("{name}{SIGNATURE_SUFFIX}"), sig);
    }
    if flag("--bad-checksum") {
        let sums = format!("{}  {name}\n", "0".repeat(64)).into_bytes();
        let sig = support::sign(&support::test_secret_key(), &sums, SUMS_NAME);
        release
            .assets
            .insert(format!("{SUMS_NAME}{SIGNATURE_SUFFIX}"), sig);
        release.assets.insert(SUMS_NAME.into(), sums);
    }
    let gh = support::FakeGitHub::start(vec![release]);
    {
        let mut b = gh.behaviour.lock().unwrap();
        b.slow = flag("--slow");
        b.rate_limited = flag("--rate-limit");
    }

    let dir = std::env::temp_dir().join("lumenwall-fake-install");
    std::fs::create_dir_all(&dir).expect("scratch folder");
    let exe = std::env::consts::EXE_SUFFIX;
    for p in ["lumenwall", "lumenwall-ui"] {
        let old = support::program_bytes(p, &lumenwall_update::Version::new(0, 0, 1));
        std::fs::write(dir.join(format!("{p}{exe}")), old).expect("fake program");
    }
    std::fs::write(dir.join(lumenwall_update::BUNDLE_MARKER), "fake\n").expect("marker");

    let key = support::TEST_PUBLIC_KEY.lines().nth(1).unwrap_or_default();
    println!("Fake GitHub with Lumenwall {version} (TEST key). In another terminal:\n");
    println!(
        "LUMENWALL_UPDATE_SERVER={} LUMENWALL_UPDATE_KEY={key} LUMENWALL_UPDATE_DIR={} cargo run -p lumenwall-ui\n",
        gh.base,
        dir.display()
    );
    println!("Ctrl+C to stop.");
    loop {
        std::thread::park();
    }
}

const NOTES: &str = "<!-- Release notes generated using configuration in .github/release.yml -->

## What's Changed
### Engine
* Video wallpapers decode on the GPU on more drivers by @contributor in https://github.com/simnJS/lumenwall/pull/41
* Scenes with many particles use 30 % less CPU in https://github.com/simnJS/lumenwall/pull/44
### App
* Settings: choose the update channel in https://github.com/simnJS/lumenwall/pull/45
* Fix the tray icon on KDE Plasma 6 in https://github.com/simnJS/lumenwall/pull/47

**Full Changelog**: https://github.com/simnJS/lumenwall/compare/v0.1.0...v{v}
";
