# Continuous integration

Workflow: `.github/workflows/ci.yml`. Shared setup (toolchain, Linux packages, cache): `.github/actions/setup-rust/action.yml`.
Runs on every push to `main`, every pull request, on demand, and every Monday (new RustSec advisories, new clippy lints from the stable toolchain).

## Jobs

| Job | Runners | Command |
|---|---|---|
| rustfmt | Linux | `cargo fmt --all --check` |
| cargo-deny | Linux | `cargo deny --locked check advisories`, then `bans licenses sources` (policy in `deny.toml`) |
| clippy | Linux, Windows | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| test | Linux, Windows | `cargo test --workspace --locked`, then GPU tests on a software adapter |
| FFmpeg (Windows DLLs) | Linux (`ubuntu-24.04`) | `packaging/ffmpeg/build.sh`: Lumenwall's minimal LGPL FFmpeg for Windows, cross-compiled with mingw-w64, restored from the cache when `build.sh` and the runner image are unchanged; uploaded as the `ffmpeg-win64` artifact; `packaging/ffmpeg/test-clips.sh` makes short H.264, HEVC (8- and 10-bit), VP9 and VP8 clips with Ubuntu's `ffmpeg` (`ffmpeg-test-clips` artifact) |
| video (Windows, FFmpeg) | Windows | After the FFmpeg job, with its build and `--features lumenwall-engine/video-ffmpeg,lumenwall-video/ffmpeg`: workspace clippy, `cargo test -p lumenwall-video -p lumenwall-engine`, `lumenwall.exe --version` with only the Windows folders on `PATH` (no import of the FFmpeg DLLs), and each generated clip decoded by the `video_png` example on WARP (software decoding) |
| release build | Linux, Windows | `cargo build --workspace --release --locked`; the binary is kept 7 days as a private artifact |

Toolchain: `rust-toolchain.toml` (stable, rustfmt, clippy, Linux and MSVC targets). Builds use `--locked`: commit `Cargo.lock` with every dependency change.
FFmpeg on Windows: the setup action's `ffmpeg: "true"` input takes the `ffmpeg-win64` artifact of the same run (the job must `needs: ffmpeg`), checks each DLL against the build's `BUILD-INFO.txt`, sets `FFMPEG_DIR` and `LIBCLANG_PATH`, and puts the DLLs on `PATH`. It is the build the release ships and developers use ([`packaging/ffmpeg/README.md`](../packaging/ffmpeg/README.md)). The default jobs build without it, like a Windows machine without the FFmpeg SDK.
The cargo cache is written only from `main`; pull requests restore it. The FFmpeg cache (key: runner image + SHA-256 of `packaging/ffmpeg/build.sh`; the build folder is 13.7 MB, about 4.5 MB compressed) is written by any run that misses it.

## Same checks locally

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
LUMENWALL_GPU_TESTS=1 cargo test --workspace --locked -- --ignored gpu
cargo deny --locked check            # cargo install cargo-deny --locked
cargo build --workspace --release --locked
# Windows video feature: FFMPEG_DIR = a build of packaging/ffmpeg/build.sh, its bin\ on PATH
# (packaging/ffmpeg/README.md, crates/lumenwall-video/README.md)
cargo clippy --workspace --all-targets --locked --features lumenwall-engine/video-ffmpeg,lumenwall-video/ffmpeg -- -D warnings
cargo test -p lumenwall-video -p lumenwall-engine --locked --features lumenwall-engine/video-ffmpeg,lumenwall-video/ffmpeg
```

Linux packages: the list in `.github/actions/setup-rust/action.yml` (Ubuntu names), plus `mesa-vulkan-drivers vulkan-tools` to run GPU tests on lavapipe without a GPU:
`VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json WGPU_BACKEND=vulkan WGPU_ADAPTER_NAME=llvmpipe` (the file is `lvp_icd.x86_64.json` on Ubuntu 24.04).

## GPU tests

Convention for every crate:

- A GPU test is `#[ignore = "GPU: LUMENWALL_GPU_TESTS=1 cargo test -- --ignored gpu"]` and has `gpu` in its path (a `gpu` module or a `gpu_` prefix). Plain `cargo test` never touches the GPU.
- `LUMENWALL_GPU_TESTS=1` means "an adapter is guaranteed": a test that cannot get one must **fail**. Without it, the test prints why and returns, so `--ignored` on a machine without GPU does not fail spuriously. CI sets it, so a broken lavapipe/WARP setup cannot pass silently.
- Pick the adapter from the environment, so CI can force the software one (`WGPU_BACKEND`, `WGPU_ADAPTER_NAME`):

```rust
/// Adapter for a GPU test; `None` means "skip" (only allowed without LUMENWALL_GPU_TESTS=1).
pub fn test_adapter() -> Option<wgpu::Adapter> {
    let required = std::env::var_os("LUMENWALL_GPU_TESTS").is_some_and(|v| v == "1");
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    match pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(&instance, None)) {
        Ok(adapter) => Some(adapter),
        Err(err) if required => panic!("LUMENWALL_GPU_TESTS=1 but no GPU adapter: {err}"),
        Err(err) => {
            eprintln!("GPU test skipped: {err}");
            None
        }
    }
}
```

| CI runner | Backend | Adapter |
|---|---|---|
| Linux | Vulkan | Mesa lavapipe (`llvmpipe`), CPU rasteriser |
| Windows | DX12 | WARP (`Microsoft Basic Render Driver`), CPU rasteriser shipped with Windows |

Software rasterisers are correct but not bit-identical to real GPUs: compare images with a tolerance, never exact bytes. If the renderer uses DXC (`Dx12Compiler::DynamicDxc`), `dxcompiler.dll` must be found at test time.

## Windows without a Windows machine

Development happens on Linux; the Windows jobs are the only Windows build. On every push they:

- compile all `cfg(windows)` code with the real MSVC toolchain and lint it with clippy;
- run the unit tests on Windows (paths, Win32 API wrappers, WASAPI/COM error handling);
- render through DX12 on WARP, the same backend Lumenwall uses on Windows;
- publish the release `lumenwall.exe` as an artifact for manual testing.

From Linux, `cargo clippy -p <crate> --target x86_64-pc-windows-msvc` type-checks the Windows code of pure-Rust crates before pushing. Crates whose dependencies compile C or assembly need an MSVC compiler and fail this way (today `lumenwall-script` through rquickjs-sys, `lumenwall-assets` through stacker); for those, rely on CI or an MSVC sysroot such as `cargo xwin`.

Not covered by CI (runner = Windows Server with no GPU, no audio device, one virtual screen): the Windows 11 24H2/25H2 desktop layering (Progman/WorkerW), real GPU drivers and hardware video decoding (D3D11VA, Vulkan Video zero copy: WARP decodes in software), WASAPI loopback on a real device, multi-monitor and DPI scaling. Before each release, run the release artifact on a Windows 11 machine or VM through that list; a self-hosted Windows 11 runner with a GPU can automate it later. Tests that need an audio device must treat "no device" as a normal outcome (`docs/PARITY-POLICY.md`, rule 2).

## Releases

`.github/workflows/release.yml` runs on `v*` tags only: it checks the tag against the `Cargo.toml` version, builds the Linux and Windows archives (no cache), writes the third-party notices with cargo-about (`packaging/about/`), packages them (`packaging/release/package.py`), signs everything with the minisign release key (`MINISIGN_SECRET_KEY`, `MINISIGN_PASSWORD` secrets), verifies the result with the app's own code, and publishes the GitHub release. Only its last job can write to the repository. Procedure and key management: [RELEASING.md](RELEASING.md).

## Maintenance

- Actions are pinned to commit SHAs (tag in the comment). Update them deliberately, e.g. with Dependabot for `github-actions`.
- `ubuntu-latest` and `windows-latest` move to new images over time; pin `ubuntu-24.04` / `windows-2025` if a migration breaks the build.
- A new stable Rust release can add clippy lints and fail CI without a code change: fix the lints, or pin an exact version in `rust-toolchain.toml`.
- Advisories are errors. To accept one, add its id to `[advisories] ignore` in `deny.toml` with a reason.
