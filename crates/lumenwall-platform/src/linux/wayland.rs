//! Wayland backend: one wlr-layer-shell surface per selected output (spec 07 §6, §9.3, §10.2).
//!
//! * layer `--layer` (default BOTTOM), anchored to every edge, size 0×0, exclusive zone −1, no keyboard;
//! * fractional scale (`wp_fractional_scale_v1`) + `wp_viewporter`: the buffer is `round(logical × scale)`
//!   pixels and the viewport destination is the logical size (the reference only knew integer scales);
//! * pacing by frame callbacks, attached in [`WallpaperSurface::pre_present`] so they ride on the present's
//!   commit. A surface whose callback does not come back (hidden, output off, session locked) is reported
//!   `OCCLUDED` once and then costs nothing until the compositor answers;
//! * fullscreen pause from `zwlr_foreign_toplevel_manager_v1` events, without the reference's per-frame
//!   round-trip.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit as sctk;

use sctk::compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region};
use sctk::output::{OutputHandler, OutputState};
use sctk::reexports::calloop::{EventLoop, ping};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::globals::{GlobalList, registry_queue_init};
use sctk::reexports::client::protocol::{
    wl_output::WlOutput, wl_pointer::WlPointer, wl_seat::WlSeat, wl_surface::WlSurface,
};
use sctk::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use sctk::reexports::protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use sctk::reexports::protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use sctk::reexports::protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as fth, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as ftm, ZwlrForeignToplevelManagerV1},
};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::pointer::{
    CursorIcon, PointerEvent as SctkPointerEvent, PointerEventKind, PointerHandler, ThemeSpec,
    ThemedPointer,
};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::shell::WaylandSurface;
use sctk::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer as SctkLayer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use sctk::shm::{Shm, ShmHandler};
use sctk::{delegate_dispatch2, delegate_registry, registry_handlers};

use super::loop_error;
use crate::fullscreen::{ToplevelFlags, apply_scope, parse_state_array};
use crate::sched::{FrameSlot, min_deadline, pause_transition, timeout_until};
use crate::types::EventQueue;
use crate::{
    BackendKind, Config, Event, Layer, OutputId, OutputInfo, OutputSelection, PauseReasons,
    PlatformError, PointerButton, PointerEvent, PointerKind, PointerMode, Wait, Waker,
    WallpaperSurface,
};

/// A requested frame whose callback has not come back after this long marks the surface as not visible.
const STARVATION: Duration = Duration::from_millis(500);

/// Linux input event codes (`linux/input-event-codes.h`).
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// User data of objects that have no events.
pub(crate) struct NoEvents;
/// User data of the fractional-scale object of an output surface.
pub(crate) struct FractionalData(OutputId);
/// User data of the foreign toplevel manager.
pub(crate) struct ToplevelManagerData;
/// User data of a foreign toplevel handle.
pub(crate) struct ToplevelData;

struct OutputSurface {
    id: OutputId,
    wl_output: WlOutput,
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    configured: bool,
    added: bool,
    logical: (u32, u32),
    /// Scale × 120 (the unit of `wp_fractional_scale_v1`).
    scale120: u32,
    physical: (u32, u32),
    /// Viewport destination / buffer scale must be re-sent with the next present.
    surface_state_dirty: bool,
    slot: FrameSlot,
    callback_pending: bool,
    callback_since: Instant,
    paused: PauseReasons,
    info: OutputInfo,
}

impl Drop for OutputSurface {
    fn drop(&mut self) {
        // Protocol objects tied to the wl_surface go first; the LayerSurface then destroys role and surface.
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
        if let Some(f) = self.fractional.take() {
            f.destroy();
        }
    }
}

impl OutputSurface {
    fn scale(&self) -> f64 {
        f64::from(self.scale120) / 120.0
    }

    /// Recomputes the pixel size; returns `true` when it changed.
    fn update_physical(&mut self) -> bool {
        let round = |v: u32| ((u64::from(v) * u64::from(self.scale120) + 60) / 120) as u32;
        let physical = (round(self.logical.0).max(1), round(self.logical.1).max(1));
        let changed = physical != self.physical;
        self.physical = physical;
        self.info.logical_size = self.logical;
        self.info.physical_size = physical;
        self.info.scale = self.scale();
        changed
    }
}

struct Toplevel {
    handle: ZwlrForeignToplevelHandleV1,
    app_id: Option<String>,
    flags: ToplevelFlags,
    outputs: Vec<WlOutput>,
}

enum SeatPointer {
    Themed(ThemedPointer),
    Plain(WlPointer),
}

impl SeatPointer {
    fn wl_pointer(&self) -> &WlPointer {
        match self {
            SeatPointer::Themed(p) => p.pointer(),
            SeatPointer::Plain(p) => p,
        }
    }
}

pub(crate) struct WlState {
    conn: Connection,
    qh: QueueHandle<WlState>,
    registry: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    viewporter: Option<WpViewporter>,
    fractional: Option<WpFractionalScaleManagerV1>,
    toplevel_manager: Option<ZwlrForeignToplevelManagerV1>,
    toplevels: Vec<Toplevel>,
    surfaces: Vec<OutputSurface>,
    infos: Vec<OutputInfo>,
    graveyard: Vec<OutputSurface>,
    pointers: Vec<(WlSeat, SeatPointer)>,
    scratch: Vec<PauseReasons>,
    queue: EventQueue,
    config: Config,
    max_fps: u32,
    next_id: u32,
    fullscreen_dirty: bool,
    infos_dirty: bool,
    lost: Option<String>,
}

/// The Wayland backend.
pub(crate) struct WaylandBackend {
    event_loop: EventLoop<'static, WlState>,
    state: WlState,
    ping: ping::Ping,
}

impl WaylandBackend {
    pub(crate) fn new(config: &Config) -> Result<Self, PlatformError> {
        let conn =
            Connection::connect_to_env().map_err(|e| PlatformError::Connect(e.to_string()))?;
        let (globals, mut queue) = registry_queue_init::<WlState>(&conn)
            .map_err(|e| PlatformError::Connect(e.to_string()))?;
        let qh = queue.handle();

        let compositor = CompositorState::bind(&globals, &qh)
            .map_err(|_| PlatformError::MissingInterface("wl_compositor"))?;
        let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| {
            PlatformError::MissingInterface(
                "zwlr_layer_shell_v1 (GNOME has none: use an X11 session or -w)",
            )
        })?;
        let shm =
            Shm::bind(&globals, &qh).map_err(|_| PlatformError::MissingInterface("wl_shm"))?;
        let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, NoEvents).ok();
        // Fractional scaling needs the viewporter to set the logical destination size.
        let fractional: Option<WpFractionalScaleManagerV1> = viewporter
            .as_ref()
            .and_then(|_| globals.bind(&qh, 1..=1, NoEvents).ok());
        let toplevel_manager = bind_toplevel_manager(&globals, &qh, config);

        let mut state = WlState {
            conn: conn.clone(),
            qh: qh.clone(),
            registry: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            seat_state: SeatState::new(&globals, &qh),
            compositor,
            layer_shell,
            shm,
            viewporter,
            fractional,
            toplevel_manager,
            toplevels: Vec::new(),
            surfaces: Vec::new(),
            infos: Vec::new(),
            graveyard: Vec::new(),
            pointers: Vec::new(),
            scratch: Vec::new(),
            queue: EventQueue::new(),
            config: config.clone(),
            max_fps: config.max_fps,
            next_id: 0,
            fullscreen_dirty: false,
            infos_dirty: false,
            lost: None,
        };

        // Output names/geometry (and xdg-output) arrive over two round-trips; surfaces are created as soon as
        // an output is complete (OutputHandler::new_output).
        for _ in 0..2 {
            queue
                .roundtrip(&mut state)
                .map_err(|e| PlatformError::Connect(e.to_string()))?;
        }
        let detected: Vec<String> = state
            .output_state
            .outputs()
            .filter_map(|o| state.output_state.info(&o).and_then(|i| i.name))
            .collect();
        if state.output_state.outputs().next().is_none() {
            return Err(PlatformError::MissingInterface("wl_output"));
        }
        if state.surfaces.is_empty() {
            return Err(PlatformError::NoMatchingOutput {
                requested: config.outputs.requested(),
                detected,
            });
        }
        tracing::info!(
            fractional_scale = state.fractional.is_some(),
            fullscreen_detection = state.toplevel_manager.is_some(),
            "wayland backend ready"
        );

        let event_loop: EventLoop<'static, WlState> = EventLoop::try_new().map_err(loop_error)?;
        WaylandSource::new(conn, queue)
            .insert(event_loop.handle())
            .map_err(|e| loop_error(e.error))?;
        let (ping, ping_source) = ping::make_ping().map_err(loop_error)?;
        event_loop
            .handle()
            .insert_source(ping_source, |_, _, state: &mut WlState| {
                state.queue.push(Event::Wake)
            })
            .map_err(|e| loop_error(e.error))?;
        Ok(Self {
            event_loop,
            state,
            ping,
        })
    }
}

fn bind_toplevel_manager(
    globals: &GlobalList,
    qh: &QueueHandle<WlState>,
    config: &Config,
) -> Option<ZwlrForeignToplevelManagerV1> {
    if !config.fullscreen.enabled {
        return None;
    }
    let manager = globals.bind(qh, 1..=3, ToplevelManagerData).ok();
    if manager.is_none() {
        tracing::info!(
            "fullscreen detection is not supported by this compositor (no wlr-foreign-toplevel)"
        );
    }
    manager
}

impl WlState {
    fn surface_index(&self, id: OutputId) -> Option<usize> {
        self.surfaces.iter().position(|s| s.id == id)
    }

    fn surface_by_wl(&self, surface: &WlSurface) -> Option<usize> {
        self.surfaces
            .iter()
            .position(|s| s.layer.wl_surface() == surface)
    }

    fn create_surface(&mut self, wl_output: &WlOutput) {
        if self.surfaces.iter().any(|s| &s.wl_output == wl_output) {
            return;
        }
        let Some(info) = self.output_state.info(wl_output) else {
            return;
        };
        let Some(name) = info.name.clone() else {
            if matches!(self.config.outputs, OutputSelection::Named(_)) {
                tracing::warn!(
                    "output without a name (wl_output < v4 and no xdg-output) cannot be selected"
                );
                return;
            }
            return self.create_surface_named(wl_output, &info, format!("wl_output-{}", info.id));
        };
        if self.config.outputs.accepts(&name) {
            self.create_surface_named(wl_output, &info, name);
        }
    }

    fn create_surface_named(
        &mut self,
        wl_output: &WlOutput,
        info: &sctk::output::OutputInfo,
        name: String,
    ) {
        let id = OutputId(self.next_id);
        self.next_id += 1;
        let qh = &self.qh;
        let wl_surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            wl_surface,
            sctk_layer(self.config.layer),
            Some(self.config.identity.clone()),
            Some(wl_output),
        );
        layer.set_anchor(Anchor::all());
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        if let Ok(opaque) = Region::new(&self.compositor) {
            opaque.add(0, 0, i32::MAX, i32::MAX);
            layer
                .wl_surface()
                .set_opaque_region(Some(opaque.wl_region()));
        }
        if self.config.pointer == PointerMode::Disabled {
            // Empty input region: clicks go through to whatever is below (§6.2, §10.5).
            if let Ok(empty) = Region::new(&self.compositor) {
                layer.wl_surface().set_input_region(Some(empty.wl_region()));
            }
        }
        let viewport = self
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(layer.wl_surface(), qh, NoEvents));
        let fractional = self
            .fractional
            .as_ref()
            .map(|f| f.get_fractional_scale(layer.wl_surface(), qh, FractionalData(id)));
        layer.commit();

        let scale120 = u32::try_from(info.scale_factor.max(1)).unwrap_or(1) * 120;
        let logical = info
            .logical_size
            .map(|(w, h)| (w.max(1) as u32, h.max(1) as u32))
            .unwrap_or((1, 1));
        let output_info = OutputInfo {
            id,
            name,
            description: describe(info),
            position: info.logical_position.unwrap_or(info.location),
            logical_size: logical,
            physical_size: logical,
            scale: f64::from(scale120) / 120.0,
            refresh_mhz: current_refresh(info),
        };
        tracing::debug!(output = %output_info.name, ?id, "creating layer surface");
        self.surfaces.push(OutputSurface {
            id,
            wl_output: wl_output.clone(),
            layer,
            viewport,
            fractional,
            configured: false,
            added: false,
            logical,
            scale120,
            physical: logical,
            surface_state_dirty: true,
            slot: FrameSlot::new(self.max_fps),
            callback_pending: false,
            callback_since: Instant::now(),
            paused: PauseReasons::NONE,
            info: output_info,
        });
        self.fullscreen_dirty = true;
    }

    fn remove_surface(&mut self, idx: usize) {
        let s = self.surfaces.remove(idx);
        if s.added {
            self.queue.push(Event::OutputRemoved(s.id));
            self.infos_dirty = true;
        }
        // Destroyed on the next `wait`, after the caller dropped its GPU surface.
        self.graveyard.push(s);
    }

    fn rebuild_infos(&mut self) {
        self.infos_dirty = false;
        self.infos.clear();
        self.infos.extend(
            self.surfaces
                .iter()
                .filter(|s| s.added)
                .map(|s| s.info.clone()),
        );
        crate::sort_outputs(&mut self.infos);
    }

    fn set_scale120(&mut self, idx: usize, scale120: u32) {
        let s = &mut self.surfaces[idx];
        if scale120 == 0 || scale120 == s.scale120 {
            return;
        }
        s.scale120 = scale120;
        s.surface_state_dirty = true;
        let resized = s.update_physical();
        if s.added {
            self.queue.push(Event::ScaleChanged {
                output: s.id,
                scale: s.scale(),
            });
            if resized {
                self.queue.push(Event::Resized {
                    output: s.id,
                    physical_size: s.physical,
                    logical_size: s.logical,
                });
            }
            self.infos_dirty = true;
        }
    }

    fn recompute_fullscreen(&mut self) {
        self.fullscreen_dirty = false;
        self.scratch.clear();
        self.scratch.resize(self.surfaces.len(), PauseReasons::NONE);
        for t in self.toplevels.iter().filter(|t| t.flags.relevant) {
            for (s, r) in self.surfaces.iter().zip(self.scratch.iter_mut()) {
                // A toplevel not (yet) on any output counts for every output.
                if t.outputs.is_empty() || t.outputs.contains(&s.wl_output) {
                    *r |= PauseReasons::FULLSCREEN;
                }
            }
        }
        apply_scope(&mut self.scratch, self.config.fullscreen.scope);
        for (s, fs) in self.surfaces.iter_mut().zip(self.scratch.iter()) {
            let new = s.paused.difference(PauseReasons::FULLSCREEN) | *fs;
            if s.added
                && let Some(ev) = pause_transition(s.id, s.paused, new)
            {
                self.queue.push(ev);
            }
            if !s.paused.is_empty() && new.is_empty() {
                // A callback left pending while hidden gets a fresh grace period before "occluded".
                s.callback_since = Instant::now();
            }
            s.paused = new;
        }
    }

    /// Starvation pauses and due frames.
    fn tick(&mut self, now: Instant) {
        if self.fullscreen_dirty {
            self.recompute_fullscreen();
        }
        for s in self.surfaces.iter_mut().filter(|s| s.added) {
            if s.slot.requested
                && s.callback_pending
                && s.paused.is_empty()
                && now >= s.callback_since + STARVATION
            {
                s.paused |= PauseReasons::OCCLUDED;
                self.queue.push(Event::Paused {
                    output: s.id,
                    reasons: s.paused,
                });
                // If the frame request was never committed (the caller skipped its present), commit it now;
                // a hidden surface still gets no answer, a visible one resumes at the next refresh. Only with
                // the viewporter: without it a pending buffer-scale change must wait for a matching buffer.
                if s.viewport.is_some() {
                    s.layer.wl_surface().commit();
                }
            }
            if s.slot
                .fire_if_due(now, !s.paused.is_empty(), !s.callback_pending)
            {
                self.queue.push(Event::Frame(s.id));
            }
        }
        if self.infos_dirty {
            self.rebuild_infos();
        }
    }

    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.surfaces
            .iter()
            .filter(|s| s.added && s.slot.requested && s.paused.is_empty())
            .map(|s| {
                if s.callback_pending {
                    s.callback_since + STARVATION
                } else {
                    s.slot.clock.earliest(now)
                }
            })
            .min()
    }
}

fn sctk_layer(layer: Layer) -> SctkLayer {
    match layer {
        Layer::Background => SctkLayer::Background,
        Layer::Bottom => SctkLayer::Bottom,
        Layer::Top => SctkLayer::Top,
        Layer::Overlay => SctkLayer::Overlay,
    }
}

fn describe(info: &sctk::output::OutputInfo) -> Option<String> {
    info.description.clone().or_else(|| {
        let s = format!("{} {}", info.make, info.model);
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_owned())
    })
}

fn current_refresh(info: &sctk::output::OutputInfo) -> Option<u32> {
    info.modes
        .iter()
        .find(|m| m.current)
        .and_then(|m| u32::try_from(m.refresh_rate).ok())
        .filter(|r| *r > 0)
}

impl WallpaperSurface for WaylandBackend {
    fn backend(&self) -> BackendKind {
        BackendKind::Wayland
    }

    fn outputs(&self) -> &[OutputInfo] {
        &self.state.infos
    }

    fn raw_display_handle(&self) -> RawDisplayHandle {
        let ptr = self.state.conn.backend().display_ptr().cast::<c_void>();
        RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(ptr).expect("libwayland display pointer"),
        ))
    }

    fn raw_window_handle(&self, id: OutputId) -> Option<RawWindowHandle> {
        let s = self.state.surfaces.iter().find(|s| s.id == id && s.added)?;
        let ptr = NonNull::new(s.layer.wl_surface().id().as_ptr().cast::<c_void>())?;
        Some(RawWindowHandle::Wayland(WaylandWindowHandle::new(ptr)))
    }

    fn request_frame(&mut self, id: OutputId) {
        if let Some(i) = self.state.surface_index(id) {
            self.state.surfaces[i].slot.requested = true;
        }
    }

    fn pre_present(&mut self, id: OutputId) {
        let Some(i) = self.state.surface_index(id) else {
            return;
        };
        let qh = self.state.qh.clone();
        let s = &mut self.state.surfaces[i];
        if !s.configured {
            return;
        }
        let wl_surface = s.layer.wl_surface();
        if s.surface_state_dirty {
            s.surface_state_dirty = false;
            match &s.viewport {
                Some(vp) => {
                    wl_surface.set_buffer_scale(1);
                    vp.set_destination(s.logical.0 as i32, s.logical.1 as i32);
                }
                None => wl_surface.set_buffer_scale((s.scale120 / 120).max(1) as i32),
            }
        }
        if !s.callback_pending {
            // Allocates one small protocol object per frame: inherent to wl_surface.frame.
            wl_surface.frame(&qh, FrameCallbackData(wl_surface.clone()));
            s.callback_pending = true;
            s.callback_since = Instant::now();
        }
    }

    fn set_max_fps(&mut self, fps: u32) {
        self.state.max_fps = fps;
        for s in &mut self.state.surfaces {
            s.slot.clock.set_max_fps(fps);
        }
    }

    fn pause_reasons(&self, id: OutputId) -> PauseReasons {
        self.state
            .surfaces
            .iter()
            .find(|s| s.id == id)
            .map_or(PauseReasons::NONE, |s| s.paused)
    }

    fn waker(&self) -> Waker {
        let ping = self.ping.clone();
        Waker::new(move || ping.ping())
    }

    fn wait(&mut self, wait: Wait) -> Result<(), PlatformError> {
        self.state.graveyard.clear();
        let limit = wait.deadline();
        let mut dispatched = false;
        loop {
            let now = Instant::now();
            self.state.tick(now);
            if !self.state.queue.is_empty() {
                return Ok(());
            }
            if let Some(reason) = self.state.lost.take() {
                return Err(PlatformError::ConnectionLost(reason));
            }
            if dispatched && limit.is_some_and(|l| now >= l) {
                return Ok(());
            }
            let timeout = timeout_until(min_deadline(self.state.next_deadline(now), limit), now);
            if let Err(e) = self.event_loop.dispatch(timeout, &mut self.state) {
                // The compositor went away: stop cleanly (spec 07 §1.2).
                tracing::error!("wayland connection lost: {e}");
                self.state.lost = Some(e.to_string());
                self.state.queue.push(Event::CloseRequested);
            }
            dispatched = true;
        }
    }

    fn next_event(&mut self) -> Option<Event> {
        self.state.queue.pop()
    }
}

// ---------------------------------------------------------------------------------------------------------
// sctk handlers

impl CompositorHandler for WlState {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        factor: i32,
    ) {
        // Integer scale only matters without fractional-scale support.
        if self.fractional.is_some() {
            return;
        }
        if let Some(i) = self.surface_by_wl(surface) {
            self.set_scale120(i, u32::try_from(factor.max(1)).unwrap_or(1) * 120);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: sctk::reexports::client::protocol::wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        let Some(i) = self.surface_by_wl(surface) else {
            return;
        };
        let s = &mut self.surfaces[i];
        s.callback_pending = false;
        if s.paused.contains(PauseReasons::OCCLUDED) {
            let new = s.paused.difference(PauseReasons::OCCLUDED);
            if let Some(ev) = pause_transition(s.id, s.paused, new) {
                self.queue.push(ev);
            }
            s.paused = new;
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}

impl OutputHandler for WlState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        self.create_surface(&output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        let Some(i) = self.surfaces.iter().position(|s| s.wl_output == output) else {
            // The name may only now be known.
            return self.create_surface(&output);
        };
        let Some(info) = self.output_state.info(&output) else {
            return;
        };
        let s = &mut self.surfaces[i];
        let position = info.logical_position.unwrap_or(info.location);
        if position != s.info.position {
            s.info.position = position;
            if s.added {
                self.queue.push(Event::Moved {
                    output: s.id,
                    position,
                });
            }
        }
        s.info.refresh_mhz = current_refresh(&info);
        s.info.description = describe(&info);
        self.infos_dirty = true;
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        for t in &mut self.toplevels {
            t.outputs.retain(|o| o != &output);
        }
        if let Some(i) = self.surfaces.iter().position(|s| s.wl_output == output) {
            self.remove_surface(i);
        }
        self.fullscreen_dirty = true;
    }
}

impl LayerShellHandler for WlState {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(i) = self.surfaces.iter().position(|s| &s.layer == layer) {
            tracing::error!(output = %self.surfaces[i].info.name, "layer surface closed by the compositor");
            self.remove_surface(i);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(i) = self.surfaces.iter().position(|s| &s.layer == layer) else {
            return;
        };
        let fallback = self
            .output_state
            .info(&self.surfaces[i].wl_output)
            .and_then(|i| i.logical_size);
        let s = &mut self.surfaces[i];
        let (mut w, mut h) = configure.new_size;
        if w == 0 || h == 0 {
            let (fw, fh) = fallback.unwrap_or((1, 1));
            (w, h) = (fw.max(1) as u32, fh.max(1) as u32);
        }
        let first = !s.configured;
        s.configured = true;
        let logical_changed = s.logical != (w, h);
        s.logical = (w, h);
        if logical_changed {
            s.surface_state_dirty = true;
        }
        let physical_changed = s.update_physical();
        if first {
            s.added = true;
            // First frame right away: the surface is only mapped once a buffer is committed (§8.2 bootstrap).
            s.slot.requested = true;
            self.queue.push(Event::OutputAdded(s.id));
            if !s.paused.is_empty() {
                self.queue.push(Event::Paused {
                    output: s.id,
                    reasons: s.paused,
                });
            }
            self.infos_dirty = true;
            // `outputs()` must already list it when the caller handles OutputAdded.
            self.rebuild_infos();
        } else if logical_changed || physical_changed {
            self.queue.push(Event::Resized {
                output: s.id,
                physical_size: s.physical,
                logical_size: s.logical,
            });
            self.infos_dirty = true;
        }
    }
}

impl SeatHandler for WlState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Pointer
            || self.config.pointer == PointerMode::Disabled
            || self.pointers.iter().any(|(s, _)| s == &seat)
        {
            return;
        }
        let cursor_surface = self.compositor.create_surface(qh);
        let pointer = match self.seat_state.get_pointer_with_theme::<WlState, ()>(
            qh,
            &seat,
            self.shm.wl_shm(),
            cursor_surface,
            ThemeSpec::System,
        ) {
            Ok(p) => SeatPointer::Themed(p),
            Err(e) => {
                tracing::warn!(
                    "themed pointer unavailable ({e}), cursor image left to the compositor"
                );
                match self.seat_state.get_pointer(qh, &seat) {
                    Ok(p) => SeatPointer::Plain(p),
                    Err(e) => return tracing::warn!("no pointer: {e}"),
                }
            }
        };
        self.pointers.push((seat, pointer));
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Pointer {
            return;
        }
        if let Some(i) = self.pointers.iter().position(|(s, _)| s == &seat) {
            let (_, p) = self.pointers.remove(i);
            if p.wl_pointer().version() >= 3 {
                p.wl_pointer().release();
            }
        }
    }

    fn remove_seat(&mut self, conn: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        self.remove_capability(conn, qh, seat, Capability::Pointer);
    }
}

impl PointerHandler for WlState {
    fn pointer_frame(
        &mut self,
        conn: &Connection,
        _: &QueueHandle<Self>,
        pointer: &WlPointer,
        events: &[SctkPointerEvent],
    ) {
        let buttons = self.config.pointer == PointerMode::MotionAndButtons;
        for ev in events {
            let Some(i) = self.surface_by_wl(&ev.surface) else {
                continue;
            };
            let s = &self.surfaces[i];
            if !s.added {
                continue;
            }
            let scale = s.scale();
            let kind = match ev.kind {
                PointerEventKind::Enter { .. } => {
                    if let Some((_, SeatPointer::Themed(p))) = self
                        .pointers
                        .iter()
                        .find(|(_, p)| p.wl_pointer() == pointer)
                        && let Err(e) = p.set_cursor(conn, CursorIcon::Default)
                    {
                        tracing::debug!("cannot set cursor: {e}");
                    }
                    PointerKind::Enter
                }
                PointerEventKind::Leave { .. } => PointerKind::Leave,
                PointerEventKind::Motion { .. } => PointerKind::Motion,
                PointerEventKind::Press { button, .. }
                | PointerEventKind::Release { button, .. }
                    if buttons =>
                {
                    let pressed = matches!(ev.kind, PointerEventKind::Press { .. });
                    let button = match button {
                        BTN_LEFT => PointerButton::Left,
                        BTN_RIGHT => PointerButton::Right,
                        BTN_MIDDLE => PointerButton::Middle,
                        other => PointerButton::Other(other),
                    };
                    PointerKind::Button { button, pressed }
                }
                _ => continue,
            };
            // parity: physical position = logical surface position × scale (§10.2).
            self.queue.push(Event::Pointer(PointerEvent {
                output: s.id,
                x: ev.position.0 * scale,
                y: ev.position.1 * scale,
                kind,
            }));
        }
    }
}

impl ShmHandler for WlState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for WlState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers!(OutputState, SeatState);
}

delegate_registry!(WlState);
delegate_dispatch2!(WlState);

// ---------------------------------------------------------------------------------------------------------
// Protocols not covered by sctk

impl Dispatch<WpViewporter, NoEvents> for WlState {
    fn event(
        _: &mut Self,
        _: &WpViewporter,
        _: <WpViewporter as Proxy>::Event,
        _: &NoEvents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewport, NoEvents> for WlState {
    fn event(
        _: &mut Self,
        _: &WpViewport,
        _: <WpViewport as Proxy>::Event,
        _: &NoEvents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleManagerV1, NoEvents> for WlState {
    fn event(
        _: &mut Self,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _: &NoEvents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleV1, FractionalData> for WlState {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        data: &FractionalData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(i) = state.surface_index(data.0)
        {
            state.set_scale120(i, scale);
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ToplevelManagerData> for WlState {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: ftm::Event,
        _: &ToplevelManagerData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ftm::Event::Toplevel { toplevel } => state.toplevels.push(Toplevel {
                handle: toplevel,
                app_id: None,
                flags: ToplevelFlags::default(),
                outputs: Vec::new(),
            }),
            ftm::Event::Finished => {
                state.toplevel_manager = None;
                state.toplevels.clear();
                state.fullscreen_dirty = true;
            }
            _ => {}
        }
    }

    event_created_child!(WlState, ZwlrForeignToplevelManagerV1, [
        ftm::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ToplevelData),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ToplevelData> for WlState {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: fth::Event,
        _: &ToplevelData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(i) = state.toplevels.iter().position(|t| &t.handle == handle) else {
            return;
        };
        let policy = &state.config.fullscreen;
        let t = &mut state.toplevels[i];
        match event {
            fth::Event::AppId { app_id } => t.app_id = Some(app_id),
            fth::Event::State { state: bytes } => {
                t.flags.set_pending_state(parse_state_array(&bytes))
            }
            fth::Event::OutputEnter { output } => {
                if !t.outputs.contains(&output) {
                    t.outputs.push(output);
                    state.fullscreen_dirty |= t.flags.relevant;
                }
            }
            fth::Event::OutputLeave { output } => {
                t.outputs.retain(|o| o != &output);
                state.fullscreen_dirty |= t.flags.relevant;
            }
            fth::Event::Done => {
                if t.flags.commit(t.app_id.as_deref(), policy) {
                    state.fullscreen_dirty = true;
                }
            }
            fth::Event::Closed => {
                let t = state.toplevels.swap_remove(i);
                t.handle.destroy();
                state.fullscreen_dirty |= t.flags.relevant;
            }
            _ => {}
        }
    }
}
