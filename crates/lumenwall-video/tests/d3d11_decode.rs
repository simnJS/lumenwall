//! FFmpeg's D3D11VA decoder on a `lumenwall-d3d11` device (`lumenwall_video::d3d11`).
//!
//! WARP by default (it has no video decoder: the tests then check the software fallback and say
//! so); `LUMENWALL_D3D11_TEST_ADAPTER=hardware` runs them on the default hardware adapter under
//! `%LOCALAPPDATA%\lumenwall-dev\gpu.lock`, where decoding must be zero copy. A few frames of
//! each clip, no sound, nothing on the desktop.
//!
//! Clips: `LUMENWALL_TEST_CLIPS` (paths separated by `;`), else the video samples that ship with
//! Windows (never committed, never Wallpaper Engine content). FFmpeg's DLLs must be found
//! (`PATH`), `FFMPEG_DIR` at build time.
#![cfg(all(windows, feature = "ffmpeg"))]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use lumenwall_d3d11::windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Texture2D,
};
use lumenwall_d3d11::windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R16_UNORM,
    DXGI_FORMAT_R16G16_UNORM,
};
use lumenwall_d3d11::{
    AdapterChoice, DecodedFrame, Device, DeviceOptions, PlaneFormat, plane_views,
};
use lumenwall_render::yuv::{ChromaPlanes, PlaneDepth};
use lumenwall_video::d3d11::{
    D3d11Decoder, D3d11Options, FrameImage, PlaneTextures, SoftwareFrame, VideoFrame,
};
use lumenwall_video::{DecodePath, HwDecode, HwKind, VideoInput};

/// Frames decoded per clip (and held at once: exercises the lifetime of pool slices).
const FRAMES: usize = 8;

static SERIAL: Mutex<()> = Mutex::new(());

/// One GPU user at a time: inside the process, and on the machine for the hardware adapter.
struct Gpu {
    device: Device,
    hardware: bool,
    lock_file: Option<PathBuf>,
    _serial: MutexGuard<'static, ()>,
}

impl Drop for Gpu {
    fn drop(&mut self) {
        if let Some(p) = &self.lock_file {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn gpu() -> Gpu {
    // Why a hardware decoder was refused is logged at info level.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let hardware = std::env::var("LUMENWALL_D3D11_TEST_ADAPTER").is_ok_and(|v| v == "hardware");
    let lock_file = hardware.then(|| {
        let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA"))
            .join("lumenwall-dev");
        std::fs::create_dir_all(&dir).expect("lock folder");
        let path = dir.join("gpu.lock");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("{} is held by another GPU user ({e})", path.display()));
        use std::io::Write;
        let _ = writeln!(f, "lumenwall-video d3d11_decode pid {}", std::process::id());
        path
    });
    let device = Device::new(&DeviceOptions {
        adapter: if hardware {
            AdapterChoice::Default
        } else {
            AdapterChoice::Warp
        },
        // The decoder works from another thread of the process.
        single_threaded: false,
        ..DeviceOptions::default()
    })
    .expect("Direct3D 11 device");
    Gpu {
        device,
        hardware,
        lock_file,
        _serial: serial,
    }
}

fn clips() -> Vec<PathBuf> {
    if let Some(list) = std::env::var_os("LUMENWALL_TEST_CLIPS") {
        return list
            .to_string_lossy()
            .split(';')
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .collect();
    }
    [
        r"C:\Windows\SystemApps\Microsoft.Windows.CloudExperienceHost_cw5n1h2txyewy\media\oobe-intro.mp4",
        r"C:\Windows\SystemResources\Windows.UI.SettingsAppThreshold\SystemSettings\Assets\SDRSample.mkv",
        r"C:\Windows\SystemResources\Windows.UI.SettingsAppThreshold\SystemSettings\Assets\HDRSample.mkv",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|p| p.is_file())
    .collect()
}

fn decode(device: &Device, clip: &Path, hwdec: HwDecode) -> (D3d11Decoder, Vec<VideoFrame>) {
    let opts = D3d11Options {
        hwdec,
        held_frames: FRAMES + 2,
    };
    let (mut decoder, first) =
        D3d11Decoder::open(device, &VideoInput::File(clip.to_path_buf()), &opts).expect("open");
    let mut frames = vec![first];
    while frames.len() < FRAMES {
        frames.push(decoder.next_frame().expect("frame"));
    }
    (decoder, frames)
}

/// A texture copied to a staging texture and mapped: `rows` rows of `pitch` bytes.
struct Readback {
    data: Vec<u8>,
    pitch: usize,
    desc: D3D11_TEXTURE2D_DESC,
}

fn read(
    device: &Device,
    texture: &ID3D11Texture2D,
    subresource: u32,
    planar_yuv: bool,
) -> Readback {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: live out-parameter.
    unsafe { texture.GetDesc(&mut desc) };
    let staging_desc = D3D11_TEXTURE2D_DESC {
        ArraySize: 1,
        MipLevels: 1,
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..desc
    };
    let mut staging = None;
    // SAFETY: valid descriptor, live out-parameter.
    unsafe {
        device
            .raw()
            .CreateTexture2D(&staging_desc, None, Some(&mut staging))
    }
    .expect("staging texture");
    let staging = staging.expect("staging texture");
    let ctx = device.context();
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: same format and size, one device; `Map(READ)` waits for the copy; the mapping holds
    // `RowPitch` bytes for each of `Height` rows (NV12/P010: and half as many rows of chroma
    // after them); read before `Unmap`.
    let data = unsafe {
        ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, texture, subresource, None);
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .expect("map");
        let rows = if planar_yuv {
            desc.Height as usize * 3 / 2
        } else {
            desc.Height as usize
        };
        let data = std::slice::from_raw_parts(
            mapped.pData.cast::<u8>().cast_const(),
            mapped.RowPitch as usize * rows,
        )
        .to_vec();
        ctx.Unmap(&staging, 0);
        data
    };
    Readback {
        data,
        pitch: mapped.RowPitch as usize,
        desc,
    }
}

/// `(Y, U, V)` sample values (at the stream's bit depth, one `u16` each) of a hardware slice.
fn hardware_planes(device: &Device, f: &DecodedFrame) -> [Vec<u16>; 3] {
    let r = read(device, &f.texture, f.slice, true);
    let (w, h) = (f.size.0 as usize, f.size.1 as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let bytes = match f.format {
        PlaneFormat::Nv12 => 1,
        PlaneFormat::P010 => 2,
    };
    let sample = |row: usize, x: usize| -> u16 {
        let at = row * r.pitch + x * bytes;
        match f.format {
            PlaneFormat::Nv12 => u16::from(r.data[at]),
            // MSB-aligned 10 bits.
            PlaneFormat::P010 => u16::from_le_bytes([r.data[at], r.data[at + 1]]) >> 6,
        }
    };
    let aligned = r.desc.Height as usize;
    let y = (0..h)
        .flat_map(|j| (0..w).map(move |i| (j, i)))
        .map(|(j, i)| sample(j, i));
    let u = (0..ch)
        .flat_map(|j| (0..cw).map(move |i| (j, i)))
        .map(|(j, i)| sample(aligned + j, 2 * i));
    let v = (0..ch)
        .flat_map(|j| (0..cw).map(move |i| (j, i)))
        .map(|(j, i)| sample(aligned + j, 2 * i + 1));
    [y.collect(), u.collect(), v.collect()]
}

/// `(Y, U, V)` sample values of a CPU frame (planar or interleaved, 8 or 16-bit containers).
fn software_planes(f: &SoftwareFrame) -> [Vec<u16>; 3] {
    let l = f.layout();
    let bytes = if l.depth == PlaneDepth::U16 { 2 } else { 1 };
    let read = |plane: usize, w: u32, h: u32, channels: usize, channel: usize| -> Vec<u16> {
        let (data, stride) = f.plane(plane).expect("plane");
        let mut out = Vec::new();
        for j in 0..h as usize {
            for i in 0..w as usize {
                let at = j * stride as usize + (i * channels + channel) * bytes;
                out.push(if bytes == 2 {
                    u16::from_le_bytes([data[at], data[at + 1]])
                } else {
                    u16::from(data[at])
                });
            }
        }
        out
    };
    let y = read(0, l.width, l.height, 1, 0);
    let (cw, ch) = (l.chroma_width, l.chroma_height);
    let (u, v) = match l.planes {
        ChromaPlanes::Planar => (read(1, cw, ch, 1, 0), read(2, cw, ch, 1, 0)),
        ChromaPlanes::Interleaved => (read(1, cw, ch, 2, 0), read(1, cw, ch, 2, 1)),
        ChromaPlanes::InterleavedSwapped => (read(1, cw, ch, 2, 1), read(1, cw, ch, 2, 0)),
    };
    [y, u, v]
}

fn count_diff(a: &[u16], b: &[u16]) -> usize {
    assert_eq!(a.len(), b.len(), "plane sizes");
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

#[test]
fn hardware_frames_equal_the_cpu_decode() {
    let gpu = gpu();
    let clips = clips();
    let mut zero_copy_clips = 0;
    assert!(!clips.is_empty(), "no test clip (LUMENWALL_TEST_CLIPS)");
    for clip in clips {
        let name = clip.file_name().unwrap().to_string_lossy().into_owned();
        let (decoder, frames) = decode(&gpu.device, &clip, HwDecode::Auto);
        let path = decoder.decode_path();
        let hardware = matches!(path, DecodePath::ZeroCopy(HwKind::D3d11va));
        println!("{name}: {path} ({})", decoder.info().codec);
        if !hardware {
            // WARP has no video device; a GPU has size and profile limits (H.264 stops at
            // 4096 pixels): the stream plays through the software fallback, tested below.
            let here = if gpu.hardware {
                "stream refused by the GPU"
            } else {
                "WARP"
            };
            println!("{name}: no D3D11VA decoding here ({here}): {path}");
            continue;
        }
        zero_copy_clips += 1;
        let (_, reference) = decode(&gpu.device, &clip, HwDecode::No);
        for (n, (hw, sw)) in frames.iter().zip(&reference).enumerate() {
            let (FrameImage::Hardware(hw_frame), FrameImage::Software(sw_frame)) =
                (&hw.image, &sw.image)
            else {
                panic!("{name} frame {n}: expected a hardware frame and a software one");
            };
            assert!((hw.time - sw.time).abs() < 1e-6, "{name} frame {n}: time");
            let decoded = hw_frame.decoded();
            assert_eq!(
                (decoded.size.0, decoded.size.1),
                (sw_frame.layout().width, sw_frame.layout().height),
                "{name} frame {n}: visible size"
            );
            // Samples read back from the slice, equal to libavcodec's.
            let (h, s) = (
                hardware_planes(&gpu.device, decoded),
                software_planes(sw_frame),
            );
            for (plane, (a, b)) in ["luma", "Cb", "Cr"].iter().zip(h.iter().zip(&s)) {
                let different = count_diff(a, b);
                assert_eq!(
                    different,
                    0,
                    "{name} frame {n}: {different} of {} {plane} samples differ from the CPU decode",
                    a.len()
                );
            }
            // The views the renderer samples: formats of the planes, one slice each.
            let (luma, chroma) = plane_views(&gpu.device, decoded).expect("plane views");
            let format = |v: &lumenwall_d3d11::windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView| -> DXGI_FORMAT {
                let mut d = D3D11_SHADER_RESOURCE_VIEW_DESC::default();
                // SAFETY: live out-parameter.
                unsafe { v.GetDesc(&mut d) };
                d.Format
            };
            let (fl, fc) = decoded.format.plane_formats();
            assert_eq!((format(&luma), format(&chroma)), (fl, fc));
            assert_ne!(
                read_bind(&decoded.texture) & D3D11_BIND_SHADER_RESOURCE.0 as u32,
                0
            );
        }
        println!("{name}: {FRAMES} frames, luma and chroma equal to the CPU decode");
    }
    if gpu.hardware {
        assert!(zero_copy_clips > 0, "the GPU decoded no clip with D3D11VA");
    }
}

fn read_bind(t: &ID3D11Texture2D) -> u32 {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: live out-parameter.
    unsafe { t.GetDesc(&mut d) };
    d.BindFlags
}

#[test]
fn software_frames_go_through_writable_textures() {
    let gpu = gpu();
    let clips = clips();
    assert!(!clips.is_empty(), "no test clip (LUMENWALL_TEST_CLIPS)");
    for clip in clips {
        let name = clip.file_name().unwrap().to_string_lossy().into_owned();
        let (decoder, frames) = decode(&gpu.device, &clip, HwDecode::No);
        assert_eq!(decoder.decode_path(), DecodePath::Software, "{name}");
        let first = match &frames[0].image {
            FrameImage::Software(s) => s,
            FrameImage::Hardware(_) => panic!("{name}: hardware frame with HwDecode::No"),
        };
        let planes = PlaneTextures::new(&gpu.device, first.layout()).expect("plane textures");
        assert_eq!(planes.views().len(), first.layout().plane_count());
        for (n, frame) in frames.iter().enumerate() {
            let FrameImage::Software(frame) = &frame.image else {
                panic!("{name} frame {n}: hardware frame with HwDecode::No");
            };
            planes.upload(&gpu.device, frame).expect("upload");
            let l = frame.layout();
            for i in 0..l.plane_count() {
                let (data, stride) = frame.plane(i).expect("plane");
                let (w, h) = l.plane_size(i);
                let row = l.plane_row_bytes(i) as usize;
                let texture = planes.texture(i).expect("texture");
                let r = read(&gpu.device, texture, 0, false);
                let expected_format = match (l.depth, i > 0 && l.planes != ChromaPlanes::Planar) {
                    (PlaneDepth::U8, false) => DXGI_FORMAT_R8_UNORM,
                    (PlaneDepth::U8, true) => DXGI_FORMAT_R8G8_UNORM,
                    (PlaneDepth::U16, false) => DXGI_FORMAT_R16_UNORM,
                    (PlaneDepth::U16, true) => DXGI_FORMAT_R16G16_UNORM,
                };
                assert_eq!(r.desc.Format, expected_format, "{name} plane {i}");
                assert_eq!((r.desc.Width, r.desc.Height), (w, h), "{name} plane {i}");
                for j in 0..h as usize {
                    assert_eq!(
                        r.data[j * r.pitch..j * r.pitch + row],
                        data[j * stride as usize..j * stride as usize + row],
                        "{name} frame {n} plane {i} row {j}"
                    );
                }
            }
        }
        println!(
            "{name}: {FRAMES} software frames uploaded to writable textures ({:?}, {:?})",
            first.layout().planes,
            first.layout().depth
        );
    }
}

#[test]
fn the_decoder_loops_with_a_continuous_timeline() {
    let gpu = gpu();
    let Some(clip) = clips().into_iter().next() else {
        panic!("no test clip (LUMENWALL_TEST_CLIPS)");
    };
    let opts = D3d11Options::default();
    let (mut decoder, first) =
        D3d11Decoder::open(&gpu.device, &VideoInput::File(clip), &opts).expect("open");
    let mut last = first.time;
    let mut backwards = 0;
    // Decode through at least one loop (short clips) or a few seconds of it.
    for _ in 0..600 {
        let f = decoder.next_frame().expect("frame");
        if f.time < last {
            backwards += 1;
        }
        last = f.time;
        if decoder.loops() >= 1 {
            break;
        }
    }
    assert_eq!(backwards, 0, "media time must not jump back at the loop");
}
