#!/usr/bin/env bash
# Lumenwall's minimal LGPL build of FFmpeg for Windows x86_64: the five DLLs that
# lumenwall.exe loads at the first video, with only what a video wallpaper needs (demuxers,
# decoders and hardware decoders below), their headers and import libraries (FFMPEG_DIR of
# ffmpeg-sys-next), and the licences of everything compiled into them.
# Why this configuration and this toolchain: packaging/ffmpeg/README.md.
#
#   packaging/ffmpeg/build.sh OUT_DIR
#
# OUT_DIR (created, must not exist) receives:
#   bin/      avutil-60.dll swresample-6.dll swscale-9.dll avcodec-62.dll avformat-62.dll
#   include/  lib/ (avcodec.lib… import libraries, .dll.a, .def, pkgconfig/)
#   licenses/ FFmpeg-LGPL-3.0.txt GPL-3.0.txt nv-codec-headers.txt Vulkan-Headers.txt
#             MinGW-w64.txt
#   BUILD-INFO.txt  what was built and how: sources and SHA-256, configure line,
#                   toolchain, SHA-256 of each DLL (packaging/release/package.py checks it
#                   against packaging/release/windows-runtime.toml)
#
# Hosts:
#   - Linux with mingw-w64 (cross-compilation; what CI and releases use, on ubuntu-24.04):
#       sudo apt-get install gcc-mingw-w64-x86-64-win32 binutils-mingw-w64-x86-64 \
#         mingw-w64-x86-64-dev nasm make pkg-config curl xz-utils
#   - Windows, MSYS2's MINGW64 shell (native gcc; for developers without Linux):
#       pacman -S --needed make diffutils mingw-w64-x86_64-gcc mingw-w64-x86_64-nasm \
#         mingw-w64-x86_64-pkgconf
#
# Environment:
#   FFMPEG_JOBS       parallel jobs (default: every core)
#   FFMPEG_DOWNLOADS  folder keeping the verified source tarballs (default: OUT_DIR.downloads)
#   FFMPEG_WORK       build tree (default: OUT_DIR.work), removed at the end unless
#                     FFMPEG_KEEP_WORK=1
#
# Nothing is downloaded but the three tarballs below, each checked against its SHA-256;
# nothing is installed outside OUT_DIR and the two folders above.

set -euo pipefail

# ---- Pinned sources ------------------------------------------------------------------
# FFmpeg's release tarball (signed by FFmpeg's release key FCF9 86EA 15E6 E293 A564
# 4F10 B432 2F04 D676 58D8: ffmpeg-8.1.3.tar.xz.asc next to it).
FFMPEG_VERSION=8.1.3
FFMPEG_URL=https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz
FFMPEG_SHA256=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
# NVIDIA's codec headers as FFmpeg packages them (MIT; FFmpeg loads nvcuda.dll and
# nvcuvid.dll at run time). 12.1: the oldest series FFmpeg 8.1 accepts, so the oldest
# drivers (Windows 531.61).
NVCODEC_VERSION=12.1.14.1
NVCODEC_URL=https://github.com/FFmpeg/nv-codec-headers/releases/download/n12.1.14.1/nv-codec-headers-12.1.14.1.tar.gz
NVCODEC_SHA256=043e46265517855f13bcb98b3a225b2761b2a83983b1645fa1d05876f1c3a781
# Khronos' Vulkan headers (Apache-2.0, some files also MIT; FFmpeg loads vulkan-1.dll at
# run time). FFmpeg 8.1 needs >= 1.3.277, and >= 1.4.317 for VP9 Vulkan decoding.
VULKAN_VERSION=1.4.341.0
VULKAN_URL=https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/vulkan-sdk-1.4.341.0.tar.gz
VULKAN_SHA256=d73bc5036b6556b741f6985ff600ca720308c5f2850e4a43ceb498bd3de069e7

# ---- What a video wallpaper needs ----------------------------------------------------
# The Workshop takes .mp4 and .webm (H.264, HEVC; VP8, VP9, AV1); lumenwall-video opens
# files and in-memory .tex videos, picks the best video stream and decodes it with
# Vulkan Video, CUDA (NVDEC), D3D11VA or in software (crates/lumenwall-video/README.md).
# The sound track is played by lumenwall-audio, but avformat_find_stream_info() probes
# every stream: without a decoder for the audio one it reads up to 5 s more of the file.
DEMUXERS=(mov matroska)
PROTOCOLS=(file)
DECODERS=(h264 hevc vp8 vp9 av1 aac mp3float opus vorbis flac pcm_s16le pcm_s16be)
PARSERS=(h264 hevc vp8 vp9 av1 aac mpegaudio opus vorbis flac)
# AV_HWDEVICE_TYPE_D3D11VA/CUDA/VULKAN through hw_device_ctx. For D3D11VA that is the
# "d3d11va2" hwaccels (AV_PIX_FMT_D3D11); FFmpeg compiles them into the objects of the
# legacy ad-hoc "d3d11va" ones (dxva2_h264.o…), so both are enabled. av1 has no software
# decoder here (FFmpeg's own is hardware-only; dav1d would be an external library): AV1
# videos need one of these.
HWACCELS=(
    h264_d3d11va hevc_d3d11va vp9_d3d11va av1_d3d11va
    h264_d3d11va2 hevc_d3d11va2 vp9_d3d11va2 av1_d3d11va2
    h264_nvdec hevc_nvdec vp8_nvdec vp9_nvdec av1_nvdec
    h264_vulkan hevc_vulkan vp9_vulkan av1_vulkan
)
DLLS=(avutil-60.dll swresample-6.dll swscale-9.dll avcodec-62.dll avformat-62.dll)
# Imports allowed in the DLLs: Windows itself (the C runtime of the toolchain: msvcrt.dll,
# or the UCRT's api-ms-win-crt-*), and each other. Anything else (libgcc_s_seh-1.dll,
# libwinpthread-1.dll, zlib1.dll…) would be a DLL that Lumenwall does not ship.
ALLOWED_IMPORTS='^(kernel32|user32|advapi32|bcrypt|ole32|shell32|msvcrt|ucrtbase|api-ms-win-crt-[a-z0-9-]+|avutil-60|swresample-6|swscale-9|avcodec-62|avformat-62)\.dll$'

die() {
    echo "build.sh: $*" >&2
    exit 1
}

[[ $# -eq 1 && -n "$1" ]] || die "usage: packaging/ffmpeg/build.sh OUT_DIR"
[[ ! -e "$1" ]] || die "$1 already exists"
mkdir -p "$(dirname "$1")"
OUT="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SCRIPT_SHA256=$(sha256sum "$SCRIPT_DIR/build.sh" | cut -d' ' -f1)
# Native Windows path for MSYS2's native tools (pkgconf, gcc), and MSYS2 path for its own
# (GNU tar reads C:/… as a remote host); the path itself elsewhere.
native() {
    if command -v cygpath >/dev/null; then cygpath -m "$1"; else printf '%s\n' "$1"; fi
}
posix() {
    if command -v cygpath >/dev/null; then cygpath -u "$1"; else printf '%s\n' "$1"; fi
}
OUT=$(posix "$OUT")
DOWNLOADS=$(posix "${FFMPEG_DOWNLOADS:-$OUT.downloads}")
WORK=$(posix "${FFMPEG_WORK:-$OUT.work}")
JOBS=${FFMPEG_JOBS:-$(nproc)}

case "$(uname -s)" in
    Linux)
        CROSS=x86_64-w64-mingw32-
        HOST_ARGS=(--enable-cross-compile --cross-prefix="$CROSS" --target-os=mingw32 --arch=x86_64)
        HOST_LDFLAGS=
        ;;
    MINGW64_NT*)
        [[ "${MSYSTEM:-}" == MINGW64 ]] || die "run it from MSYS2's MINGW64 shell (MSYSTEM=MINGW64)"
        CROSS=
        HOST_ARGS=(--target-os=mingw32 --arch=x86_64)
        # MSYS2's gcc links winpthread into everything unless told not to; configure then
        # finds clock_gettime and nanosleep there and avutil-60.dll imports
        # libwinpthread-1.dll. Without it, the same functions as with Ubuntu's -win32 gcc.
        HOST_LDFLAGS=" -no-pthread"
        ;;
    *) die "unsupported host $(uname -s): Linux with mingw-w64, or MSYS2 MINGW64" ;;
esac
for tool in "${CROSS}gcc" "${CROSS}objdump" "${CROSS}dlltool" nasm make pkg-config curl tar xz sha256sum; do
    command -v "$tool" >/dev/null || die "$tool not found (see the packages at the top of this script)"
done

# ---- Sources -------------------------------------------------------------------------
fetch() { # url sha256 -> path of the verified tarball
    local url=$1 sha=$2 file
    file="$DOWNLOADS/$(basename "$url")"
    if [[ ! -f "$file" ]] || [[ "$(sha256sum "$file" | cut -d' ' -f1)" != "$sha" ]]; then
        mkdir -p "$DOWNLOADS"
        curl -fsSL --retry 3 -A lumenwall-build -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    local got
    got=$(sha256sum "$file" | cut -d' ' -f1)
    [[ "$got" == "$sha" ]] || die "$url: SHA-256 is $got, expected $sha"
    printf '%s\n' "$file"
}

rm -rf "$WORK"
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
FFMPEG_TAR=$(fetch "$FFMPEG_URL" "$FFMPEG_SHA256")
NVCODEC_TAR=$(fetch "$NVCODEC_URL" "$NVCODEC_SHA256")
VULKAN_TAR=$(fetch "$VULKAN_URL" "$VULKAN_SHA256")
tar -xJf "$FFMPEG_TAR" -C "$WORK"
tar -xzf "$NVCODEC_TAR" -C "$WORK"
tar -xzf "$VULKAN_TAR" -C "$WORK"
SRC="$WORK/ffmpeg-$FFMPEG_VERSION"
NVCODEC="$WORK/nv-codec-headers-$NVCODEC_VERSION"
VULKAN="$WORK/Vulkan-Headers-vulkan-sdk-$VULKAN_VERSION"
[[ -f "$SRC/configure" && -f "$NVCODEC/Makefile" && -d "$VULKAN/include/vulkan" ]] ||
    die "unexpected tarball layout under $WORK"

# Build-time headers, seen by FFmpeg's configure through pkg-config only (no host .pc).
DEPS="$WORK/deps"
make -s -C "$NVCODEC" PREFIX="$(native "$DEPS")" install
mkdir -p "$DEPS/include" "$DEPS/lib/pkgconfig"
cp -R "$VULKAN/include/vulkan" "$VULKAN/include/vk_video" "$DEPS/include/"
cat > "$DEPS/lib/pkgconfig/vulkan.pc" <<EOF
prefix=$(native "$DEPS")
includedir=\${prefix}/include

Name: Vulkan-Headers
Description: Vulkan headers only (FFmpeg loads vulkan-1.dll at run time)
Version: ${VULKAN_VERSION%.*}
Cflags: -I\${includedir}
EOF
export PKG_CONFIG_LIBDIR="$(native "$DEPS/lib/pkgconfig")"
unset PKG_CONFIG_PATH PKG_CONFIG_SYSROOT_DIR

# ---- Configure -----------------------------------------------------------------------
join() { local IFS=,; printf '%s' "$*"; }
CONFIGURE=(
    "${HOST_ARGS[@]}"
    # A fixed prefix: the configure line is compiled into the DLLs, keep paths out of it.
    --prefix=/ffmpeg
    --pkg-config=pkg-config
    # Licence: LGPL only. version3 because the Vulkan headers compiled in are Apache-2.0,
    # compatible with the (L)GPL version 3, not with version 2.1.
    --disable-gpl --disable-nonfree --enable-version3
    --enable-shared --disable-static
    --disable-programs --disable-doc --disable-debug
    --disable-avdevice --disable-avfilter --disable-network
    # No external library at all, then exactly these: Win32 threads, and the hardware
    # interfaces that FFmpeg loads at run time (headers only at build time).
    --disable-autodetect --disable-everything
    --enable-w32threads
    --enable-d3d11va
    --enable-ffnvcodec --enable-cuda --enable-nvdec
    --enable-vulkan
    --enable-protocol="$(join "${PROTOCOLS[@]}")"
    --enable-demuxer="$(join "${DEMUXERS[@]}")"
    --enable-decoder="$(join "${DECODERS[@]}")"
    --enable-parser="$(join "${PARSERS[@]}")"
    --enable-hwaccel="$(join "${HWACCELS[@]}")"
    # No libgcc_s_seh-1.dll; no PE timestamp (same inputs, same DLLs).
    --extra-ldflags="-static-libgcc -Wl,--no-insert-timestamp$HOST_LDFLAGS"
)
cd "$SRC"
if ! ./configure "${CONFIGURE[@]}" > "$WORK/configure.out"; then
    tail -n 30 ffbuild/config.log >&2 || true
    die "configure failed (log: $SRC/ffbuild/config.log)"
fi

# configure silently drops what a header or dependency lacks: require every component.
problems=()
check_def() { # file macro value
    grep -Eq "^#define $2 $3\$" "$1" || problems+=("$2 is not $3 in $(basename "$1")")
}
upper() { printf '%s' "$1" | tr '[:lower:]' '[:upper:]'; }
for d in "${DEMUXERS[@]}"; do check_def config_components.h "CONFIG_$(upper "$d")_DEMUXER" 1; done
for p in "${PROTOCOLS[@]}"; do check_def config_components.h "CONFIG_$(upper "$p")_PROTOCOL" 1; done
for d in "${DECODERS[@]}"; do check_def config_components.h "CONFIG_$(upper "$d")_DECODER" 1; done
for p in "${PARSERS[@]}"; do check_def config_components.h "CONFIG_$(upper "$p")_PARSER" 1; done
for h in "${HWACCELS[@]}"; do check_def config_components.h "CONFIG_$(upper "$h")_HWACCEL" 1; done
for c in D3D11VA CUDA NVDEC FFNVCODEC VULKAN SHARED SWSCALE SWRESAMPLE VERSION3; do check_def config.h "CONFIG_$c" 1; done
for c in GPL NONFREE STATIC AVDEVICE AVFILTER NETWORK; do check_def config.h "CONFIG_$c" 0; done
check_def config.h HAVE_W32THREADS 1
check_def config.h FFMPEG_LICENSE '"LGPL version 3 or later"'
if (( ${#problems[@]} )); then
    printf '  %s\n' "${problems[@]}" >&2
    die "the configuration is not the one asked for (configure output: $WORK/configure.out)"
fi
LICENSE=$(sed -n 's/^#define FFMPEG_LICENSE "\(.*\)"$/\1/p' config.h)
CONFIGURATION=$(sed -n 's/^#define FFMPEG_CONFIGURATION "\(.*\)"$/\1/p' config.h)

# ---- Build and install ---------------------------------------------------------------
# `make install` strips the DLLs, and strip writes the current time into the PE header
# unless SOURCE_DATE_EPOCH is set: 0, as the linker's --no-insert-timestamp.
export SOURCE_DATE_EPOCH=0
if ! make -j"$JOBS" > "$WORK/make.out" 2>&1; then
    grep -E -i -m 20 'error|undefined reference' "$WORK/make.out" >&2 || tail -n 40 "$WORK/make.out" >&2
    die "make failed (log: $WORK/make.out)"
fi
STAGE="$WORK/stage"
make install DESTDIR="$STAGE" > "$WORK/install.out"
DEST="$STAGE/ffmpeg"  # moved to OUT_DIR once every check passed
# The MSVC import libraries (made with dlltool) go next to the other libraries, where
# ffmpeg-sys-next looks (FFMPEG_DIR/lib); FFmpeg's install puts them next to the DLLs.
mv "$DEST"/bin/*.lib "$DEST/lib/"
rm -rf "$DEST/share"

# Exactly the five DLLs, importing nothing Lumenwall does not ship.
mapfile -t built < <(cd "$DEST/bin" && ls)
[[ "$(printf '%s\n' "${built[@]}" | sort)" == "$(printf '%s\n' "${DLLS[@]}" | sort)" ]] ||
    die "bin/ holds ${built[*]}, expected ${DLLS[*]}"
for dll in "${DLLS[@]}"; do
    while read -r import; do
        lower=$(printf '%s' "$import" | tr '[:upper:]' '[:lower:]')
        [[ "$lower" =~ $ALLOWED_IMPORTS ]] || problems+=("$dll imports $import")
    done < <("${CROSS}objdump" -p "$DEST/bin/$dll" | sed -n 's/^[[:space:]]*DLL Name: //p' | tr -d '\r')
done
if (( ${#problems[@]} )); then
    printf '  %s\n' "${problems[@]}" >&2
    die "the DLLs need libraries Lumenwall does not ship"
fi

# ---- Licences of what is inside ------------------------------------------------------
L="$DEST/licenses"
mkdir -p "$L"
cp "$SRC/COPYING.LGPLv3" "$L/FFmpeg-LGPL-3.0.txt"
# The LGPL-3.0 is a set of additional permissions on top of the GPL-3.0.
cp "$SRC/COPYING.GPLv3" "$L/GPL-3.0.txt"
{
    echo "nv-codec-headers $NVCODEC_VERSION ($NVCODEC_URL), MIT."
    echo "Compiled into avutil-60.dll and avcodec-62.dll (CUDA and NVDEC). Notices of its headers:"
    seen=()
    for h in "$NVCODEC"/include/ffnvcodec/*.h; do
        block=$(sed -n '1,/\*\//p' "$h")
        for s in "${seen[@]+"${seen[@]}"}"; do [[ "$s" == "$block" ]] && continue 2; done
        seen+=("$block")
        printf '\n%s\n' "$block"
    done
} > "$L/nv-codec-headers.txt"
{
    echo "Vulkan-Headers $VULKAN_VERSION ($VULKAN_URL)."
    echo "Compiled into avutil-60.dll and avcodec-62.dll (Vulkan Video decoding)."
    echo "Copyright 2015-2026 The Khronos Group Inc."
    echo
    cat "$VULKAN/LICENSE.md"
    printf '\n\n'
    cat "$VULKAN/LICENSES/Apache-2.0.txt"
    printf '\n\n'
    cat "$VULKAN/LICENSES/MIT.txt"
} > "$L/Vulkan-Headers.txt"
# The MinGW-w64 runtime (start-up code, a few C functions) is linked statically into each
# DLL; some of its parts ask for their notices in binary distributions. libgcc is too,
# under the GCC Runtime Library Exception, which sets no condition on the result.
SYSROOT=$("${CROSS}gcc" -print-sysroot)
MINGW_NOTICES=(
    /usr/share/doc/mingw-w64-x86-64-dev/copyright                   # Debian, Ubuntu
    "${MINGW_PREFIX:-/mingw64}/share/licenses/crt/COPYING.MinGW-w64-runtime.txt" # MSYS2
    "$SYSROOT/mingw/share/licenses/crt/COPYING.MinGW-w64-runtime.txt"
)
for f in "${MINGW_NOTICES[@]}"; do
    if [[ -f "$f" ]]; then MINGW_NOTICE=$f; break; fi
done
[[ -n "${MINGW_NOTICE:-}" ]] || die "no licence file of the MinGW-w64 runtime among: ${MINGW_NOTICES[*]}"
MINGW_VERSION=$(printf '#include <_mingw.h>\n__MINGW64_VERSION_STR\n' | "${CROSS}gcc" -E -P - | tail -n 1 | tr -d '" ')
{
    echo "MinGW-w64 runtime $MINGW_VERSION, linked statically into each DLL."
    echo "Licences, from the toolchain that built them ($MINGW_NOTICE):"
    echo
    cat "$MINGW_NOTICE"
} > "$L/MinGW-w64.txt"

# ---- Record --------------------------------------------------------------------------
{
    echo "# Lumenwall's FFmpeg build (packaging/ffmpeg/build.sh), read by packaging/release/package.py."
    echo "ffmpeg-version=$FFMPEG_VERSION"
    echo "ffmpeg-source=$FFMPEG_URL"
    echo "ffmpeg-sha256=$FFMPEG_SHA256"
    echo "nv-codec-headers=$NVCODEC_VERSION $NVCODEC_URL $NVCODEC_SHA256"
    echo "vulkan-headers=$VULKAN_VERSION $VULKAN_URL $VULKAN_SHA256"
    echo "licence=$LICENSE"
    echo "configuration=$CONFIGURATION"
    echo "build-script-sha256=$SCRIPT_SHA256"
    echo "host=$(uname -s)"
    echo "compiler=$("${CROSS}gcc" --version | head -n 1)"
    echo "compiler-threads=$("${CROSS}gcc" -v 2>&1 | sed -n 's/^Thread model: //p')"
    echo "mingw-w64=$MINGW_VERSION"
    echo "binutils=$("${CROSS}objdump" --version | head -n 1)"
    echo "nasm=$(nasm -v)"
    for dll in "${DLLS[@]}"; do
        echo "dll=$dll $(stat -c %s "$DEST/bin/$dll") $(sha256sum "$DEST/bin/$dll" | cut -d' ' -f1)"
    done
} > "$DEST/BUILD-INFO.txt"

mv "$DEST" "$OUT"
cd "$OUT"
if [[ "${FFMPEG_KEEP_WORK:-0}" != 1 ]]; then rm -rf "$WORK"; fi
total=0
for dll in "${DLLS[@]}"; do total=$((total + $(stat -c %s "bin/$dll"))); done
cat BUILD-INFO.txt
echo "FFmpeg $FFMPEG_VERSION ($LICENSE) in $OUT: ${#DLLS[@]} DLLs, $total bytes"
