//! Web host process built on the Chromium Embedded Framework (feature `cef`).
//!
//! The same executable serves as the browser process (launched by Lumenwall with
//! [`crate::process::HOST_ARG`]) and as every CEF subprocess (renderer, GPU, utility…), which
//! CEF launches with `--type=…`. Entry point: [`run`].
//!
//! Threads in the browser process:
//! - CEF UI thread (= main thread): all browser calls, the [`BridgeSession`], painting;
//! - IPC reader: blocks on the channel; frame releases go straight to the paint wait, other
//!   commands are posted to the UI thread;
//! - CEF IO thread: `wp<id>://` requests and the `file://` policy;
//! - audio output thread (see `audio_out`).

mod app;
mod audio_out;
mod client;
#[cfg(any(target_os = "linux", windows))]
mod memory;
mod scheme;

use std::cell::RefCell;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};

use cef::*;

use crate::access::LocalFileAccess;
use crate::audio::AUDIO_FRAME_LEN;
use crate::files::{FileService, initial_url, scheme_name};
use crate::ipc::{AudioOutput, FramePacing, FrameTransport, HostCommand, HostEvent, LaunchParams};
use crate::mouse::{MouseButton, MouseInput};
use crate::process::HOST_ARG;
use crate::properties::GeneralProperties;
use crate::session::{BridgeSession, SessionEffect};

/// Process message: page → host JSON (renderer → browser).
const PAGE_MESSAGE: &str = "lumenwall.page";
/// Process message: the main frame's JavaScript context was released.
const RELEASED_MESSAGE: &str = "lumenwall.released";
/// Switch carrying the scheme name to CEF subprocesses.
const SCHEME_SWITCH: &str = "lumenwall-scheme";

/// State shared between CEF threads.
pub struct Shared {
    files: Arc<FileService>,
    access: RwLock<LocalFileAccess>,
    events: Mutex<crate::ipc::Sender>,
    releases: Mutex<mpsc::Receiver<u64>>,
    size: AtomicU64,
    next_frame: AtomicU64,
    software_warned: AtomicBool,
    audio: audio_out::AudioOut,
    audio_channels: AtomicU32,
    /// Software-painted frames ([`FrameTransport::Memory`]).
    #[cfg(any(target_os = "linux", windows))]
    memory: Mutex<Option<memory::SharedPixels>>,
    params: LaunchParams,
}

impl Shared {
    fn size(&self) -> (u32, u32) {
        let packed = self.size.load(Ordering::Relaxed);
        ((packed >> 32) as u32, packed as u32)
    }

    fn set_size(&self, width: u32, height: u32) {
        self.size.store(
            (u64::from(width) << 32) | u64::from(height),
            Ordering::Relaxed,
        );
    }

    fn send_event(&self, event: &HostEvent) {
        let Ok(mut events) = self.events.lock() else {
            return;
        };
        #[cfg(unix)]
        let result = events.send(event, &[]);
        #[cfg(windows)]
        let result = events.send(event);
        if let Err(e) = result {
            tracing::debug!("event channel: {e}");
        }
    }

    #[cfg(unix)]
    fn send_frame(&self, event: &HostEvent, fds: &[std::os::fd::BorrowedFd<'_>]) -> bool {
        self.events
            .lock()
            .is_ok_and(|mut events| events.send(event, fds).is_ok())
    }

    #[cfg(windows)]
    fn send_frame(&self, event: &HostEvent, _fds: &[()]) -> bool {
        self.events
            .lock()
            .is_ok_and(|mut events| events.send(event).is_ok())
    }
}

/// UI-thread state.
struct Ui {
    shared: Arc<Shared>,
    session: BridgeSession,
    browser: Option<Browser>,
    paused: bool,
    audio: AudioOutput,
    closing: bool,
    effects: Vec<SessionEffect>,
    devtools_id: i32,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Work for the UI thread.
enum UiAction {
    Command(HostCommand),
    PageMessage(String),
    ContextReleased,
    BrowserCreated(Browser),
    BrowserClosed,
}

/// Runs `action` on the UI thread: immediately if we are on it and not inside another UI
/// action, otherwise as a posted task.
fn on_ui(action: UiAction) {
    let ui_thread = currently_on(ThreadId::from(sys::cef_thread_id_t::TID_UI)) != 0;
    if ui_thread && UI.with(|ui| ui.try_borrow_mut().is_ok()) {
        handle(action);
    } else {
        post(action);
    }
}

/// False before CEF is initialised and once it is shutting down: tasks can no longer be posted.
static CEF_RUNNING: AtomicBool = AtomicBool::new(false);

fn post(action: UiAction) {
    if !CEF_RUNNING.load(Ordering::Acquire) {
        return;
    }
    let mut task = UiTask::new(Arc::new(Mutex::new(Some(SendAction(action)))));
    post_task(
        ThreadId::from(sys::cef_thread_id_t::TID_UI),
        Some(&mut task),
    );
}

/// Moves an action to the UI thread. CEF objects inside are only touched there.
struct SendAction(UiAction);
// SAFETY: the wrapped CEF handles are reference-counted thread-safely by CEF and are only used
// on the UI thread, where the task runs.
unsafe impl Send for SendAction {}

wrap_task! {
    struct UiTask {
        action: Arc<Mutex<Option<SendAction>>>,
    }

    impl Task {
        fn execute(&self) {
            let action = self.action.lock().ok().and_then(|mut a| a.take());
            if let Some(SendAction(action)) = action {
                handle(action);
            }
        }
    }
}

/// What to do on the browser after the UI state borrow is released.
enum BrowserCall {
    Script(String),
    Resize,
    FrameRate(i32),
    BeginFrame,
    Hidden(bool),
    Lifecycle(i32, bool),
    AudioMuted(bool),
    Invalidate,
    MouseMove(i32, i32, bool),
    MouseClick(i32, i32, MouseButton, bool),
    Reload,
    Close,
    Quit,
}

fn handle(action: UiAction) {
    let mut calls = Vec::new();
    let browser = UI.with(|cell| {
        let mut guard = cell.borrow_mut();
        let ui = guard.as_mut()?;
        ui.apply(action, &mut calls);
        ui.browser.clone()
    });
    for call in calls {
        if let BrowserCall::Quit = call {
            quit_message_loop();
            continue;
        }
        let Some(browser) = browser.as_ref() else {
            continue;
        };
        execute(browser, call);
    }
}

fn execute(browser: &Browser, call: BrowserCall) {
    let host = browser.host();
    match call {
        BrowserCall::Script(code) => {
            if let Some(frame) = browser.main_frame() {
                frame.execute_java_script(Some(&CefString::from(code.as_str())), None, 0);
            }
        }
        BrowserCall::Resize => {
            if let Some(host) = host {
                host.was_resized();
            }
        }
        BrowserCall::FrameRate(fps) => {
            if let Some(host) = host {
                host.set_windowless_frame_rate(fps);
            }
        }
        BrowserCall::BeginFrame => {
            if let Some(host) = host {
                host.send_external_begin_frame();
            }
        }
        BrowserCall::Hidden(hidden) => {
            if let Some(host) = host {
                host.was_hidden(i32::from(hidden));
            }
        }
        BrowserCall::Lifecycle(id, frozen) => {
            // Freezes timers and script while paused (Chrome DevTools Protocol).
            if let (Some(host), Some(mut params)) = (host, dictionary_value_create()) {
                params.set_string(
                    Some(&CefString::from("state")),
                    Some(&CefString::from(if frozen { "frozen" } else { "active" })),
                );
                host.execute_dev_tools_method(
                    id,
                    Some(&CefString::from("Page.setWebLifecycleState")),
                    Some(&mut params),
                );
            }
        }
        BrowserCall::AudioMuted(muted) => {
            if let Some(host) = host {
                host.set_audio_muted(i32::from(muted));
            }
        }
        BrowserCall::Invalidate => {
            if let Some(host) = host {
                host.invalidate(PaintElementType::from(
                    sys::cef_paint_element_type_t::PET_VIEW,
                ));
            }
        }
        BrowserCall::MouseMove(x, y, left) => {
            if let Some(host) = host {
                // The C enum is signed on Windows, unsigned elsewhere.
                #[allow(clippy::unnecessary_cast)]
                let modifiers = if left {
                    sys::cef_event_flags_t::EVENTFLAG_LEFT_MOUSE_BUTTON.0 as u32
                } else {
                    0
                };
                host.send_mouse_move_event(Some(&MouseEvent { x, y, modifiers }), 0);
            }
        }
        BrowserCall::MouseClick(x, y, button, down) => {
            if let Some(host) = host {
                let kind = match button {
                    MouseButton::Left => sys::cef_mouse_button_type_t::MBT_LEFT,
                    MouseButton::Right => sys::cef_mouse_button_type_t::MBT_RIGHT,
                };
                host.send_mouse_click_event(
                    Some(&MouseEvent { x, y, modifiers: 0 }),
                    MouseButtonType::from(kind),
                    i32::from(!down),
                    1,
                );
            }
        }
        BrowserCall::Reload => browser.reload(),
        BrowserCall::Close => {
            if let Some(host) = host {
                host.close_browser(1);
            }
        }
        BrowserCall::Quit => {}
    }
}

impl Ui {
    fn apply(&mut self, action: UiAction, calls: &mut Vec<BrowserCall>) {
        match action {
            UiAction::Command(command) => self.command(command, calls),
            UiAction::PageMessage(json) => self.session.page_message(&json, &mut self.effects),
            UiAction::ContextReleased => self.session.context_released(&mut self.effects),
            UiAction::BrowserCreated(browser) => {
                self.browser = Some(browser);
                self.apply_audio(calls);
                if self.paused {
                    self.devtools_id += 1;
                    calls.push(BrowserCall::Hidden(true));
                    calls.push(BrowserCall::Lifecycle(self.devtools_id, true));
                }
            }
            UiAction::BrowserClosed => {
                self.browser = None;
                calls.push(BrowserCall::Quit);
            }
        }
        self.flush_effects(calls);
    }

    fn flush_effects(&mut self, calls: &mut Vec<BrowserCall>) {
        for effect in self.effects.drain(..) {
            match effect {
                SessionEffect::Script(code) => calls.push(BrowserCall::Script(code)),
                SessionEffect::AudioListener(active) => {
                    self.shared.send_event(&HostEvent::AudioListener { active })
                }
                SessionEffect::MediaListeners(listeners) => {
                    self.shared
                        .send_event(&HostEvent::MediaListeners { listeners });
                }
            }
        }
    }

    fn command(&mut self, command: HostCommand, calls: &mut Vec<BrowserCall>) {
        match command {
            HostCommand::Init(_) | HostCommand::FrameReleased { .. } => {}
            HostCommand::Resize { width, height } => {
                self.shared.set_size(width, height);
                calls.push(BrowserCall::Resize);
            }
            HostCommand::SetFps { fps } => {
                self.session.set_fps(fps, &mut self.effects);
                calls.push(BrowserCall::FrameRate(fps.max(1) as i32));
            }
            HostCommand::BeginFrame => {
                if !self.paused && self.shared.params.pacing == FramePacing::External {
                    calls.push(BrowserCall::BeginFrame);
                }
            }
            HostCommand::SetPaused { paused } => {
                if self.paused == paused {
                    return;
                }
                self.paused = paused;
                self.devtools_id += 1;
                // Tell the page first, then stop painting, freeze script and silence it.
                self.session.set_paused(paused, &mut self.effects);
                self.flush_effects(calls);
                calls.push(BrowserCall::Hidden(paused));
                calls.push(BrowserCall::Lifecycle(self.devtools_id, paused));
                self.apply_audio(calls);
                if !paused {
                    calls.push(BrowserCall::Invalidate);
                }
            }
            HostCommand::SetAudio(audio) => {
                self.audio = audio;
                self.apply_audio(calls);
            }
            HostCommand::Properties { changes } => {
                self.session.set_properties(changes, &mut self.effects);
                let access = self.session.local_file_access(self.shared.files.root());
                if let Ok(mut current) = self.shared.access.write() {
                    *current = access;
                }
            }
            HostCommand::Audio { values } => {
                let mut frame = [0.0f32; AUDIO_FRAME_LEN];
                for (dst, src) in frame.iter_mut().zip(&values) {
                    *dst = *src;
                }
                if let Some(script) = self.session.audio(&frame) {
                    calls.push(BrowserCall::Script(script.to_owned()));
                }
            }
            HostCommand::Media(event) => self.session.media(event, &mut self.effects),
            HostCommand::RescanDirectories => self.session.rescan_directories(&mut self.effects),
            HostCommand::Mouse(input) => match input {
                MouseInput::Move { x, y, left_down } => {
                    calls.push(BrowserCall::MouseMove(x, y, left_down))
                }
                MouseInput::Button { x, y, button, down } => {
                    calls.push(BrowserCall::MouseClick(x, y, button, down))
                }
            },
            HostCommand::Reload => calls.push(BrowserCall::Reload),
            HostCommand::Shutdown => {
                if !self.closing {
                    self.closing = true;
                    if self.browser.is_some() {
                        calls.push(BrowserCall::Close);
                    } else {
                        calls.push(BrowserCall::Quit);
                    }
                }
            }
        }
    }

    fn apply_audio(&self, calls: &mut Vec<BrowserCall>) {
        let muted = self.audio.muted || self.paused;
        self.shared.audio.set_state(AudioOutput {
            volume: self.audio.volume,
            muted,
        });
        calls.push(BrowserCall::AudioMuted(muted));
    }
}

/// Creates the offscreen browser (called once CEF is initialised).
fn create_browser(shared: &Arc<Shared>) {
    let params = &shared.params;
    let (width, height) = shared.size();
    let window_info = WindowInfo {
        windowless_rendering_enabled: 1,
        // Software painting into shared memory is the fallback when the main process cannot
        // import the browser's GPU textures.
        shared_texture_enabled: i32::from(params.transport == FrameTransport::Gpu),
        external_begin_frame_enabled: i32::from(params.pacing == FramePacing::External),
        bounds: Rect {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        },
        ..Default::default()
    };
    let settings = BrowserSettings {
        // Target fps of the wallpaper, no 60 fps floor (spec 07 §8.2 deviation, see README).
        windowless_frame_rate: params.fps.max(1) as i32,
        background_color: 0xFFFF_FFFF,
        ..Default::default()
    };
    let url = initial_url(&params.wallpaper_id, &params.file);
    let mut client = client::build_client(shared.clone());
    if browser_host_create_browser(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(url.as_str())),
        Some(&settings),
        None,
        None,
    ) == 0
    {
        shared.send_event(&HostEvent::Error {
            message: "cannot create the browser".into(),
        });
        on_ui(UiAction::BrowserClosed);
    }
}

fn scheme_from_args() -> String {
    let prefix = format!("--{SCHEME_SWITCH}=");
    std::env::args()
        .find_map(|a| a.strip_prefix(&prefix).map(str::to_owned))
        .unwrap_or_default()
}

fn recv_command(receiver: &mut crate::ipc::Receiver) -> std::io::Result<Option<HostCommand>> {
    #[cfg(unix)]
    return receiver
        .recv::<HostCommand>()
        .map(|m| m.map(|(command, _)| command));
    #[cfg(windows)]
    return receiver.recv::<HostCommand>();
}

/// Entry point of the `lumenwall-web-host` executable.
pub fn run() -> ExitCode {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = args::Args::new();
    let is_subprocess = args
        .as_cmd_line()
        .is_some_and(|cmd| cmd.has_switch(Some(&CefString::from("type"))) != 0);

    if is_subprocess {
        let mut app = app::LumenApp::new(app::AppConfig {
            scheme: scheme_from_args(),
            shared: None,
        });
        let code = execute_process(
            Some(args.as_main_args()),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        return ExitCode::from(code.clamp(0, 255) as u8);
    }
    if !std::env::args().any(|a| a == HOST_ARG) {
        eprintln!("lumenwall-web-host is started by lumenwall for web wallpapers");
        return ExitCode::from(2);
    }

    let (mut receiver, sender) = match crate::process::host::channel() {
        Ok(channel) => channel,
        Err(e) => {
            eprintln!("lumenwall-web-host: no channel to lumenwall: {e}");
            return ExitCode::FAILURE;
        }
    };
    let params = match recv_command(&mut receiver) {
        Ok(Some(HostCommand::Init(params))) => *params,
        other => {
            eprintln!("lumenwall-web-host: expected Init, got {other:?}");
            return ExitCode::FAILURE;
        }
    };
    match host_browser(params, receiver, sender, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("lumenwall-web-host: {message}");
            ExitCode::FAILURE
        }
    }
}

fn host_browser(
    params: LaunchParams,
    mut receiver: crate::ipc::Receiver,
    sender: crate::ipc::Sender,
    args: &args::Args,
) -> Result<(), String> {
    let files =
        Arc::new(FileService::new(&params.root).map_err(|e| format!("wallpaper folder: {e}"))?);
    let scheme = scheme_name(&params.wallpaper_id);
    let mut session = BridgeSession::new(
        params.properties.clone(),
        GeneralProperties { fps: params.fps },
    );
    session.set_paused(params.paused, &mut Vec::new());
    let access = session.local_file_access(files.root());

    // QO-17: a temporary profile is removed on exit; a configured one persists (localStorage).
    let (profile, temporary) = match &params.profile_dir {
        Some(dir) => (dir.clone(), false),
        None => {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            (
                std::env::temp_dir()
                    .join(format!("lumenwall-web-{}-{nanos:x}", std::process::id())),
                true,
            )
        }
    };
    std::fs::create_dir_all(&profile).map_err(|e| format!("profile {}: {e}", profile.display()))?;

    let (release_tx, release_rx) = mpsc::channel::<u64>();
    let shared = Arc::new(Shared {
        files,
        access: RwLock::new(access),
        events: Mutex::new(sender),
        releases: Mutex::new(release_rx),
        size: AtomicU64::new(0),
        next_frame: AtomicU64::new(0),
        software_warned: AtomicBool::new(false),
        audio: audio_out::AudioOut::new(params.audio),
        audio_channels: AtomicU32::new(0),
        #[cfg(any(target_os = "linux", windows))]
        memory: Mutex::new(None),
        params: params.clone(),
    });
    shared.set_size(params.width.max(1), params.height.max(1));

    UI.with(|ui| {
        *ui.borrow_mut() = Some(Ui {
            shared: shared.clone(),
            session,
            browser: None,
            paused: params.paused,
            audio: params.audio,
            closing: false,
            effects: Vec::new(),
            devtools_id: 0,
        });
    });

    let mut app = app::LumenApp::new(app::AppConfig {
        scheme,
        shared: Some(shared.clone()),
    });
    if execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    ) >= 0
    {
        return Err("unexpected subprocess role".into());
    }
    let profile_str = profile.to_string_lossy().into_owned();
    let settings = Settings {
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        multi_threaded_message_loop: 0,
        external_message_pump: 0,
        root_cache_path: CefString::from(profile_str.as_str()),
        cache_path: CefString::from(profile_str.as_str()),
        persist_session_cookies: i32::from(!temporary),
        log_severity: LogSeverity::from(sys::cef_log_severity_t::LOGSEVERITY_ERROR),
        // QO-19: remote debugging only on request, bound to localhost by Chromium.
        remote_debugging_port: params.devtools_port.map_or(0, i32::from),
        background_color: 0xFFFF_FFFF,
        ..Default::default()
    };
    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        return Err("CEF initialisation failed".into());
    }
    CEF_RUNNING.store(true, Ordering::Release);

    // Commands arrive on their own thread; nothing polls.
    let reader = std::thread::Builder::new()
        .name("lumenwall-web-ipc".into())
        .spawn(move || {
            loop {
                match recv_command(&mut receiver) {
                    Ok(Some(HostCommand::FrameReleased { id })) => {
                        let _ = release_tx.send(id);
                    }
                    Ok(Some(command)) => post(UiAction::Command(command)),
                    Ok(None) | Err(_) => {
                        // Lumenwall is gone: close down.
                        post(UiAction::Command(HostCommand::Shutdown));
                        break;
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;

    run_message_loop();
    CEF_RUNNING.store(false, Ordering::Release);
    UI.with(|ui| ui.borrow_mut().take());
    shutdown();
    drop(reader); // Detached: it ends with the channel.
    if temporary {
        let _ = std::fs::remove_dir_all(&profile);
    }
    Ok(())
}
