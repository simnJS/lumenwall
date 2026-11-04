//! End-to-end test of the process boundary: this executable plays both roles. Without arguments
//! it is the main process and spawns itself with `--lumenwall-web-host`; as the host it answers like
//! the CEF host would (start event, one shared frame with a descriptor or handle, release wait,
//! a shared-memory frame, shutdown).

#[cfg(any(unix, windows))]
fn main() {
    if std::env::args().any(|a| a == lumenwall_web::process::HOST_ARG) {
        fake_host();
    } else {
        main_process();
        println!("host_process: ok");
    }
}

#[cfg(not(any(unix, windows)))]
fn main() {}

#[cfg(unix)]
fn fake_host() {
    use lumenwall_web::ipc::{FrameInfo, HostCommand, HostEvent, PixelFormat, PlaneInfo};
    use std::io::Write;
    use std::os::fd::AsFd;

    let (mut rx, mut tx) = lumenwall_web::process::host::channel().expect("channel");
    let Some((HostCommand::Init(params), _)) = rx.recv::<HostCommand>().expect("recv") else {
        panic!("first message must be Init");
    };
    assert_eq!(params.wallpaper_id, "42");
    tx.send(
        &HostEvent::Started {
            pid: std::process::id(),
        },
        &[],
    )
    .unwrap();

    let mut plane = tempfile::tempfile().unwrap();
    plane.write_all(b"frame-bytes").unwrap();
    let frame = HostEvent::Frame(FrameInfo {
        id: 1,
        width: 2,
        height: 1,
        visible: [0, 0, 2, 1],
        format: PixelFormat::Bgra8,
        modifier: 0,
        planes: vec![PlaneInfo {
            offset: 0,
            stride: 8,
            size: 8,
        }],
        handle: 0,
        memory: false,
    });
    tx.send(&frame, &[plane.as_fd()]).unwrap();

    let mut released = 0;
    while let Ok(Some((command, _))) = rx.recv::<HostCommand>() {
        match command {
            HostCommand::FrameReleased { id: 1 } => {
                released += 1;
                // Software painting: two rows of 2 BGRA pixels in shared memory.
                let mut pixels = tempfile::tempfile().unwrap();
                pixels.write_all(&[7u8; 16]).unwrap();
                let frame = HostEvent::Frame(FrameInfo {
                    id: 2,
                    width: 2,
                    height: 2,
                    visible: [0, 0, 2, 2],
                    format: PixelFormat::Bgra8,
                    modifier: 0,
                    planes: vec![PlaneInfo {
                        offset: 0,
                        stride: 8,
                        size: 16,
                    }],
                    handle: 0,
                    memory: true,
                });
                tx.send(&frame, &[pixels.as_fd()]).unwrap();
            }
            HostCommand::FrameReleased { id: 2 } => {
                released += 1;
                tx.send(&HostEvent::AudioListener { active: true }, &[])
                    .unwrap();
            }
            HostCommand::Shutdown => break,
            _ => {}
        }
    }
    assert_eq!(released, 2, "both frames were released before shutdown");
}

#[cfg(unix)]
fn main_process() {
    use lumenwall_web::ipc::frame::SharedTexture;
    use lumenwall_web::{LaunchParams, PropertySet, WebEvent, WebHost};
    use std::io::{Read, Seek};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    let signal = Arc::new((Mutex::new(0u32), Condvar::new()));
    let waker_signal = signal.clone();
    let waker = Arc::new(move || {
        let (count, cv) = &*waker_signal;
        *count.lock().unwrap() += 1;
        cv.notify_all();
    });
    let params = LaunchParams {
        wallpaper_id: "42".into(),
        root: std::env::temp_dir(),
        file: "index.html".into(),
        width: 2,
        height: 1,
        fps: 30,
        pacing: Default::default(),
        transport: Default::default(),
        properties: PropertySet::new(),
        audio: Default::default(),
        paused: false,
        profile_dir: None,
        devtools_port: None,
    };
    let exe = std::env::current_exe().unwrap();
    let host = WebHost::spawn(&exe, &[], params, waker).expect("spawn");

    let next = || -> WebEvent {
        let (count, cv) = &*signal;
        let mut guard = count.lock().unwrap();
        loop {
            if let Some(event) = host.try_event() {
                return event;
            }
            let (g, timeout) = cv.wait_timeout(guard, Duration::from_secs(10)).unwrap();
            assert!(!timeout.timed_out(), "no event from the host");
            guard = g;
        }
    };

    assert!(matches!(next(), WebEvent::Started { pid } if Some(pid) == host.pid()));
    let WebEvent::Frame(frame) = next() else {
        panic!("expected a frame")
    };
    assert_eq!((frame.id, frame.width, frame.height), (1, 2, 1));
    assert!(!frame.is_revoked());
    let SharedTexture::DmaBuf { planes, .. } = &frame.texture else {
        panic!("expected a DMA-BUF frame")
    };
    assert_eq!(planes.len(), 1);
    let mut file = std::fs::File::from(planes[0].fd.try_clone().unwrap());
    file.rewind().unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "frame-bytes");
    frame.release();

    let WebEvent::Frame(frame) = next() else {
        panic!("expected a memory frame")
    };
    let SharedTexture::Memory { fd, stride, size } = &frame.texture else {
        panic!("expected a shared-memory frame")
    };
    assert_eq!((frame.id, *stride, *size), (2, 8, 16));
    let mapped = lumenwall_web::ipc::frame::MappedMemory::map(fd, *size as usize).unwrap();
    assert_eq!(mapped.as_slice(), &[7u8; 16]);
    drop(mapped);
    frame.release();
    assert!(matches!(next(), WebEvent::AudioListener(true)));

    drop(host); // Sends Shutdown; the host exits by itself.
}

/// A paging-file section holding `bytes`, as the host's software-painting buffer (Windows).
#[cfg(windows)]
fn section_with(bytes: &[u8]) -> std::os::windows::io::OwnedHandle {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
        PAGE_READWRITE, UnmapViewOfFile,
    };
    // SAFETY: plain Win32 calls; the view is written within its length and unmapped, the
    // section handle is owned by the returned value.
    unsafe {
        let raw = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            std::ptr::null(),
            PAGE_READWRITE,
            0,
            bytes.len() as u32,
            std::ptr::null(),
        );
        assert!(!raw.is_null(), "CreateFileMappingW");
        let view = MapViewOfFile(raw, FILE_MAP_WRITE, 0, 0, bytes.len());
        assert!(!view.Value.is_null(), "MapViewOfFile");
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), view.Value.cast::<u8>(), bytes.len());
        UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
        OwnedHandle::from_raw_handle(raw)
    }
}

#[cfg(windows)]
fn fake_host() {
    use lumenwall_web::ipc::{FrameInfo, HostCommand, HostEvent, PixelFormat, PlaneInfo};
    use std::os::windows::io::AsRawHandle;

    let (mut rx, mut tx) = lumenwall_web::process::host::channel().expect("channel");
    let Some(HostCommand::Init(params)) = rx.recv::<HostCommand>().expect("recv") else {
        panic!("first message must be Init");
    };
    assert_eq!(params.wallpaper_id, "42");
    tx.send(&HostEvent::Started {
        pid: std::process::id(),
    })
    .unwrap();

    // Stands for the browser's shared texture: any kernel object can be duplicated by handle;
    // the same object is sent twice, as the browser lends the same pool texture again.
    let texture = section_with(b"frame-bytes");
    let gpu_frame = |id| {
        HostEvent::Frame(FrameInfo {
            id,
            width: 2,
            height: 1,
            visible: [0, 0, 2, 1],
            format: PixelFormat::Bgra8,
            modifier: 0,
            planes: Vec::new(),
            handle: texture.as_raw_handle() as usize as u64,
            memory: false,
        })
    };
    tx.send(&gpu_frame(1)).unwrap();

    // Software painting: two rows of 2 BGRA pixels in shared memory.
    let pixels = section_with(&[7u8; 16]);
    let mut released = 0;
    while let Ok(Some(command)) = rx.recv::<HostCommand>() {
        match command {
            HostCommand::FrameReleased { id: 1 } => {
                released += 1;
                tx.send(&gpu_frame(2)).unwrap();
            }
            HostCommand::FrameReleased { id: 2 } => {
                released += 1;
                let frame = HostEvent::Frame(FrameInfo {
                    id: 3,
                    width: 2,
                    height: 2,
                    visible: [0, 0, 2, 2],
                    format: PixelFormat::Bgra8,
                    modifier: 0,
                    planes: vec![PlaneInfo {
                        offset: 0,
                        stride: 8,
                        size: 16,
                    }],
                    handle: pixels.as_raw_handle() as usize as u64,
                    memory: true,
                });
                tx.send(&frame).unwrap();
            }
            HostCommand::FrameReleased { id: 3 } => {
                released += 1;
                tx.send(&HostEvent::AudioListener { active: true }).unwrap();
            }
            HostCommand::Shutdown => break,
            _ => {}
        }
    }
    assert_eq!(released, 3, "every frame was released before shutdown");
}

#[cfg(windows)]
fn main_process() {
    use lumenwall_web::ipc::frame::{MappedMemory, SharedTexture, same_object};
    use lumenwall_web::{LaunchParams, PropertySet, WebEvent, WebHost};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    let signal = Arc::new((Mutex::new(0u32), Condvar::new()));
    let waker_signal = signal.clone();
    let waker = Arc::new(move || {
        let (count, cv) = &*waker_signal;
        *count.lock().unwrap() += 1;
        cv.notify_all();
    });
    let params = LaunchParams {
        wallpaper_id: "42".into(),
        root: std::env::temp_dir(),
        file: "index.html".into(),
        width: 2,
        height: 1,
        fps: 30,
        pacing: Default::default(),
        transport: Default::default(),
        properties: PropertySet::new(),
        audio: Default::default(),
        paused: false,
        profile_dir: None,
        devtools_port: None,
    };
    let exe = std::env::current_exe().unwrap();
    let host = WebHost::spawn(&exe, &[], params, waker).expect("spawn");

    let next = || -> WebEvent {
        let (count, cv) = &*signal;
        let mut guard = count.lock().unwrap();
        loop {
            if let Some(event) = host.try_event() {
                return event;
            }
            let (g, timeout) = cv.wait_timeout(guard, Duration::from_secs(10)).unwrap();
            assert!(!timeout.timed_out(), "no event from the host");
            guard = g;
        }
    };

    assert!(matches!(next(), WebEvent::Started { pid } if Some(pid) == host.pid()));
    let WebEvent::Frame(first) = next() else {
        panic!("expected a frame")
    };
    assert_eq!((first.id, first.width, first.height), (1, 2, 1));
    assert!(!first.is_revoked());
    let SharedTexture::D3D11 {
        handle: first_handle,
    } = &first.texture
    else {
        panic!("expected a shared texture handle")
    };
    // The handle was duplicated into this process: the host's object is readable through it.
    let mapped = MappedMemory::map(first_handle, 11).unwrap();
    assert_eq!(mapped.as_slice(), b"frame-bytes");
    drop(mapped);
    let first_handle = first_handle.try_clone().unwrap();
    first.release();

    let WebEvent::Frame(second) = next() else {
        panic!("expected a second frame")
    };
    let SharedTexture::D3D11 { handle } = &second.texture else {
        panic!("expected a shared texture handle")
    };
    assert!(same_object(handle, &first_handle), "same pool buffer");
    second.release();

    let WebEvent::Frame(frame) = next() else {
        panic!("expected a memory frame")
    };
    let SharedTexture::Memory {
        handle,
        stride,
        size,
    } = &frame.texture
    else {
        panic!("expected a shared-memory frame")
    };
    assert_eq!((frame.id, *stride, *size), (3, 8, 16));
    assert!(!same_object(handle, &first_handle), "another buffer");
    let mapped = MappedMemory::map(handle, *size as usize).unwrap();
    assert_eq!(mapped.as_slice(), &[7u8; 16]);
    drop(mapped);
    frame.release();
    assert!(matches!(next(), WebEvent::AudioListener(true)));

    drop(host); // Sends Shutdown; the host exits by itself.
}
