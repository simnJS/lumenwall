#!/usr/bin/env python3
"""Builds the Debian/Ubuntu package of a Lumenwall release with dpkg-deb.

Used by .github/workflows/release.yml (see docs/RELEASING.md); runs the same locally
on Debian or Ubuntu (needs dpkg-deb, dpkg-shlibdeps from dpkg-dev, and pkg-config
with FFmpeg's development files for the Recommends):

    python3 packaging/release/deb.py --version 0.2.0 \\
        --bin-dir target/x86_64-unknown-linux-gnu/release \\
        --notices THIRD-PARTY-NOTICES.html --out dist

Writes dist/lumenwall-<version>-amd64.deb, a system-wide installation:

    /usr/bin/lumenwall, /usr/bin/lumenwall-ui
    /usr/share/applications/lumenwall-ui.desktop
    /usr/share/icons/hicolor/...
    /usr/share/gnome-shell/extensions/lumenwall@simnjs.github.io/   (each user turns it on)
    /usr/share/doc/lumenwall/copyright, THIRD-PARTY-NOTICES.html

Depends: the shared libraries the programs link (dpkg-shlibdeps: libc, libgcc,
ALSA, PulseAudio) and the Vulkan loader, which is opened at run time. Recommends:
the FFmpeg libraries of the build machine (opened at the first video wallpaper) and
a Vulkan driver. The programs in /usr/bin are recognised by the built-in updater as
a distribution package: it stays off and apt updates them.

Same inputs, same package: files are sorted, owned by root, with fixed permissions
and the SOURCE_DATE_EPOCH timestamp when it is set.
"""

import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROGRAMS = ["lumenwall", "lumenwall-ui"]
EXTENSION = "lumenwall@simnjs.github.io"
FFMPEG = ["libavcodec", "libavformat", "libavutil", "libswscale"]
HOMEPAGE = "https://github.com/simnJS/lumenwall"

DESCRIPTION = """\
Description: animated wallpapers from Wallpaper Engine's Workshop
 Lumenwall plays Wallpaper Engine Workshop wallpapers (scenes, videos, web
 pages) on the Linux desktop with a native engine that renders only when the
 wallpaper is visible. It reads the Workshop items and Wallpaper Engine's
 assets where Steam installed them and ships none of their files.
 .
 This package contains the engine (lumenwall), the desktop app and tray icon
 (lumenwall-ui) and the optional GNOME Shell extension that pauses the
 wallpaper behind maximised windows and passes desktop clicks to it; each user
 turns the extension on from the app (Settings > Installation).
"""


def die(msg: str) -> None:
    print(f"deb.py: {msg}", file=sys.stderr)
    sys.exit(1)


def deb_version(version: str) -> str:
    """0.3.0-beta.1 -> 0.3.0~beta.1, so that a pre-release sorts before 0.3.0."""
    return version.replace("-", "~", 1)


def run(cmd: list[str], cwd: Path | None = None) -> str:
    try:
        return subprocess.run(cmd, cwd=cwd, check=True, capture_output=True, text=True).stdout
    except FileNotFoundError:
        die(f"{cmd[0]} is not installed")
    except subprocess.CalledProcessError as e:
        die(f"{' '.join(cmd)} failed:\n{e.stderr.strip()}")
    return ""


def shlib_depends(binaries: list[Path]) -> str:
    """Depends of the binaries, as dpkg-shlibdeps computes it for a source package."""
    with tempfile.TemporaryDirectory() as tmp:
        d = Path(tmp)
        (d / "debian").mkdir()
        (d / "debian" / "control").write_text(
            "Source: lumenwall\n\nPackage: lumenwall\nArchitecture: any\n"
        )
        args = ["dpkg-shlibdeps", "-O", "--warnings=0"] + [f"-e{b}" for b in binaries]
        out = run(args, cwd=d)
    for line in out.splitlines():
        if line.startswith("shlibs:Depends="):
            return line.split("=", 1)[1].strip()
    die(f"dpkg-shlibdeps printed no shlibs:Depends: {out!r}")
    return ""


def ffmpeg_recommends() -> list[str]:
    """libavcodec61, ...: the libraries matching the headers lumenwall-video was built with."""
    out = []
    for lib in FFMPEG:
        version = run(["pkg-config", "--modversion", lib]).strip()
        major = version.split(".")[0]
        if not major.isdigit():
            die(f"pkg-config {lib}: unexpected version {version!r}")
        out.append(f"{lib}{major}")
    return out


def copyright_file() -> str:
    mit = (ROOT / "LICENSE-MIT").read_text().rstrip()
    apache = (ROOT / "LICENSE-APACHE").read_text().rstrip()
    indent = lambda text: "\n".join(f" {line}" if line.strip() else " ." for line in text.splitlines())  # noqa: E731
    return (
        "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n"
        "Upstream-Name: Lumenwall\n"
        f"Source: {HOMEPAGE}\n"
        "Comment: Third-party Rust crates compiled into the programs and their\n"
        " licences: /usr/share/doc/lumenwall/THIRD-PARTY-NOTICES.html\n"
        "\n"
        "Files: *\n"
        "Copyright: Lumenwall contributors\n"
        "License: MIT or Apache-2.0\n"
        "\n"
        "License: MIT\n"
        f"{indent(mit)}\n"
        "\n"
        "License: Apache-2.0\n"
        f"{indent(apache)}\n"
    )


def desktop_entry() -> str:
    """The app's entry without the per-user installation comments."""
    text = (ROOT / "packaging" / "linux" / "lumenwall-ui.desktop").read_text()
    return "".join(line for line in text.splitlines(keepends=True) if not line.startswith("#"))


def stage(args: argparse.Namespace, root: Path) -> None:
    def put(rel: str, data: bytes, mode: int = 0o644) -> None:
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        p.chmod(mode)

    for name in PROGRAMS:
        src = Path(args.bin_dir) / name
        if not src.is_file():
            die(f"{src} is missing: build the workspace in release mode first")
        put(f"usr/bin/{name}", src.read_bytes(), 0o755)
    put("usr/share/applications/lumenwall-ui.desktop", desktop_entry().encode())
    icons = ROOT / "packaging" / "linux" / "icons" / "hicolor"
    for f in sorted(icons.rglob("*")):
        if f.is_file():
            put(f"usr/share/icons/hicolor/{f.relative_to(icons).as_posix()}", f.read_bytes())
    ext = ROOT / "packaging" / "gnome-extension" / EXTENSION
    for f in sorted(ext.rglob("*")):
        if f.is_file():
            put(f"usr/share/gnome-shell/extensions/{EXTENSION}/{f.relative_to(ext).as_posix()}", f.read_bytes())
    notices = Path(args.notices)
    if not notices.is_file():
        die(f"{notices} is missing: run cargo about first")
    put("usr/share/doc/lumenwall/THIRD-PARTY-NOTICES.html", notices.read_bytes())
    put("usr/share/doc/lumenwall/copyright", copyright_file().encode())


def control(args: argparse.Namespace, root: Path) -> str:
    size = sum(p.stat().st_size for p in root.rglob("*") if p.is_file())
    depends = args.depends or shlib_depends([root / "usr" / "bin" / p for p in PROGRAMS])
    # ash opens the Vulkan loader at run time: dpkg-shlibdeps cannot see it.
    depends = ", ".join(filter(None, [depends, "libvulkan1"]))
    recommends = [] if args.no_ffmpeg else ffmpeg_recommends()
    recommends.append("mesa-vulkan-drivers | vulkan-icd")
    return (
        "Package: lumenwall\n"
        f"Version: {deb_version(args.version)}\n"
        f"Architecture: {args.arch}\n"
        "Maintainer: Lumenwall project <https://github.com/simnJS/lumenwall/issues>\n"
        f"Installed-Size: {(size + 1023) // 1024}\n"
        f"Depends: {depends}\n"
        f"Recommends: {', '.join(recommends)}\n"
        "Section: x11\n"
        "Priority: optional\n"
        f"Homepage: {HOMEPAGE}\n"
        f"{DESCRIPTION}"
    )


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--version", required=True)
    ap.add_argument("--bin-dir", required=True)
    ap.add_argument("--notices", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--arch", default="amd64")
    ap.add_argument("--depends", help="use this Depends instead of running dpkg-shlibdeps")
    ap.add_argument("--no-ffmpeg", action="store_true", help="no FFmpeg Recommends (no pkg-config)")
    args = ap.parse_args()
    if args.version.startswith("v"):
        die("--version takes the version without the v")
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    mtime = int(os.environ.get("SOURCE_DATE_EPOCH", "0")) or None
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "lumenwall"
        stage(args, root)
        sums = []
        for p in sorted(root.rglob("*")):
            if p.is_file():
                sums.append(f"{hashlib.md5(p.read_bytes()).hexdigest()}  {p.relative_to(root).as_posix()}\n")
        meta = root / "DEBIAN"
        meta.mkdir()
        (meta / "control").write_text(control(args, root))
        (meta / "md5sums").write_text("".join(sums))
        for p in [root, *root.rglob("*")]:
            if p.is_dir():
                p.chmod(0o755)
            elif p.parent == meta:
                p.chmod(0o644)
            if mtime is not None:
                os.utime(p, (mtime, mtime), follow_symlinks=False)
        deb = out / f"lumenwall-{args.version}-{args.arch}.deb"
        run(["dpkg-deb", "--root-owner-group", "-Zxz", "--build", str(root), str(deb)])
    print(deb)


if __name__ == "__main__":
    main()
