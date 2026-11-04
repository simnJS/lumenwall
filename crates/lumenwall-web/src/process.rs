//! Main-process side of the web host: spawns the host process (only when a web wallpaper is
//! shown), sends it commands and receives its events on background threads.
//!
//! Nothing here polls: the reader thread blocks on the channel and calls the `waker` given to
//! [`WebHost::spawn`] after queueing each event, so the main event loop can sleep until then.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::ipc::frame::{SharedFrame, SharedTexture};
use crate::ipc::{FrameInfo, HostCommand, HostEvent, LaunchParams};
use crate::media::MediaListeners;

/// Argument that tells the host executable it was launched by Lumenwall.
pub const HOST_ARG: &str = "--lumenwall-web-host";

/// File name of the host executable built by this crate (`cef` feature).
pub fn host_executable_name() -> String {
    format!("lumenwall-web-host{}", std::env::consts::EXE_SUFFIX)
}

/// The host executable next to the current executable.
pub fn default_host_executable() -> io::Result<PathBuf> {
    Ok(std::env::current_exe()?.with_file_name(host_executable_name()))
}

/// Environment variable naming the host executable explicitly.
pub const HOST_ENV: &str = "LUMENWALL_WEB_HOST";

/// The host executable to use: [`HOST_ENV`] when set, otherwise next to the current executable,
/// otherwise one folder up (Cargo examples and test binaries live in a subfolder of the
/// folder holding the binaries). `None` when none exists.
pub fn find_host_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(HOST_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path)).filter(|p| p.is_file());
    }
    let next = default_host_executable().ok()?;
    if next.is_file() {
        return Some(next);
    }
    let up = next.parent()?.parent()?.join(host_executable_name());
    up.is_file().then_some(up)
}

/// Something that happened in the web host.
#[derive(Debug)]
pub enum WebEvent {
    /// The browser exists; `pid` is the host process (its audio stream is ours, not "another
    /// application" for automute).
    Started {
        pid: u32,
    },
    /// A frame to import now, then release (see [`SharedFrame`]).
    Frame(SharedFrame),
    Loaded {
        url: String,
        status: i32,
    },
    LoadFailed {
        url: String,
        error: String,
    },
    /// Send audio frames only while true.
    AudioListener(bool),
    /// Send media events only for registered kinds.
    MediaListeners(MediaListeners),
    Error(String),
    /// The host process ended or the channel broke.
    Exited,
}

/// Handle to a running web host process.
pub struct WebHost {
    child: Option<Child>,
    commands: mpsc::Sender<HostCommand>,
    events: mpsc::Receiver<WebEvent>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for WebHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebHost")
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

impl WebHost {
    /// Starts `executable` (see [`default_host_executable`]) for one web wallpaper.
    pub fn spawn(
        executable: &Path,
        extra_args: &[OsString],
        params: LaunchParams,
        waker: Waker,
    ) -> io::Result<Self> {
        let mut command = Command::new(executable);
        command
            .arg(HOST_ARG)
            .args(extra_args)
            .stderr(Stdio::inherit());
        // Chromium puts a Unix socket in a folder of the temporary directory; a socket path
        // longer than 107 bytes aborts the browser. Fall back on /tmp for long TMPDIRs.
        #[cfg(unix)]
        if std::env::temp_dir().as_os_str().len() > 64 && Path::new("/tmp").is_dir() {
            command.env("TMPDIR", "/tmp");
        }

        let (commands, command_rx) = mpsc::channel::<HostCommand>();
        let (event_tx, events) = mpsc::channel::<WebEvent>();
        commands
            .send(HostCommand::Init(Box::new(params)))
            .map_err(|_| io::Error::other("command channel closed"))?;

        #[cfg(unix)]
        let (child, threads) = {
            let (ours, theirs) = crate::ipc::pair()?;
            command
                .stdin(Stdio::from(std::os::fd::OwnedFd::from(theirs)))
                .stdout(Stdio::null());
            let child = command.spawn()?;
            let mut sender = crate::ipc::Sender::new(ours.try_clone()?);
            let mut receiver = crate::ipc::Receiver::new(ours);
            let writer = std::thread::Builder::new()
                .name("lumenwall-web-tx".into())
                .spawn(move || {
                    for command in command_rx {
                        if sender.send(&command, &[]).is_err() {
                            break;
                        }
                    }
                })?;
            let release = commands.clone();
            let reader = std::thread::Builder::new()
                .name("lumenwall-web-rx".into())
                .spawn(move || {
                    let mut dispatcher = Dispatcher::new(event_tx, release, waker);
                    loop {
                        match receiver.recv::<HostEvent>() {
                            Ok(Some((event, fds))) => {
                                dispatcher.dispatch(event, |info| unix_texture(info, fds))
                            }
                            Ok(None) => break,
                            Err(e) => {
                                dispatcher.push(WebEvent::Error(format!("web host channel: {e}")));
                                break;
                            }
                        }
                    }
                    dispatcher.push(WebEvent::Exited);
                })?;
            (child, vec![writer, reader])
        };

        #[cfg(windows)]
        let (child, threads) = {
            use std::os::windows::io::AsHandle;
            command.stdin(Stdio::piped()).stdout(Stdio::piped());
            let mut child = command.spawn()?;
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("no stdin pipe"))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("no stdout pipe"))?;
            let process = child.as_handle().try_clone_to_owned()?;
            let mut sender = crate::ipc::Sender::new(stdin);
            let mut receiver = crate::ipc::Receiver::new(stdout);
            let writer = std::thread::Builder::new()
                .name("lumenwall-web-tx".into())
                .spawn(move || {
                    for command in command_rx {
                        if sender.send(&command).is_err() {
                            break;
                        }
                    }
                })?;
            let release = commands.clone();
            let reader = std::thread::Builder::new()
                .name("lumenwall-web-rx".into())
                .spawn(move || {
                    let mut dispatcher = Dispatcher::new(event_tx, release, waker);
                    loop {
                        match receiver.recv::<HostEvent>() {
                            Ok(Some(event)) => {
                                dispatcher.dispatch(event, |info| windows_texture(&process, info))
                            }
                            Ok(None) => break,
                            Err(e) => {
                                dispatcher.push(WebEvent::Error(format!("web host channel: {e}")));
                                break;
                            }
                        }
                    }
                    dispatcher.push(WebEvent::Exited);
                })?;
            (child, vec![writer, reader])
        };

        Ok(Self {
            child: Some(child),
            commands,
            events,
            threads,
        })
    }

    /// Queues a command; false if the host is gone.
    pub fn send(&self, command: HostCommand) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Next pending event, without blocking.
    pub fn try_event(&self) -> Option<WebEvent> {
        self.events.try_recv().ok()
    }

    /// Host process id.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
}

impl Drop for WebHost {
    fn drop(&mut self) {
        let _ = self.commands.send(HostCommand::Shutdown);
        let Some(mut child) = self.child.take() else {
            return;
        };
        let threads = std::mem::take(&mut self.threads);
        // Reap in the background: never block the caller's event loop on shutdown.
        let _ = std::thread::Builder::new()
            .name("lumenwall-web-reap".into())
            .spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(50))
                        }
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                    }
                }
                drop(threads);
            });
    }
}

/// Turns wire events into [`WebEvent`]s on the reader thread.
struct Dispatcher {
    events: mpsc::Sender<WebEvent>,
    commands: mpsc::Sender<HostCommand>,
    waker: Waker,
    pending: Option<(u64, Arc<AtomicBool>)>,
}

impl Dispatcher {
    fn new(
        events: mpsc::Sender<WebEvent>,
        commands: mpsc::Sender<HostCommand>,
        waker: Waker,
    ) -> Self {
        Self {
            events,
            commands,
            waker,
            pending: None,
        }
    }

    fn push(&self, event: WebEvent) {
        if self.events.send(event).is_ok() {
            (self.waker)();
        }
    }

    fn dispatch(
        &mut self,
        event: HostEvent,
        texture: impl FnOnce(&FrameInfo) -> io::Result<SharedTexture>,
    ) {
        let event = match event {
            HostEvent::Started { pid } => WebEvent::Started { pid },
            HostEvent::Frame(info) => match texture(&info) {
                Ok(texture) => {
                    let revoked = Arc::new(AtomicBool::new(false));
                    self.pending = Some((info.id, revoked.clone()));
                    let commands = self.commands.clone();
                    let release = Box::new(move |id| {
                        let _ = commands.send(HostCommand::FrameReleased { id });
                    });
                    WebEvent::Frame(SharedFrame::new(&info, texture, revoked, release))
                }
                Err(e) => {
                    // Let the browser continue; the frame is lost.
                    let _ = self
                        .commands
                        .send(HostCommand::FrameReleased { id: info.id });
                    WebEvent::Error(format!("cannot receive frame {}: {e}", info.id))
                }
            },
            HostEvent::FrameRevoked { id } => {
                if let Some((pending, flag)) = &self.pending
                    && *pending == id
                {
                    flag.store(true, Ordering::Release);
                }
                return;
            }
            HostEvent::Loaded { url, status } => WebEvent::Loaded { url, status },
            HostEvent::LoadFailed { url, error } => WebEvent::LoadFailed { url, error },
            HostEvent::AudioListener { active } => WebEvent::AudioListener(active),
            HostEvent::MediaListeners { listeners } => WebEvent::MediaListeners(listeners),
            HostEvent::Error { message } => WebEvent::Error(message),
        };
        self.push(event);
    }
}

/// The single plane of a shared-memory frame, checked against the frame size.
fn memory_plane(info: &FrameInfo) -> io::Result<crate::ipc::PlaneInfo> {
    let [plane] = info.planes[..] else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "memory frame without exactly one plane",
        ));
    };
    let needed = u64::from(plane.stride) * u64::from(info.height);
    if plane.offset != 0
        || plane.size < needed
        || u64::from(plane.stride) < u64::from(info.width) * 4
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "memory frame smaller than its rows",
        ));
    }
    Ok(plane)
}

#[cfg(unix)]
fn unix_texture(info: &FrameInfo, fds: Vec<std::os::fd::OwnedFd>) -> io::Result<SharedTexture> {
    use crate::ipc::frame::DmaBufPlane;
    if fds.len() != info.planes.len() || fds.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "plane/descriptor count mismatch",
        ));
    }
    if info.memory {
        let plane = memory_plane(info)?;
        let fd = fds.into_iter().next().expect("one descriptor");
        return Ok(SharedTexture::Memory {
            fd,
            stride: plane.stride,
            size: plane.size,
        });
    }
    let planes = fds
        .into_iter()
        .zip(&info.planes)
        .map(|(fd, p)| DmaBufPlane {
            fd,
            offset: p.offset,
            stride: p.stride,
            size: p.size,
        })
        .collect();
    Ok(SharedTexture::DmaBuf {
        planes,
        modifier: info.modifier,
    })
}

#[cfg(windows)]
fn windows_texture(
    process: &std::os::windows::io::OwnedHandle,
    info: &FrameInfo,
) -> io::Result<SharedTexture> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    if info.handle == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "null shared handle",
        ));
    }
    let plane = if info.memory {
        Some(memory_plane(info)?)
    } else {
        None
    };
    let mut local: HANDLE = std::ptr::null_mut();
    // SAFETY: the host keeps the source handle open until it receives FrameReleased; the
    // duplicate is owned by the returned OwnedHandle.
    let ok = unsafe {
        DuplicateHandle(
            process.as_raw_handle() as HANDLE,
            info.handle as usize as HANDLE,
            GetCurrentProcess(),
            &mut local,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `local` is a fresh handle owned by this process.
    let handle = unsafe { OwnedHandle::from_raw_handle(local as _) };
    Ok(match plane {
        Some(plane) => SharedTexture::Memory {
            handle,
            stride: plane.stride,
            size: plane.size,
        },
        None => SharedTexture::D3D11 { handle },
    })
}

/// Host-process side of the channel (used by the host executable).
pub mod host {
    use std::io;

    /// The host's command receiver and event sender, taken from its standard streams.
    pub fn channel() -> io::Result<(crate::ipc::Receiver, crate::ipc::Sender)> {
        #[cfg(unix)]
        {
            let socket = crate::ipc::take_stdin_socket()?;
            Ok((
                crate::ipc::Receiver::new(socket.try_clone()?),
                crate::ipc::Sender::new(socket),
            ))
        }
        #[cfg(windows)]
        {
            let (input, output) = crate::ipc::take_std_pipes()?;
            Ok((
                crate::ipc::Receiver::new(input),
                crate::ipc::Sender::new(output),
            ))
        }
    }
}
