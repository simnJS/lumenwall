# lumen-formats

Readers for Wallpaper Engine binary formats: `.pkg` archives, `.tex` textures, `MDLV` puppet meshes.

## Sources (clean room)

- `specs/01-formats-binaires.md` §0–§3, §6, fixtures §7, open questions §8.
- `docs/PERFORMANCE.md`, `docs/research/render.md`.
- Public documentation of `memmap2`, `lz4_flex`, `bcdec_rs`, `image`, `serde_json`.
- Real Wallpaper Engine files, used only as test input (`docs/TESTING.md`, ignored tests, nothing copied).

## Coverage

| Module | Content |
|---|---|
| `pkg` | `PkgArchive`: mmap (`open`), whole read (`OpenMode::Read`), memory (`from_vec`, `from_static`). One-pass parse, bounds checked at open, sorted hash index built once. `get` is exact, case-sensitive, first duplicate wins, allocation-free. Entries are `&[u8]` borrowed from the mapping. `release_mapping()` drops the mapping (Windows file lock). |
| `tex` | TEXV/TEXI/TEXB0001–0004/TEXS0001–0003, format and `TIE` enums, the flags rule (`// parity:` in `TexFlags::from_raw`), v4 fallback to v3, MP4, compression 0/1/≥2, LZ4 decoded straight into a buffer of size `TD`, frames, spritesheet inference, `.tex-json` (`apply_tex_json`), resolution, UV area, `TEX0FORMAT` combo, frame selection (§2.12). |
| `tex::into_upload` | `PreparedTexture`: per image and per mip, raw BC1/BC2/BC3 blocks or R8/RG8/RGBA8 pixels, borrowed from the input when nothing changes (`Cow`). Sampler (clamp/repeat, nearest/trilinear), video path, `E_TEX_FORMAT_GPU`, `E_TEX_VIDEO_VIDE`. Embedded images are decoded by content sniffing (PNG, JPEG, GIF first frame, BMP, TGA, HDR, PNM) to RGBA8. |
| BCn fallback | `PreparedTexture::requires_cpu_decode` / `decode_compressed`, `decode_block_compressed` (bcdec_rs). |
| `mdl` | The §3.1 heuristic, byte for byte. |

Tests: fixtures F1–F10 with their variants (`tests/fixtures.rs`), proptest fuzzing on random and mutated inputs (`tests/fuzz.rs`; `PROPTEST_CASES=300000` for a deeper run), real content (`tests/real_data.rs`, `--ignored`).

Benchmarks (`cargo bench -p lumen-formats`, dev machine, release):

| Bench | Time |
|---|---|
| parse and index a 5 000-entry `.pkg` in memory | 96 µs |
| mmap open of the same file | 118 µs |
| 5 000 lookups | 191 µs (38 ns each) |
| parse a 2048² RGBA LZ4 `.tex` (16 MiB out) | 5.8 ms (2.7 GiB/s) |
| parse and prepare a 2048² DXT5 with 12 mips | 0.4 µs (zero-copy) |

Real content: the reference `scene.pkg` (70 entries) opens in about 30 µs; its 9 textures and 283 of the 311 assets textures parse and prepare without a single warning.

## Deliberate differences from the reference

- Bounds checks (QO-01, QO-02): `FinDeFluxInattendue`, `E_PKG_ENTREE_HORS_LIMITES` (checked at open, which refuses the whole archive), `E_TEX_TAILLE` for negative sizes and for levels above `max_level_bytes` (1 GiB by default).
- Upload fix-ups, logged: level data longer than its size is cut; shorter data is zero-padded; zero dimensions become 1; mip levels that break the `max(1, base >> k)` chain are dropped (wgpu requires a consistent chain).
- QO-09: an image without mips becomes a 1×1 transparent texture. QO-07: an undecodable embedded image becomes transparent black at the header size. QO-08: a short LZ4 output is zero-filled. QO-17: saturating float to int.
- Anisotropy is 8 for linear filtering and 1 for nearest (wgpu refuses anisotropy with nearest filters).
- `UploadOptions::extended_formats` (off by default) maps RGB888 and RGB565 (CPU expansion), RG1616f, R16f, RGBA16161616f, RGB161616f, BC7, RGBA1010102 and WEBP images instead of failing.

## Open questions

1. **Blocking: flag `0x40` (undocumented).** All 28 `materials/lut/*.tex` of the WE assets folder have flags `0x42` and fail with `E_TEX_TEXB`, which is the spec-conformant result: the spec says undocumented bits do not change the layout. Observed in these files: one extra `u32` after the reserved field at 0x2A (`TEXB` then starts at 0x32), with RGBA-like values whose last byte is always `0xFF`. Each mip header has one extra `u32` between height and compression (value 32). With those two fields the files parse exactly to EOF. Each one embeds a 32×1024 PNG (a 32³ LUT strip). These files are reported, not supported, until the spec covers this flag.
2. PSD and Softimage PIC embedded images: required by §2.6 but not supported by `image`, so they currently decode as transparent black.
3. HDR → 8 bits uses `image`'s clamping, and 16-bit PNG → 8 bits rounds. The exact reference conversions are not specified.
4. `TEX0FORMAT` is forced from the file's format field even when the image is embedded (GPU format RGBA8). Should it?
5. A negative `TD` with `C = 0` (field unused) is refused, following the literal reading of §2.7.2.
6. The animation total `T` is summed in `f64`, while the spritesheet duration is summed in `f32`.
7. Extended formats only: assumed RGB565 bit layout (red high) and RGBA1010102 channel order.
8. The workspace pins `lz4_flex` 0.11 (research suggests 0.14). Bumping it is a root `Cargo.toml` decision.
