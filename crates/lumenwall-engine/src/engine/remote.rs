//! The daemon's side of the engine process (Windows).
//!
//! The daemon never opens a GPU device: once a Vulkan driver is loaded into a process it stays
//! loaded for the life of that process (NVIDIA, measured: `nvoglv64.dll`, 6 driver threads and
//! ≥ 30 MiB private after the device and instance are destroyed). The engine runs instead in a
//! child, `lumenwall --lumenwall-engine-host` ([`super::host`]):
//!
//! - started when a plan brings a wallpaper while none runs, with that plan;
//! - kept between two wallpapers: a new plan is applied in place by the same process, its GPU
//!   device included;
//! - ended when the last wallpaper goes (`clear`), so the empty daemon holds no driver at all;
//! - tied to the daemon (job object): it dies with it, however the daemon ends; the end of its
//!   stdin also stops it;
//! - restarted once with the current plan when it dies unexpectedly; a second death within
//!   [`CRASH_WINDOW`] leaves the targets in error, reported to clients, until the next plan.
//!
//! The monitor shows the child's reports (outputs seen by earlier processes stay listed, not
//! shown) and its frame counters on request; its log records go through the daemon's logger.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write as _};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::wire::{FromHost, Init, ToHost};
use super::{EngineWaker, host, logfwd};
use crate::monitor::{
    Change, CounterSource, EngineMonitor, EngineReport, FrameCounters, OutputReport, TargetReport,
    TargetState,
};
use crate::plan::{EngineControl, EnginePlan, Stopped};
use crate::vulkan_driver;

/// A second unexpected end of the engine process within this window is not restarted.
const CRASH_WINDOW: Duration = Duration::from_secs(30);
/// How long a stop request may take before the process is killed.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a client waits for the counters of the engine process.
const COUNTERS_TIMEOUT: Duration = Duration::from_secs(1);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The writing half to the engine process, shared with the monitor (counters).
struct Link {
    stdin: Mutex<Option<ChildStdin>>,
    counters: Mutex<HashMap<u64, FrameCounters>>,
    answered: Condvar,
    next_counter: AtomicU64,
}

impl Link {
    fn send(&self, msg: &ToHost) -> bool {
        let line = msg.encode();
        let mut stdin = lock(&self.stdin);
        let Some(pipe) = stdin.as_mut() else {
            return false;
        };
        pipe.write_all(line.as_bytes())
            .and_then(|()| pipe.flush())
            .is_ok()
    }

    /// Closes stdin: the engine process stops when it reads the end.
    fn close(&self) {
        lock(&self.stdin).take();
    }
}

impl CounterSource for Link {
    fn set_sampling(&self, on: bool) {
        self.send(&ToHost::Sampling(on));
    }

    fn take_counters(&self) -> Option<FrameCounters> {
        let id = self.next_counter.fetch_add(1, Ordering::Relaxed);
        if !self.send(&ToHost::Counters(id)) {
            return None;
        }
        let deadline = Instant::now() + COUNTERS_TIMEOUT;
        let mut answers = lock(&self.counters);
        loop {
            if let Some(c) = answers.remove(&id) {
                return Some(c);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            answers = self
                .answered
                .wait_timeout(answers, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// A running engine process.
struct Child {
    process: std::process::Child,
    link: Arc<Link>,
    /// Messages for the engine thread; disconnected once the process closed its stdout.
    inbox: mpsc::Receiver<FromHost>,
    reader: Option<JoinHandle<()>>,
    _tie: Option<lumenwall_platform::process::Tie>,
}

/// The engine of a daemon whose wallpapers run in an engine process.
pub(crate) struct Remote {
    plan: EnginePlan,
    user_paused: bool,
    waker: EngineWaker,
    monitor: EngineMonitor,
    child: Option<Child>,
    /// Outputs reported by every engine process so far (a new process only reports what it
    /// opens): kept listed, not shown, so clients can put a wallpaper back on them.
    known: Vec<OutputReport>,
    /// Targets of the last report, for the error report after a crash.
    last_targets: Vec<TargetReport>,
    last_crash: Option<Instant>,
    /// The engine process ended its loop by itself (window closed, session end) or failed.
    stopped: Option<Result<Stopped, String>>,
    next_reload: u64,
}

impl Remote {
    pub(crate) fn new(waker: EngineWaker, monitor: EngineMonitor) -> Self {
        Self {
            plan: EnginePlan::default(),
            user_paused: false,
            waker,
            monitor,
            child: None,
            known: Vec::new(),
            last_targets: Vec::new(),
            last_crash: None,
            stopped: None,
            next_reload: 0,
        }
    }

    /// Keeps the plan; the engine process reads the wallpapers when it starts.
    pub(crate) fn load(&mut self, plan: &EnginePlan) -> Result<(), String> {
        self.plan = plan.clone();
        Ok(())
    }

    pub(crate) fn start(&mut self) -> Result<(), String> {
        if self.plan.targets.is_empty() {
            tracing::info!("no wallpaper to show: waiting for one (IPC)");
            self.publish_idle(Change::State);
            return Ok(());
        }
        let plan = self.plan.clone();
        self.spawn(&plan)
    }

    pub(crate) fn run(
        &mut self,
        control: &mut dyn FnMut() -> Option<EngineControl>,
    ) -> Result<Stopped, String> {
        loop {
            if let Some(stopped) = self.stopped.take() {
                self.end_child();
                return stopped;
            }
            while let Some(msg) = control() {
                match msg {
                    EngineControl::Stop(sig) => {
                        self.stop_child(sig);
                        return Ok(sig.map_or(Stopped::Requested, Stopped::Signal));
                    }
                    EngineControl::SetFps(fps) => {
                        self.plan.fps = fps.max(1);
                        self.send(&ToHost::Fps(self.plan.fps));
                    }
                    EngineControl::SetProperties(props) => {
                        self.send(&ToHost::Properties(props.clone()));
                        self.plan.properties = props;
                    }
                    EngineControl::SetPaused(paused) => {
                        self.user_paused = paused;
                        if !self.send(&ToHost::Paused(paused)) {
                            self.publish_idle(Change::State);
                        }
                    }
                    EngineControl::Reload { plan, done } => {
                        let result = self.reload(*plan);
                        if let Err(e) = &result {
                            tracing::error!("new settings not applied: {e}");
                        }
                        if let Some(done) = done {
                            let _ = done.try_send(result);
                        }
                    }
                }
            }
            self.drain();
            if self.stopped.is_none() {
                self.waker.idle_wait();
            }
        }
    }

    fn send(&self, msg: &ToHost) -> bool {
        self.child.as_ref().is_some_and(|c| c.link.send(msg))
    }

    fn reload(&mut self, plan: EnginePlan) -> Result<(), String> {
        if plan.targets.is_empty() {
            let had = self.child.is_some();
            self.stop_child(None);
            self.plan = plan;
            self.last_targets.clear();
            self.publish_idle(Change::State);
            if had {
                tracing::info!("no wallpaper left: engine process ended");
                // What the reports, plans and pipes of that process used goes back too.
                super::trim_heap();
                if let Some(t) = lumenwall_platform::process::trim_working_set() {
                    tracing::debug!("working set emptied in {:.1} ms", t.as_secs_f64() * 1e3);
                }
            }
            return Ok(());
        }
        if self.child.is_none() {
            self.spawn(&plan)?;
            self.plan = plan;
            return Ok(());
        }
        let id = self.next_reload;
        self.next_reload += 1;
        let sent = self.send(&ToHost::Reload {
            id,
            plan: Box::new(plan.clone()),
        });
        let answer = if sent {
            self.wait_for(
                |m| matches!(m, FromHost::Done { id: i, .. } if *i == id),
                None,
            )
        } else {
            None
        };
        match answer {
            Some(FromHost::Done { result, .. }) => {
                if result.is_ok() {
                    self.plan = plan;
                }
                result
            }
            _ => {
                // Ended by itself (its window closed): the loop stops with it. Otherwise a crash.
                if self.stopped.is_none() {
                    self.crashed();
                }
                Err("the engine process stopped while applying the new settings".into())
            }
        }
    }

    /// Starts an engine process on `plan` and waits until it shows it (or failed to).
    fn spawn(&mut self, plan: &EnginePlan) -> Result<(), String> {
        let t0 = Instant::now();
        let exe = std::env::current_exe().map_err(|e| format!("engine process: {e}"))?;
        let mut cmd = Command::new(exe);
        cmd.arg(host::HOST_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // The process restricts the Vulkan loader itself, from the remembered driver.
        for var in vulkan_driver::own_variables() {
            cmd.env_remove(var);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut process = cmd
            .spawn()
            .map_err(|e| format!("cannot start the engine process: {e}"))?;
        let tie = match lumenwall_platform::process::Tie::new(&process) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!("engine process not tied to the daemon: {e}");
                None
            }
        };
        let stdout = process.stdout.take().expect("piped");
        let link = Arc::new(Link {
            stdin: Mutex::new(process.stdin.take()),
            counters: Mutex::new(HashMap::new()),
            answered: Condvar::new(),
            next_counter: AtomicU64::new(0),
        });
        let (tx, inbox) = mpsc::channel();
        let reader = {
            let link = link.clone();
            let waker = self.waker.clone();
            std::thread::Builder::new()
                .name("lumenwall-engine-link".into())
                .spawn(move || read_engine(stdout, &link, &tx, &waker))
        };
        let reader = match reader {
            Ok(r) => r,
            Err(e) => {
                let _ = process.kill();
                let _ = process.wait();
                return Err(format!("engine process: {e}"));
            }
        };
        let pid = process.id();
        self.child = Some(Child {
            process,
            link: link.clone(),
            inbox,
            reader: Some(reader),
            _tie: tie,
        });
        link.send(&ToHost::Init(Box::new(Init {
            plan: plan.clone(),
            user_paused: self.user_paused,
            sampling: self.monitor.sampling(),
            log_level: logfwd::current_level(),
        })));
        self.monitor.set_counter_source(Some(link));
        match self.wait_for(|m| matches!(m, FromHost::Started(_)), None) {
            Some(FromHost::Started(Ok(()))) => {
                tracing::info!(
                    "engine process {pid} showing the wallpapers {:.0} ms after its start",
                    t0.elapsed().as_secs_f64() * 1000.0
                );
                Ok(())
            }
            Some(FromHost::Started(Err(e))) => {
                self.end_child();
                Err(e)
            }
            _ => {
                let status = self.end_child();
                Err(format!(
                    "the engine process ended during start-up ({status})"
                ))
            }
        }
    }

    /// Handles the process's messages until one matches `want` (returned), the process ends
    /// (`None`) or the deadline passes (`None`).
    fn wait_for(
        &mut self,
        want: impl Fn(&FromHost) -> bool,
        deadline: Option<Instant>,
    ) -> Option<FromHost> {
        loop {
            let child = self.child.as_ref()?;
            let msg = match deadline {
                None => child.inbox.recv().ok()?,
                Some(d) => child
                    .inbox
                    .recv_timeout(d.saturating_duration_since(Instant::now()))
                    .ok()?,
            };
            if want(&msg) {
                return Some(msg);
            }
            self.handle(msg);
        }
    }

    /// Messages that arrived while the loop slept; notices the end of the process.
    fn drain(&mut self) {
        loop {
            let Some(child) = &self.child else {
                return;
            };
            match child.inbox.try_recv() {
                Ok(msg) => self.handle(msg),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if self.stopped.is_none() {
                        self.crashed();
                    }
                    return;
                }
            }
        }
    }

    fn handle(&mut self, msg: FromHost) {
        match msg {
            FromHost::Report { change, report } => self.on_report(change, report),
            FromHost::Stopped(s) => self.stopped = Some(Ok(s)),
            FromHost::Failed(e) => self.stopped = Some(Err(e)),
            // Late answers (the waiter gave up) and messages handled by the reader thread.
            FromHost::Started(_)
            | FromHost::Done { .. }
            | FromHost::Counters { .. }
            | FromHost::Log { .. } => {}
        }
    }

    fn on_report(&mut self, change: Change, mut report: EngineReport) {
        for o in &report.outputs {
            match self.known.iter_mut().find(|k| k.name == o.name) {
                Some(k) => *k = o.clone(),
                None => self.known.push(o.clone()),
            }
        }
        self.known.sort_by(|a, b| a.name.cmp(&b.name));
        self.last_targets = report.targets.clone();
        for k in &mut self.known {
            if !report.outputs.iter().any(|o| o.name == k.name) {
                // Shown by an earlier process only: no target of this one.
                *k = not_shown(k);
                report.outputs.push(k.clone());
            }
        }
        report.outputs.sort_by(|a, b| a.name.cmp(&b.name));
        self.monitor.publish(report, change);
    }

    /// The report while no engine process runs. After a crash (`after_crash`) each output
    /// keeps the target it showed, so clients see that target's error on it.
    fn publish_idle(&self, change: Change) {
        self.publish_idle_with(change, false);
    }

    fn publish_idle_with(&self, change: Change, after_crash: bool) {
        let outputs = self
            .known
            .iter()
            .map(|o| OutputReport {
                target: if after_crash { o.target } else { None },
                ..not_shown(o)
            })
            .collect();
        self.monitor.publish(
            EngineReport {
                active: false,
                user_paused: self.user_paused,
                outputs,
                targets: self.last_targets.clone(),
            },
            change,
        );
    }

    /// The process ended without being asked to: restart it once, or report the targets in
    /// error until the next plan.
    fn crashed(&mut self) {
        let status = self.end_child();
        tracing::error!("the engine process stopped unexpectedly ({status})");
        let now = Instant::now();
        let again = !restart_allowed(self.last_crash, now);
        self.last_crash = Some(now);
        if !again && !self.plan.targets.is_empty() {
            tracing::warn!("restarting the engine process");
            let plan = self.plan.clone();
            match self.spawn(&plan) {
                Ok(()) => return,
                Err(e) => tracing::error!("engine process not restarted: {e}"),
            }
        } else if again {
            tracing::error!(
                "the engine process stopped twice within {} s: not restarted until new settings arrive",
                CRASH_WINDOW.as_secs()
            );
        }
        let message = format!(
            "the engine process stopped unexpectedly ({status}); apply a wallpaper to try again"
        );
        for t in &mut self.last_targets {
            t.state = TargetState::Error(message.clone());
        }
        self.publish_idle_with(Change::State, true);
    }

    /// Asks the process to stop and waits for it (killed after [`STOP_TIMEOUT`]).
    fn stop_child(&mut self, sig: Option<i32>) {
        if self.child.is_none() {
            return;
        }
        if self.send(&ToHost::Stop(sig)) {
            let deadline = Instant::now() + STOP_TIMEOUT;
            let _ = self.wait_for(|m| matches!(m, FromHost::Stopped(_)), Some(deadline));
        }
        self.end_child();
    }

    /// Closes the pipes, waits for the process to end (killing it after [`STOP_TIMEOUT`]) and
    /// returns how it ended.
    fn end_child(&mut self) -> String {
        let Some(mut child) = self.child.take() else {
            return "not running".into();
        };
        self.monitor.set_counter_source(None);
        child.link.close();
        let deadline = Instant::now() + STOP_TIMEOUT;
        let status = loop {
            match child.process.try_wait() {
                Ok(Some(status)) => break status.to_string(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.process.kill();
                    break match child.process.wait() {
                        Ok(s) => format!("killed after {} s: {s}", STOP_TIMEOUT.as_secs()),
                        Err(e) => format!("killed: {e}"),
                    };
                }
                Err(e) => break e.to_string(),
            }
        };
        if let Some(reader) = child.reader.take() {
            let _ = reader.join();
        }
        status
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.end_child();
    }
}

/// Whether an engine process that ended unexpectedly at `now` is started again: not when the
/// previous one ended less than [`CRASH_WINDOW`] before (a wallpaper that crashes the engine
/// at once would otherwise restart it forever). Pure, for testing.
fn restart_allowed(last_crash: Option<Instant>, now: Instant) -> bool {
    last_crash.is_none_or(|t| now.saturating_duration_since(t) >= CRASH_WINDOW)
}

fn not_shown(o: &OutputReport) -> OutputReport {
    OutputReport {
        shown: false,
        target: None,
        paused: None,
        ..o.clone()
    }
}

/// Reader thread: log records and counters are handled here, the rest goes to the engine
/// thread, which is woken.
fn read_engine(stdout: ChildStdout, link: &Link, tx: &mpsc::Sender<FromHost>, waker: &EngineWaker) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            break;
        };
        match FromHost::decode(&line) {
            Ok(FromHost::Log {
                level,
                target,
                message,
            }) => logfwd::emit(level, &target, &message),
            Ok(FromHost::Counters { id, counters }) => {
                lock(&link.counters).insert(id, counters);
                link.answered.notify_all();
            }
            Ok(msg) => {
                if tx.send(msg).is_err() {
                    break;
                }
                waker.wake();
            }
            Err(e) => {
                let shown: String = line.chars().take(200).collect();
                tracing::warn!("unreadable message from the engine process ({e}): {shown}");
            }
        }
    }
    waker.wake();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::PauseCause;
    use lumenwall_render::ScalingMode;

    #[test]
    fn one_restart_per_crash_window() {
        let t0 = Instant::now();
        assert!(restart_allowed(None, t0));
        assert!(!restart_allowed(Some(t0), t0 + Duration::from_secs(5)));
        assert!(restart_allowed(Some(t0), t0 + CRASH_WINDOW));
        // A clock that went backwards never allows a loop either.
        assert!(!restart_allowed(Some(t0 + Duration::from_secs(1)), t0));
    }

    fn output(name: &str, shown: bool) -> OutputReport {
        OutputReport {
            name: name.into(),
            description: None,
            position: (0, 0),
            size: (2560, 1440),
            scale: 1.0,
            refresh_mhz: None,
            shown,
            target: shown.then_some(0),
            paused: shown.then_some(PauseCause::Hidden),
        }
    }

    /// Outputs of an earlier engine process stay listed (not shown) in the next one's reports
    /// and while none runs; targets of the last report are kept for the crash report.
    #[test]
    fn reports_keep_the_outputs_of_earlier_processes() {
        let monitor = EngineMonitor::default();
        let mut remote = Remote::new(EngineWaker::default(), monitor.clone());
        let target = TargetReport {
            key: "DISPLAY2".into(),
            outputs: vec!["DISPLAY2".into()],
            wallpaper: "/w/1".into(),
            title: "t".into(),
            scaling: ScalingMode::Fill,
            state: TargetState::Running,
            properties: vec![],
        };
        remote.on_report(
            Change::Outputs,
            EngineReport {
                active: true,
                user_paused: false,
                outputs: vec![output("DISPLAY1", true), output("DISPLAY2", true)],
                targets: vec![target.clone()],
            },
        );
        remote.on_report(
            Change::Outputs,
            EngineReport {
                active: true,
                user_paused: false,
                outputs: vec![output("DISPLAY2", true)],
                targets: vec![target.clone()],
            },
        );
        let r = monitor.report();
        assert_eq!(r.outputs.len(), 2);
        assert_eq!(r.outputs[0], output("DISPLAY1", false));
        assert!(r.outputs[1].shown);
        remote.user_paused = true;
        remote.publish_idle(Change::State);
        let r = monitor.report();
        assert!(!r.active && r.user_paused);
        assert!(r.outputs.iter().all(|o| !o.shown && o.target.is_none()));
        assert_eq!(r.targets, vec![target]);
        // After a crash the outputs keep their target, so its error reaches the clients.
        remote.publish_idle_with(Change::State, true);
        let r = monitor.report();
        assert!(r.outputs.iter().all(|o| !o.shown && o.paused.is_none()));
        assert_eq!(r.outputs[1].target, Some(0));
        assert_eq!(r.outputs[0].target, None);
    }
}
