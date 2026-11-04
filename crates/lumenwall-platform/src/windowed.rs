//! Windowed mode (`-w`, or no output selected): a plain winit window for debugging (spec 07 §7).
//!
//! One output named `default`. Normal mode: resizable 640×480 window. Explicit geometry: undecorated,
//! fixed-size, always-on-top window at the given position. Driven with `pump_app_events`, so `wait` blocks
//! in winit exactly like the desktop backends block in their own loops. winit allows one event loop per
//! process: a closed backend leaves its loop to the next one of the thread (the engine reopens its surfaces
//! when a wallpaper comes back after `clear`).
//!
//! Deviations from the reference window: no fullscreen detector (debug mode only), the pointer is only
//! tracked while it is over the window (winit does not report positions outside it).

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Instant;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

use crate::sched::{FrameSlot, min_deadline, pause_transition};
use crate::types::EventQueue;
use crate::{
    BackendKind, Config, Event, OutputId, OutputInfo, PauseReasons, PlatformError, PointerButton,
    PointerEvent, PointerKind, PointerMode, Wait, Waker, WallpaperSurface, WindowGeometry,
};

const OUTPUT: OutputId = OutputId(0);

struct App {
    attributes: Option<WindowAttributes>,
    window: Option<Window>,
    infos: Vec<OutputInfo>,
    slot: FrameSlot,
    paused: PauseReasons,
    queue: EventQueue,
    pointer: PointerMode,
    last_pointer: (f64, f64),
    create_error: Option<String>,
}

impl App {
    fn info(&mut self) -> &mut OutputInfo {
        &mut self.infos[0]
    }

    fn set_paused(&mut self, reason: PauseReasons, on: bool) {
        let mut new = self.paused;
        new.set(reason, on);
        if let Some(ev) = pause_transition(OUTPUT, self.paused, new) {
            self.queue.push(ev);
        }
        self.paused = new;
    }

    fn update_size(&mut self, size: PhysicalSize<u32>, scale: f64) {
        let logical = size.to_logical::<u32>(scale);
        let physical = (size.width.max(1), size.height.max(1));
        let logical = (logical.width.max(1), logical.height.max(1));
        // A minimised window reports a zero size.
        self.set_paused(PauseReasons::OCCLUDED, size.width == 0 || size.height == 0);
        let info = self.info();
        if info.physical_size != physical {
            info.physical_size = physical;
            info.logical_size = logical;
            self.queue.push(Event::Resized {
                output: OUTPUT,
                physical_size: physical,
                logical_size: logical,
            });
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.create_window(event_loop);
    }

    /// A reused event loop (see [`SPARE_LOOP`]) never sends `resumed` again: the window of a
    /// later backend is created on its first loop iteration.
    fn new_events(&mut self, event_loop: &ActiveEventLoop, _: StartCause) {
        self.create_window(event_loop);
    }

    fn user_event(&mut self, _: &ActiveEventLoop, (): ()) {
        self.queue.push(Event::Wake);
    }

    fn window_event(&mut self, _: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Late events of the window of a closed backend (the loop outlives it).
        if self.window.as_ref().map(Window::id) != Some(id) {
            return;
        }
        self.handle_window_event(event);
    }
}

impl App {
    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        let Some(attributes) = self.attributes.take() else {
            return;
        };
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let size = window.inner_size();
                let scale = window.scale_factor();
                let logical = size.to_logical::<u32>(scale);
                let position = window
                    .outer_position()
                    .map(|p| (p.x, p.y))
                    .unwrap_or((0, 0));
                let refresh = window
                    .current_monitor()
                    .and_then(|m| m.refresh_rate_millihertz());
                self.infos.push(OutputInfo {
                    id: OUTPUT,
                    name: "default".to_owned(),
                    description: None,
                    position,
                    logical_size: (logical.width.max(1), logical.height.max(1)),
                    physical_size: (size.width.max(1), size.height.max(1)),
                    scale,
                    refresh_mhz: refresh,
                });
                self.window = Some(window);
                self.slot.requested = true;
                self.queue.push(Event::OutputAdded(OUTPUT));
            }
            Err(e) => self.create_error = Some(e.to_string()),
        }
    }

    fn handle_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                self.queue.push(Event::CloseRequested)
            }
            WindowEvent::Resized(size) => {
                let scale = self.window.as_ref().map_or(1.0, Window::scale_factor);
                self.update_size(size, scale);
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if (self.info().scale - scale_factor).abs() > f64::EPSILON {
                    self.info().scale = scale_factor;
                    self.queue.push(Event::ScaleChanged {
                        output: OUTPUT,
                        scale: scale_factor,
                    });
                }
            }
            WindowEvent::Moved(PhysicalPosition { x, y }) => {
                self.info().position = (x, y);
                self.queue.push(Event::Moved {
                    output: OUTPUT,
                    position: (x, y),
                });
            }
            WindowEvent::Occluded(occluded) => self.set_paused(PauseReasons::OCCLUDED, occluded),
            // Expose / damage from the OS.
            WindowEvent::RedrawRequested => self.slot.requested = true,
            _ if self.pointer == PointerMode::Disabled => {}
            WindowEvent::CursorEntered { .. } => self.push_pointer(0.0, 0.0, PointerKind::Enter),
            WindowEvent::CursorLeft { .. } => self.push_pointer(0.0, 0.0, PointerKind::Leave),
            WindowEvent::CursorMoved { position, .. } => {
                self.push_pointer(position.x, position.y, PointerKind::Motion)
            }
            WindowEvent::MouseInput { state, button, .. }
                if self.pointer == PointerMode::MotionAndButtons =>
            {
                let button = match button {
                    MouseButton::Left => PointerButton::Left,
                    MouseButton::Right => PointerButton::Right,
                    MouseButton::Middle => PointerButton::Middle,
                    MouseButton::Back => PointerButton::Other(3),
                    MouseButton::Forward => PointerButton::Other(4),
                    MouseButton::Other(n) => PointerButton::Other(n.into()),
                };
                let pressed = state == ElementState::Pressed;
                self.push_pointer(f64::NAN, f64::NAN, PointerKind::Button { button, pressed });
            }
            _ => {}
        }
    }

    fn push_pointer(&mut self, x: f64, y: f64, kind: PointerKind) {
        // Buttons carry the last known position.
        let (x, y) = if x.is_nan() {
            self.last_pointer
        } else {
            (x, y)
        };
        if matches!(kind, PointerKind::Motion) {
            self.last_pointer = (x, y);
        }
        self.queue.push(Event::Pointer(PointerEvent {
            output: OUTPUT,
            x,
            y,
            kind,
        }));
    }
}

thread_local! {
    /// winit allows one event loop per process, and the engine closes its surfaces when its
    /// last wallpaper goes, then opens new ones for the next: a closed backend leaves its loop
    /// here for the next backend of the thread.
    static SPARE_LOOP: RefCell<Option<EventLoop<()>>> = const { RefCell::new(None) };
}

/// The windowed backend.
pub(crate) struct WindowedBackend {
    /// Always `Some` until `drop` hands it to [`SPARE_LOOP`].
    event_loop: Option<EventLoop<()>>,
    app: App,
    display: RawDisplayHandle,
    waker: Waker,
    closed: bool,
}

impl WindowedBackend {
    pub(crate) fn new(
        config: &Config,
        geometry: Option<WindowGeometry>,
    ) -> Result<Self, PlatformError> {
        let event_loop = match SPARE_LOOP.with(|s| s.borrow_mut().take()) {
            Some(l) => l,
            None => EventLoop::<()>::with_user_event()
                .build()
                .map_err(|e| PlatformError::Connect(e.to_string()))?,
        };
        let display = event_loop
            .display_handle()
            .map_err(|e| PlatformError::Connect(e.to_string()))?
            .as_raw();
        let proxy = Mutex::new(event_loop.create_proxy());
        let waker = Waker::new(move || {
            if let Ok(p) = proxy.lock() {
                let _ = p.send_event(());
            }
        });

        let mut attributes = Window::default_attributes().with_title(config.identity.clone());
        attributes = match geometry {
            // parity: explicit geometry = borderless, fixed, always on top, placed and sized from `-w`.
            Some(g) => attributes
                .with_inner_size(PhysicalSize::new(g.width.max(1), g.height.max(1)))
                .with_position(PhysicalPosition::new(g.x, g.y))
                .with_decorations(false)
                .with_resizable(false)
                .with_window_level(WindowLevel::AlwaysOnTop),
            None => attributes.with_inner_size(PhysicalSize::new(640, 480)),
        };
        #[cfg(all(unix, not(target_vendor = "apple")))]
        {
            attributes = winit::platform::x11::WindowAttributesExtX11::with_name(
                attributes,
                config.identity.clone(),
                config.identity.clone(),
            );
            attributes = winit::platform::wayland::WindowAttributesExtWayland::with_name(
                attributes,
                config.identity.clone(),
                config.identity.clone(),
            );
        }

        let mut backend = Self {
            event_loop: Some(event_loop),
            app: App {
                attributes: Some(attributes),
                window: None,
                infos: Vec::with_capacity(1),
                slot: FrameSlot::new(config.max_fps),
                paused: PauseReasons::NONE,
                queue: EventQueue::new(),
                pointer: config.pointer,
                create_error: None,
                last_pointer: (0.0, 0.0),
            },
            display,
            waker,
            closed: false,
        };
        // The window can only be created from inside the loop (`resumed`, `new_events`).
        for _ in 0..100 {
            if backend.app.window.is_some() {
                return Ok(backend);
            }
            if let Some(e) = backend.app.create_error.take() {
                return Err(PlatformError::Os(format!("cannot create the window: {e}")));
            }
            if let PumpStatus::Exit(code) = backend.pump(Some(std::time::Duration::from_millis(10)))
            {
                return Err(PlatformError::Os(format!(
                    "event loop exited during startup ({code})"
                )));
            }
        }
        Err(PlatformError::Os("the window was never created".to_owned()))
    }

    fn pump(&mut self, timeout: Option<std::time::Duration>) -> PumpStatus {
        let Some(event_loop) = self.event_loop.as_mut() else {
            return PumpStatus::Exit(0);
        };
        let status = event_loop.pump_app_events(timeout, &mut self.app);
        if matches!(status, PumpStatus::Exit(_)) {
            // An exited loop cannot run again: the next backend builds none (winit refuses).
            self.closed = true;
        }
        status
    }
}

impl Drop for WindowedBackend {
    fn drop(&mut self) {
        // The window goes now; its last events are ignored by the next backend (other id).
        self.app.window = None;
        if !self.closed
            && let Some(event_loop) = self.event_loop.take()
        {
            SPARE_LOOP.with(|s| *s.borrow_mut() = Some(event_loop));
        }
    }
}

impl WallpaperSurface for WindowedBackend {
    fn backend(&self) -> BackendKind {
        BackendKind::Windowed
    }

    fn outputs(&self) -> &[OutputInfo] {
        &self.app.infos
    }

    fn raw_display_handle(&self) -> RawDisplayHandle {
        self.display
    }

    fn raw_window_handle(&self, id: OutputId) -> Option<RawWindowHandle> {
        if id != OUTPUT {
            return None;
        }
        self.app
            .window
            .as_ref()?
            .window_handle()
            .ok()
            .map(|h| h.as_raw())
    }

    fn request_frame(&mut self, id: OutputId) {
        if id == OUTPUT {
            self.app.slot.requested = true;
        }
    }

    fn pre_present(&mut self, id: OutputId) {
        if id == OUTPUT
            && let Some(w) = &self.app.window
        {
            w.pre_present_notify();
        }
    }

    fn set_max_fps(&mut self, fps: u32) {
        self.app.slot.clock.set_max_fps(fps);
    }

    fn pause_reasons(&self, id: OutputId) -> PauseReasons {
        if id == OUTPUT {
            self.app.paused
        } else {
            PauseReasons::NONE
        }
    }

    fn waker(&self) -> Waker {
        self.waker.clone()
    }

    fn wait(&mut self, wait: Wait) -> Result<(), PlatformError> {
        let limit = wait.deadline();
        let mut pumped = false;
        loop {
            let now = Instant::now();
            if self
                .app
                .slot
                .fire_if_due(now, !self.app.paused.is_empty(), true)
            {
                self.app.queue.push(Event::Frame(OUTPUT));
            }
            if !self.app.queue.is_empty() {
                return Ok(());
            }
            if self.closed {
                return Err(PlatformError::ConnectionLost(
                    "window event loop exited".to_owned(),
                ));
            }
            if pumped && limit.is_some_and(|l| now >= l) {
                return Ok(());
            }
            let due = self.app.slot.due(now, !self.app.paused.is_empty(), true);
            let timeout = min_deadline(due, limit).map(|d| d.saturating_duration_since(now));
            if let PumpStatus::Exit(_) = self.pump(timeout) {
                self.app.queue.push(Event::CloseRequested);
            }
            pumped = true;
        }
    }

    fn next_event(&mut self) -> Option<Event> {
        self.app.queue.pop()
    }
}
