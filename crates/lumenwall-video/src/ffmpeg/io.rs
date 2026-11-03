//! Demuxing from memory (videos embedded in `.tex` files, spec 01 §2.10.2 / spec 07 §14.2):
//! a read-only, seekable custom `AVIOContext` over shared bytes (a slice of a memory-mapped
//! `.pkg`, no copy).

use std::ffi::{c_int, c_void};
use std::ptr;

use super::sys as ff;

use crate::VideoBytes;

/// FFmpeg's I/O buffer (it reads through it; the data itself is not copied elsewhere).
const BUFFER: usize = 64 * 1024;
/// `AVSEEK_SIZE`: the seek callback is asked for the stream size.
const AVSEEK_SIZE: c_int = 0x10000;

struct Cursor {
    data: VideoBytes,
    pos: usize,
}

/// A custom `AVIOContext` reading `data`.
pub(crate) struct MemoryIo {
    ctx: *mut ff::AVIOContext,
    cursor: *mut Cursor,
}

// SAFETY: used by the thread owning the `Source` only.
unsafe impl Send for MemoryIo {}

unsafe extern "C" fn read(opaque: *mut c_void, buf: *mut u8, size: c_int) -> c_int {
    // SAFETY: `opaque` is the `Cursor` of a live `MemoryIo`; `buf` holds `size` bytes.
    unsafe {
        let c = &mut *opaque.cast::<Cursor>();
        let data = (*c.data).as_ref();
        let n = (data.len().saturating_sub(c.pos)).min(size.max(0) as usize);
        if n == 0 {
            return ff::AVERROR_EOF;
        }
        ptr::copy_nonoverlapping(data.as_ptr().add(c.pos), buf, n);
        c.pos += n;
        n as c_int
    }
}

unsafe extern "C" fn seek(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    // SAFETY: `opaque` is the `Cursor` of a live `MemoryIo`.
    let c = unsafe { &mut *opaque.cast::<Cursor>() };
    let len = (*c.data).as_ref().len() as i64;
    if whence & AVSEEK_SIZE != 0 {
        return len;
    }
    let base = match whence & !ff::AVSEEK_FORCE {
        libc::SEEK_SET => 0,
        libc::SEEK_CUR => c.pos as i64,
        libc::SEEK_END => len,
        _ => return -1,
    };
    let to = base + offset;
    if !(0..=len).contains(&to) {
        return -1;
    }
    c.pos = to as usize;
    to
}

impl MemoryIo {
    pub fn new(data: VideoBytes) -> Option<Self> {
        let cursor = Box::into_raw(Box::new(Cursor { data, pos: 0 }));
        // SAFETY: FFmpeg-allocated buffer handed to the context (which may reallocate it).
        let ctx = unsafe {
            let buffer = ff::av_malloc(BUFFER).cast::<u8>();
            if buffer.is_null() {
                drop(Box::from_raw(cursor));
                return None;
            }
            ff::avio_alloc_context(
                buffer,
                BUFFER as c_int,
                0,
                cursor.cast(),
                Some(read),
                None,
                Some(seek),
            )
        };
        if ctx.is_null() {
            // SAFETY: not handed to FFmpeg.
            drop(unsafe { Box::from_raw(cursor) });
            return None;
        }
        Some(Self { ctx, cursor })
    }

    pub fn as_ptr(&self) -> *mut ff::AVIOContext {
        self.ctx
    }
}

impl Drop for MemoryIo {
    fn drop(&mut self) {
        // SAFETY: owned context (the format context using it is closed first) and cursor.
        unsafe {
            if !self.ctx.is_null() {
                ff::av_freep(ptr::addr_of_mut!((*self.ctx).buffer).cast());
                ff::avio_context_free(&mut self.ctx);
            }
            drop(Box::from_raw(self.cursor));
        }
    }
}
