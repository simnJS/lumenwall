# Lumen

A fast animated wallpaper engine written in Rust, for Linux (Wayland and X11) and Windows. Lumen plays scene, video and web wallpapers from the Steam Workshop for Wallpaper Engine.

> Work in progress. Nothing renders yet.

Lumen is not affiliated with or endorsed by Wallpaper Engine or its developers. Playing Workshop wallpapers requires owning Wallpaper Engine on Steam; Lumen ships none of its files.

## Goals

1. **Performance first:** zero work when the desktop is hidden, zero allocations per frame, native GPU texture formats, hardware video decoding without CPU copies. See [docs/PERFORMANCE.md](docs/PERFORMANCE.md).
2. **Parity:** wallpapers look and behave the same as in the reference implementation. See [docs/PARITY-POLICY.md](docs/PARITY-POLICY.md).
3. **Clean-room:** written from behavioural specifications only. See [CLEANROOM.md](CLEANROOM.md).

## Layout

| Crate | Role |
|---|---|
| `lumen` | Command-line application |
| `lumen-formats` | `.pkg` archives and `.tex` textures |
| `lumen-assets` | Virtual file system, Steam discovery |
| `lumen-scene` | Project, scene, material and effect model |
| `lumen-shader` | Wallpaper shaders to GPU modules |
| `lumen-render` | wgpu renderer |
| `lumen-particles` | Particle simulation |
| `lumen-platform` | Desktop surfaces, pause detection, input |
| `lumen-audio` | Sound playback, audio capture and spectrum |
| `lumen-video` | Video wallpapers |
| `lumen-script` | SceneScript runtime |
| `lumen-web` | Web wallpapers |

## Building

```bash
cargo build --release
```

See [docs/CI.md](docs/CI.md) for the full set of checks and [docs/TESTING.md](docs/TESTING.md) for tests against real Workshop content.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Lumen by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

Contributors must follow the clean-room rules in [CLEANROOM.md](CLEANROOM.md): never copy or adapt code from GPL or AGPL Wallpaper Engine reimplementations.
