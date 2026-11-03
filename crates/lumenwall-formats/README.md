# lumenwall-formats

Readers for Wallpaper Engine binary formats: `.pkg` archives, `.tex` textures, `MDLV` puppets (mesh, skeleton, attachments, animations).

## Sources (clean room)

- `specs/01-formats-binaires.md` §0–§3, §6, fixtures §7 (F1–F11), open questions §8, amendments A.1–A.5 (2026-11-03).
- `specs/08-puppet-squelette.md` §0–§8 and §10 (structured `.mdl` reader, fixture F-P1 and its variants, F-P2).
- `docs/PERFORMANCE.md`, `docs/PARITY-POLICY.md`, `docs/research/render.md`.
- Public documentation of `memmap2`, `lz4_flex`, `bcdec_rs`, `image`, `serde_json`, and Adobe's public PSD file format documentation (section layout only).
- Real Wallpaper Engine files, used only as test input (`docs/TESTING.md`, ignored tests, nothing copied).

## Coverage

| Module | Content |
|---|---|
| `pkg` | `PkgArchive`: mmap (`open`), whole read (`OpenMode::Read`), memory (`from_vec`, `from_static`). One-pass parse of the header and table, sorted hash index built once. `get` is exact, case-sensitive, first duplicate wins, allocation-free. `PkgEntry::data()` borrows from the mapping and checks that entry's bounds only (A.4). `release_mapping()` drops the mapping (Windows file lock). |
| `tex` | TEXV/TEXI/TEXB0001–0004/TEXS0001–0003, format and `TIE` enums, the flags rule (`// parity:` in `TexFlags::from_raw`), `VOLUME` layout (depth in TEXI and in every mip header, A.1), v4 fallback to v3, MP4, compression 0/1/≥2, LZ4 decoded straight into a buffer of size `TD`, frames, spritesheet inference, `.tex-json` (`apply_tex_json`), resolution, UV area, frame selection (§2.12), `end_offset`. |
| `tex::into_upload` | `PreparedTexture`: one GPU texture per image, 2D or 3D (`TextureDimension`). Per mip, raw BC1/BC2/BC3 blocks or R8/RG8/RGBA8 pixels, borrowed from the input when nothing changes (`Cow`). Sampler (clamp/repeat, nearest/trilinear), video path, `E_TEX_FORMAT_GPU`, `E_TEX_VOLUME`, `E_TEX_VIDEO_VIDE`. |
| `TEX0FORMAT` | `TexFormat::tex0_format_combo()` and `PreparedTexture::{source_format, tex0_format_combo}`: 8 or 9 from the **declared** format (offset 0x12), whatever is uploaded (A.3). Choosing the object's main texture and forcing the combo on every pass is the renderer's job. |
| Embedded images | Content sniffing (A.2): PNG (1–16 bits, grey, grey + alpha, RGB, RGBA, palette, `tRNS`, Adam7; 16 bits keep the high byte; no gamma/ICC) and baseline/progressive JPEG via `image`; GIF (first frame), BMP, TGA; optional HDR and PNM; PSD composite through an in-crate decoder (`tex/psd.rs`: RGB, 8/16 bits, raw or PackBits RLE, up to 16 channels, white matte removal). |
| BCn fallback | `PreparedTexture::requires_cpu_decode` / `decode_compressed` (slice by slice for volumes), `decode_block_compressed` (bcdec_rs). |
| `mdl` | `parse_puppet`: the spec 01 §3.1 heuristic, byte for byte (unchanged). `parse_model` (spec 08): structured `MDLV0023` mesh when the layout is the observed puppet one (`nm` = 1, `vf` = `0x0180000F`), else the heuristic with weights read at +40/+56 (§2.8); per-vertex `VertexSkin` (normal, tangent, bone indices, weights); mesh tail (layout positions, part ranges); section chain of §3 (`SectionInfo`); `MDLS0004` bones (parents, local rest `mat4`) and editor data; `MDAT0001` attachments; `MDLA0006` animations (id, name, `loop`/`mirror`/`single`, fps, N, one track of N + 1 samples per bone); `MDLE0002` editor pose. Every §8 failure is a `MdlWarning` (section + absolute offset) that drops only its section; a weight on a missing bone rejects the skeleton. |

Tests: fixtures F1–F11 with their variants (`tests/fixtures.rs`), F-P1 of spec 08 (SHA-256 checked against the spec) with its seven negative variants and the `mirror`/`single` versions (`tests/puppet.rs`), A.2 image variants built byte by byte (`tests/embedded.rs`), proptest fuzzing on random and mutated `.pkg`, `.tex`, PNG, PSD and puppet inputs, including random section contents behind a valid mesh (`tests/fuzz.rs`; `PROPTEST_CASES=300000` on a **debug** build, so overflows panic), real content (`tests/real_data.rs`, `tests/puppet.rs`, `--ignored`: the 8 puppets of 3463520581 match F-P2 with no warning and the same mesh both ways; the 28 static models of the installation have no section).

Benchmarks (`cargo bench -p lumenwall-formats`, dev machine, release):

| Bench | Time |
|---|---|
| parse and index a 5 000-entry `.pkg` in memory | 94 µs |
| mmap open of the same file | 118 µs |
| 5 000 lookups | 186 µs (37 ns each) |
| parse a 2048² RGBA LZ4 `.tex` (16 MiB out) | 5.5 ms (2.8 GiB/s, lz4_flex 0.14) |
| parse and prepare a 2048² DXT5 with 12 mips | 0.4 µs (zero-copy) |

Real content: the reference `scene.pkg` (70 entries) opens in about 30 µs. Its 9 textures and all 311 assets textures (including the 28 colour LUTs, 32×32×32 volumes) parse and prepare without a single warning.

## Deviations

- **`VOLUME` textures (A.1):** the reference fails with `E_TEX_TEXB` on flag `0x40`; Lumenwall reads the shifted layout and produces 3D textures (parity policy 2 and 4). `VOLUME` with `ANIMATED`, `VIDEO` or MP4 gives `E_TEX_VOLUME`. Volume samplers use anisotropy 1. An embedded volume image that is not exactly `width × (height × depth)` is zeroed with a warning. BCn volumes (never observed) always take the CPU decode path.
- **`.pkg` bounds (A.4, QO-37):** checked when an entry is read, with 64-bit arithmetic. The faulty entry fails alone with `E_PKG_ENTREE_HORS_LIMITES`; the reference returns zeros and then poisons the whole archive.
- Bounds checks (QO-01, QO-02): `FinDeFluxInattendue`, and `E_TEX_TAILLE` for negative sizes and for levels above `max_level_bytes` (1 GiB by default).
- Upload fix-ups, logged: level data longer than its size is cut; shorter data is zero-padded; zero dimensions become 1; mip levels that break the `max(1, base >> k)` chain (on every axis) are dropped, because wgpu requires a consistent chain.
- QO-09: an image without mips becomes a 1×1 transparent texture. QO-07: an undecodable or unsupported embedded image (Softimage PIC, TIFF, WEBP by default…) becomes transparent black at the header size. QO-08: a short LZ4 output is zero-filled. QO-17: saturating float to int.
- Anisotropy is 8 for linear filtering and 1 for nearest (wgpu refuses anisotropy with nearest filters).
- PSD 16-bit RLE is decoded too (A.2 only guarantees 16-bit without compression).
- Puppets (spec 08): a structured `MDLV0023` read whose tail flags are neither 0 nor 1 falls back to the heuristic; a layout block whose size is not `12 × nv` is ignored with a warning; a second `MDLS` section is ignored; an `MDLE` whose matrix count differs from the bone count is dropped. Names are read as UTF-8 (invalid bytes become U+FFFD).
- `UploadOptions::extended_formats` (off by default) maps RGB888 and RGB565 (CPU expansion), RG1616f, R16f, RGBA16161616f, RGB161616f, BC7, RGBA1010102 and WEBP images instead of failing.

## Not implemented (optional in A.2)

- Softimage PIC: sniffed and refused explicitly, then zero-filled. No corpus file uses it.
- HDR and PNM are decoded by `image`: HDR is clamped to 8 bits, with no tone mapping.

## Open questions

1. HDR → 8 bits conversion is not specified; `image` clamps.
2. PSD white-matte removal uses single-precision floats and truncation. A ±1 difference from the reference is possible at rounding boundaries.
3. A negative `TD` with `C = 0` (field unused) is refused, following the literal reading of §2.7.2.
4. The animation total `T` is summed in `f64`, while the spritesheet duration is summed in `f32`.
5. Extended formats only: assumed RGB565 bit layout (red high) and RGBA1010102 channel order.
6. QO-35: the `VOLUME` layouts of mip versions 1, 2 and 4, and raw-pixel volumes, are extrapolated and implemented as the amendment describes, but untested on real data.
