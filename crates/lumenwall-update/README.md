# lumenwall-update

Signed self-update of Lumenwall from its [GitHub Releases](https://github.com/simnJS/lumenwall/releases). The app (`lumenwall-ui`) drives it; publishing a release is described in [`docs/RELEASING.md`](../../docs/RELEASING.md).

Why a crate of its own: the code that decides what replaces the user's programs is small, security-sensitive and reviewable alone; its tests run against a fake GitHub without building the UI; and `lumenwall-ui` gets one new module instead of HTTP, crypto and file-replacement code mixed into its views.

## Flow

| Step | Function | What happens |
|---|---|---|
| May this copy update itself? | `availability(exe)` | `Ready(layout)`, or why not: `Managed` (package manager), `ReadOnly` (folder not writable), `Development`, `NoKey`, `Disabled`, `Unsupported` |
| Is there a newer release? | `check(source, current, target, channel, &mut state, now)` | One API request, conditional on the last ETag |
| Download and verify | `download(source, current, &release, dir, &cancel, progress)` | Signed `SHA256SUMS`, then the archive's signature, then the archive streamed to disk while hashed |
| Install | `install(&verified, &layout)` | Extract to a staging folder, replace file by file, roll back on failure |
| Restart the engine | `restart_daemon(endpoint, exe, spawn)` | IPC `shutdown`, wait until the endpoint is free, start the new program, wait for its handshake |
| Next start | `cleanup(dir)` | Remove the staging folder and Windows backups that were still in use |

## Requests

- **When:** at most once every 12 hours (`State::due`), checked when the app starts and when its window gets focus; one hour after a failed check; never before GitHub's rate-limit deadline (`x-ratelimit-reset`, `retry-after`), even for "Check now". No timer, no loop.
- **What:** `GET https://api.github.com/repos/simnJS/lumenwall/releases/latest` (stable channel) or `.../releases?per_page=20` (pre-releases), without a token, with `If-None-Match`: an unchanged answer is a `304` that does not count against GitHub's 60 requests per hour. Then, only when the user clicks "Update", four files from `github.com/simnJS/lumenwall/releases/download/` and GitHub's storage hosts.
- **Allowed hosts:** `api.github.com` for the API; for downloads the first URL must be this repository's release asset, and redirects may only go to `github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com` or `github-releases.githubusercontent.com`, over HTTPS on the default port. Redirects are followed by this crate, one hop at a time, after the check.
- **Privacy:** User-Agent `Lumenwall/<version>`, nothing else about the user: no token, no identifier, no query string. GitHub sees the IP address, as for any download.
- **State:** `<cache dir>/lumenwall/update.json` (last check, ETags, rate-limit deadline, the release found). Deleting it only means the next start checks again.

## Verification

Each release publishes, per platform, `lumenwall-<version>-<target>.tar.gz` (Linux) or `.zip` (Windows), its `.minisig`, and one `SHA256SUMS` with its `.minisig`. All signatures are minisign's pre-hashed Ed25519 (BLAKE2b-512), checked with `minisign-verify` against `RELEASE_PUBLIC_KEY`, compiled into the binary.

1. `SHA256SUMS.minisig` must verify `SHA256SUMS`, and its trusted comment must name `file:SHA256SUMS`.
2. The archive's `.minisig` must come from the same key and its trusted comment must name `file:<archive name>`: a valid signature of another file (an older archive served under a newer name) is refused before downloading.
3. The archive is streamed to `<name>.part`; its BLAKE2b (signature) and SHA-256 are computed on the way. Both must match; otherwise the file is deleted and nothing is extracted. More bytes than the API announced stop the download.
4. Only versions strictly newer than the running one are offered (no downgrade); the stable channel ignores versions with a pre-release tag even if they are not marked as pre-releases.

Without `RELEASE_PUBLIC_KEY` (`None` until the production key exists) the updater reports `NoKey` and makes no request at all.

## Installation

- **Bundle** (the folder extracted from a release archive, recognised by its `lumenwall.bundle` file): every file of the new archive is installed, other files in the folder are left alone.
- **Programs copied elsewhere** (e.g. `~/.local/bin`): only `lumenwall`, `lumenwall-ui` and `lumenwall-web-host` already present there are replaced.
- **Linux:** each new file is written in a staging folder inside the installation folder (same file system), `fsync`ed, then renamed over the old one after a hard-linked backup: the path always names a complete program; running processes keep the old one.
- **Windows:** a running `.exe`/`.dll` cannot be overwritten but can be renamed: the old file becomes `<name>.lumenwall-old`, the new one takes its name. Backups still in use are removed at the next start (`cleanup`).
- Any failure puts back the files already replaced. Archives are extracted with strict checks (one top folder, no absolute paths, no `..`, no links or devices, size and entry limits).
- The engine is restarted through the IPC `shutdown` request; the app then starts its new version and exits. The tray process (`lumenwall-ui --tray`) keeps running the previous version until the next session.

## Package managers

Copies installed by a package manager are never touched: Flatpak (`FLATPAK_ID`, `/.flatpak-info`, `/app`), Snap (`SNAP`, `/snap`), AppImage (`APPIMAGE`), Nix and Guix stores, Homebrew, distribution packages (`/usr` except `/usr/local`, `/bin`, `/sbin`), Scoop, winget, Chocolatey, Microsoft Store (`WindowsApps`). Packagers can also say so explicitly:

- at build time: `LUMENWALL_PACKAGED="Arch Linux (AUR)" cargo build --release`;
- at install time: a `lumenwall.packaged` file next to the programs, whose first line names the package manager.

`LUMENWALL_NO_UPDATE=1` turns the updater off for a session; the app's Settings turn automatic checks off for good (`ui.json`: `"updates": {"auto_check": false}`).

## Tests

`cargo test -p lumenwall-update` runs, against a fake GitHub on `127.0.0.1` (`tests/support`) serving releases signed with the **test** key of `tests/keys` (public on purpose, never the release key):

- up to date, then a conditional request answered `304`; no downgrade;
- new version: download with progress, verification, installation into a bundle (permissions kept, user files untouched, no leftovers) and into a plain `bin` folder;
- invalid signature (unknown key, archive modified after signing, signature of another file, `SHA256SUMS` signed by another key);
- checksum mismatch, archive missing from `SHA256SUMS`, more bytes than announced;
- network down (next try in one hour), rate limit (deadline stored, no request until it passes, even on demand);
- pre-release channel, release without assets, redirect to a foreign host refused;
- engine restart over a real IPC endpoint (`tests/restart.rs`); archive extraction, rollback, cleanup, package-manager detection, release-notes text (unit tests).

To try the whole flow in the app (debug builds only; nothing is installed over the app itself):

```sh
cargo run -p lumenwall-update --example fake_github -- 0.2.0 --slow
# prints LUMENWALL_UPDATE_SERVER=… LUMENWALL_UPDATE_KEY=… LUMENWALL_UPDATE_DIR=… cargo run -p lumenwall-ui
```

`--bad-signature`, `--bad-checksum` and `--rate-limit` show the error states.

## Dependencies

`ureq` + `rustls` (ring) + `rustls-platform-verifier`: the HTTP stack `lumenwall-ui` already uses. `ring` for SHA-256 (already linked for TLS). `minisign-verify` (MIT, no dependencies) for signatures. `semver`. `tar` + `flate2` on Linux, `zip` (deflate only) on Windows. Dev only: `minisign` (signs the fake releases), `tempfile`.

## Specs

None: this is Lumenwall's own distribution mechanism, with no upstream equivalent.
