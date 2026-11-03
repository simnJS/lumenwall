# lumen-assets

Per-wallpaper virtual file system and Steam / assets folder discovery.

## Sources (clean room)

- `specs/01-formats-binaires.md` §4 (VFS), §5 (Steam, assets), questions QO-19 to QO-32 and QO-37, amendment A.4 (2026-11-03).
- `docs/research/platform.md` (steamlocate).
- Public documentation of `steamlocate` and `winreg`.
- Real Wallpaper Engine install, used only as test input (`docs/TESTING.md`).

## Coverage

- `Vfs`: ordered mount points; adapters for memory, `.pkg` (zero-copy), folder and `$mediaThumbnail` (via a `MediaArtSource` trait). Factory order (§4.1.2), §4.7 lookup including the `"/" + n` retry, without allocations. `read`, `physical_path`, `exists`, `resolve`.
- `.pkg` entries out of bounds (A.4): the archive mounts, the entry "exists" (lookup stops on that mount, no fallback), and reading it gives `AssetError::PkgEntry` (`E_PKG_ENTREE_HORS_LIMITES`, category `Invalid`). Other entries stay readable.
- `Vfs::for_wallpaper`: the §4.8 mount order. Built-in memory files (§4.4.1, `BUILTIN_FILES`), thumbnail, wallpaper (errors propagated), `scene.pkg` / `gifscene.pkg` (skipped when absent, logged when invalid), assets folder (fatal error), current directory (`mount_current_dir`). `release_mappings()` for Windows.
- `path`: lexical normalization (§4.6), texture and `.tex-json` paths, shader paths with extension replacement, workshop `zcompat` substitution (§4.10.1), `config.json` playlist path cleanup (§5.6).
- `Vfs::read_shader`, `Vfs::read_texture` + `TextureFiles::parse` (applies `.tex-json`), `find_texture` across wallpapers in byte order of their keys (§4.11; only `NotFound` moves on), `is_reserved_texture_name`.
- `steam`: wallpaper argument (§5.1), workshop and installation folders with the two Linux candidate orders (native, `~/.steam`, Flatpak, Snap), `resolve_assets_dir` (`--assets-dir` → installation → next to the executable).

Tests: `tests/vfs.rs` (F1/F2 through the VFS, mount order, adapters, thumbnail, shaders, textures), `tests/steam.rs` (fake home folders, `libraryfolders.vdf`), `tests/real_data.rs` (`--ignored`: Snap Steam install, reference wallpaper, assets).

## Deviations

Reference bugs not reproduced:

- QO-37: an out-of-bounds `.pkg` entry fails alone instead of returning zeros and poisoning every later read of the archive.
- QO-26: the adapter receives the path that matched during lookup, not the original query. `/materials/x.tex` and `../x` therefore open the file they resolved to (fixture F2 expects an open failure).
- QO-22: a memory file returns its full content on every read.
- QO-20: folder containment is checked by path component, not by string prefix.
- QO-23: `file://` artwork URLs are percent-decoded. QO-24: thumbnail physical path is `NotFound`.
- Empty, `.` and `..` workshop ids are refused instead of naming the content folder.

## Additions beyond the spec

- When the spec's fixed candidates fail, extra Steam libraries are searched: those listed in each root's `libraryfolders.vdf`, plus installs found by `steamlocate`.
- Windows (QO-30): Steam roots come from `HKCU\Software\Valve\Steam\SteamPath`, `steamlocate` (HKLM), then `C:\Program Files (x86)\Steam`, followed by their libraries. Requests convert `\` to `/` before normalization (QO-25), and `\` is a separator in the wallpaper argument.

## Kept for parity

QO-19 (`virtual` source), QO-27 (current directory mounted last; can be turned off), QO-28, QO-32. The QO-29 texture cache lives in the renderer.

## Open questions

1. QO-25: NTFS lookups are case-insensitive while `.pkg` lookups are case-sensitive. Should Windows force a case check?
2. On Windows `canonicalize` returns `\\?\` paths, which `physical_path` hands to video players. Should they be simplified?
3. The built-in `copy.vert` / `copy.frag` use `in`/`out` and `out_FragColor`. The exact dialect depends on the shader spec (03), which is not available yet.
4. Should the current-directory mount (QO-27) stay on by default?
