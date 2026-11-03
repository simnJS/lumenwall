# Contributing to Lumenwall

Thanks for helping. Bug reports from real desktops are as useful as code: Lumenwall has mostly been tested on one GNOME machine so far, so a report from KDE, sway, Hyprland, a native Xorg session or Windows is worth a lot.

## Reporting a problem

Open an issue with the **Bug report** template. It asks for the information we always need: Lumenwall version or commit, OS, desktop and compositor, session type, GPU and driver, and the Workshop ID of the wallpaper. Logs: run with `LUMENWALL_LOG=debug`; for performance problems add the output of `LUMENWALL_STATS=1`.

Never attach Wallpaper Engine or Workshop files (`.pkg`, `.tex`, `project.json` of someone else's wallpaper). The Workshop ID is enough to reproduce.

## Before writing code

1. Read [`CLEANROOM.md`](CLEANROOM.md). Code is written from `specs/` and public documentation only. **Do not read, copy or adapt** code from GPL or AGPL Wallpaper Engine reimplementations or their forks, even to "check" something. A behaviour question goes to the spec instead: open an issue and it will be answered in `specs/`.
2. Read [`docs/PERFORMANCE.md`](docs/PERFORMANCE.md) and [`docs/PARITY-POLICY.md`](docs/PARITY-POLICY.md). A change that looks the same but costs more CPU, GPU, memory or wake-ups is a regression.
3. For larger changes, open an issue first so we can agree on the approach.

## Rules

- **No Wallpaper Engine content in the repository**, not even as a test fixture. Tests against real content are `#[ignore]` and read local paths ([`docs/TESTING.md`](docs/TESTING.md)).
- **Dependencies** must pass `cargo deny check` ([`deny.toml`](deny.toml)): permissive licences only, no GPL/AGPL, crates.io sources only. Explain any new dependency in the pull request.
- **One concern per pull request.** Keep changes inside the crate they belong to; update that crate's README (specs used, measurements, deviations) in the same pull request.
- **Measure.** Performance-sensitive changes include numbers (before and after, machine, conditions), like the tables in the crate READMEs.
- **UI and visuals** follow [`docs/BRAND.md`](docs/BRAND.md): theme tokens instead of raw colours, Lumenwall's own icons, screenshots from the demo library only.
- **Privacy:** no telemetry, and no personal data (e-mail, user name, paths) in network requests.

## Checks

Run what CI runs ([`docs/CI.md`](docs/CI.md)):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny --locked check
```

## Licence

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Lumenwall by you, as defined in the Apache-2.0 license, shall be dual licensed under MIT OR Apache-2.0, without any additional terms or conditions.
