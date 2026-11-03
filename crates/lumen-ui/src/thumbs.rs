//! Preview decoding off the UI thread, with a disk cache.
//!
//! A preview is cropped to the 16:9 band shown by the cards (centre), then scaled
//! to at most [`MAX_EDGE`] pixels wide ([`MAX_GIF_EDGE`] for animations). Cropping
//! here instead of at draw time saves memory and works with every renderer. Animated GIFs
//! keep every frame, LZ4-compressed RGBA (GIF content compresses well, and one frame
//! decompresses in tens of microseconds), so a playing card costs one small texture
//! upload per GIF frame and no GIF decoding. The cache file is that same data, so a
//! second start shows previews without decoding anything.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;
use image::imageops::FilterType;

pub const MAX_EDGE: u32 = 480;
pub const MAX_GIF_EDGE: u32 = 320;
const CACHE_MAGIC: &[u8; 8] = b"LMNTHMB2";
/// GIF frames shorter than this play at 10 fps, as browsers do.
const MIN_DELAY_MS: u32 = 20;
const MAX_FRAMES: usize = 600;

#[derive(Debug)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    /// First frame, RGBA, uncompressed (shown immediately).
    pub first: Vec<u8>,
    /// Empty for still images.
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub delay_ms: u32,
    pub lz4: Box<[u8]>,
}

impl Thumb {
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    pub fn compressed_bytes(&self) -> usize {
        self.frames.iter().map(|f| f.lz4.len()).sum()
    }
}

/// Decompresses `frame` into `out` (resized to `w * h * 4`).
pub fn decompress_into(frame: &Frame, w: u32, h: u32, out: &mut Vec<u8>) -> bool {
    out.resize((w * h * 4) as usize, 0);
    matches!(lz4_flex::block::decompress_into(&frame.lz4, out), Ok(n) if n == out.len())
}

pub enum ThumbResult {
    Ready(PathBuf, Arc<Thumb>),
    Failed(PathBuf, String),
}

/// A small pool of decoding threads fed by a LIFO queue: the cards the user looks
/// at right now are the latest requested, so they are decoded first.
pub struct ThumbLoader {
    queue: Arc<Queue>,
}

struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct QueueState {
    jobs: VecDeque<PathBuf>,
    queued: HashSet<PathBuf>,
    closed: bool,
}

impl ThumbLoader {
    /// `deliver` receives each result on a worker thread; returning false (the UI
    /// is gone) stops the workers.
    pub fn new(
        cache_dir: Option<PathBuf>,
        workers: usize,
        deliver: impl Fn(ThumbResult) -> bool + Send + Sync + 'static,
    ) -> Self {
        let queue = Arc::new(Queue {
            state: Mutex::default(),
            ready: Condvar::new(),
        });
        let deliver = Arc::new(deliver);
        for i in 0..workers.max(1) {
            let queue = Arc::clone(&queue);
            let deliver = Arc::clone(&deliver);
            let cache_dir = cache_dir.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("lumen-ui-thumb{i}"))
                .spawn(move || {
                    while let Some(path) = queue.pop() {
                        let result = match load(&path, cache_dir.as_deref()) {
                            Ok(t) => ThumbResult::Ready(path.clone(), Arc::new(t)),
                            Err(e) => ThumbResult::Failed(path.clone(), e),
                        };
                        queue.done(&path);
                        if !deliver(result) {
                            return;
                        }
                    }
                });
            if let Err(e) = spawned {
                tracing::error!("cannot start preview decoder: {e}");
            }
        }
        Self { queue }
    }

    /// Queues `path` unless it is already queued or being decoded.
    pub fn request(&self, path: &Path) {
        let mut s = self
            .queue
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if s.queued.insert(path.to_owned()) {
            s.jobs.push_front(path.to_owned());
            self.queue.ready.notify_one();
        }
    }
}

impl Drop for ThumbLoader {
    fn drop(&mut self) {
        self.queue
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.queue.ready.notify_all();
    }
}

impl Queue {
    fn pop(&self) -> Option<PathBuf> {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if s.closed {
                return None;
            }
            if let Some(p) = s.jobs.pop_front() {
                return Some(p);
            }
            s = self.ready.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn done(&self, path: &Path) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queued
            .remove(path);
    }
}

pub fn load(path: &Path, cache_dir: Option<&Path>) -> Result<Thumb, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let cache_file =
        cache_dir.map(|dir| dir.join(format!("{:016x}.thumb", cache_key(path, &meta))));
    if let Some(file) = &cache_file
        && let Ok(t) = read_cache(file)
    {
        return Ok(t);
    }
    let thumb = decode(path)?;
    if let Some(file) = &cache_file
        && let Err(e) = write_cache(file, &thumb)
    {
        tracing::debug!("preview cache not written ({}): {e}", file.display());
    }
    Ok(thumb)
}

/// FNV-1a over the path, size, mtime and decoder settings: any change of the source
/// file or of [`MAX_EDGE`] gives a new entry. Stable across builds, unlike `DefaultHasher`.
fn cache_key(path: &Path, meta: &std::fs::Metadata) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(path.as_os_str().as_encoded_bytes());
    eat(&meta.len().to_le_bytes());
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    eat(&mtime.to_le_bytes());
    eat(&MAX_EDGE.to_le_bytes());
    eat(CACHE_MAGIC);
    h
}

fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let longest = w.max(h);
    if longest <= max {
        return (w.max(1), h.max(1));
    }
    let scale = max as f32 / longest as f32;
    (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    )
}

/// Centre rectangle of a `w × h` image with a 16:9 aspect.
pub fn crop_16_9(w: u32, h: u32) -> (u32, u32, u32, u32) {
    let (wf, hf) = (w.max(1) as f32, h.max(1) as f32);
    if wf / hf > 16.0 / 9.0 {
        let cw = ((hf * 16.0 / 9.0).round() as u32).clamp(1, w);
        ((w - cw) / 2, 0, cw, h)
    } else {
        let ch = ((wf * 9.0 / 16.0).round() as u32).clamp(1, h);
        (0, (h - ch) / 2, w, ch)
    }
}

fn crop_and_fit(img: &image::RgbaImage, max: u32) -> image::RgbaImage {
    let (x, y, cw, ch) = crop_16_9(img.width(), img.height());
    let view = image::imageops::crop_imm(img, x, y, cw, ch);
    let (w, h) = fit(cw, ch, max);
    if (w, h) == (cw, ch) {
        view.to_image()
    } else {
        image::imageops::resize(&*view, w, h, FilterType::Triangle)
    }
}

fn decode(path: &Path) -> Result<Thumb, String> {
    let is_gif = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"));
    if is_gif {
        let file = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
        let decoder = GifDecoder::new(file).map_err(|e| e.to_string())?;
        let mut frames = Vec::new();
        let mut first = None;
        let (mut tw, mut th) = (0, 0);
        for frame in decoder.into_frames().take(MAX_FRAMES) {
            let frame = frame.map_err(|e| e.to_string())?;
            let (num, den) = frame.delay().numer_denom_ms();
            let delay = num.checked_div(den).unwrap_or(0);
            let delay_ms = if delay < MIN_DELAY_MS { 100 } else { delay };
            let buf = crop_and_fit(&frame.into_buffer(), MAX_GIF_EDGE);
            (tw, th) = buf.dimensions();
            let raw = buf.into_raw();
            frames.push(Frame {
                delay_ms,
                lz4: lz4_flex::block::compress(&raw).into(),
            });
            if first.is_none() {
                first = Some(raw);
            }
        }
        let first = first.ok_or("GIF without frames")?;
        if frames.len() == 1 {
            frames.clear();
        }
        return Ok(Thumb {
            width: tw,
            height: th,
            first,
            frames,
        });
    }
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;
    // Shrink big images first (cheap box filter), then crop and fit precisely.
    let img = if img.width() > MAX_EDGE * 3 {
        img.thumbnail(MAX_EDGE * 2, MAX_EDGE * 2)
    } else {
        img
    };
    let img = crop_and_fit(&img.into_rgba8(), MAX_EDGE);
    let (w, h) = img.dimensions();
    Ok(Thumb {
        width: w,
        height: h,
        first: img.into_raw(),
        frames: Vec::new(),
    })
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_cache(file: &Path) -> io::Result<Thumb> {
    let mut r = BufReader::new(File::open(file)?);
    let mut magic = [0; 8];
    r.read_exact(&mut magic)?;
    if &magic != CACHE_MAGIC {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let width = read_u32(&mut r)?;
    let height = read_u32(&mut r)?;
    let count = read_u32(&mut r)? as usize;
    if width == 0
        || height == 0
        || width > MAX_EDGE.max(MAX_GIF_EDGE)
        || height > MAX_EDGE
        || count > MAX_FRAMES
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let raw_len = (width * height * 4) as usize;
    let mut frames = Vec::with_capacity(count);
    for _ in 0..count.max(1) {
        let delay_ms = read_u32(&mut r)?;
        let len = read_u32(&mut r)? as usize;
        if len > raw_len + raw_len / 255 + 16 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut lz4 = vec![0; len];
        r.read_exact(&mut lz4)?;
        frames.push(Frame {
            delay_ms,
            lz4: lz4.into(),
        });
    }
    let mut first = Vec::new();
    if !decompress_into(&frames[0], width, height, &mut first) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    if count <= 1 {
        frames.clear();
    }
    Ok(Thumb {
        width,
        height,
        first,
        frames,
    })
}

fn write_cache(file: &Path, t: &Thumb) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut w = BufWriter::new(File::create(&tmp)?);
        w.write_all(CACHE_MAGIC)?;
        w.write_all(&t.width.to_le_bytes())?;
        w.write_all(&t.height.to_le_bytes())?;
        let still;
        let frames: &[Frame] = if t.frames.is_empty() {
            still = [Frame {
                delay_ms: 0,
                lz4: lz4_flex::block::compress(&t.first).into(),
            }];
            &still
        } else {
            &t.frames
        };
        w.write_all(&(frames.len() as u32).to_le_bytes())?;
        for f in frames {
            w.write_all(&f.delay_ms.to_le_bytes())?;
            w.write_all(&(f.lz4.len() as u32).to_le_bytes())?;
            w.write_all(&f.lz4)?;
        }
        w.flush()?;
    }
    std::fs::rename(&tmp, file)
}

/// Playback position of an animated preview. Frame `i` is shown during
/// `[ends[i-1], ends[i])` milliseconds of each loop.
#[derive(Debug, Clone)]
pub struct Timeline {
    ends: Vec<u32>,
}

impl Timeline {
    pub fn new(frames: &[Frame]) -> Self {
        let mut t = 0u32;
        let ends = frames
            .iter()
            .map(|f| {
                t = t.saturating_add(f.delay_ms.max(1));
                t
            })
            .collect();
        Self { ends }
    }

    pub fn total_ms(&self) -> u32 {
        self.ends.last().copied().unwrap_or(0)
    }

    /// Frame to show `elapsed_ms` after the start, and milliseconds until it changes.
    pub fn at(&self, elapsed_ms: u64) -> (usize, u32) {
        let total = self.total_ms();
        if total == 0 {
            return (0, u32::MAX);
        }
        let t = (elapsed_ms % u64::from(total)) as u32;
        let idx = self.ends.partition_point(|&end| end <= t);
        let idx = idx.min(self.ends.len() - 1);
        (idx, self.ends[idx] - t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-ui-thumbs-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_gif(path: &Path, frames: u8, size: u16) {
        use image::codecs::gif::GifEncoder;
        let mut enc = GifEncoder::new(File::create(path).unwrap());
        let frames = (0..frames).map(|i| {
            let buf = image::RgbaImage::from_pixel(
                u32::from(size),
                u32::from(size),
                image::Rgba([i * 40, 0, 255 - i * 40, 255]),
            );
            image::Frame::from_parts(buf, 0, 0, image::Delay::from_numer_denom_ms(50, 1))
        });
        enc.encode_frames(frames).unwrap();
    }

    #[test]
    fn decodes_and_caches_an_animated_gif() {
        let dir = tmpdir();
        let gif = dir.join("a.gif");
        write_gif(&gif, 4, 300);
        let cache = dir.join("cache");
        let t = load(&gif, Some(&cache)).unwrap();
        // 300×300 → centre 300×169 band, scaled to 320 wide max (no upscaling).
        assert_eq!((t.width, t.height), (300, 169));
        assert_eq!(t.frames.len(), 4);
        assert!(t.frames.iter().all(|f| f.delay_ms == 50));
        let mut px = Vec::new();
        assert!(decompress_into(&t.frames[2], t.width, t.height, &mut px));
        assert_eq!(&px[..4], &[80, 0, 175, 255]);
        // Second load comes from the cache file, identical.
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
        let again = load(&gif, Some(&cache)).unwrap();
        assert_eq!(again.first, t.first);
        assert_eq!(again.frames.len(), 4);
    }

    #[test]
    fn still_images_and_broken_files() {
        let dir = tmpdir();
        let png = dir.join("s.png");
        image::RgbaImage::from_pixel(1024, 1024, image::Rgba([1, 2, 3, 255]))
            .save(&png)
            .unwrap();
        let t = load(&png, Some(&dir.join("cache2"))).unwrap();
        assert_eq!((t.width, t.height), (480, 270));
        assert!(!t.is_animated());
        let t = load(&png, Some(&dir.join("cache2"))).unwrap();
        assert_eq!(t.first.len(), 480 * 270 * 4);
        let bad = dir.join("bad.jpg");
        std::fs::write(&bad, b"nope").unwrap();
        assert!(load(&bad, None).is_err());
    }

    #[test]
    fn crops_the_centre_16_9_band() {
        assert_eq!(crop_16_9(256, 256), (0, 56, 256, 144));
        assert_eq!(crop_16_9(3440, 1440), (440, 0, 2560, 1440));
        assert_eq!(crop_16_9(1920, 1080), (0, 0, 1920, 1080));
    }

    #[test]
    fn timeline_positions() {
        let f = |d| Frame {
            delay_ms: d,
            lz4: Box::new([]),
        };
        let tl = Timeline::new(&[f(100), f(50), f(200)]);
        assert_eq!(tl.total_ms(), 350);
        assert_eq!(tl.at(0), (0, 100));
        assert_eq!(tl.at(99), (0, 1));
        assert_eq!(tl.at(100), (1, 50));
        assert_eq!(tl.at(349), (2, 1));
        assert_eq!(tl.at(350 * 7 + 120), (1, 30));
    }
}
