//! Windows automute: audio sessions of other processes (spec 07 §22), event-driven through
//! `IAudioSessionManager2` / `IAudioSessionNotification` / `IAudioSessionEvents`, and
//! `IMMNotificationClient` to follow the default render device.
//!
//! "Another application plays" ⇔ a session of another process (not the system-sounds
//! session) is media being played ([`classify`]): active, not muted, audible volume, not a
//! call. Calls are recognised by the executable's name and by the system's ducking
//! notifications (`IAudioVolumeDuckNotification`), which name the communications sessions
//! (streams opened on the `eCommunications` endpoint role).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    AudioSessionDisconnectReason, AudioSessionState, AudioSessionStateActive,
    AudioSessionStateExpired, DEVICE_STATE, EDataFlow, ERole, IAudioSessionControl,
    IAudioSessionControl2, IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2,
    IAudioSessionNotification, IAudioSessionNotification_Impl, IAudioVolumeDuckNotification,
    IAudioVolumeDuckNotification_Impl, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, ISimpleAudioVolume, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::core::{BOOL, GUID, Interface, PCWSTR, PWSTR, Ref, Result as WinResult, implement};

use super::{
    ChangeCallback, PlaybackWatcher, PlayingFlag, Stream, Verdict, classify, describe,
    exe_from_session_id,
};

/// A COM pointer moved between MTA threads.
struct Mta<T>(T);
// SAFETY: every thread touching these objects joined the multithreaded apartment, where
// interface pointers may be used from any thread.
unsafe impl<T> Send for Mta<T> {}

enum Msg {
    DefaultChanged,
    SessionCreated(Mta<IAudioSessionControl>),
    Stop,
}

struct Info {
    key: u64,
    pid: u32,
    system: bool,
    /// Executable file name, from the session identifier (`Discord.exe`).
    exe: Option<String>,
    display_name: Option<String>,
    /// Session instance identifier, as ducking notifications name sessions.
    instance: Option<String>,
    state: AudioSessionState,
    volume: f32,
    muted: bool,
}

#[derive(Default)]
struct Sessions {
    list: Vec<Info>,
    /// Instance identifiers of the communications sessions the system reported.
    calls: Vec<String>,
}

/// Session table shared with COM callbacks.
struct Table {
    sessions: Mutex<Sessions>,
    flag: PlayingFlag,
    own_pid: u32,
}

impl Table {
    fn update(&self, key: u64, f: impl FnOnce(&mut Info)) {
        let media = match self.sessions.lock() {
            Ok(mut s) => {
                if let Some(i) = s.list.iter_mut().find(|i| i.key == key) {
                    f(i);
                }
                s.list.retain(|i| i.state != AudioSessionStateExpired);
                self.media(&s)
            }
            Err(_) => return,
        };
        self.flag.set_media(media);
    }

    /// A communications session started (`call`) or ended.
    fn set_call(&self, instance: String, call: bool) {
        let media = match self.sessions.lock() {
            Ok(mut s) => {
                s.calls.retain(|c| *c != instance);
                if call {
                    s.calls.push(instance);
                }
                self.media(&s)
            }
            Err(_) => return,
        };
        self.flag.set_media(media);
    }

    /// The first session that is media being played, described for the logs.
    fn media(&self, s: &Sessions) -> Option<String> {
        s.list.iter().find_map(|i| {
            let call = i
                .instance
                .as_ref()
                .is_some_and(|id| s.calls.iter().any(|c| c == id));
            let stream = Stream {
                ours: i.pid == self.own_pid,
                has_owner: i.pid != 0 && !i.system,
                app_name: i.display_name.as_deref().filter(|n| !n.is_empty()),
                binary: i.exe.as_deref(),
                role: call.then_some("communication"),
                paused: i.state != AudioSessionStateActive,
                muted: i.muted,
                volume: i.volume,
            };
            (classify(&stream) == Verdict::Media).then(|| describe(&stream))
        })
    }

    fn recompute(&self) {
        let media = self.sessions.lock().ok().and_then(|s| self.media(&s));
        self.flag.set_media(media);
    }
}

/// Takes a string the audio service allocated (`CoTaskMemAlloc`) and frees it.
///
/// # Safety
/// `p` is null or a valid string allocated with `CoTaskMemAlloc`, not used afterwards.
unsafe fn take_string(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: valid, NUL-terminated per the caller's contract; freed once just after.
    let text = unsafe { p.to_string() }.ok();
    // SAFETY: allocated by the audio service with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    text
}

#[implement(IAudioVolumeDuckNotification)]
struct DuckEvents {
    table: Arc<Table>,
}

impl DuckEvents_Impl {
    fn session(id: &PCWSTR) -> Option<String> {
        if id.is_null() {
            return None;
        }
        // SAFETY: the system passes a valid NUL-terminated string for the call's duration.
        unsafe { id.to_string() }.ok()
    }
}

impl IAudioVolumeDuckNotification_Impl for DuckEvents_Impl {
    fn OnVolumeDuckNotification(&self, sessionid: &PCWSTR, _count: u32) -> WinResult<()> {
        if let Some(id) = Self::session(sessionid) {
            self.table.set_call(id, true);
        }
        Ok(())
    }
    fn OnVolumeUnduckNotification(&self, sessionid: &PCWSTR) -> WinResult<()> {
        if let Some(id) = Self::session(sessionid) {
            self.table.set_call(id, false);
        }
        Ok(())
    }
}

#[implement(IAudioSessionEvents)]
struct SessionEvents {
    key: u64,
    table: Arc<Table>,
}

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> WinResult<()> {
        Ok(())
    }
    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> WinResult<()> {
        Ok(())
    }
    fn OnSimpleVolumeChanged(&self, volume: f32, mute: BOOL, _: *const GUID) -> WinResult<()> {
        self.table.update(self.key, |i| {
            i.volume = volume;
            i.muted = mute.as_bool();
        });
        Ok(())
    }
    fn OnChannelVolumeChanged(
        &self,
        _: u32,
        _: *const f32,
        _: u32,
        _: *const GUID,
    ) -> WinResult<()> {
        Ok(())
    }
    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> WinResult<()> {
        Ok(())
    }
    fn OnStateChanged(&self, state: AudioSessionState) -> WinResult<()> {
        self.table.update(self.key, |i| i.state = state);
        Ok(())
    }
    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> WinResult<()> {
        self.table
            .update(self.key, |i| i.state = AudioSessionStateExpired);
        Ok(())
    }
}

#[implement(IAudioSessionNotification)]
struct SessionNotifier {
    tx: Mutex<Sender<Msg>>,
}

impl IAudioSessionNotification_Impl for SessionNotifier_Impl {
    fn OnSessionCreated(&self, session: Ref<IAudioSessionControl>) -> WinResult<()> {
        if let Some(s) = session.as_ref()
            && let Ok(tx) = self.tx.lock()
        {
            let _ = tx.send(Msg::SessionCreated(Mta(s.clone())));
        }
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceNotifier {
    tx: Mutex<Sender<Msg>>,
}

impl IMMNotificationClient_Impl for DeviceNotifier_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> WinResult<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> WinResult<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> WinResult<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> WinResult<()> {
        if flow == eRender
            && role == eConsole
            && let Ok(tx) = self.tx.lock()
        {
            let _ = tx.send(Msg::DefaultChanged);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> WinResult<()> {
        Ok(())
    }
}

/// Session watcher thread.
pub struct SessionWatcher {
    flag: PlayingFlag,
    worker: Option<(Sender<Msg>, JoinHandle<()>)>,
}

impl SessionWatcher {
    /// Starts watching; never blocks.
    pub fn new(on_change: Option<ChangeCallback>) -> Self {
        let flag = PlayingFlag::new(on_change);
        let (tx, rx) = channel();
        let table = Arc::new(Table {
            sessions: Mutex::new(Sessions::default()),
            flag: flag.clone(),
            own_pid: std::process::id(),
        });
        let thread_tx = tx.clone();
        let worker = thread::Builder::new()
            .name("lumenwall-automute".into())
            .spawn(move || run(&table, thread_tx, rx))
            .map_err(|e| tracing::error!("automute: cannot spawn thread: {e}"))
            .ok()
            .map(|t| (tx, t));
        Self { flag, worker }
    }
}

impl PlaybackWatcher for SessionWatcher {
    fn other_app_playing(&self) -> bool {
        self.flag.playing()
    }
    fn is_connected(&self) -> bool {
        self.flag.connected()
    }
}

impl Drop for SessionWatcher {
    fn drop(&mut self) {
        if let Some((tx, t)) = self.worker.take() {
            let _ = tx.send(Msg::Stop);
            let _ = t.join();
        }
    }
}

/// Sessions of the current default device and their registered callbacks.
struct Attached {
    manager: IAudioSessionManager2,
    notifier: IAudioSessionNotification,
    /// Ducking notifications, when the system accepted the registration.
    duck: Option<IAudioVolumeDuckNotification>,
    sessions: Vec<(IAudioSessionControl, IAudioSessionEvents)>,
    next_key: u64,
}

impl Attached {
    fn detach(self, table: &Table) {
        // SAFETY: COM calls on objects owned by this MTA thread.
        unsafe {
            let _ = self.manager.UnregisterSessionNotification(&self.notifier);
            if let Some(duck) = &self.duck {
                let _ = self.manager.UnregisterDuckNotification(duck);
            }
            for (control, events) in &self.sessions {
                let _ = control.UnregisterAudioSessionNotification(events);
            }
        }
        if let Ok(mut s) = table.sessions.lock() {
            // Calls in progress stay known: instance identifiers are system-wide.
            s.list.clear();
        }
        table.recompute();
    }

    fn track(&mut self, control: IAudioSessionControl, table: &Arc<Table>) {
        let key = self.next_key;
        self.next_key += 1;
        // SAFETY: COM calls on valid interface pointers within the MTA.
        let info = unsafe {
            let c2: Option<IAudioSessionControl2> = control.cast().ok();
            let volume: Option<ISimpleAudioVolume> = control.cast().ok();
            let identifier = c2
                .as_ref()
                .and_then(|c| c.GetSessionIdentifier().ok())
                .and_then(|p| take_string(p));
            Info {
                key,
                pid: c2.as_ref().and_then(|c| c.GetProcessId().ok()).unwrap_or(0),
                exe: identifier
                    .as_deref()
                    .and_then(exe_from_session_id)
                    .map(str::to_owned),
                display_name: control.GetDisplayName().ok().and_then(|p| take_string(p)),
                instance: c2
                    .as_ref()
                    .and_then(|c| c.GetSessionInstanceIdentifier().ok())
                    .and_then(|p| take_string(p)),
                // S_OK means "is the system sounds session", S_FALSE means not.
                system: c2
                    .as_ref()
                    .is_some_and(|c| c.IsSystemSoundsSession().0 == 0),
                state: control.GetState().unwrap_or(AudioSessionStateExpired),
                volume: volume
                    .as_ref()
                    .and_then(|v| v.GetMasterVolume().ok())
                    .unwrap_or(1.0),
                muted: volume
                    .as_ref()
                    .and_then(|v| v.GetMute().ok())
                    .is_some_and(|m| m.as_bool()),
            }
        };
        if info.state == AudioSessionStateExpired {
            return;
        }
        let events: IAudioSessionEvents = SessionEvents {
            key,
            table: table.clone(),
        }
        .into();
        // SAFETY: as above.
        if unsafe { control.RegisterAudioSessionNotification(&events) }.is_err() {
            return;
        }
        if let Ok(mut s) = table.sessions.lock() {
            s.list.push(info);
        }
        self.sessions.push((control, events));
        table.recompute();
    }
}

fn attach(
    enumerator: &IMMDeviceEnumerator,
    table: &Arc<Table>,
    tx: &Sender<Msg>,
) -> WinResult<Attached> {
    // SAFETY: COM calls within the MTA on valid pointers.
    unsafe {
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let notifier: IAudioSessionNotification = SessionNotifier {
            tx: Mutex::new(tx.clone()),
        }
        .into();
        // The session enumerator must be obtained for notifications to be delivered.
        let list = manager.GetSessionEnumerator()?;
        manager.RegisterSessionNotification(&notifier)?;
        // NULL session: every ducking notification, without changing our own ducking.
        let duck: IAudioVolumeDuckNotification = DuckEvents {
            table: table.clone(),
        }
        .into();
        let duck = manager
            .RegisterDuckNotification(PCWSTR::null(), &duck)
            .is_ok()
            .then_some(duck);
        let mut attached = Attached {
            manager,
            notifier,
            duck,
            sessions: Vec::new(),
            next_key: 0,
        };
        for i in 0..list.GetCount()? {
            if let Ok(control) = list.GetSession(i) {
                attached.track(control, table);
            }
        }
        Ok(attached)
    }
}

fn run(table: &Arc<Table>, tx: Sender<Msg>, rx: Receiver<Msg>) {
    // SAFETY: initialises COM for this thread; balanced by CoUninitialize below.
    if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
        tracing::warn!("automute: COM initialisation failed");
        return;
    }
    {
        // SAFETY: COM is initialised on this thread.
        let enumerator: WinResult<IMMDeviceEnumerator> =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) };
        if let Ok(enumerator) = enumerator {
            let device_notifier: IMMNotificationClient = DeviceNotifier {
                tx: Mutex::new(tx.clone()),
            }
            .into();
            // SAFETY: valid enumerator and callback.
            let registered =
                unsafe { enumerator.RegisterEndpointNotificationCallback(&device_notifier) }
                    .is_ok();
            let mut attached = attach(&enumerator, table, &tx).ok();
            table.flag.set_connected(attached.is_some());
            // Blocks between events: no polling.
            while let Ok(msg) = rx.recv() {
                match msg {
                    Msg::Stop => break,
                    Msg::DefaultChanged => {
                        if let Some(a) = attached.take() {
                            a.detach(table);
                        }
                        attached = attach(&enumerator, table, &tx).ok();
                        table.flag.set_connected(attached.is_some());
                    }
                    Msg::SessionCreated(Mta(control)) => {
                        if let Some(a) = attached.as_mut() {
                            a.track(control, table);
                        }
                    }
                }
            }
            if let Some(a) = attached.take() {
                a.detach(table);
            }
            if registered {
                // SAFETY: unregisters the callback registered above.
                let _ =
                    unsafe { enumerator.UnregisterEndpointNotificationCallback(&device_notifier) };
            }
        } else {
            tracing::warn!("automute: no audio device enumerator");
        }
        table.flag.set_connected(false);
    }
    // SAFETY: balances CoInitializeEx.
    unsafe { CoUninitialize() };
}
