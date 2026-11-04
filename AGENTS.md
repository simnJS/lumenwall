# Rules for agents working on Lumenwall

Read these before anything else, together with `CLEANROOM.md`, `docs/PERFORMANCE.md` and `docs/PARITY-POLICY.md`.

- **Privacy:** never put the user's e-mail address, git identity or any personal data in a network request (User-Agent headers, query strings, API payloads). When an API asks for a contact in the User-Agent (crates.io does), use `lumenwall-dev-agent` with no address.
- **Clean-room:** implementers never read the upstream project, its forks, or other third-party Wallpaper Engine reimplementations. See `CLEANROOM.md`.
- **Wallpaper Engine content** is for local testing only and is never committed. See `docs/TESTING.md`.
- **Builds:** use your own `CARGO_TARGET_DIR=target/agent-<name>` so parallel agents don't block each other on the build lock.
- **Scope:** only edit the crate you were assigned; never the root `Cargo.toml` or other crates.
- **Commits:** agents don't commit; the orchestrator does, with no AI co-author trailer.
- **User's session:** never change the user's real desktop state (GSettings/dconf, GNOME extensions, `~/.config`, `~/.local`, autostart, the running daemon). A temporary `HOME` is not enough: also use a private D-Bus session (`dbus-run-session`) and an isolated GNOME Shell, so nothing can reach the real one. No audible tests.
