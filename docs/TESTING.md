# Testing against real Wallpaper Engine content

Wallpaper Engine files are copyrighted by their authors and by the Wallpaper Engine developers. They are used for **local testing only**: never commit them, never embed them in test fixtures, never ship them. `/testdata/we/` is gitignored for this reason.

## Environment variables

Tests that need real content are `#[ignore]` by default and read these variables. Set them to absolute paths: Rust does not expand `~`.

| Variable | Example (Steam snap install, `~` = home directory) |
|---|---|
| `LUMEN_WE_ASSETS` | `~/snap/steam/common/.local/share/Steam/steamapps/common/wallpaper_engine/assets` |
| `LUMEN_WE_WORKSHOP` | `~/snap/steam/common/.local/share/Steam/steamapps/workshop/content/431960` |
| `LUMEN_TEST_WALLPAPER` | `$LUMEN_WE_WORKSHOP/3018340632` |

Run them with:

```bash
cargo test -- --ignored
```

## Reference wallpaper

**3018340632 — "Boykisser, Ryan Gosling, Blade Runner theme, 4k"** (scene). It exercises most of the engine:

- `scene.pkg` (PKGV0018, 70 entries, 38 MB) with 9 `.tex` textures
- one image object (`models/edit1688319042946.json`) carrying 11 effects: depthparallax, iris, shake ×3, foliagesway ×2, pulse, shine, localcontrast, chromaticaberration
- 4 particle systems: `particles/presets/ash.json` ×2, `particles/presets/rainperspective.json` ×2
- 1 sound object (mp3)
- user properties: `ash`, `rain`, `schemecolor`

An unpacked copy lives in `testdata/we/3018340632/` (gitignored) for tests that need loose files.

## Second target

**3655057993 — "Boykisser, Winter theme [4K]"** (video, 76 MB MP4). Blocked until `lumen-video` can build (FFmpeg/VA-API dev libraries).
