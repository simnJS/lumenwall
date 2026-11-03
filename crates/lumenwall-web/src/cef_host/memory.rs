//! Shared memory buffer for software-painted frames ([`crate::ipc::FrameTransport::Memory`]).

use std::os::fd::OwnedFd;
use std::ptr::NonNull;

use rustix::fs::{MemfdFlags, memfd_create};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};

/// A `memfd` mapped read-write in the host; the main process maps the descriptor read-only.
pub struct SharedPixels {
    fd: OwnedFd,
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is owned by this value and only accessed through `&mut self`.
unsafe impl Send for SharedPixels {}

impl SharedPixels {
    pub fn new(len: usize) -> std::io::Result<Self> {
        let fd = memfd_create("lumenwall-web-frame", MemfdFlags::CLOEXEC)?;
        rustix::fs::ftruncate(&fd, len as u64)?;
        // SAFETY: a fresh shared mapping of the memfd, unmapped in Drop.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )?
        };
        let ptr =
            NonNull::new(ptr.cast::<u8>()).ok_or_else(|| std::io::Error::other("null mapping"))?;
        Ok(Self { fd, ptr, len })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `len` writable bytes mapped in `new`, alive until drop.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for SharedPixels {
    fn drop(&mut self) {
        // SAFETY: the mapping created in `new`, unmapped once.
        let _ = unsafe { munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}
