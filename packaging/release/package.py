#!/usr/bin/env python3
"""Builds one Lumenwall release archive from compiled programs.

Used by .github/workflows/release.yml (see docs/RELEASING.md); runs the same locally:

    python3 packaging/release/package.py --version 0.2.0 \\
        --target x86_64-unknown-linux-gnu \\
        --bin-dir target/x86_64-unknown-linux-gnu/release \\
        --notices THIRD-PARTY-NOTICES.html --out dist

Windows, with video and web wallpapers (what the release ships):

    python packaging/release/package.py --version 0.2.0 \\
        --target x86_64-pc-windows-msvc --bin-dir <target dir>/x86_64-pc-windows-msvc/release \\
        --notices THIRD-PARTY-NOTICES.html --out dist \\
        --ffmpeg-dir "$FFMPEG_DIR" --web-runtime "$CEF_PATH/<version>/cef_windows_x86_64"

Archive layout (what crates/lumenwall-update installs):

    lumenwall-<version>-<target>/
        lumenwall, lumenwall-ui          (.exe on Windows)
        lumenwall-web-host               only if it was built (with --web-runtime: CEF files)
        lumenwall.bundle                 marks the folder for the built-in updater
        LICENSE-MIT, LICENSE-APACHE, THIRD-PARTY-NOTICES.html, README.txt
        share/applications/*.desktop     Linux
        share/icons/hicolor/...          Linux
        share/gnome-shell/extensions/lumenwall@simnjs.github.io/   Linux
        avutil-60.dll, swresample-6.dll, swscale-9.dll, avcodec-62.dll, avformat-62.dll
                                         Windows, --ffmpeg-dir (FFmpeg, LGPL)
        libcef.dll, chrome_elf.dll, *.pak, icudtl.dat, v8_context_snapshot.bin,
        d3dcompiler_47.dll, vk_swiftshader*, vulkan-1.dll, locales/*.pak
                                         Windows, --web-runtime (CEF)
        licenses/                        Windows: FFmpeg's LGPL-3.0 and the GPL-3.0 text,
                                         CEF's licence and Chromium's CREDITS.html

On Windows, the files that Cargo does not build are listed in windows-runtime.toml:
exactly those are copied, their notices are written into THIRD-PARTY-NOTICES.html, and
the zip is then checked to hold exactly the expected files. `--check <zip>` runs that
check alone on a release zip, requiring FFmpeg and CEF (the workflow's last check).

Linux gets a .tar.gz, Windows a .zip. Entries are sorted, owned by root, with
fixed permissions and the SOURCE_DATE_EPOCH timestamp when it is set, so the same
inputs give the same archive.
"""

import argparse
import gzip
import html
import json
import os
import re
import shutil
import string
import sys
import tarfile
import time
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROGRAMS = ["lumenwall", "lumenwall-ui"]
OPTIONAL_PROGRAMS = ["lumenwall-web-host"]
EXTENSION = "lumenwall@simnjs.github.io"
WINDOWS_RUNTIME = Path(__file__).resolve().parent / "windows-runtime.toml"
ABOUT = ROOT / "packaging" / "about"
TOP_FILES = ["LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES.html", "README.txt", "lumenwall.bundle"]
# Where cargo-about's template (packaging/about/about.hbs) leaves room for them.
BUNDLED_MARKER = re.compile(r"<!-- lumenwall:bundled-libraries[^>]*-->")

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

Video wallpapers use FFmpeg (the av*.dll and sw*.dll files) and web wallpapers
Chromium (lumenwall-web-host.exe, libcef.dll and the files around it, the
locales folder). Their licences are in THIRD-PARTY-NOTICES.html and licenses\\.

"""


def die(msg: str) -> None:
    print(f"package.py: {msg}", file=sys.stderr)
    sys.exit(1)


def windows_runtime() -> dict:
    with WINDOWS_RUNTIME.open("rb") as f:
        return tomllib.load(f)


def copy_file(src: Path, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dest)


def add_ffmpeg(top: Path, ffmpeg_dir: Path, spec: dict) -> None:
    """FFmpeg's DLLs next to lumenwall.exe, exactly those of the manifest, from the
    pinned LGPL build only."""
    if ffmpeg_dir.name != spec["folder"]:
        die(
            f"--ffmpeg-dir is {ffmpeg_dir.name}, but windows-runtime.toml describes {spec['folder']}: "
            "ship the pinned LGPL build, or update the manifest and its notice with it"
        )
    for dll in spec["dlls"]:
        src = ffmpeg_dir / "bin" / dll
        if not src.is_file():
            die(f"{src} is missing")
        copy_file(src, top / dll)
    licence = ffmpeg_dir / "LICENSE.txt"
    text = licence.read_text(encoding="utf-8", errors="replace") if licence.is_file() else ""
    if "GNU LESSER GENERAL PUBLIC LICENSE" not in text or "Version 3" not in text:
        die(f"{licence} is not the LGPL-3.0 text: not an LGPL v3 build")
    copy_file(licence, top / spec["licence-file"])
    copy_file(ABOUT / "GPL-3.0.txt", top / spec["gpl-file"])


def cef_licence(web_runtime: Path, spec: dict) -> bytes:
    """CEF's LICENSE.txt, from the distribution archive the `cef` crate keeps next to the
    folder it extracted (that folder holds the runtime files but not the licence)."""
    archive = web_runtime.parent / spec["archive"]
    if not archive.is_file():
        die(f"{archive} is missing: CEF's LICENSE.txt is read from it")
    top = spec["archive"].removesuffix(".tar.bz2")
    with tarfile.open(archive, mode="r|bz2") as tar:
        for member in tar:
            if member.name == f"{top}/LICENSE.txt" and member.isfile():
                data = tar.extractfile(member)
                if data is not None:
                    return data.read()
    die(f"{archive} has no {top}/LICENSE.txt")
    return b""


def add_cef(top: Path, web_runtime: Path, spec: dict) -> None:
    """The CEF runtime next to lumenwall-web-host.exe: required and recommended files and
    every locale, never the build-time files."""
    try:
        name = json.loads((web_runtime / "archive.json").read_text(encoding="utf-8"))["name"]
    except (OSError, ValueError, KeyError) as e:
        die(f"--web-runtime {web_runtime}: no CEF archive.json ({e})")
    if name != spec["archive"]:
        die(
            f"--web-runtime holds {name}, but windows-runtime.toml describes {spec['archive']}: "
            "update the manifest and its notice with the cef crate"
        )
    for f in spec["required"] + spec["recommended"]:
        src = web_runtime / f
        if not src.is_file():
            die(f"{src} is missing")
        copy_file(src, top / f)
    locales = sorted((web_runtime / "locales").glob("*.pak"))
    if len(locales) != spec["locales"] or not (web_runtime / "locales" / "en-US.pak").is_file():
        die(f"{web_runtime / 'locales'} holds {len(locales)} .pak files, expected {spec['locales']} with en-US.pak")
    for pak in locales:
        copy_file(pak, top / "locales" / pak.name)
    credits = web_runtime / "CREDITS.html"
    if not credits.is_file():
        die(f"{credits} is missing")
    copy_file(credits, top / spec["credits-file"])
    licence = top / spec["licence-file"]
    licence.parent.mkdir(parents=True, exist_ok=True)
    licence.write_bytes(cef_licence(web_runtime, spec))


def bundled_notices(runtime: dict, ffmpeg: bool, cef: bool) -> str:
    """The THIRD-PARTY-NOTICES.html section for the libraries shipped as files."""
    parts = ['<h2 id="bundled">Libraries shipped beside the programs</h2>']

    def fill(template: str, values: dict) -> str:
        text = (ABOUT / template).read_text(encoding="utf-8")
        text = re.sub(r"\A<!--.*?-->\n", "", text, flags=re.S)  # the file's own comment
        return string.Template(text).substitute({k: html.escape(str(v)) for k, v in values.items()})

    if ffmpeg:
        f = runtime["ffmpeg"]
        parts.append(
            fill(
                "ffmpeg.html",
                {
                    "version": f["version"],
                    "major": f["version"].removeprefix("n").split(".")[0],
                    "dlls": ", ".join(f["dlls"]),
                    "source": f["source"],
                    "build_scripts": f["build-scripts"],
                    "archive": f["archive"],
                    "sha256": f["sha256"],
                    "licence_file": f["licence-file"],
                    "gpl_file": f["gpl-file"],
                },
            )
        )
    if cef:
        c = runtime["cef"]
        cef_version, _, chromium = c["version"].partition("+chromium-")
        parts.append(
            fill(
                "cef.html",
                {
                    "version": cef_version,
                    "chromium": chromium,
                    "files": ", ".join(c["required"] + c["recommended"]),
                    "source": c["source"],
                    "chromium_source": c["chromium-source"],
                    "binaries": c["binaries"],
                    "licence_file": c["licence-file"],
                    "credits_file": c["credits-file"],
                },
            )
        )
    return "\n".join(parts)


def write_notices(src: Path, dest: Path, runtime: dict | None, ffmpeg: bool, cef: bool) -> None:
    if not src.is_file():
        die(f"{src} is missing: run cargo about first")
    if runtime is None or not (ffmpeg or cef):
        shutil.copy2(src, dest)
        return
    text = src.read_text(encoding="utf-8")
    if len(BUNDLED_MARKER.findall(text)) != 1:
        die(f"{src} has no '<!-- lumenwall:bundled-libraries -->' marker: generate it with packaging/about/about.hbs")
    section = bundled_notices(runtime, ffmpeg, cef)
    dest.write_text(BUNDLED_MARKER.sub(lambda _: section, text), encoding="utf-8", newline="")


def stage(args: argparse.Namespace) -> tuple[Path, bool, bool]:
    windows = "windows" in args.target
    exe = ".exe" if windows else ""
    name = f"lumenwall-{args.version}-{args.target}"
    out = Path(args.out)
    top = out / name
    if top.exists():
        shutil.rmtree(top)
    top.mkdir(parents=True)
    bin_dir = Path(args.bin_dir)
    runtime = windows_runtime() if windows else None
    if args.ffmpeg_dir and not windows:
        die("--ffmpeg-dir is for Windows targets (Linux uses the system's FFmpeg)")

    for p in PROGRAMS:
        src = bin_dir / f"{p}{exe}"
        if not src.is_file():
            die(f"{src} is missing: build the workspace in release mode first")
        shutil.copy2(src, top / src.name)
    web = False
    for p in OPTIONAL_PROGRAMS:
        src = bin_dir / f"{p}{exe}"
        if not src.is_file():
            if args.web_runtime:
                die(f"--web-runtime is given but {src} was not built")
            continue
        shutil.copy2(src, top / src.name)
        if windows:
            if not args.web_runtime:
                die(f"{src.name} needs the CEF runtime next to it: give --web-runtime")
            add_cef(top, Path(args.web_runtime), runtime["cef"])
            web = True
        elif args.web_runtime:
            # CEF runtime files sit next to the host (crates/lumenwall-web/README.md).
            for item in sorted(Path(args.web_runtime).iterdir()):
                dest = top / item.name
                if item.is_dir():
                    shutil.copytree(item, dest)
                else:
                    shutil.copy2(item, dest)
    ffmpeg = bool(args.ffmpeg_dir)
    if ffmpeg:
        add_ffmpeg(top, Path(args.ffmpeg_dir), runtime["ffmpeg"])

    for lic in ["LICENSE-MIT", "LICENSE-APACHE"]:
        shutil.copy2(ROOT / lic, top / lic)
    write_notices(Path(args.notices), top / "THIRD-PARTY-NOTICES.html", runtime, ffmpeg, web)

    platform = (WINDOWS if windows else LINUX).format(version=args.version, target=args.target)
    readme = README.format(version=args.version, target=args.target, platform=platform)
    newline = "\r\n" if windows else "\n"
    (top / "README.txt").write_bytes(readme.replace("\n", newline).encode())
    # Bytes: the same marker whichever system packages it (write_text would give CRLF on Windows).
    (top / "lumenwall.bundle").write_bytes(f"{args.target} {args.version}\n".encode())

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
    return top, ffmpeg, web


def check_windows_zip(archive: Path, ffmpeg: bool = True, web: bool = True) -> None:
    """Fails unless the Windows zip holds exactly the files a release ships: the
    programs, the bundle files, and (when expected) FFmpeg's DLLs and the CEF runtime of
    windows-runtime.toml with their licences and notices. Nothing else: no build-time
    file of CEF, no FFmpeg tool or other library."""
    runtime = windows_runtime()
    f, c = runtime["ffmpeg"], runtime["cef"]
    try:
        z = zipfile.ZipFile(archive)
    except (OSError, zipfile.BadZipFile) as e:
        die(f"{archive}: {e}")
    with z:
        names = [i.filename for i in z.infolist() if not i.is_dir()]
        tops = {n.split("/", 1)[0] for n in z.namelist()}
        m = re.fullmatch(r"lumenwall-(.+)-(x86_64-pc-windows-msvc)", next(iter(tops), "")) if len(tops) == 1 else None
        if m is None:
            die(f"{archive}: expected one lumenwall-<version>-x86_64-pc-windows-msvc folder, found {sorted(tops)}")
        top, version, target = m.group(0), m.group(1), m.group(2)
        files = {n.removeprefix(f"{top}/") for n in names}
        want = {f"{p}.exe" for p in PROGRAMS} | set(TOP_FILES)
        if ffmpeg:
            want |= set(f["dlls"]) | {f["licence-file"], f["gpl-file"]}
        if web:
            want |= {"lumenwall-web-host.exe", c["licence-file"], c["credits-file"]}
            want |= set(c["required"]) | set(c["recommended"])
        locales = {n for n in files if n.startswith("locales/")}
        if web:
            want |= locales
        problems = [f"missing: {n}" for n in sorted(want - files)]
        problems += [f"not part of a release: {n}" for n in sorted(files - want)]
        never = {n.lower() for n in c["never"]}
        problems += [f"never shipped (windows-runtime.toml): {n}" for n in sorted(files) if n.split("/")[0].lower() in never]
        if web:
            paks = [n for n in locales if re.fullmatch(r"locales/[^/]+\.pak", n)]
            if len(paks) != len(locales) or len(paks) != c["locales"] or "locales/en-US.pak" not in locales:
                problems.append(f"locales/: {len(paks)} .pak files of {len(locales)}, expected {c['locales']} with en-US.pak")
        if not problems:
            notices = z.read(f"{top}/THIRD-PARTY-NOTICES.html").decode("utf-8", errors="replace")
            for shipped, anchor, needles in [
                (ffmpeg, 'id="ffmpeg"', [f["source"], f["build-scripts"], "LGPL-3.0-or-later"]),
                (web, 'id="cef"', [c["source"], c["credits-file"]]),
            ]:
                if shipped and (anchor not in notices or not all(html.escape(s) in notices for s in needles)):
                    problems.append(f"THIRD-PARTY-NOTICES.html lacks the {anchor} section")
            if (ffmpeg or web) and BUNDLED_MARKER.search(notices):
                problems.append("THIRD-PARTY-NOTICES.html still holds the bundled-libraries marker")
            bundle = z.read(f"{top}/lumenwall.bundle").decode(errors="replace")
            if bundle != f"{target} {version}\n":
                problems.append(f"lumenwall.bundle says {bundle!r}")
    if problems:
        die(f"{archive}:\n  " + "\n  ".join(problems))
    print(
        f"checked: {archive.name} holds the programs"
        + (f", {len(f['dlls'])} FFmpeg DLLs" if ffmpeg else "")
        + (f", the CEF runtime ({len(c['required']) + len(c['recommended'])} files, {c['locales']} locales)" if web else "")
        + " and nothing else"
    )


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
    ap.add_argument("--version")
    ap.add_argument("--target")
    ap.add_argument("--bin-dir")
    ap.add_argument("--notices")
    ap.add_argument("--out")
    ap.add_argument(
        "--web-runtime",
        help="CEF runtime to put next to lumenwall-web-host (Windows: the cef crate's "
        "extracted distribution, CEF_PATH/<version>/cef_windows_x86_64, files picked from "
        "windows-runtime.toml; Linux: every file of the folder)",
    )
    ap.add_argument("--ffmpeg-dir", help="Windows: the extracted FFmpeg build (FFMPEG_DIR), DLLs from its bin/")
    ap.add_argument("--check", metavar="ZIP", help="only check a Windows release zip (FFmpeg and CEF required)")
    args = ap.parse_args()
    if args.check:
        check_windows_zip(Path(args.check))
        return
    missing = [f"--{k.replace('_', '-')}" for k in ["version", "target", "bin_dir", "notices", "out"] if not getattr(args, k)]
    if missing:
        ap.error(f"required: {', '.join(missing)}")
    if args.version.startswith("v"):
        die("--version takes the version without the v")
    top, ffmpeg, web = stage(args)
    mtime = int(os.environ.get("SOURCE_DATE_EPOCH", time.time()))
    if "windows" in args.target:
        archive = top.parent / f"{top.name}.zip"
        zip_(top, archive, mtime)
        check_windows_zip(archive, ffmpeg=ffmpeg, web=web)
    else:
        archive = top.parent / f"{top.name}.tar.gz"
        tar_gz(top, archive, mtime)
    shutil.rmtree(top)
    print(archive)


if __name__ == "__main__":
    main()
