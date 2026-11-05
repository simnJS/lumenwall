//! Output passes on a real Direct3D 11 device. WARP by default, so the workspace check never
//! touches the GPU; `LUMENWALL_D3D11_TEST_ADAPTER=hardware` runs the same tests on the default
//! hardware adapter (under `%LOCALAPPDATA%\lumenwall-dev\gpu.lock`).
//!
//! Expected values are the ones `lumenwall-render` asserts on its wgpu path: the conversion
//! matrix test below is a copy of `yuv::conversion_matrix` (pure arithmetic), and the checks of
//! `yuv.rs`'s tests (BT.709 limited black / white / red, BT.601 full range, P010 MSB-aligned
//! black / white) are replayed through the GPU; the composition modes of `compose.rs` (clamp,
//! border, repeat, vertical flip, alpha 1, UV windows) are checked at the pixel.

#![cfg(windows)]

use std::time::Duration;

use lumenwall_d3d11::layout::{Dimension, MipLevel};
use lumenwall_d3d11::output::*;
use lumenwall_d3d11::windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Texture2D,
};
use lumenwall_d3d11::windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_P010,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use lumenwall_d3d11::windows::Win32::Graphics::Dxgi::{
    DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIResource1,
};
use lumenwall_d3d11::windows::core::{Interface, PCWSTR};
use lumenwall_d3d11::*;
use lumenwall_formats::tex::GpuFormat;

fn device() -> Device {
    let hardware = std::env::var("LUMENWALL_D3D11_TEST_ADAPTER").is_ok_and(|v| v == "hardware");
    Device::new(&DeviceOptions {
        adapter: if hardware {
            AdapterChoice::Default
        } else {
            AdapterChoice::Warp
        },
        ..DeviceOptions::default()
    })
    .expect("Direct3D 11 device")
}

fn module(wgsl: &str) -> naga::Module {
    naga::front::wgsl::parse_str(wgsl).expect("WGSL")
}

type Rgba = [u8; 4];

fn rgba_texture(d: &Device, size: (u32, u32), texels: &[Rgba]) -> Texture {
    assert_eq!(texels.len(), (size.0 * size.1) as usize);
    let data: Vec<u8> = texels.iter().flatten().copied().collect();
    Texture::new(
        d,
        GpuFormat::Rgba8Unorm,
        Dimension::D2,
        &[MipLevel {
            width: size.0,
            height: size.1,
            depth: 1,
            data: &data,
        }],
    )
    .expect("texture")
}

fn pixels(d: &Device, out: &HeadlessOutput) -> Vec<Rgba> {
    out.read_rgba8(d)
        .expect("readback")
        .as_chunks::<4>()
        .0
        .to_vec()
}

fn close(a: Rgba, b: Rgba, tolerance: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tolerance)
}

/// A, B, C, D (4×1): four distinguishable colours, the alpha is deliberately not 255.
const SRC: [Rgba; 4] = [
    [255, 0, 0, 10],
    [0, 255, 0, 20],
    [0, 0, 255, 30],
    [255, 255, 0, 40],
];
const OPAQUE: [Rgba; 4] = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [255, 255, 0, 255],
];
const BLACK: Rgba = [0, 0, 0, 255];

struct Rig {
    d: Device,
    compositor: Compositor,
    source: Texture,
}

fn rig() -> Rig {
    let d = device();
    let mut states = StateCache::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = BytecodeCache::new(dir.path().join("dxbc")).expect("cache");
    let compositor =
        Compositor::new(&d, &mut states, Some(&cache), &module(COMPOSE_WGSL)).expect("compositor");
    // Second construction reads the bytecode back from the cache.
    Compositor::new(&d, &mut states, Some(&cache), &module(COMPOSE_WGSL)).expect("cached");
    let source = rgba_texture(&d, (4, 1), &SRC);
    Rig {
        d,
        compositor,
        source,
    }
}

impl Rig {
    fn compose(
        &mut self,
        size: (u32, u32),
        uv: [f32; 4],
        clamp: AddressMode,
        flip: bool,
    ) -> Vec<Rgba> {
        let out = HeadlessOutput::new(&self.d, size).unwrap();
        self.compositor
            .encode(
                &self.d,
                self.source.srv(),
                out.target().draw_view(),
                &ComposeParams {
                    uv,
                    clamp,
                    flip_vertical: flip,
                    size,
                    clear: Some([0.0, 0.0, 0.0, 1.0]),
                },
            )
            .unwrap();
        pixels(&self.d, &out)
    }
}

#[test]
fn compose_identity_alpha_one_and_clear() {
    let mut r = rig();
    // Same size, full window: texel centres on texel centres, an exact copy; alpha forced to 1.
    let p = r.compose((4, 1), [0.0, 0.0, 1.0, 1.0], AddressMode::Clamp, false);
    assert_eq!(p, OPAQUE);
}

#[test]
fn compose_uv_window() {
    let mut r = rig();
    // The right half of the source on a 2×1 output: texels C and D.
    let p = r.compose((2, 1), [0.5, 0.0, 1.0, 1.0], AddressMode::Clamp, false);
    assert_eq!(p, OPAQUE[2..]);
    // The left half: A and B.
    let p = r.compose((2, 1), [0.0, 0.0, 0.5, 1.0], AddressMode::Clamp, false);
    assert_eq!(p, OPAQUE[..2]);
}

#[test]
fn compose_vertical_flip() {
    let d = device();
    let mut states = StateCache::new();
    let mut compositor =
        Compositor::new(&d, &mut states, None, &module(COMPOSE_WGSL)).expect("compositor");
    // 1×2 source: red above green.
    let source = rgba_texture(&d, (1, 2), &[[255, 0, 0, 255], [0, 255, 0, 255]]);
    for (flip, expected) in [
        (false, [[255, 0, 0, 255], [0, 255, 0, 255]]),
        (true, [[0, 255, 0, 255], [255, 0, 0, 255]]),
    ] {
        let out = HeadlessOutput::new(&d, (1, 2)).unwrap();
        compositor
            .encode(
                &d,
                source.srv(),
                out.target().draw_view(),
                &ComposeParams {
                    uv: [0.0, 0.0, 1.0, 1.0],
                    clamp: AddressMode::Clamp,
                    flip_vertical: flip,
                    size: (1, 2),
                    clear: None,
                },
            )
            .unwrap();
        assert_eq!(pixels(&d, &out), expected, "flip {flip}");
    }
}

#[test]
fn compose_clamp_border_repeat() {
    let mut r = rig();
    // Windows entirely left (u in [-1, 0]) and right (u in [1, 2]) of the source.
    for (mode, left, right) in [
        (AddressMode::Clamp, [OPAQUE[0]; 4], [OPAQUE[3]; 4]),
        // Transparent border, presented opaque: black bands.
        (AddressMode::Border, [BLACK; 4], [BLACK; 4]),
        (AddressMode::Repeat, OPAQUE, OPAQUE),
    ] {
        let p = r.compose((4, 1), [-1.0, 0.0, 0.0, 1.0], mode, false);
        assert_eq!(p, left, "{mode:?} left");
        let p = r.compose((4, 1), [1.0, 0.0, 2.0, 1.0], mode, false);
        assert_eq!(p, right, "{mode:?} right");
    }
    // A window twice the source: the source, then its continuation.
    let p = r.compose((8, 1), [0.0, 0.0, 2.0, 1.0], AddressMode::Border, false);
    assert_eq!(p[..4], OPAQUE);
    assert_eq!(p[4..], [BLACK; 4]);
    let p = r.compose((8, 1), [0.0, 0.0, 2.0, 1.0], AddressMode::Clamp, false);
    assert_eq!(p[4..], [OPAQUE[3]; 4]);
    let p = r.compose((8, 1), [0.0, 0.0, 2.0, 1.0], AddressMode::Repeat, false);
    assert_eq!(p[..4], OPAQUE);
    assert_eq!(p[4..], OPAQUE);
}

#[test]
fn headless_output_png_round_trip() {
    let mut r = rig();
    let out = HeadlessOutput::new(&r.d, (4, 1)).unwrap();
    r.compositor
        .encode(
            &r.d,
            r.source.srv(),
            out.target().draw_view(),
            &ComposeParams {
                uv: [0.0, 0.0, 1.0, 1.0],
                clamp: AddressMode::Clamp,
                flip_vertical: false,
                size: (4, 1),
                clear: None,
            },
        )
        .unwrap();
    let rgba = out.read_rgba8(&r.d).unwrap();
    let png = out.png(&r.d).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.png");
    out.write_png(&r.d, &path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), png);
    assert_eq!(decode_stored_png(&png), (4, 1, rgba));
}

/// Reads back a PNG made of stored deflate blocks, checking the chunk CRCs and the Adler-32.
fn decode_stored_png(png: &[u8]) -> (u32, u32, Vec<u8>) {
    assert_eq!(&png[..8], [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let (mut pos, mut size, mut idat) = (8, (0, 0), Vec::new());
    while pos < png.len() {
        let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &png[pos + 4..pos + 8];
        let data = &png[pos + 8..pos + 8 + len];
        let crc = u32::from_be_bytes(png[pos + 8 + len..pos + 12 + len].try_into().unwrap());
        let mut c = !0u32;
        for &b in kind.iter().chain(data) {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        assert_eq!(!c, crc, "CRC of {}", String::from_utf8_lossy(kind));
        match kind {
            b"IHDR" => {
                size = (
                    u32::from_be_bytes(data[..4].try_into().unwrap()),
                    u32::from_be_bytes(data[4..8].try_into().unwrap()),
                );
                assert_eq!(&data[8..], [8, 6, 0, 0, 0]);
            }
            b"IDAT" => idat.extend_from_slice(data),
            _ => {}
        }
        pos += 12 + len;
    }
    assert_eq!(&idat[..2], [0x78, 0x01]);
    let (mut raw, mut p) = (Vec::new(), 2);
    loop {
        let last = idat[p] & 1 == 1;
        assert_eq!(idat[p] >> 1, 0, "stored block");
        let len = u16::from_le_bytes([idat[p + 1], idat[p + 2]]);
        assert_eq!(u16::from_le_bytes([idat[p + 3], idat[p + 4]]), !len);
        raw.extend_from_slice(&idat[p + 5..p + 5 + len as usize]);
        p += 5 + len as usize;
        if last {
            break;
        }
    }
    let (mut a, mut s) = (1u32, 0u32);
    for &b in &raw {
        a = (a + u32::from(b)) % 65521;
        s = (s + a) % 65521;
    }
    assert_eq!(
        u32::from_be_bytes(idat[p..p + 4].try_into().unwrap()),
        (s << 16) | a
    );
    let row = size.0 as usize * 4;
    let mut out = Vec::new();
    for line in raw.chunks_exact(row + 1) {
        assert_eq!(line[0], 0, "no filter");
        out.extend_from_slice(&line[1..]);
    }
    (size.0, size.1, out)
}

// --- YUV -------------------------------------------------------------------------------------

/// Copy of `lumenwall-render`'s `yuv::conversion_matrix` (pure arithmetic) for the cases below.
/// `kr`, `kb`: matrix coefficients; `limited`: range; `bits`/`shift`: sample depth and low
/// padding; `sixteen`: samples are raw 16-bit container values.
fn conversion_matrix(
    (kr, kb): (f64, f64),
    limited: bool,
    bits: u32,
    shift: u32,
    sixteen: bool,
) -> [[f32; 4]; 3] {
    let to_code = if sixteen {
        1.0 / f64::from(1u32 << shift)
    } else {
        255.0
    };
    let scale = f64::from(1u32 << (bits - 8));
    let max = f64::from((1u32 << bits) - 1);
    let (ay, by, ac, bc) = if limited {
        (
            1.0 / (219.0 * scale),
            -16.0 / 219.0,
            1.0 / (224.0 * scale),
            -128.0 / 224.0,
        )
    } else {
        (
            1.0 / max,
            0.0,
            1.0 / max,
            -f64::from(1u32 << (bits - 1)) / max,
        )
    };
    let (ay, ac) = (ay * to_code, ac * to_code);
    let kg = 1.0 - kr - kb;
    let rows = [
        [1.0, 0.0, 2.0 * (1.0 - kr)],
        [
            1.0,
            -2.0 * kb * (1.0 - kb) / kg,
            -2.0 * kr * (1.0 - kr) / kg,
        ],
        [1.0, 2.0 * (1.0 - kb), 0.0],
    ];
    rows.map(|[y, cb, cr]| {
        [
            (y * ay) as f32,
            (cb * ac) as f32,
            (cr * ac) as f32,
            (y * by + (cb + cr) * bc) as f32,
        ]
    })
}

const BT709: (f64, f64) = (0.2126, 0.0722);
const BT601: (f64, f64) = (0.299, 0.114);
const BT2020: (f64, f64) = (0.2627, 0.0593);

fn params(matrix: [[f32; 4]; 3]) -> YuvParams {
    YuvParams {
        matrix,
        chroma_offset: [0.0, 0.0],
        crop: [1.0, 1.0],
    }
}

struct Yuv {
    d: Device,
    conv: YuvConverter,
}

fn yuv() -> Yuv {
    let d = device();
    let mut states = StateCache::new();
    let conv = YuvConverter::new(&d, &mut states, None, &module(YUV_WGSL)).expect("converter");
    Yuv { d, conv }
}

/// An NV12 / P010 texture array with the shader-resource flag, one initial image per slice.
/// `planes[slice] = (luma, chroma)` in bytes, `size` even.
fn video_array(
    d: &Device,
    format: DXGI_FORMAT,
    size: (u32, u32),
    slices: &[(Vec<u8>, Vec<u8>)],
) -> ID3D11Texture2D {
    let bytes_per_sample = if format == DXGI_FORMAT_P010 { 2 } else { 1 };
    let pitch = size.0 * bytes_per_sample;
    // One subresource per slice: the luma rows, then the interleaved chroma rows.
    let images: Vec<Vec<u8>> = slices
        .iter()
        .map(|(l, c)| {
            assert_eq!(l.len(), (pitch * size.1) as usize);
            assert_eq!(c.len(), (pitch * size.1 / 2) as usize);
            [l.as_slice(), c.as_slice()].concat()
        })
        .collect();
    let mut t = None;
    // SAFETY: valid descriptor without initial data (video formats are filled below).
    unsafe {
        d.raw().CreateTexture2D(
            &D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: slices.len() as u32,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            },
            None,
            Some(&mut t),
        )
    }
    .expect("video texture array");
    let t: ID3D11Texture2D = t.expect("texture");
    for (slice, image) in images.iter().enumerate() {
        // SAFETY: one whole subresource (both planes) of this device's texture; `image` holds the
        // luma rows then the chroma rows at `pitch`, and outlives the call.
        unsafe {
            d.context()
                .UpdateSubresource(&t, slice as u32, None, image.as_ptr().cast(), pitch, 0);
        }
    }
    t
}

/// 16×8 frame: luma `luma(x, y)`, chroma constant per 8×4 quadrant `quadrant(qx, qy) = (cb, cr)`.
fn nv12_planes(
    luma: impl Fn(u32, u32) -> u8,
    quadrant: impl Fn(u32, u32) -> (u8, u8),
) -> (Vec<u8>, Vec<u8>) {
    let l = (0..8)
        .flat_map(|y| (0..16).map(move |x| (x, y)))
        .map(|(x, y)| luma(x, y))
        .collect();
    let c = (0..4)
        .flat_map(|y| (0..8).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let (cb, cr) = quadrant(x / 4, y / 2);
            [cb, cr]
        })
        .collect();
    (l, c)
}

fn render_yuv(y: &mut Yuv, frame: &DecodedFrame, p: &YuvParams, size: (u32, u32)) -> Vec<Rgba> {
    let out = HeadlessOutput::new(&y.d, size).unwrap();
    y.conv.convert(&y.d, frame, p, out.target()).unwrap();
    pixels(&y.d, &out)
}

fn on_hardware() -> bool {
    std::env::var("LUMENWALL_D3D11_TEST_ADAPTER").is_ok_and(|v| v == "hardware")
}

#[test]
fn nv12_array_slice_converts_exactly() {
    let mut y = yuv();
    let m = conversion_matrix(BT709, true, 8, 0, false);
    // Slice 0 is all black; slice 1 (the frame) has a luma ramp over neutral chroma.
    let black = nv12_planes(|_, _| 16, |_, _| (128, 128));
    let frame = nv12_planes(
        |x, y| {
            if x < 8 && y < 4 {
                16 + (x as u8) * 28
            } else {
                235
            }
        },
        |_, _| (128, 128),
    );
    let texture = video_array(&y.d, DXGI_FORMAT_NV12, (16, 8), &[black, frame]);
    let decoded = |slice| DecodedFrame {
        texture: texture.clone(),
        slice,
        format: PlaneFormat::Nv12,
        size: (16, 8),
    };
    // WARP mis-addresses the lower half of the planes of a planar texture *array* of more than
    // one slice (the same data in a one-slice texture, and the hardware adapter, are exact: see
    // `bt709_red_and_bt601_full_range_values`), so on WARP only the upper rows are compared.
    let rows = if on_hardware() { 0..8 } else { 0..3 };

    // Slice 0: black, exactly.
    let p = render_yuv(&mut y, &decoded(0), &params(m), (16, 8));
    for row in rows.clone() {
        assert!(
            p[row * 16..(row + 1) * 16].iter().all(|&px| px == BLACK),
            "row {row}: {p:?}"
        );
    }

    // Slice 1, top-left quadrant: neutral chroma, so grey = (Y − 16)/219 at the pixel.
    let p = render_yuv(&mut y, &decoded(1), &params(m), (16, 8));
    for x in 0..8usize {
        let v = ((x as f32 * 28.0) / 219.0 * 255.0).round().min(255.0) as u8;
        for row in [1usize, 2] {
            let got = p[row * 16 + x];
            assert!(
                close(got, [v, v, v, 255], 1),
                "x {x} row {row}: {got:?} vs grey {v}"
            );
        }
    }
    if on_hardware() {
        // Bottom-right quadrant: white (Y 235, neutral chroma), exact.
        assert_eq!(p[6 * 16 + 12], [255, 255, 255, 255]);
    }
}

#[test]
fn bt709_red_and_bt601_full_range_values() {
    let mut y = yuv();
    // yuv.rs: BT.709 limited red is Y 63, Cb 102, Cr 240 -> (255, 0, 0) within 0.01.
    let red = nv12_planes(|_, _| 63, |_, _| (102, 240));
    let t = video_array(&y.d, DXGI_FORMAT_NV12, (16, 8), &[red]);
    let f = DecodedFrame {
        texture: t,
        slice: 0,
        format: PlaneFormat::Nv12,
        size: (16, 8),
    };
    let p = render_yuv(
        &mut y,
        &f,
        &params(conversion_matrix(BT709, true, 8, 0, false)),
        (8, 8),
    );
    assert!(close(p[3 * 8 + 3], [255, 0, 0, 255], 3), "{:?}", p[27]);

    // BT.601 full range: Y 0 / 255 with neutral chroma are black / white; JPEG green is (0,255,0).
    let m = conversion_matrix(BT601, false, 8, 0, false);
    let frame = nv12_planes(|x, _| if x < 8 { 0 } else { 255 }, |_, _| (128, 128));
    let green = nv12_planes(|_, _| 150, |_, _| (44, 21));
    let one = |planes| DecodedFrame {
        texture: video_array(&y.d, DXGI_FORMAT_NV12, (16, 8), &[planes]),
        slice: 0,
        format: PlaneFormat::Nv12,
        size: (16, 8),
    };
    let (frame, green) = (one(frame), one(green));
    let p = render_yuv(&mut y, &frame, &params(m), (16, 8));
    assert_eq!(p[3 * 16 + 2], BLACK);
    assert_eq!(p[3 * 16 + 13], [255, 255, 255, 255]);
    let p = render_yuv(&mut y, &green, &params(m), (16, 8));
    assert!(close(p[3 * 16 + 8], [0, 255, 0, 255], 4), "{:?}", p[56]);
}

#[test]
fn p010_array_converts_exactly() {
    let mut y = yuv();
    // BT.2020 limited 10-bit in 16-bit words, MSB-aligned (shift 6): black 64, white 940,
    // neutral chroma 512, and a mid grey at Y 502 -> 128.
    let m = conversion_matrix(BT2020, true, 10, 6, true);
    let word = |code: u16| (code << 6).to_le_bytes();
    let luma: Vec<u8> = (0..16 * 8)
        .flat_map(|i| match (i % 16) / 4 {
            0 => word(64),
            1 => word(940),
            2 => word(502),
            _ => word(64),
        })
        .collect();
    let chroma: Vec<u8> = (0..8 * 4)
        .flat_map(|_| [word(512), word(512)].concat())
        .collect();
    let t = video_array(&y.d, DXGI_FORMAT_P010, (16, 8), &[(luma, chroma)]);
    let f = DecodedFrame {
        texture: t,
        slice: 0,
        format: PlaneFormat::P010,
        size: (16, 8),
    };
    let p = render_yuv(&mut y, &f, &params(m), (16, 8));
    assert_eq!(p[3 * 16 + 1], BLACK);
    assert_eq!(p[3 * 16 + 5], [255, 255, 255, 255]);
    let grey = p[3 * 16 + 9];
    assert!(close(grey, [128, 128, 128, 255], 1), "{grey:?}");
}

#[test]
fn cpu_planes_and_decoder_padding() {
    let mut y = yuv();
    let m = conversion_matrix(BT709, true, 8, 0, false);
    let make = |format, size: (u32, u32), data: &[u8]| {
        Texture::new_writable(&y.d, format, size, Some(data)).expect("plane")
    };
    // 16×8 picture: red everywhere (Y 63, U 102, V 240), as NV12 (U, V interleaved).
    let luma = make(GpuFormat::R8Unorm, (16, 8), &[63; 128]);
    let uv: Vec<u8> = (0..32).flat_map(|_| [102u8, 240]).collect();
    let chroma = make(GpuFormat::Rg8Unorm, (8, 4), &uv);
    let out = HeadlessOutput::new(&y.d, (16, 8)).unwrap();
    let mut run = |planes: YuvPlanes<'_>, p: &YuvParams| {
        y.conv
            .convert_planes(&y.d, &planes, p, out.target())
            .unwrap();
        pixels(&y.d, &out)
    };
    let nv12 = YuvPlanes {
        luma: luma.srv(),
        chroma: chroma.srv(),
        chroma_v: None,
        layout: ChromaLayout::Interleaved,
        sixteen_bit: false,
    };
    let p = run(nv12, &params(m));
    assert!(
        p.iter().all(|&px| close(px, [255, 0, 0, 255], 3)),
        "{:?}",
        p[0]
    );

    // NV21 order (V before U) with the U and V swapped in memory gives the same red.
    let vu: Vec<u8> = (0..32).flat_map(|_| [240u8, 102]).collect();
    let chroma_vu = make(GpuFormat::Rg8Unorm, (8, 4), &vu);
    let nv21 = YuvPlanes {
        chroma: chroma_vu.srv(),
        layout: ChromaLayout::InterleavedSwapped,
        ..YuvPlanes {
            luma: luma.srv(),
            chroma: chroma.srv(),
            chroma_v: None,
            layout: ChromaLayout::Interleaved,
            sixteen_bit: false,
        }
    };
    let p = run(nv21, &params(m));
    assert!(p.iter().all(|&px| close(px, [255, 0, 0, 255], 3)));

    // I420: three planes.
    let u = make(GpuFormat::R8Unorm, (8, 4), &[102; 32]);
    let v = make(GpuFormat::R8Unorm, (8, 4), &[240; 32]);
    let i420 = YuvPlanes {
        luma: luma.srv(),
        chroma: u.srv(),
        chroma_v: Some(v.srv()),
        layout: ChromaLayout::Planar,
        sixteen_bit: false,
    };
    let p = run(i420, &params(m));
    assert!(p.iter().all(|&px| close(px, [255, 0, 0, 255], 3)));

    // Decoder padding: the planes are 16 wide, the picture 8 (crop u1 = 0.5); the padding is
    // white and must not show: an 8×8 output of the left half is red.
    let padded: Vec<u8> = (0..8 * 16)
        .map(|i| if i % 16 < 8 { 63 } else { 235 })
        .collect();
    let luma_p = make(GpuFormat::R8Unorm, (16, 8), &padded);
    let small = HeadlessOutput::new(&y.d, (8, 8)).unwrap();
    y.conv
        .convert_planes(
            &y.d,
            &YuvPlanes {
                luma: luma_p.srv(),
                chroma: chroma.srv(),
                chroma_v: None,
                layout: ChromaLayout::Interleaved,
                sixteen_bit: false,
            },
            &YuvParams {
                crop: [0.5, 1.0],
                ..params(m)
            },
            small.target(),
        )
        .unwrap();
    let p = pixels(&y.d, &small);
    assert!(
        p.iter().all(|&px| close(px, [255, 0, 0, 255], 3)),
        "{:?}",
        p[7]
    );
}

// --- External copy ---------------------------------------------------------------------------

fn shared_texture(
    d: &Device,
    format: DXGI_FORMAT,
    size: (u32, u32),
    bytes: &[u8],
) -> (
    ID3D11Texture2D,
    lumenwall_d3d11::windows::Win32::Foundation::HANDLE,
) {
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: bytes.as_ptr().cast(),
        SysMemPitch: size.0 * 4,
        SysMemSlicePitch: 0,
    };
    let mut t = None;
    // SAFETY: valid descriptor and initial data (`bytes` outlives the call).
    unsafe {
        d.raw().CreateTexture2D(
            &D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                    as u32,
            },
            Some(&init),
            Some(&mut t),
        )
    }
    .expect("shared texture");
    let texture = t.expect("texture");
    let shared: IDXGIResource1 = texture.cast().expect("IDXGIResource1");
    // SAFETY: default security, no name; the caller closes the handle.
    let handle = unsafe {
        shared.CreateSharedHandle(
            None,
            DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
            PCWSTR::null(),
        )
    }
    .expect("shared handle");
    d.flush();
    assert!(d.wait_idle(Duration::from_secs(10)).unwrap());
    (texture, handle)
}

#[test]
fn shared_texture_from_a_second_device_is_copied_at_the_pixel() {
    use lumenwall_d3d11::windows::Win32::Foundation::CloseHandle;

    let (producer, consumer) = (device(), device());
    let mut states = StateCache::new();
    let mut copy =
        ExternalCopy::new(&consumer, &mut states, None, &module(COPY_WGSL)).expect("copy");
    // 4×2 pixels with distinct channels and alpha.
    let rgba: [Rgba; 8] = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [10, 20, 30, 128],
        [40, 50, 60, 64],
        [70, 80, 90, 0],
        [1, 2, 3, 4],
        [250, 251, 252, 253],
    ];
    for format in [DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM] {
        let bytes: Vec<u8> = rgba
            .iter()
            .flat_map(|&[r, g, b, a]| {
                if format == DXGI_FORMAT_B8G8R8A8_UNORM {
                    [b, g, r, a]
                } else {
                    [r, g, b, a]
                }
            })
            .collect();
        let (_keep, handle) = shared_texture(&producer, format, (4, 2), &bytes);
        let opened = consumer.open_shared_texture(handle).expect("open");
        let target = RenderTarget::new(&consumer, TargetDesc::new((4, 2))).unwrap();
        copy.copy(&consumer, &opened, &target).expect("copy");
        let (w, h, got) = read_texture_rgba8(&consumer, target.texture()).unwrap();
        assert_eq!((w, h), (4, 2));
        let got: Vec<Rgba> = got.as_chunks::<4>().0.to_vec();
        assert_eq!(got, rgba, "format {}", format.0);
        // SAFETY: the handle was created by `shared_texture` and is closed once.
        unsafe { CloseHandle(handle) }.unwrap();
    }
}

#[test]
fn external_copy_refuses_what_it_cannot_sample() {
    let d = device();
    let mut states = StateCache::new();
    let mut copy = ExternalCopy::new(&d, &mut states, None, &module(COPY_WGSL)).unwrap();
    let target = RenderTarget::new(&d, TargetDesc::new((2, 2))).unwrap();
    // A float target has no 8-bit external format.
    let float = RenderTarget::new(
        &d,
        TargetDesc {
            format: TargetFormat::Rgba16Float,
            ..TargetDesc::new((2, 2))
        },
    )
    .unwrap();
    assert!(copy.copy(&d, float.texture(), &target).is_err());
}

#[test]
fn nv12_chroma_quadrants_and_siting() {
    let mut y = yuv();
    let m = conversion_matrix(BT709, true, 8, 0, false);
    // Four chroma quadrants of 8×4 pixels at Y 126 (Cb, Cr: neutral, red, blue, green-ish):
    // pixels in the middle of a quadrant sample one chroma texel region, so the conversion is
    // the plain matrix product.
    let quadrant = |qx: u32, qy: u32| match (qx, qy) {
        (0, 0) => (128u8, 128u8),
        (1, 0) => (102, 240),
        (0, 1) => (240, 102),
        _ => (54, 34),
    };
    let planes = nv12_planes(|_, _| 126, quadrant);
    let texture = video_array(&y.d, DXGI_FORMAT_NV12, (16, 8), &[planes]);
    let f = DecodedFrame {
        texture,
        slice: 0,
        format: PlaneFormat::Nv12,
        size: (16, 8),
    };
    let p = render_yuv(&mut y, &f, &params(m), (16, 8));
    for (qx, qy) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
        let (cb, cr) = quadrant(qx, qy);
        let v = [
            126.0f32 / 255.0,
            f32::from(cb) / 255.0,
            f32::from(cr) / 255.0,
            1.0,
        ];
        let expected: Vec<u8> = m
            .iter()
            .map(|row| {
                let s: f32 = row.iter().zip(v).map(|(a, b)| a * b).sum();
                (s.clamp(0.0, 1.0) * 255.0).round() as u8
            })
            .collect();
        // The 4 × 2 pixels around the centre of the quadrant.
        for row in 0..2 {
            for col in 0..4 {
                let px = p[((qy * 4 + 1 + row) * 16 + qx * 8 + 2 + col) as usize];
                assert!(
                    close(px, [expected[0], expected[1], expected[2], 255], 1),
                    "quadrant {qx},{qy}: {px:?} vs {expected:?}"
                );
            }
        }
    }
    // Chroma siting: a half-texel offset to the right shifts the chroma sample, so the edge
    // between the neutral and the red quadrant moves by one pixel.
    let sited = render_yuv(
        &mut y,
        &f,
        &YuvParams {
            chroma_offset: [0.5, 0.0],
            ..params(m)
        },
        (16, 8),
    );
    assert_ne!(sited[16 + 7], p[16 + 7], "siting moves the edge");
    assert_eq!(
        sited[16 + 1],
        p[16 + 1],
        "away from the edge nothing changes"
    );
}
