# FFmpeg for the Windows release

`build.sh` builds the FFmpeg that Lumenwall ships on Windows: FFmpeg's release tarball,
unmodified, configured with only what a video wallpaper needs, LGPL only, with no external
library. The five DLLs that `lumenwall.exe` loads at the first video
(`crates/lumenwall-video/README.md`, "Loaded at the first video") keep FFmpeg's standard
names, so any other FFmpeg 8 shared build can replace them.

| | BtbN's `lgpl-shared` (until 0.1.0) | This build |
|---|---|---|
| DLLs | 128.2 MB (40.3 MB once LZMA-compressed in the installer) | 10.9 MB (10.4 MiB; 2.9 MB in the installer)¹ |
| `Lumenwall-Setup.exe` | ≈ 182 MB | ≈ 145 MB (estimate)¹; the rest is CEF |
| Compiled in | FFmpeg plus dav1d, libaom, libvpx, OpenH264, libass, HarfBuzz, FreeType… (notices not reproduced) | FFmpeg, the nv-codec-headers and Vulkan-Headers headers, the MinGW-w64 runtime: five licence files (FFmpeg's LGPL-3.0 and GPL-3.0, one for each of the others), all shipped |
| Source | a git commit, and BtbN's scripts | `ffmpeg-8.1.3.tar.xz`, SHA-256 pinned here |

¹ Measured on the build of this script in MSYS2 (gcc 16.2, November 2026): `avcodec-62.dll`
6 867 456 bytes, `swscale-9.dll` 2 073 088, `avutil-60.dll` 1 206 784, `avformat-62.dll`
588 800, `swresample-6.dll` 177 664. The build of CI and releases (Ubuntu's gcc 13) was
not measured yet: the `ffmpeg` job prints its sizes. The
compressed sizes are one solid LZMA stream with an 8 MiB dictionary (makensis'
`SetCompressor /SOLID lzma`) over each set of DLLs; the installer estimate is the previous
one (`docs/RELEASING.md`, "Sizes") minus BtbN's 40.3 MB plus these 2.9 MB.

Who uses it:

- CI (`.github/workflows/ci.yml`): the `ffmpeg` job builds it on `ubuntu-24.04` (cached by
  the runner image and the SHA-256 of `build.sh`) and uploads it as the `ffmpeg-win64`
  artifact; the `video (Windows, FFmpeg)` job takes it through the setup action's
  `ffmpeg: "true"` input and decodes the clips of `test-clips.sh` with it.
- Releases (`.github/workflows/release.yml`): the same job without cache; `package.py`
  ships the DLLs and the licences, and checks the build's `BUILD-INFO.txt` against
  `packaging/release/windows-runtime.toml`.
- Developers: the same script, or the CI artifact (`gh run download --repo simnJS/lumenwall
  --name ffmpeg-win64 <run id of a main CI run>`), unpacked anywhere, as `FFMPEG_DIR`.

## Toolchain: mingw-w64, cross-compiled on Linux

The two candidates give the same DLL names (`avutil-60.dll`…, FFmpeg's names on Windows)
and import libraries that MSVC's linker takes (`lib/avcodec.lib`…, made by `dlltool` here).
mingw-w64 on Linux was chosen because:

- **It is the toolchain the decoding paths were validated with.** BtbN's builds are made
  with gcc and mingw-w64 too: zero copy (Vulkan Video on wgpu's device), CUDA and D3D11VA
  were measured with them (`crates/lumenwall-video/README.md`). Same compiler family,
  same headers, same code generation of `hwcontext_vulkan`, `hwcontext_d3d11va` and the
  NVDEC loader.
- **It loads cleanly.** The DLLs import only Windows' own DLLs (`KERNEL32`, `msvcrt`…:
  `build.sh` fails on anything else, e.g. `libgcc_s_seh-1.dll` or `libwinpthread-1.dll`),
  libgcc and the MinGW-w64 runtime are linked statically, threads are Win32 threads.
  `lumenwall.exe` still has no import entry on them (`LoadLibraryExW` at the first video).
- **It is fast and fixed.** FFmpeg's `configure` runs in about a minute on Linux, against
  5–10 minutes under MSYS2 (every check spawns the compiler through MSYS2's fork
  emulation). The toolchain is the `ubuntu-24.04` image's packages (gcc 13, mingw-w64
  11), the same for CI and releases. The PE timestamp is 0 (`--no-insert-timestamp` for the
  linker, `SOURCE_DATE_EPOCH=0` for the strip of `make install`) and the configure line
  holds no path of the machine: two builds in MSYS2, in different folders, gave
  byte-identical DLLs. Each build records the SHA-256 of its DLLs in `BUILD-INFO.txt`, so
  CI's and a release's can be compared.
- **The maintainers build on Linux** (`docs/CI.md`); on Windows, MSYS2's MINGW64 shell runs
  the same script with its native gcc (below).

MSVC through MSYS2's make would also work, but needs MSYS2, `vcvars64.bat` and a `PATH`
where MSVC's `link.exe` comes before MSYS2's, compiles without FFmpeg's inline assembly
(slower software decoding), and takes whichever Visual Studio the `windows-latest` image
holds. Its one advantage, no MinGW-w64 runtime notice, is a single text file.

## Configuration

`build.sh` passes, besides the host options:

| Options | Why |
|---|---|
| `--disable-gpl --disable-nonfree --enable-version3` | LGPL only. Version 3 because the Vulkan headers compiled in are Apache-2.0, compatible with the (L)GPL version 3, not 2.1 |
| `--enable-shared --disable-static` | DLLs only, never linked statically (`deny.toml`, LGPL rule) |
| `--disable-programs --disable-doc --disable-debug --disable-avdevice --disable-avfilter --disable-network` | Lumenwall uses none of them (`ffmpeg-sys-next` is built with `avcodec avformat avutil swscale`; swresample is needed by avcodec) |
| `--disable-autodetect --disable-everything` | no external library, no component, then only these: |
| `--enable-w32threads` | threads (no winpthread) |
| `--enable-d3d11va`, `--enable-ffnvcodec --enable-cuda --enable-nvdec`, `--enable-vulkan` | the hardware devices lumenwall-video opens (`AV_HWDEVICE_TYPE_D3D11VA`, `CUDA`, `VULKAN`); FFmpeg loads `d3d11.dll`, `nvcuda.dll`/`nvcuvid.dll` and `vulkan-1.dll` itself at run time, the build needs their headers only |
| protocol `file` | `avformat_open_input` on a path (in-memory `.tex` videos use a custom `AVIOContext`) |
| demuxers `mov`, `matroska` | `.mp4`/`.m4v`/`.mov` and `.webm`/`.mkv`: the Workshop takes `.mp4` and `.webm` |
| decoders `h264 hevc vp8 vp9 av1` and their parsers | the video codecs of those containers. `av1` is FFmpeg's hardware-only AV1 decoder: no software AV1 (dav1d would be an external library) |
| hwaccels `{h264,hevc,vp9,av1}_d3d11va` and `_d3d11va2`, `{h264,hevc,vp8,vp9,av1}_nvdec`, `{h264,hevc,vp9,av1}_vulkan` | every hardware decoder of those codecs for the three devices. `d3d11va2` is the one used with a device context (`AV_PIX_FMT_D3D11`); FFmpeg compiles it into the objects of the legacy ad-hoc `d3d11va` one, which therefore comes along |
| decoders `aac mp3float opus vorbis flac pcm_s16le pcm_s16be` and their parsers | sound tracks: lumenwall-audio plays them, but `avformat_find_stream_info` probes every stream and, without a decoder, reads up to 5 s more of the file looking for its parameters |

swscale (pixel formats the GPU converter cannot read) and swresample stay. Bitstream filters
and other internal parts come with the decoders that select them (`vp9_superframe_split`…).
`build.sh` fails when `configure` leaves out anything asked for (an older header, a missing
dependency): it checks `config.h` and `config_components.h`.

Left out, with what happens: AVI and WMV files (local imports only, never on the Workshop),
MPEG-4 Part 2, MJPEG and ProRes video, AC-3 and E-AC-3 sound (the video plays, the probe of
that stream reads further), compressed Matroska tracks and compressed MOV headers (zlib),
AV1 without a hardware decoder. Each is one more name in `build.sh`.

## Licences of what ships

The DLLs contain FFmpeg (LGPL-3.0-or-later), code from the headers of nv-codec-headers
(MIT) and Vulkan-Headers (Apache-2.0, some files also MIT), the MinGW-w64 runtime (its own
licence file lists the notices some of its parts require) and libgcc (GPL-3.0 with the GCC
Runtime Library Exception: no condition on the DLLs). `build.sh` writes into `licenses/`:

| File | From |
|---|---|
| `FFmpeg-LGPL-3.0.txt`, `GPL-3.0.txt` | `COPYING.LGPLv3`, `COPYING.GPLv3` of the FFmpeg tarball |
| `nv-codec-headers.txt` | the MIT notices of its headers |
| `Vulkan-Headers.txt` | its `LICENSE.md`, `LICENSES/Apache-2.0.txt`, `LICENSES/MIT.txt` |
| `MinGW-w64.txt` | the toolchain's own: `/usr/share/doc/mingw-w64-x86-64-dev/copyright` (Ubuntu), `COPYING.MinGW-w64-runtime.txt` (MSYS2) |

`package.py` ships them in the zip's `licenses\` with `BUILD-INFO.txt` (as
`FFmpeg-BUILD-INFO.txt`: sources and SHA-256, configure line, toolchain), and writes
FFmpeg's section of `THIRD-PARTY-NOTICES.html` (`packaging/about/ffmpeg.html`): the LGPL
statement, the tarball's URL and SHA-256, this script at the release's tag, the configure
options, and the other licences. The corresponding source is FFmpeg's tarball, unmodified,
plus this script.

## Building it

Linux (Debian/Ubuntu package names; `ubuntu-24.04` is what CI uses):

```sh
sudo apt-get install gcc-mingw-w64-x86-64-win32 binutils-mingw-w64-x86-64 mingw-w64-x86-64-dev nasm make pkg-config
packaging/ffmpeg/build.sh /tmp/ffmpeg-win64
```

Windows: MSYS2 (no installer needed: the `msys2-base-x86_64` archive extracted in a folder
of yours), then from its MINGW64 shell:

```sh
pacman -S --needed make diffutils mingw-w64-x86_64-gcc mingw-w64-x86_64-nasm mingw-w64-x86_64-pkgconf
packaging/ffmpeg/build.sh "$LOCALAPPDATA/lumenwall-dev/ffmpeg/lumenwall-8.1.3"
```

MSYS2's gcc is newer than Ubuntu's (gcc 16.2 and mingw-w64 15.0 in November 2026), so its
DLLs are equivalent to CI's, not identical. It also links winpthread into everything by
default: `build.sh` passes `-no-pthread` there, so that `configure` finds the same
functions as with Ubuntu's `-win32` compiler and the DLLs import no `libwinpthread-1.dll`.
`FFMPEG_JOBS` limits the parallel jobs. The output
folder is `FFMPEG_DIR` (crates/lumenwall-video/README.md, "Windows"): `bin/` (the DLLs, also
what goes on `PATH`), `include/`, `lib/`, `licenses/`, `BUILD-INFO.txt`.

## Updating

Change the version, URL and SHA-256 in `build.sh` and `[ffmpeg]` of
`packaging/release/windows-runtime.toml` together (`package.py` refuses a build its manifest
does not describe), check FFmpeg's tarball signature (`ffmpeg-<version>.tar.xz.asc`, FFmpeg's
release key FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8), and run the decoding checks of
`crates/lumenwall-video/README.md` on a GPU. A new FFmpeg major changes the DLL names: the
bindings (`ffmpeg-sys-next`), the loader's names and the manifest follow it.
