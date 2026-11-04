//! Process lifetime and memory helpers for the engine process.
//!
//! - [`Tie`]: a child process (and every process it starts) is killed when this process exits,
//!   however it exits (Windows job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: the job
//!   handle is closed by the system when the process dies, crash included).
//! - [`trim_heaps`]: the heaps give their free memory back to the system (private bytes).
//! - [`trim_working_set`]: gives the resident pages of this process back to the system (they stay
//!   committed and fault back on access).
//!
//! Elsewhere both do nothing (the engine process runs on Windows only for now).

use std::time::Duration;

use crate::PlatformError;

/// Child processes killed with this process (see the module documentation). Dropping it kills
/// what is left of them.
pub struct Tie {
    #[cfg(windows)]
    job: windows::Win32::Foundation::HANDLE,
}

// SAFETY: a job handle can be used and closed from any thread.
#[cfg(windows)]
unsafe impl Send for Tie {}

impl Tie {
    /// Ties `child`, which must not have started processes of its own yet (they would stay
    /// outside the job).
    pub fn new(child: &std::process::Child) -> Result<Self, PlatformError> {
        #[cfg(windows)]
        {
            windows_impl::tie(child)
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(Self {})
        }
    }
}

impl Drop for Tie {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: our own job handle, closed once.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.job);
        }
    }
}

/// Empties the working set of this process (Windows; `None` elsewhere or on failure). Returns
/// how long it took. Private bytes do not change: the pages stay committed and come back on
/// access (standby list, or the page file for dirty pages written out meanwhile).
pub fn trim_working_set() -> Option<Duration> {
    #[cfg(windows)]
    {
        let t0 = std::time::Instant::now();
        // SAFETY: the current-process pseudo-handle needs no closing; the call only changes
        // which pages are resident.
        let ok = unsafe {
            windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(
                windows::Win32::System::Threading::GetCurrentProcess(),
            )
        }
        .as_bool();
        ok.then(|| t0.elapsed())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Gives the free memory the heaps of this process keep back to the system (Windows,
/// `HeapOptimizeResources` on every heap: the low-fragmentation heap's caches and free blocks
/// are decommitted, which private bytes count). The Windows counterpart of glibc's
/// `malloc_trim`. Returns how long it took; `None` elsewhere or on failure.
pub fn trim_heaps() -> Option<Duration> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Memory::{HeapOptimizeResources, HeapSetInformation};
        /// `HEAP_OPTIMIZE_RESOURCES_INFORMATION` (version 1, no flags).
        #[repr(C)]
        struct Optimize {
            version: u32,
            flags: u32,
        }
        let info = Optimize {
            version: 1,
            flags: 0,
        };
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Memory::{GetProcessHeaps, HEAP_NONE, HeapCompact};
        let t0 = std::time::Instant::now();
        // SAFETY: no handle (= every heap of the process) and a valid structure of the
        // declared size for the call.
        let ok = unsafe {
            HeapSetInformation(
                None,
                HeapOptimizeResources,
                Some((&info as *const Optimize).cast()),
                size_of::<Optimize>(),
            )
        }
        .is_ok();
        // Coalescing the free blocks lets the heap decommit the large ones.
        let mut heaps = [HANDLE::default(); 32];
        // SAFETY: the buffer holds as many handles as its length says.
        let n = unsafe { GetProcessHeaps(&mut heaps) } as usize;
        for &heap in &heaps[..n.min(heaps.len())] {
            // SAFETY: a heap of this process, alive for the call (heaps are not destroyed
            // concurrently here: only the process heap and the CRT's exist).
            unsafe {
                HeapCompact(heap, HEAP_NONE);
            }
        }
        ok.then(|| t0.elapsed())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows::core::PCWSTR;

    use super::Tie;
    use crate::PlatformError;

    fn err(what: &str, e: windows::core::Error) -> PlatformError {
        PlatformError::Os(format!("{what}: {e}"))
    }

    pub(super) fn tie(child: &std::process::Child) -> Result<Tie, PlatformError> {
        // SAFETY: plain Win32 calls on handles we own (the job) or that `child` keeps open
        // for the duration of the call (its process handle).
        unsafe {
            let job =
                CreateJobObjectW(None, PCWSTR::null()).map_err(|e| err("CreateJobObjectW", e))?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .and_then(|()| AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())));
            if let Err(e) = set {
                let _ = CloseHandle(job);
                return Err(err("job object", e));
            }
            Ok(Tie { job })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tied child dies with the tie (Windows: closing the job kills it).
    #[cfg(windows)]
    #[test]
    fn a_tied_child_dies_with_the_tie() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 30 127.0.0.1 >nul"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("cmd.exe");
        let tie = Tie::new(&child).expect("job object");
        assert!(child.try_wait().expect("try_wait").is_none());
        drop(tie);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().expect("try_wait").is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the child survived its job"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn trimming_never_fails_the_caller() {
        // Windows: Some(duration); elsewhere None. Either way nothing else happens.
        let _ = trim_working_set();
        // Freed blocks are given back; live ones stay usable.
        let keep = vec![7u8; 1 << 20];
        drop((0..256).map(|i| vec![i as u8; 4096]).collect::<Vec<_>>());
        let t = trim_heaps();
        assert_eq!(t.is_some(), cfg!(windows));
        assert!(keep.iter().all(|&b| b == 7));
    }
}
