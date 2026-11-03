//! End-to-end test of the process boundary: this executable plays both roles. Without arguments
//! it is the main process and spawns itself with `--lumenwall-web-host`; as the host it answers like
//! the CEF host would (start event, one shared frame with a descriptor, release wait, shutdown).

#[cfg(unix)]
fn main() {
    if std::env::args().any(|a| a == lumenwall_web::process::HOST_ARG) {
        fake_host();
    } else {
        main_process();
        println!("host_process: ok");
    }
}

#[cfg(not(unix))]
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
    });
    tx.send(&frame, &[plane.as_fd()]).unwrap();

    let mut released = false;
    while let Ok(Some((command, _))) = rx.recv::<HostCommand>() {
        match command {
            HostCommand::FrameReleased { id } => {
                assert_eq!(id, 1);
                released = true;
                tx.send(&HostEvent::AudioListener { active: true }, &[])
                    .unwrap();
            }
            HostCommand::Shutdown => break,
            _ => {}
        }
    }
    assert!(released, "frame was released before shutdown");
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
    let SharedTexture::DmaBuf { planes, .. } = &frame.texture;
    assert_eq!(planes.len(), 1);
    let mut file = std::fs::File::from(planes[0].fd.try_clone().unwrap());
    file.rewind().unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "frame-bytes");
    frame.release();
    assert!(matches!(next(), WebEvent::AudioListener(true)));

    drop(host); // Sends Shutdown; the host exits by itself.
}
