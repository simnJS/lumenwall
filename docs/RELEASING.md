# Releasing Lumenwall

A release is a GitHub Release built by [`.github/workflows/release.yml`](../.github/workflows/release.yml) when a `v<version>` tag is pushed. The app's updater ([`crates/lumenwall-update`](../crates/lumenwall-update/README.md)) installs only files listed in a `SHA256SUMS` signed with the **release key**, whose public half is compiled into the app. This page covers the one-time key setup, each release, and what to do when something goes wrong.

Each release publishes these seven files and nothing else (the workflow checks the list):

| Asset | Content | Made by |
|---|---|---|
| `Lumenwall-Setup.exe` | Windows installer of the zip's files: per user, `%LOCALAPPDATA%\Programs\Lumenwall`, Start menu shortcut, optional start with Windows, uninstaller. Same name in every release: `releases/latest/download/Lumenwall-Setup.exe` | `packaging/windows/build-installer.sh` (NSIS, `lumenwall.nsi`) |
| `lumenwall-<version>-x86_64-pc-windows-msvc.zip` | `lumenwall.exe`, `lumenwall-ui.exe`, `lumenwall-web-host.exe`; FFmpeg's five LGPL DLLs (video wallpapers: Lumenwall's minimal build, [`packaging/ffmpeg/`](../packaging/ffmpeg/README.md)) and the CEF runtime with every locale (web wallpapers), exactly the files of [`packaging/release/windows-runtime.toml`](../packaging/release/windows-runtime.toml); licences (`licenses\`: FFmpeg's LGPL-3.0 and the GPL-3.0 it incorporates, the licences of nv-codec-headers, Vulkan-Headers and the MinGW-w64 runtime compiled into the FFmpeg DLLs, `FFmpeg-BUILD-INFO.txt`, CEF's licence, Chromium's `CREDITS.html`), notices, `README.txt`, `lumenwall.bundle` | `packaging/release/package.py` |
| `lumenwall-<version>-x86_64-unknown-linux-gnu.tar.gz` | `lumenwall`, `lumenwall-ui`, under `share/` the `.desktop` files, icons and the GNOME Shell extension, licences, `THIRD-PARTY-NOTICES.html` (cargo-about), `README.txt`, `lumenwall.bundle` | `packaging/release/package.py` |
| `lumenwall-<version>-amd64.deb` | System-wide package: `/usr/bin`, `/usr/share/applications`, icons, `/usr/share/gnome-shell/extensions`; Depends from `dpkg-shlibdeps` plus `libvulkan1`, Recommends FFmpeg and a Vulkan driver | `packaging/release/deb.py` (`dpkg-deb`) |
| `install.sh` | The one-line installer (`curl -fsSL …/releases/latest/download/install.sh \| sh`), same file in every release | `packaging/linux/install.sh` |
| `SHA256SUMS` | SHA-256 of the five files above | `release_sign sign` |
| `SHA256SUMS.minisig` | minisign signature of `SHA256SUMS`, the release's only signature | `release_sign sign` |

**One signature for the whole release.** `SHA256SUMS` binds each file's name to its SHA-256, and its signature (trusted comment `file:SHA256SUMS`) proves the release key wrote that list. A file whose SHA-256 equals its signed line is therefore exactly the file the key holder listed, as surely as if it had its own signature: forging one would need either the key or a second file with the same SHA-256. The updater and `install.sh` check it this way: signature of the list, then the hash of the downloaded file against its line.

The release notes start with a download section, [`packaging/release/release-notes.md`](../packaging/release/release-notes.md) (`@VERSION@` replaced by the version), followed by GitHub's generated notes. The app's update panel skips what is between its `<!-- downloads -->` and `<!-- /downloads -->` markers.

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
   | FFmpeg (Windows DLLs) | on `ubuntu-24.04`, no cache: `packaging/ffmpeg/build.sh` downloads FFmpeg's release tarball and two header packages, checks their SHA-256, cross-compiles the minimal LGPL build with mingw-w64, checks its configuration and the DLLs' imports, and uploads it as the `ffmpeg-win64` artifact |
   | build (Linux, Windows) | `cargo build --workspace --release --locked` without cache; Windows: see below; `cargo about generate` (notices), `packaging/release/package.py` (archive, timestamps of the tagged commit); Linux also: `packaging/release/deb.py` (the `.deb`, shown with `dpkg-deb --info/--contents`) and `shellcheck` on `install.sh` |
   | windows installer | on Ubuntu: `apt install nsis`, `packaging/windows/build-installer.sh` on the Windows zip, which writes `Lumenwall-Setup.exe` (fails on any `makensis` error, then checks with `7z` that the programs are inside); the second `AUTHENTICODE` step, skipped likewise |
   | sign and publish | builds `release_sign` before any secret is loaded; adds `install.sh`; `release_sign sign` writes `SHA256SUMS` and `SHA256SUMS.minisig` (refuses a key that is not `RELEASE_PUBLIC_KEY`, and any other `.minisig`); `release_sign verify` checks every archive with the app's own code and installs the Linux one into a temporary folder; `sha256sum --check`; `package.py --check` on the Windows zip (below); checks that exactly the seven files above are there and that `SHA256SUMS` lists exactly the five that are not the list or its signature; runs `install.sh --require-signature` on a copy whose `SHA256SUMS` was changed after signing (must fail, nothing installed), then on the signed files in a throw-away home, then `--uninstall`; `gh release create --draft --generate-notes --notes-file` (the download section, then the generated notes), then publishes it once every file is uploaded |

   The Windows build, in order (recipes: [`crates/lumenwall-video/README.md`](../crates/lumenwall-video/README.md#windows-distribution-for-the-release-workflow-and-the-installer) and [`crates/lumenwall-web/README.md`](../crates/lumenwall-web/README.md#windows-runtime-files)):

   1. The setup action with `ffmpeg: "true"`: the `ffmpeg-win64` artifact of the FFmpeg job, each DLL checked against its `BUILD-INFO.txt` (`FFMPEG_DIR`, `LIBCLANG_PATH`).
   2. `CARGO_TARGET_DIR=$RUNNER_TEMP/t` (short: `cl.exe` hits `MAX_PATH` on CEF's C++ wrapper under a deep folder) and `CEF_PATH=$RUNNER_TEMP/cef`. For both builds, `CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static -D linker-messages"` (build job `env`): the **C runtime is linked statically**, so the three programs import no `VCRUNTIME140.dll` and start on a Windows without the Visual C++ redistributable, which the per-user installer could not install; `cc` and `cef-dll-sys` build all C and C++ code with `/MT`, and any linker warning (LNK4098: objects built for another C runtime) fails the build. It costs 0.6 MB (measured on main 8d196eb: `lumenwall.exe` 17.82 → 18.12 MB, `lumenwall-ui.exe` 15.43 → 15.65 MB, `lumenwall-web-host.exe` 0.77 → 0.87 MB). Releases only: dev builds keep the shared runtime (the reasons, and why no allocation crosses a C runtime: `.cargo/config.toml`).
   3. `cargo build --workspace --release --locked --target x86_64-pc-windows-msvc --features lumenwall-engine/video-ffmpeg`.
   4. In the MSVC environment (`vcvars64.bat`, found with `vswhere`; Ninja from Visual Studio's CMake tools if not on `PATH`): `cargo build --release --locked --target x86_64-pc-windows-msvc -p lumenwall-web --features cef --bin lumenwall-web-host`, a build of its own so that the `cef` feature never reaches `lumenwall.exe`. The `cef` crate downloads CEF 156's windows64 minimal distribution (175 MB) from the official CEF builds into `CEF_PATH`.
   5. The `AUTHENTICODE` step (skipped until [Authenticode signing](#authenticode-signing) is set up).
   6. `cargo about generate --features "lumenwall-engine/video-ffmpeg lumenwall-web/cef"`: the notices then list `ffmpeg-sys-next` and the `cef` crates.
   7. `package.py --ffmpeg-dir "$FFMPEG_DIR" --web-runtime "$CEF_PATH/<version>/cef_windows_x86_64"`: copies exactly the files of `windows-runtime.toml` (and refuses an FFmpeg or CEF build other than the one it describes: for FFmpeg, its `BUILD-INFO.txt` must give the manifest's version, tarball URL and SHA-256 and "LGPL version 3 or later"), adds the licences under `licenses\` and FFmpeg's and CEF's sections to `THIRD-PARTY-NOTICES.html` (FFmpeg's names the tarball, its SHA-256, `packaging/ffmpeg/build.sh` at the release's tag and the configure options), zips, then checks the zip: the programs, the bundle files, the five FFmpeg DLLs, the 11 CEF files, the 220 locales, the licences and notices, and nothing else (no `dxcompiler.dll`, `bootstrap*.exe`, `libcef.lib`, headers, FFmpeg import libraries or other FFmpeg library); and, from each `.exe` and `.dll`'s PE import tables (normal and delay-loaded), that none imports the VC++ redistributable (`vcruntime*.dll`, `msvcp*.dll`, `concrt*`, `vccorlib*`, `vcomp*`, `mfc*`, the debug `ucrtbased.dll`), so a build without the static C runtime cannot ship.

4. Check the published release:

   ```sh
   gh release view v0.2.0
   mkdir /tmp/lumenwall-0.2.0 && gh release download v0.2.0 --repo simnJS/lumenwall --dir /tmp/lumenwall-0.2.0
   cd /tmp/lumenwall-0.2.0
   ls    # the seven files of the table above, nothing else
   minisign -Vm SHA256SUMS -p ~/.lumenwall-release/lumenwall-release.pub
   sha256sum --check SHA256SUMS
   ```

   Open the release page: it starts with the download section, and its `Lumenwall-Setup.exe` link works, as does `https://github.com/simnJS/lumenwall/releases/latest/download/Lumenwall-Setup.exe` (latest stable release). Then open the previous version of the app: Settings › Updates › Check now offers 0.2.0 (its notes start at "What's Changed"), and "Update and restart" installs it. Try the installers once per release: `curl -fsSL https://github.com/simnJS/lumenwall/releases/latest/download/install.sh | sh` in a test account (it must report the new version), the `.deb` in a container or VM (`apt install ./lumenwall-0.2.0-amd64.deb`, `lumenwall-ui --version`), and the Windows installer over the previous version on a Windows machine (the app keeps running, the engine is restarted, Settings › Apps shows 0.2.0).

5. The generated notes, after the download section, list the merged pull requests. Edit them on GitHub or with `gh release edit v0.2.0 --notes-file notes.md` (keep the download section and its two markers); the app shows them as text (headings, lists, links reduced to their text).

## When something goes wrong

- **The workflow failed before "Publish":** nothing is public. Fix the cause, then delete and push the tag again (`git push --delete origin v0.2.0 && git tag -d v0.2.0`), or tag the next patch version.
- **A published release is broken:** publish a fixed version (0.2.1). The updater never installs an older version, so deleting the broken release does not bring users back; but it stops new installations. To stop offering it while the fix is prepared: `gh release edit v0.2.0 --draft` (`releases/latest` then returns the previous release).
- **The secret key leaked:** whoever has it can sign updates that installed copies accept. Create a new pair, put its public key in `RELEASE_PUBLIC_KEY`, replace both secrets, and publish a release **signed with the old key** that carries the new public key: it moves every copy to the new key. Then destroy the old key and say so in the release notes. Installed copies trust exactly one key, so there is no way to revoke the old one remotely.
- **The secret key is lost:** installed copies cannot be updated automatically any more. Set a new key as above and announce that the next version must be downloaded by hand.

## What the updater expects

This is a contract with every installed copy; change it only together with `lumenwall-update`.

- Tag `v<semver>`, release not a draft; pre-releases marked as such.
- For each platform: the archive named above, plus `SHA256SUMS` and `SHA256SUMS.minisig` in the same release. A release missing any of them is ignored. `SHA256SUMS` lists the archive as `<sha256 hex>  <archive name>` (`sha256sum` format).
- `SHA256SUMS.minisig` in minisign's pre-hashed mode (the default since minisign 0.8), with a trusted comment containing `file:SHA256SUMS` (`release_sign` writes `timestamp:<unix time>`, `file:SHA256SUMS` and `lumenwall <version>`, separated by tabs). There is no signature per file.
- Archives: one top-level folder holding at least `lumenwall` and `lumenwall-ui` (`.exe` on Windows), and `lumenwall.bundle`. No links, no absolute paths. In a bundle folder (every Windows installation: the installer and the app's own copy both write `lumenwall.bundle`) the updater installs every file of the new archive, new files and folders included (FFmpeg's DLLs, `locales\`), skips those already installed with the same content, and moves a DLL still loaded by a running program (`libcef.dll` in a web host) aside instead of overwriting it. Files that a release no longer has are left in place.
- Other files in the release (installer, `.deb`, `install.sh`) are ignored by the updater; the extra `SHA256SUMS` lines are fine.

`install.sh`, once published, relies on the same layout: `releases/latest/download/SHA256SUMS` lists `lumenwall-<version>-x86_64-unknown-linux-gnu.tar.gz` (the version is read from that line), and `releases/download/v<version>/` holds the archive and `SHA256SUMS.minisig`, with `file:SHA256SUMS` in its trusted comment.

## Signing by hand

If the workflow cannot run, sign on the machine that holds the key (archives built with `packaging/release/package.py`, the `.deb` and `Lumenwall-Setup.exe` in `dist/`):

```sh
cp packaging/linux/install.sh dist/
cd dist
sha256sum Lumenwall-Setup.exe lumenwall-0.2.0-* install.sh > SHA256SUMS
minisign -S -s ~/.lumenwall-release/lumenwall-release.key -m SHA256SUMS \
  -t "timestamp:$(date +%s) file:SHA256SUMS lumenwall 0.2.0"
cd .. && cargo run --locked -p lumenwall-update --example release_sign -- verify --version 0.2.0 dist
sed 's/@VERSION@/0.2.0/g' packaging/release/release-notes.md > /tmp/notes.md
gh release create v0.2.0 --verify-tag --generate-notes --notes-file /tmp/notes.md --title "Lumenwall 0.2.0" dist/*
```

## Verify a download

`SHA256SUMS.minisig` signs `SHA256SUMS`, which holds the SHA-256 of every file of the release. Download both next to your file, check the list's signature with the release public key, then your file's hash against the list.

Linux (`minisign` from your distribution):

```sh
minisign -Vm SHA256SUMS -P <release public key>
sha256sum --check --ignore-missing SHA256SUMS
```

Windows (PowerShell, with `scoop install minisign`):

```powershell
minisign -Vm SHA256SUMS -P <release public key>
(Get-FileHash Lumenwall-Setup.exe -Algorithm SHA256).Hash.ToLower()
Select-String Lumenwall-Setup.exe SHA256SUMS
```

`minisign` must print `Signature and comment signature verified` and a trusted comment with `file:SHA256SUMS` and the release's version; on Windows the two hashes must be equal.

Release public key (the same one is compiled into the app):

```
RWQt/7tWeHNqM8Z/BqlP0VmbIHU4DhM9iWA8dzM7rfrHXK5ZxtoIHMbY
```

## Authenticode signing

**Not set up.** The Windows programs and installer have no Authenticode signature: SmartScreen shows "Windows protected your PC" (users click More info › Run anyway, see the README) and Windows names no publisher. The minisign-signed `SHA256SUMS` is what proves where the files come from meanwhile. The candidates are Azure Artifact Signing (Microsoft's service, formerly Trusted Signing) and SignPath; neither is chosen, and no secret exists for it.

The workflow already has the two places, marked `AUTHENTICODE` in `release.yml`, in the order the signatures must be made, all before `SHA256SUMS` is computed (signing changes the bytes it hashes):

| Step | Job (runner) | Signs | When |
|---|---|---|---|
| `AUTHENTICODE: sign the programs and the unsigned DLLs` | build (`windows-latest`) | in place: `lumenwall.exe`, `lumenwall-ui.exe`, `lumenwall-web-host.exe` (in `$BIN_DIR`), FFmpeg's five DLLs (in `$FFMPEG_DIR/bin`; the SHA-256 that the build's `BUILD-INFO.txt` records, and the setup action checks, are those of the unsigned DLLs), and CEF's `libcef.dll`, `chrome_elf.dll`, `vk_swiftshader.dll`, `vulkan-1.dll` (in `$CEF_RUNTIME`), which ship without a signature; `d3dcompiler_47.dll` keeps Microsoft's | after both builds, before `package.py` copies and zips them: the zip, the updater's copies and the installer all carry the signed files |
| `AUTHENTICODE: sign Lumenwall-Setup.exe` | windows installer (`ubuntu-latest`) | `dist/Lumenwall-Setup.exe`, in place | after `build-installer.sh`, before the upload to "sign and publish" |

Both run only when `env.AUTHENTICODE` is `true`, i.e. when a repository secret named `AUTHENTICODE_TOKEN` exists (the workflow-level `env` reads whether it is empty, never its value). Until a signing command replaces them, both fail with an error if that secret appears: a release is never published unsigned by mistake once signing is meant to be on.

To plug a service in:

1. Set up the service's account and certificate profile, and add its credential as a repository secret (preferably in the `release` environment, see the one-time setup). Name it `AUTHENTICODE_TOKEN`, or change the `AUTHENTICODE:` line at the top of `release.yml` to test the secret the service needs.
2. Replace the `run:` of both steps by the service's signing step, using the `FILES` each step lists, with an RFC 3161 timestamp so the signatures stay valid after the certificate expires. Give its secrets to that step only (`env:` or `with:` of the step), never to the job: the build job runs `cargo build` first.
3. The installer step runs on Ubuntu. A tool that needs Windows (`signtool`, Microsoft's GitHub action) needs that job moved to `windows-latest`, or a small Windows job between `installer` and `publish` that downloads `release-windows-installer`, signs it and uploads it again; SignPath's action submits the file to its service and works from any runner.
4. The uninstaller that NSIS writes inside the setup is only signed if `packaging/windows/lumenwall.nsi` signs it too (`!uninstfinalize`).
5. After each signing step, check it (`signtool verify /pa`, or `osslsigncode verify` on Linux), then update the SmartScreen sentence of `README.md`, `README.fr.md` and `packaging/release/release-notes.md`. A new certificate can still trigger SmartScreen until its files have built a reputation.

## Trying it without publishing

- `cargo test -p lumenwall-update`: the whole check, download, verification, installation and restart path against a fake GitHub whose `SHA256SUMS` is signed with the test key; a missing, foreign or replayed `SHA256SUMS.minisig`, an edited list and an archive that does not match its line are refused.
- In the app (debug build): `cargo run -p lumenwall-update --example fake_github -- 0.2.0 --slow`, then start `lumenwall-ui` with the three variables it prints. `--bad-signature` (`SHA256SUMS` signed by another key), `--bad-checksum`, `--rate-limit` show the error states.
- The `.deb`, from release binaries (Debian or Ubuntu with `dpkg-dev`; `--no-ffmpeg` without FFmpeg's development files): `python3 packaging/release/deb.py --version 0.2.0 --bin-dir target/release --notices /tmp/rel/notices.html --out /tmp/rel/dist`, then `dpkg-deb --info` and `dpkg-deb --contents` on it.
- `python3 packaging/release/test_package.py`: `package.py`'s PE import reader and its VC++ redistributable check, on fake PE images (any system).
- The Windows zip with FFmpeg and CEF, on Windows (Python ≥ 3.11; the official "embeddable" zip from python.org works without installing anything), after the two builds of the workflow (`crates/lumenwall-video/README.md`, `crates/lumenwall-web/README.md`) in a short `CARGO_TARGET_DIR`, with `--target x86_64-pc-windows-msvc` and the workflow's `CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS` (in a target directory of its own: other flags rebuild everything):

  ```powershell
  python packaging/release/package.py --version 0.2.0 --target x86_64-pc-windows-msvc `
    --bin-dir "$env:CARGO_TARGET_DIR\x86_64-pc-windows-msvc\release" --notices notices.html --out dist `
    --ffmpeg-dir $env:FFMPEG_DIR --web-runtime "$env:CEF_PATH\156.0.15\cef_windows_x86_64"
  python packaging/release/package.py --check dist\lumenwall-0.2.0-x86_64-pc-windows-msvc.zip
  ```

  `notices.html` must come from `packaging/about/about.hbs` (it holds the marker where FFmpeg's and CEF's sections go). Extract the zip anywhere and start `lumenwall.exe --no-config --silent -w 40x40x960x540 <id>` from it, with `LUMENWALL_IPC_ENDPOINT` set to a private name so that it never meets an installed engine.
- The Windows installer, from a Windows zip (`package.py --target x86_64-pc-windows-msvc`), with NSIS installed: `packaging/windows/build-installer.sh 0.2.0 /tmp/rel/dist/lumenwall-0.2.0-x86_64-pc-windows-msvc.zip /tmp/rel/dist` writes `/tmp/rel/dist/Lumenwall-Setup.exe`. Running it needs Windows.
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
- **Windows installer.** NSIS, per user (above). Not Authenticode-signed: SmartScreen warns until a code-signing service is plugged in ([Authenticode signing](#authenticode-signing); the signed `SHA256SUMS` is the check meanwhile). An MSIX/Store package would instead be updated by the Store (the updater detects `WindowsApps` and stays off).
- **Debian package.** Built on `ubuntu-latest` (glibc and the FFmpeg major versions of that image, see `Recommends`); no apt repository yet, so users install each new `.deb` by hand (the app says its updates come from the package). `lumenwall-web-host` is not in it.
- **FFmpeg and CEF on Windows.** Shipped in the zip and the installer (`packaging/release/windows-runtime.toml`). To move to another FFmpeg, change the pins of `packaging/ffmpeg/build.sh` and `[ffmpeg]` of the manifest together (`packaging/ffmpeg/README.md`, "Updating"); to another CEF, the `cef` crate of `crates/lumenwall-web/Cargo.toml` and `[cef]` (version, archive, source, number of locales): `package.py` refuses a build its manifest does not describe, so the notices always name what ships. Chromium's security fixes reach users only through a new CEF, hence a new Lumenwall release.
- **Sizes** (0.1.0, measured on Windows 11 from a local build, with BtbN's FFmpeg): the installed folder is 565.5 MB (539.3 MiB, 248 files): CEF 403.6 MB (384.9 MiB, `libcef.dll` alone 294 MB), FFmpeg 128.2 MB, the three programs 33.2 MB. The zip is 251.1 MB (deflate). The installer was not built here (no NSIS on Windows): its solid LZMA payload with makensis' default 8 MiB dictionary is 181.9 MB, so `Lumenwall-Setup.exe` comes out at about 182 MB. With Lumenwall's own FFmpeg build (`packaging/ffmpeg/README.md`; 10.9 MB of DLLs measured on a local build, against 128.2 MB; 2.9 MB instead of 40.3 MB in the same LZMA stream, 4.0 MB instead of 53.3 MB in deflate), the estimates become: installed folder ≈ 448 MB, zip ≈ 202 MB, `Lumenwall-Setup.exe` ≈ 145 MB (CEF is now nearly all of it). To be confirmed on the first release built that way. An in-app update downloads the zip, not the installer, but rewrites only the files that changed.
- **FFmpeg's licence obligations.** Lumenwall ships its own minimal FFmpeg build (`packaging/ffmpeg/README.md`). What it contains, and what ships for each part: FFmpeg 8.1.3 under the LGPL-3.0-or-later (the LGPL-3.0 text and the GPL-3.0 text it incorporates); code from the nv-codec-headers (MIT: their notices) and Vulkan-Headers (Apache-2.0: its licence) headers; the MinGW-w64 runtime (the toolchain's licence file, which lists the notices its parts require); libgcc, under the GCC Runtime Library Exception, which needs nothing. Nothing else is compiled in (`build.sh` checks the configuration and the DLLs' imports), and every one of these files is in `licenses\`. `THIRD-PARTY-NOTICES.html` says that Lumenwall uses FFmpeg under the LGPL and names the corresponding source: FFmpeg's release tarball on ffmpeg.org (URL, SHA-256), used unmodified, and `packaging/ffmpeg/build.sh` at the release's tag, with the configure options; `licenses\FFmpeg-BUILD-INFO.txt` adds the toolchain. The DLLs are loaded by name at run time and can be replaced (LGPL-3.0 §4(d)(1)). One point is left to confirm, with the rest of the legal check that `deny.toml` mentions (it also covers the WTFPL of `ffmpeg-sys-next`): the source is offered as a link to ffmpeg.org, not as a copy in the release. Under GPL-3.0 §6(d), which the LGPL-3.0 relies on for the library's object code, a third party may host it, but Lumenwall stays responsible for it remaining available as long as the release is distributed. Attaching `ffmpeg-8.1.3.tar.xz` (11.7 MB) to each GitHub release, as an eighth asset the updater ignores, would remove that dependency; it is not done.
- **Linux web wallpapers.** The Linux archive and the `.deb` do not ship `lumenwall-web-host` nor CEF (built from source only, `README.md`).
