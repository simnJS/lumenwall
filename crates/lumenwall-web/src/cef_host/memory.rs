//! Shared memory buffer for software-painted frames ([`crate::ipc::FrameTransport::Memory`]).
//!
//! Linux: a `memfd` whose descriptor travels with the frame message. Windows: an anonymous
//! section (`CreateFileMappingW` on the paging file) whose handle value travels in
//! [`crate::ipc::FrameInfo::handle`]; the main process duplicates it out of this process.

use std::ptr::NonNull;

/// The buffer, mapped read-write in the host; the main process maps it read-only.
pub struct SharedPixels {
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
    #[cfg(windows)]
    section: std::os::windows::io::OwnedHandle,
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is owned by this value and only accessed through `&mut self`.
unsafe impl Send for SharedPixels {}

impl SharedPixels {
    #[cfg(target_os = "linux")]
    pub fn new(len: usize) -> std::io::Result<Self> {
        use rustix::fs::{MemfdFlags, memfd_create};
        use rustix::mm::{MapFlags, ProtFlags, mmap};
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

    #[cfg(windows)]
    pub fn new(len: usize) -> std::io::Result<Self> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, PAGE_READWRITE,
        };
        if len == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty frame buffer",
            ));
        }
        let size = len as u64;
        // SAFETY: an unnamed section backed by the paging file, default security (the handle
        // is not inheritable); the handle is owned by `section`.
        let raw = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                std::ptr::null(),
            )
        };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh handle owned by nobody else.
        let section = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: maps the whole section read-write; unmapped in Drop.
        let view = unsafe { MapViewOfFile(raw, FILE_MAP_WRITE, 0, 0, len) };
        let ptr =
            NonNull::new(view.Value.cast::<u8>()).ok_or_else(std::io::Error::last_os_error)?;
        Ok(Self { section, ptr, len })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    #[cfg(target_os = "linux")]
    pub fn fd(&self) -> &std::os::fd::OwnedFd {
        &self.fd
    }

    /// Value of the section handle in this process (duplicated by the main process while the
    /// host waits for the frame's release).
    #[cfg(windows)]
    pub fn handle_value(&self) -> u64 {
        use std::os::windows::io::AsRawHandle;
        self.section.as_raw_handle() as usize as u64
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `len` writable bytes mapped in `new`, alive until drop.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for SharedPixels {
    fn drop(&mut self) {
        // SAFETY: the mapping created in `new`, unmapped once (the section handle closes after).
        #[cfg(target_os = "linux")]
        let _ = unsafe { rustix::mm::munmap(self.ptr.as_ptr().cast(), self.len) };
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Memory::UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.ptr.as_ptr().cast(),
                },
            );
        }
    }
}
