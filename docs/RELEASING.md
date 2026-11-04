# Releasing Lumenwall

A release is a GitHub Release built by [`.github/workflows/release.yml`](../.github/workflows/release.yml) when a `v<version>` tag is pushed. The app's updater ([`crates/lumenwall-update`](../crates/lumenwall-update/README.md)) installs only files signed with the **release key**, whose public half is compiled into the app. This page covers the one-time key setup, each release, and what to do when something goes wrong.

| Asset | Content | Made by |
|---|---|---|
| `lumenwall-<version>-x86_64-unknown-linux-gnu.tar.gz` | `lumenwall`, `lumenwall-ui`, under `share/` the `.desktop` files, icons and the GNOME Shell extension, licences, `THIRD-PARTY-NOTICES.html` (cargo-about), `README.txt`, `lumenwall.bundle` | `packaging/release/package.py` |
| `lumenwall-<version>-x86_64-pc-windows-msvc.zip` | `lumenwall.exe`, `lumenwall-ui.exe`, licences, notices, `README.txt`, `lumenwall.bundle` | `packaging/release/package.py` |
| `lumenwall-<version>-x86_64-windows-setup.exe` | Windows installer of the zip's files: per user, `%LOCALAPPDATA%\Programs\Lumenwall`, Start menu shortcut, optional start with Windows, uninstaller | `packaging/windows/build-installer.sh` (NSIS, `lumenwall.nsi`) |
| `lumenwall-<version>-amd64.deb` | System-wide package: `/usr/bin`, `/usr/share/applications`, icons, `/usr/share/gnome-shell/extensions`; Depends from `dpkg-shlibdeps` plus `libvulkan1`, Recommends FFmpeg and a Vulkan driver | `packaging/release/deb.py` (`dpkg-deb`) |
| `install.sh` | The one-line installer (`curl -fsSL …/releases/latest/download/install.sh \| sh`), same file in every release | `packaging/linux/install.sh` |
| `<file>.minisig` | minisign signature of each file above | `release_sign sign` |
| `SHA256SUMS`, `SHA256SUMS.minisig` | SHA-256 of every file above, and its signature | `release_sign sign` |

The updater uses the two archives only. The installer and the `.deb` install the same programs: an installer copy (in the user's `%LOCALAPPDATA%`, marked by `lumenwall.bundle`) keeps updating itself; a `.deb` copy (`/usr/bin`) is recognised as a distribution package and never touched by the updater.

**Why NSIS for the Windows installer.** NSIS is under the zlib licence (OSI-approved, no condition on the installers it makes) and `makensis` runs on Linux (`apt install nsis`): the installer is built on an Ubuntu runner from the released zip itself, so it holds exactly the files the updater installs, and maintainers can rebuild it without Windows. It installs per user with `RequestExecutionLevel user`. Inno Setup would also fit (per-user installs, Restart Manager) but its compiler runs on Windows only, and since 6.5 it asks commercial users for a paid licence; WiX (`cargo-wix`) builds MSI packages on Windows only, per-user MSIs are awkward, and Windows Installer's file tracking would undo the in-app updater's replacements on a repair.

## One-time setup: the release key

Do this once, on a machine you trust, **outside the repository**. The secret key never goes into git; the test key in `crates/lumenwall-update/tests/keys/` is public and must never be used for releases.

1. Install [minisign](https://jedisct1.github.io/minisign/): `sudo apt install minisign`, `sudo pacman -S minisign`, `brew install minisign` or `scoop install minisign`.

2. Create the key pair, with a strong password (keep it in a password manager):

   ```sh
   mkdir -p ~/.lumenwall-release && chmod 700 ~/.lumenwall-release
   minisign -G -p ~/.lumenwall-release/lumenwall-release.pub -s ~/.lumenwall-release/lumenwall-release.key
   ```

   Without a password (`minisign -G -W ...`) works too, but then the file alone is enough to sign Lumenwall updates.

3. Back up `lumenwall-release.key` and its password offline, in two places. If both are lost, installed copies can no longer verify any update: users would have to download a new version by hand.

4. Give the secret to the workflow (repository secrets; `gh` reads the value from standard input, so it stays out of the shell history):

   ```sh
   gh secret set MINISIGN_SECRET_KEY --repo simnJS/lumenwall < ~/.lumenwall-release/lumenwall-release.key
   gh secret set MINISIGN_PASSWORD --repo simnJS/lumenwall   # paste the password at the prompt
   ```

   With a key made with `-W`, skip `MINISIGN_PASSWORD`.

5. Compile the public key into the app. Its second line (starting with `RW`) goes into `crates/lumenwall-update/src/lib.rs`:

   ```sh
   sed -n 2p ~/.lumenwall-release/lumenwall-release.pub
   ```

   ```rust
   pub const RELEASE_PUBLIC_KEY: Option<&str> = Some("RW...");
   ```

   Commit it ("Set the release signing key") and add the same line under [Verify a download](#verify-a-download) below. Until this constant is set, the updater makes no request at all and the release workflow refuses to run.

6. Optional hardening: create a `release` [environment](https://docs.github.com/actions/deployment/targeting-different-environments) with required reviewers, move both secrets into it (`gh secret set ... --env release`) and add `environment: release` to the `publish` job. Every release then waits for an approval before anything is signed.

## Each release

1. Set the version in `Cargo.toml` (`[workspace.package] version = "0.2.0"`), run `cargo check` so `Cargo.lock` follows, commit, and merge into `main` with CI green.

2. Tag and push:

   ```sh
   git switch main && git pull
   git tag -a v0.2.0 -m "Lumenwall 0.2.0"
   git push origin v0.2.0
   ```

   A tag with a pre-release part (`v0.3.0-beta.1`) becomes a GitHub pre-release: only users who turned on "Include pre-releases" are offered it.

3. Follow the run: `gh run watch $(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')`. The jobs:

   | Job | What it does |
   |---|---|
   | check the tag | `v<semver>`, equal to the `Cargo.toml` version; `RELEASE_PUBLIC_KEY` set |
   | build (Linux, Windows) | `cargo build --workspace --release --locked` without cache, `cargo about generate` (notices), `packaging/release/package.py` (archive, timestamps of the tagged commit); Linux also: `packaging/release/deb.py` (the `.deb`, shown with `dpkg-deb --info/--contents`) and `shellcheck` on `install.sh` |
   | windows installer | on Ubuntu: `apt install nsis`, `packaging/windows/build-installer.sh` on the Windows zip (fails on any `makensis` error, then checks with `7z` that the programs are inside) |
   | sign and publish | builds `release_sign` before any secret is loaded; adds `install.sh`; `release_sign sign` writes `SHA256SUMS` and one `.minisig` per file (refuses a key that is not `RELEASE_PUBLIC_KEY`); `release_sign verify` checks every archive with the app's own code and installs the Linux one into a temporary folder; `sha256sum --check`; checks that every expected file, its signature and its `SHA256SUMS` line exist; runs `install.sh --require-signature` then `--uninstall` on the signed files in a throw-away home; `gh release create --draft --generate-notes`, then publishes it once every file is uploaded |

4. Check the published release:

   ```sh
   gh release view v0.2.0
   mkdir /tmp/lumenwall-0.2.0 && gh release download v0.2.0 --repo simnJS/lumenwall --dir /tmp/lumenwall-0.2.0
   cd /tmp/lumenwall-0.2.0
   minisign -Vm SHA256SUMS -p ~/.lumenwall-release/lumenwall-release.pub
   minisign -Vm lumenwall-0.2.0-x86_64-unknown-linux-gnu.tar.gz -p ~/.lumenwall-release/lumenwall-release.pub
   sha256sum --check SHA256SUMS
   ```

   Then open the previous version of the app: Settings › Updates › Check now offers 0.2.0, and "Update and restart" installs it. Try the installers once per release: `curl -fsSL https://github.com/simnJS/lumenwall/releases/latest/download/install.sh | sh` in a test account (it must report the new version), the `.deb` in a container or VM (`apt install ./lumenwall-0.2.0-amd64.deb`, `lumenwall-ui --version`), and the Windows installer over the previous version on a Windows machine (the app keeps running, the engine is restarted, Settings › Apps shows 0.2.0).

5. The generated notes list the merged pull requests. Edit them on GitHub or with `gh release edit v0.2.0 --notes-file notes.md`; the app shows them as text (headings, lists, links reduced to their text).

## When something goes wrong

- **The workflow failed before "Publish":** nothing is public. Fix the cause, then delete and push the tag again (`git push --delete origin v0.2.0 && git tag -d v0.2.0`), or tag the next patch version.
- **A published release is broken:** publish a fixed version (0.2.1). The updater never installs an older version, so deleting the broken release does not bring users back; but it stops new installations. To stop offering it while the fix is prepared: `gh release edit v0.2.0 --draft` (`releases/latest` then returns the previous release).
- **The secret key leaked:** whoever has it can sign updates that installed copies accept. Create a new pair, put its public key in `RELEASE_PUBLIC_KEY`, replace both secrets, and publish a release **signed with the old key** that carries the new public key: it moves every copy to the new key. Then destroy the old key and say so in the release notes. Installed copies trust exactly one key, so there is no way to revoke the old one remotely.
- **The secret key is lost:** installed copies cannot be updated automatically any more. Set a new key as above and announce that the next version must be downloaded by hand.

## What the updater expects

This is a contract with every installed copy; change it only together with `lumenwall-update`.

- Tag `v<semver>`, release not a draft; pre-releases marked as such.
- For each platform: the archive named above, `<archive>.minisig`, plus `SHA256SUMS` and `SHA256SUMS.minisig` in the same release. A release missing any of them is ignored.
- Signatures in minisign's pre-hashed mode (the default since minisign 0.8), with a trusted comment containing `file:<asset name>` (`release_sign` writes `timestamp:<unix time>`, `file:<name>` and `lumenwall <version>`, separated by tabs).
- Archives: one top-level folder holding at least `lumenwall` and `lumenwall-ui` (`.exe` on Windows), and `lumenwall.bundle`. No links, no absolute paths.
- Other files in the release (installer, `.deb`, `install.sh`) are ignored by the updater; the extra `SHA256SUMS` lines are fine.

`install.sh`, once published, relies on the same layout: `releases/latest/download/SHA256SUMS` lists `lumenwall-<version>-x86_64-unknown-linux-gnu.tar.gz` (the version is read from that line), and `releases/download/v<version>/` holds the archive, its `.minisig` and `SHA256SUMS.minisig`, with `file:<name>` in each trusted comment.

## Signing by hand

If the workflow cannot run, sign on the machine that holds the key (archives built with `packaging/release/package.py` in `dist/`):

```sh
cp packaging/linux/install.sh dist/
cd dist
sha256sum lumenwall-0.2.0-* install.sh > SHA256SUMS
for f in lumenwall-0.2.0-* install.sh SHA256SUMS; do
  minisign -S -s ~/.lumenwall-release/lumenwall-release.key -m "$f" \
    -t "timestamp:$(date +%s) file:$f lumenwall 0.2.0"
done
cd .. && cargo run --locked -p lumenwall-update --example release_sign -- verify --version 0.2.0 dist
gh release create v0.2.0 --verify-tag --generate-notes --title "Lumenwall 0.2.0" dist/*
```

## Verify a download

Users can check an archive by hand with the release public key:

```sh
minisign -Vm lumenwall-<version>-x86_64-unknown-linux-gnu.tar.gz -P <release public key>
```

Release public key (the same one is compiled into the app):

```
RWQt/7tWeHNqM8Z/BqlP0VmbIHU4DhM9iWA8dzM7rfrHXK5ZxtoIHMbY
```

## Trying it without publishing

- `cargo test -p lumenwall-update`: the whole check, download, verification, installation and restart path against a fake GitHub signed with the test key.
- In the app (debug build): `cargo run -p lumenwall-update --example fake_github -- 0.2.0 --slow`, then start `lumenwall-ui` with the three variables it prints. `--bad-signature`, `--bad-checksum`, `--rate-limit` show the error states.
- The `.deb`, from release binaries (Debian or Ubuntu with `dpkg-dev`; `--no-ffmpeg` without FFmpeg's development files): `python3 packaging/release/deb.py --version 0.2.0 --bin-dir target/release --notices /tmp/rel/notices.html --out /tmp/rel/dist`, then `dpkg-deb --info` and `dpkg-deb --contents` on it.
- The Windows installer, from a Windows zip (`package.py --target x86_64-pc-windows-msvc`), with NSIS installed: `packaging/windows/build-installer.sh 0.2.0 /tmp/rel/dist/lumenwall-0.2.0-x86_64-pc-windows-msvc.zip /tmp/rel/dist`. Running it needs Windows.
- `install.sh` against a local copy of a release signed with the test key: serve a folder holding `releases/latest/download/` and `releases/download/v0.2.0/` (both the signed `dist`), e.g. with `python3 -m http.server`, then run it in a throw-away home with `LUMENWALL_RELEASES_URL=http://127.0.0.1:<port>/releases` and `LUMENWALL_PUBLIC_KEY=<test key>` (testing only). Use a non-GNOME `XDG_CURRENT_DESKTOP`, or fake `gnome-extensions` and `gsettings` first in `PATH`: GSettings writes go to the session's real configuration whatever `HOME` says.
- The release tooling, with fake programs and the test key:

  ```sh
  mkdir -p /tmp/rel/bin /tmp/rel/dist && printf '#!/bin/sh\n' | tee /tmp/rel/bin/lumenwall > /tmp/rel/bin/lumenwall-ui
  echo '<html></html>' > /tmp/rel/notices.html
  python3 packaging/release/package.py --version 0.2.0 --target x86_64-unknown-linux-gnu \
    --bin-dir /tmp/rel/bin --notices /tmp/rel/notices.html --out /tmp/rel/dist
  KEY=$(sed -n 2p crates/lumenwall-update/tests/keys/TEST-ONLY-lumenwall-update.pub)
  MINISIGN_SECRET_KEY="$(cat crates/lumenwall-update/tests/keys/TEST-ONLY-lumenwall-update.key)" \
    cargo run -p lumenwall-update --example release_sign -- sign --version 0.2.0 --public-key "$KEY" /tmp/rel/dist
  cargo run -p lumenwall-update --example release_sign -- verify --version 0.2.0 --public-key "$KEY" \
    --target x86_64-unknown-linux-gnu /tmp/rel/dist
  ```

## Open points

- **Linux build image.** The archive is built on `ubuntu-latest` and links against that image's glibc and FFmpeg shared libraries: users need a distribution at least as recent, with FFmpeg's libraries for video wallpapers. Pinning the image (e.g. `ubuntu-24.04`) would make the requirement explicit.
- **Windows installer.** NSIS, per user (above). Not Authenticode-signed: SmartScreen warns until a code-signing certificate exists (its minisign signature and `SHA256SUMS` are the check meanwhile). An MSIX/Store package would instead be updated by the Store (the updater detects `WindowsApps` and stays off).
- **Debian package.** Built on `ubuntu-latest` (glibc and the FFmpeg major versions of that image, see `Recommends`); no apt repository yet, so users install each new `.deb` by hand (the app says its updates come from the package). `lumenwall-web-host` is not in it.
- **Web wallpapers.** `lumenwall-web-host` and the CEF runtime are not built by the workflow yet. `package.py` already includes the host when it is built, and the CEF files with `--web-runtime <dir>` (layout in `crates/lumenwall-web/README.md`).
- **Video on Windows.** FFmpeg DLLs are not shipped yet (`crates/lumenwall-video/README.md`); their LGPL obligations (`deny.toml`) apply when they are.
