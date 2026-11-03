//! Windows automute: audio sessions of other processes (spec 07 §22), event-driven through
//! `IAudioSessionManager2` / `IAudioSessionNotification` / `IAudioSessionEvents`, and
//! `IMMNotificationClient` to follow the default render device.
//!
//! "Another application plays" ⇔ a session of another process (not the system-sounds
//! session) is active, not muted, with a non-zero volume.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    AudioSessionDisconnectReason, AudioSessionState, AudioSessionStateActive,
    AudioSessionStateExpired, DEVICE_STATE, EDataFlow, ERole, IAudioSessionControl,
    IAudioSessionControl2, IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2,
    IAudioSessionNotification, IAudioSessionNotification_Impl, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, ISimpleAudioVolume, MMDeviceEnumerator,
    eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::{BOOL, GUID, Interface, PCWSTR, Ref, Result as WinResult, implement};

use super::{ChangeCallback, PlaybackWatcher, PlayingFlag};

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

#[derive(Clone, Copy)]
struct Info {
    key: u64,
    pid: u32,
    system: bool,
    state: AudioSessionState,
    volume: f32,
    muted: bool,
}

/// Session table shared with COM callbacks.
struct Table {
    sessions: Mutex<Vec<Info>>,
    flag: PlayingFlag,
    own_pid: u32,
}

impl Table {
    fn update(&self, key: u64, f: impl FnOnce(&mut Info)) {
        let playing = match self.sessions.lock() {
            Ok(mut s) => {
                if let Some(i) = s.iter_mut().find(|i| i.key == key) {
                    f(i);
                }
                s.retain(|i| i.state != AudioSessionStateExpired);
                self.playing(&s)
            }
            Err(_) => return,
        };
        self.flag.set_playing(playing);
    }

    fn playing(&self, sessions: &[Info]) -> bool {
        sessions.iter().any(|i| {
            !i.system
                && i.pid != 0
                && i.pid != self.own_pid
                && i.state == AudioSessionStateActive
                && !i.muted
                && i.volume > 0.0
        })
    }

    fn recompute(&self) {
        let playing = self
            .sessions
            .lock()
            .map(|s| self.playing(&s))
            .unwrap_or(false);
        self.flag.set_playing(playing);
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
            sessions: Mutex::new(Vec::new()),
            flag: flag.clone(),
            own_pid: std::process::id(),
        });
        let thread_tx = tx.clone();
        let worker = thread::Builder::new()
            .name("lumen-automute".into())
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
    sessions: Vec<(IAudioSessionControl, IAudioSessionEvents)>,
    next_key: u64,
}

impl Attached {
    fn detach(self, table: &Table) {
        // SAFETY: COM calls on objects owned by this MTA thread.
        unsafe {
            let _ = self.manager.UnregisterSessionNotification(&self.notifier);
            for (control, events) in &self.sessions {
                let _ = control.UnregisterAudioSessionNotification(events);
            }
        }
        if let Ok(mut s) = table.sessions.lock() {
            s.clear();
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
            Info {
                key,
                pid: c2.as_ref().and_then(|c| c.GetProcessId().ok()).unwrap_or(0),
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
            s.push(info);
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
        let mut attached = Attached {
            manager,
            notifier,
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
