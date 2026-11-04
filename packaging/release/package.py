#!/usr/bin/env python3
"""Builds one Lumenwall release archive from compiled programs.

Used by .github/workflows/release.yml (see docs/RELEASING.md); runs the same locally:

    python3 packaging/release/package.py --version 0.2.0 \\
        --target x86_64-unknown-linux-gnu \\
        --bin-dir target/x86_64-unknown-linux-gnu/release \\
        --notices THIRD-PARTY-NOTICES.html --out dist

Archive layout (what crates/lumenwall-update installs):

    lumenwall-<version>-<target>/
        lumenwall, lumenwall-ui          (.exe on Windows)
        lumenwall-web-host               only if it was built (with --web-runtime: CEF files)
        lumenwall.bundle                 marks the folder for the built-in updater
        LICENSE-MIT, LICENSE-APACHE, THIRD-PARTY-NOTICES.html, README.txt
        share/applications/*.desktop     Linux
        share/icons/hicolor/...          Linux
        share/gnome-shell/extensions/lumenwall@simnjs.github.io/   Linux

Linux gets a .tar.gz, Windows a .zip. Entries are sorted, owned by root, with
fixed permissions and the SOURCE_DATE_EPOCH timestamp when it is set, so the same
inputs give the same archive.
"""

import argparse
import gzip
import os
import shutil
import sys
import tarfile
import time
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROGRAMS = ["lumenwall", "lumenwall-ui"]
OPTIONAL_PROGRAMS = ["lumenwall-web-host"]
EXTENSION = "lumenwall@simnjs.github.io"

README = """Lumenwall {version} ({target})
================================

Lumenwall plays Wallpaper Engine Workshop wallpapers on Linux and Windows.
Free software under the MIT or Apache-2.0 license; third-party licences are in
THIRD-PARTY-NOTICES.html. https://github.com/simnJS/lumenwall

Run
---
Keep this folder together and start "lumenwall-ui" (the app). It starts the
engine ("lumenwall") when needed.

{platform}
Updates
-------
The app checks GitHub for new versions at most every 12 hours and asks before
installing one. Every download is verified with Lumenwall's release signature
before anything is replaced. Turn the checks off in Settings > Updates.
The "lumenwall.bundle" file tells the updater that this whole folder is
Lumenwall's: leave it here.
"""

LINUX = """Install for your user
--------------------
Start ./lumenwall-ui: at the first start it offers to copy the programs to
~/.local/bin and to add the menu entry, the icons, the start-up entry and, on
GNOME, the Shell extension. Each item can be left out; Settings > Installation
repairs or removes them later. The same, from a terminal, without this folder:

    curl -fsSL https://github.com/simnJS/lumenwall/releases/latest/download/install.sh | sh

Lumenwall needs the Vulkan loader, and FFmpeg's shared libraries for video
wallpapers.

"""

WINDOWS = """Install
-------
The installer (Lumenwall-Setup.exe on the release page) is the simplest way.
From this folder, start lumenwall-ui.exe: at the first start it offers to copy
itself to %LOCALAPPDATA%\\Programs\\Lumenwall and to add a Start menu shortcut
and the start with Windows. Keep Lumenwall in a folder your account can write
to (not Program Files), so the app can update itself.

"""


def die(msg: str) -> None:
    print(f"package.py: {msg}", file=sys.stderr)
    sys.exit(1)


def stage(args: argparse.Namespace) -> Path:
    windows = "windows" in args.target
    exe = ".exe" if windows else ""
    name = f"lumenwall-{args.version}-{args.target}"
    out = Path(args.out)
    top = out / name
    if top.exists():
        shutil.rmtree(top)
    top.mkdir(parents=True)
    bin_dir = Path(args.bin_dir)

    for p in PROGRAMS:
        src = bin_dir / f"{p}{exe}"
        if not src.is_file():
            die(f"{src} is missing: build the workspace in release mode first")
        shutil.copy2(src, top / src.name)
    for p in OPTIONAL_PROGRAMS:
        src = bin_dir / f"{p}{exe}"
        if src.is_file():
            shutil.copy2(src, top / src.name)
            if args.web_runtime:
                # CEF runtime files sit next to the host (crates/lumenwall-web/README.md).
                for item in sorted(Path(args.web_runtime).iterdir()):
                    dest = top / item.name
                    if item.is_dir():
                        shutil.copytree(item, dest)
                    else:
                        shutil.copy2(item, dest)

    for lic in ["LICENSE-MIT", "LICENSE-APACHE"]:
        shutil.copy2(ROOT / lic, top / lic)
    notices = Path(args.notices)
    if not notices.is_file():
        die(f"{notices} is missing: run cargo about first")
    shutil.copy2(notices, top / "THIRD-PARTY-NOTICES.html")

    platform = (WINDOWS if windows else LINUX).format(version=args.version, target=args.target)
    readme = README.format(version=args.version, target=args.target, platform=platform)
    newline = "\r\n" if windows else "\n"
    (top / "README.txt").write_bytes(readme.replace("\n", newline).encode())
    (top / "lumenwall.bundle").write_text(f"{args.target} {args.version}\n")

    if not windows:
        apps = top / "share" / "applications"
        apps.mkdir(parents=True)
        for desktop in sorted((ROOT / "packaging" / "linux").glob("*.desktop")):
            shutil.copy2(desktop, apps / desktop.name)
        shutil.copytree(ROOT / "packaging" / "linux" / "icons" / "hicolor", top / "share" / "icons" / "hicolor")
        shutil.copytree(
            ROOT / "packaging" / "gnome-extension" / EXTENSION,
            top / "share" / "gnome-shell" / "extensions" / EXTENSION,
        )
    return top


def is_program(rel: str) -> bool:
    base = rel.rsplit("/", 1)[-1]
    stem = base[:-4] if base.endswith(".exe") else base
    return "/" not in rel and stem in PROGRAMS + OPTIONAL_PROGRAMS


def entries(top: Path):
    """(relative path, absolute path) under `top`, sorted, folders first in each level."""
    out = []
    for dirpath, dirnames, filenames in os.walk(top):
        dirnames.sort()
        for d in dirnames:
            p = Path(dirpath) / d
            out.append((p.relative_to(top).as_posix() + "/", p))
        for f in sorted(filenames):
            p = Path(dirpath) / f
            out.append((p.relative_to(top).as_posix(), p))
    return sorted(out)


def tar_gz(top: Path, archive: Path, mtime: int) -> None:
    with archive.open("wb") as out, gzip.GzipFile(fileobj=out, mode="wb", mtime=mtime, filename="") as gz:
        with tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as tar:
            def add(arcname: str, path: Path | None, mode: int) -> None:
                info = tarfile.TarInfo(arcname)
                info.mtime = mtime
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                info.mode = mode
                if path is None:
                    info.type = tarfile.DIRTYPE
                    tar.addfile(info)
                else:
                    info.size = path.stat().st_size
                    with path.open("rb") as f:
                        tar.addfile(info, f)

            add(top.name + "/", None, 0o755)
            for rel, path in entries(top):
                if rel.endswith("/"):
                    add(f"{top.name}/{rel}", None, 0o755)
                else:
                    executable = is_program(rel) or path.stat().st_mode & 0o111 != 0
                    add(f"{top.name}/{rel}", path, 0o755 if executable else 0o644)


def zip_(top: Path, archive: Path, mtime: int) -> None:
    stamp = time.gmtime(max(mtime, 315532800))[:6]  # zip dates start in 1980
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        def add(arcname: str, path: Path | None, mode: int) -> None:
            info = zipfile.ZipInfo(arcname, date_time=stamp)
            info.create_system = 3  # Unix: keeps the permission bits below
            info.external_attr = (mode & 0xFFFF) << 16
            if path is None:
                info.external_attr |= 0x10  # MS-DOS directory flag
                z.writestr(info, b"")
            else:
                info.compress_type = zipfile.ZIP_DEFLATED
                z.writestr(info, path.read_bytes())

        add(top.name + "/", None, 0o40755)
        for rel, path in entries(top):
            if rel.endswith("/"):
                add(f"{top.name}/{rel}", None, 0o40755)
            else:
                add(f"{top.name}/{rel}", path, 0o100755 if is_program(rel) else 0o100644)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--version", required=True)
    ap.add_argument("--target", required=True)
    ap.add_argument("--bin-dir", required=True)
    ap.add_argument("--notices", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--web-runtime", help="folder of CEF runtime files to put next to lumenwall-web-host")
    args = ap.parse_args()
    if args.version.startswith("v"):
        die("--version takes the version without the v")
    top = stage(args)
    mtime = int(os.environ.get("SOURCE_DATE_EPOCH", time.time()))
    if "windows" in args.target:
        archive = top.parent / f"{top.name}.zip"
        zip_(top, archive, mtime)
    else:
        archive = top.parent / f"{top.name}.tar.gz"
        tar_gz(top, archive, mtime)
    shutil.rmtree(top)
    print(archive)


if __name__ == "__main__":
    main()
