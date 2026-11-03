//! X11 backend: one `_NET_WM_WINDOW_TYPE_DESKTOP` window per selected RandR output (spec 07 §5, §9.2, §10.2).
//!
//! The GPU renders straight into these windows (no per-frame read-back into a root pixmap as the reference
//! does). Everything is event driven:
//!
//! * outputs: RandR screen/CRTC/output change notifications (hot-plug supported);
//! * fullscreen: the root's children are cached from `SubstructureNotify` events and matched against the CRTC
//!   rectangles (reference rule), plus `_NET_ACTIVE_WINDOW` / `_NET_WM_STATE_FULLSCREEN` from `PropertyNotify`;
//! * covered: `VisibilityNotify` (only meaningful without a compositor);
//! * locked / screen off: MIT-SCREEN-SAVER notify and DPMS 1.2 `InfoNotify`;
//! * pointer: one `QueryPointer` per rendered frame, pipelined (the reply is read one frame later, by which
//!   time it has long arrived), never while paused. Clicks come from `ButtonPress` on our windows, i.e. only
//!   on the visible desktop.

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Instant;

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode as LoopMode, PostAction, ping};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, XcbDisplayHandle, XcbWindowHandle};
use x11rb::connection::{Connection as _, RequestConnection as _, SequenceNumber};
use x11rb::cookie::Cookie;
use x11rb::properties::{WmHints, WmSizeHints, WmSizeHintsSpecification};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
    EventMask, MapState, PropMode, QueryPointerReply, Visibility, Window, WindowClass,
};
use x11rb::protocol::{Event as XEvent, dpms, randr, screensaver};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use super::loop_error;
use crate::fullscreen::{RootChild, RootChildren, Verdict, apply_scope};
use crate::geometry::{Rect, best_overlap};
use crate::sched::{FrameSlot, min_deadline, pause_transition, timeout_until};
use crate::types::EventQueue;
use crate::{
    BackendKind, Config, Event, OutputId, OutputInfo, PauseReasons, PlatformError, PointerButton,
    PointerEvent, PointerKind, PointerMode, Wait, Waker, WallpaperSurface,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_STICKY,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_DESKTOP,
        _NET_ACTIVE_WINDOW,
        _NET_WM_NAME,
        UTF8_STRING,
        _MOTIF_WM_HINTS,
    }
}

/// A RandR output with an active CRTC.
#[derive(Clone, Debug, PartialEq)]
struct Monitor {
    name: String,
    rect: Rect,
    refresh_mhz: Option<u32>,
}

/// Refresh rate of a RandR mode in millihertz.
fn refresh_mhz(
    dot_clock: u32,
    htotal: u16,
    vtotal: u16,
    interlace: bool,
    double_scan: bool,
) -> Option<u32> {
    let mut lines = u64::from(vtotal);
    if double_scan {
        lines *= 2;
    }
    let total = u64::from(htotal) * lines;
    if total == 0 {
        return None;
    }
    let mut mhz = u64::from(dot_clock) * 1000 / total;
    if interlace {
        mhz *= 2;
    }
    u32::try_from(mhz).ok().filter(|v| *v > 0)
}

/// EWMH `_NET_WM_STATE` flags we care about.
#[derive(Clone, Copy, Debug, Default)]
struct WmState {
    fullscreen: bool,
    hidden: bool,
    maximized: bool,
}

struct X11Output {
    id: OutputId,
    window: Window,
    rect: Rect,
    slot: FrameSlot,
    paused: PauseReasons,
    obscured: bool,
    last_pointer: Option<(i32, i32)>,
    info: OutputInfo,
}

pub(crate) struct X11State {
    conn: Arc<XCBConnection>,
    screen: usize,
    root: Window,
    root_visual: u32,
    root_size: (u16, u16),
    atoms: Atoms,
    has_randr: bool,
    outputs: Vec<X11Output>,
    infos: Vec<OutputInfo>,
    graveyard: Vec<Window>,
    children: RootChildren,
    /// Our windows and the frames a window manager may have put around them.
    exclude: Vec<Window>,
    active: Window,
    active_fullscreen: Option<Rect>,
    screensaver_on: bool,
    display_off: bool,
    pointer_seq: Option<SequenceNumber>,
    cursor: Option<(i32, i32)>,
    queue: EventQueue,
    config: Config,
    max_fps: u32,
    next_id: u32,
    outputs_dirty: bool,
    active_dirty: bool,
    pause_dirty: bool,
    lost: Option<String>,
    rects: Vec<Rect>,
    scratch: Vec<PauseReasons>,
    candidates: Vec<u32>,
}

/// The X11 backend.
pub(crate) struct X11Backend {
    event_loop: EventLoop<'static, X11State>,
    state: X11State,
    ping: ping::Ping,
}

fn conn_err(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Connect(e.to_string())
}

impl X11Backend {
    pub(crate) fn new(config: &Config) -> Result<Self, PlatformError> {
        let (conn, screen) = XCBConnection::connect(None).map_err(conn_err)?;
        let conn = Arc::new(conn);
        let s = &conn.setup().roots[screen];
        let (root, root_visual, root_size) = (
            s.root,
            s.root_visual,
            (s.width_in_pixels, s.height_in_pixels),
        );
        let atoms = Atoms::new(&*conn)
            .map_err(conn_err)?
            .reply()
            .map_err(conn_err)?;

        // Root events: children map/unmap/configure (fullscreen cache) and _NET_ACTIVE_WINDOW.
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::SUBSTRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE),
        )
        .map_err(conn_err)?;

        let has_randr = has_extension(&conn, randr::X11_EXTENSION_NAME)
            && randr::query_version(&*conn, 1, 3)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 3));
        if has_randr {
            let mask = randr::NotifyMask::SCREEN_CHANGE
                | randr::NotifyMask::CRTC_CHANGE
                | randr::NotifyMask::OUTPUT_CHANGE;
            randr::select_input(&*conn, root, mask).map_err(conn_err)?;
        } else {
            tracing::warn!(
                "XRandR >= 1.3 unavailable: the whole X screen is used as a single output"
            );
        }

        let mut display_off = false;
        if has_extension(&conn, dpms::X11_EXTENSION_NAME) {
            let v = dpms::get_version(&*conn, 1, 2)
                .ok()
                .and_then(|c| c.reply().ok());
            if v.is_some_and(|v| (v.server_major_version, v.server_minor_version) >= (1, 2)) {
                let _ = dpms::select_input(&*conn, dpms::EventMask::INFO_NOTIFY);
            }
            if let Some(info) = dpms::info(&*conn).ok().and_then(|c| c.reply().ok()) {
                display_off = info.state && info.power_level != dpms::DPMSMode::ON;
            }
        }
        let mut screensaver_on = false;
        if has_extension(&conn, screensaver::X11_EXTENSION_NAME) {
            let _ = screensaver::select_input(&*conn, root, screensaver::Event::NOTIFY_MASK);
            if let Some(info) = screensaver::query_info(&*conn, root)
                .ok()
                .and_then(|c| c.reply().ok())
            {
                screensaver_on = info.state == u8::from(screensaver::State::ON);
            }
        }

        let mut children = RootChildren::default();
        children.reset(snapshot_root_children(&conn, root));

        let mut state = X11State {
            conn: conn.clone(),
            screen,
            root,
            root_visual,
            root_size,
            atoms,
            has_randr,
            outputs: Vec::new(),
            infos: Vec::new(),
            graveyard: Vec::new(),
            children,
            exclude: Vec::new(),
            active: 0,
            active_fullscreen: None,
            screensaver_on,
            display_off,
            pointer_seq: None,
            cursor: None,
            queue: EventQueue::new(),
            config: config.clone(),
            max_fps: config.max_fps,
            next_id: 0,
            outputs_dirty: false,
            active_dirty: true,
            pause_dirty: true,
            lost: None,
            rects: Vec::new(),
            scratch: Vec::new(),
            candidates: Vec::new(),
        };
        state.refresh_outputs(true)?;
        state.process_dirty();
        conn.flush().map_err(conn_err)?;

        let event_loop: EventLoop<'static, X11State> = EventLoop::try_new().map_err(loop_error)?;
        // Readiness only wakes the loop; events are drained by `wait` (libxcb may also have buffered some).
        event_loop
            .handle()
            .insert_source(
                Generic::new(conn, Interest::READ, LoopMode::Level),
                |_, _, _| Ok(PostAction::Continue),
            )
            .map_err(|e| loop_error(e.error))?;
        let (ping, ping_source) = ping::make_ping().map_err(loop_error)?;
        event_loop
            .handle()
            .insert_source(ping_source, |_, _, state: &mut X11State| {
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

fn has_extension(conn: &XCBConnection, name: &'static str) -> bool {
    conn.extension_information(name).ok().flatten().is_some()
}

/// Startup snapshot of the root's children (pipelined requests).
fn snapshot_root_children(conn: &XCBConnection, root: Window) -> Vec<RootChild> {
    let Some(tree) = conn.query_tree(root).ok().and_then(|c| c.reply().ok()) else {
        return Vec::new();
    };
    let cookies: Vec<_> = tree
        .children
        .iter()
        .map(|&w| {
            (
                w,
                conn.get_window_attributes(w).ok(),
                conn.get_geometry(w).ok(),
            )
        })
        .collect();
    cookies
        .into_iter()
        .filter_map(|(id, attrs, geom)| {
            let attrs = attrs?.reply().ok()?;
            let geom = geom?.reply().ok()?;
            let verdict = if attrs.class == WindowClass::INPUT_ONLY {
                Verdict::Ignored
            } else {
                Verdict::Unknown
            };
            Some(RootChild {
                id,
                rect: Rect::new(
                    geom.x.into(),
                    geom.y.into(),
                    geom.width.into(),
                    geom.height.into(),
                ),
                mapped: attrs.map_state == MapState::VIEWABLE,
                hidden: false,
                maximized: false,
                verdict,
            })
        })
        .collect()
}

impl X11State {
    fn is_ours(&self, w: Window) -> bool {
        self.outputs.iter().any(|o| o.window == w)
    }

    fn output_by_window(&mut self, w: Window) -> Option<&mut X11Output> {
        self.outputs.iter_mut().find(|o| o.window == w)
    }

    fn enumerate_monitors(&self) -> Vec<Monitor> {
        if self.has_randr {
            match self.randr_monitors() {
                Ok(v) if !v.is_empty() => return v,
                Ok(_) => tracing::warn!("RandR reports no active output"),
                Err(e) => tracing::warn!("RandR query failed: {e}"),
            }
        }
        vec![Monitor {
            name: "screen".to_owned(),
            rect: Rect::new(0, 0, self.root_size.0.into(), self.root_size.1.into()),
            refresh_mhz: None,
        }]
    }

    fn randr_monitors(&self) -> Result<Vec<Monitor>, x11rb::errors::ReplyError> {
        let conn = &*self.conn;
        let res = randr::get_screen_resources_current(conn, self.root)?.reply()?;
        let cookies = res
            .outputs
            .iter()
            .map(|&o| randr::get_output_info(conn, o, res.config_timestamp))
            .collect::<Result<Vec<_>, _>>()?;
        let mut monitors = Vec::new();
        for cookie in cookies {
            let Ok(info) = cookie.reply() else { continue };
            if info.connection != randr::Connection::CONNECTED || info.crtc == x11rb::NONE {
                continue;
            }
            let Ok(crtc) = randr::get_crtc_info(conn, info.crtc, res.config_timestamp)?.reply()
            else {
                continue;
            };
            if crtc.width == 0 || crtc.height == 0 {
                continue;
            }
            let refresh = res.modes.iter().find(|m| m.id == crtc.mode).and_then(|m| {
                refresh_mhz(
                    m.dot_clock,
                    m.htotal,
                    m.vtotal,
                    m.mode_flags.contains(randr::ModeFlag::INTERLACE),
                    m.mode_flags.contains(randr::ModeFlag::DOUBLE_SCAN),
                )
            });
            monitors.push(Monitor {
                name: String::from_utf8_lossy(&info.name).into_owned(),
                rect: Rect::new(
                    crtc.x.into(),
                    crtc.y.into(),
                    crtc.width.into(),
                    crtc.height.into(),
                ),
                refresh_mhz: refresh,
            });
        }
        Ok(monitors)
    }

    fn create_window(&self, rect: Rect) -> Result<Window, PlatformError> {
        let conn = &*self.conn;
        let window = conn.generate_id().map_err(conn_err)?;
        let mut mask =
            EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY | EventMask::VISIBILITY_CHANGE;
        if self.config.pointer == PointerMode::MotionAndButtons {
            mask |= EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE;
        }
        // No background: the server never clears the window between our frames (no flicker).
        let aux = CreateWindowAux::new()
            .background_pixmap(x11rb::NONE)
            .border_pixel(0)
            .event_mask(mask);
        let (w, h) = (
            rect.width.clamp(1, 65535) as u16,
            rect.height.clamp(1, 65535) as u16,
        );
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            self.root,
            rect.x as i16,
            rect.y as i16,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &aux,
        )
        .map_err(conn_err)?;

        let a = &self.atoms;
        let name = self.config.identity.as_bytes();
        let mut class = Vec::with_capacity(name.len() * 2 + 2);
        class.extend_from_slice(name);
        class.push(0);
        class.extend_from_slice(name);
        class.push(0);
        let props = [
            conn.change_property32(
                PropMode::REPLACE,
                window,
                a._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                &[a._NET_WM_WINDOW_TYPE_DESKTOP],
            ),
            conn.change_property32(
                PropMode::REPLACE,
                window,
                a._NET_WM_STATE,
                AtomEnum::ATOM,
                &[
                    a._NET_WM_STATE_BELOW,
                    a._NET_WM_STATE_STICKY,
                    a._NET_WM_STATE_SKIP_TASKBAR,
                    a._NET_WM_STATE_SKIP_PAGER,
                ],
            ),
            conn.change_property32(
                PropMode::REPLACE,
                window,
                a._NET_WM_DESKTOP,
                AtomEnum::CARDINAL,
                &[u32::MAX],
            ),
            // MWM hints: flags = decorations, decorations = none.
            conn.change_property32(
                PropMode::REPLACE,
                window,
                a._MOTIF_WM_HINTS,
                a._MOTIF_WM_HINTS,
                &[2, 0, 0, 0, 0],
            ),
            conn.change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                &class,
            ),
            conn.change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                name,
            ),
            conn.change_property8(
                PropMode::REPLACE,
                window,
                a._NET_WM_NAME,
                a.UTF8_STRING,
                name,
            ),
        ];
        for p in props {
            p.map_err(conn_err)?;
        }
        let hints = WmHints {
            input: Some(false),
            ..WmHints::new()
        };
        hints.set(conn, window).map_err(conn_err)?;
        let size_hints = WmSizeHints {
            position: Some((WmSizeHintsSpecification::UserSpecified, rect.x, rect.y)),
            size: Some((
                WmSizeHintsSpecification::UserSpecified,
                rect.width,
                rect.height,
            )),
            min_size: Some((rect.width, rect.height)),
            max_size: Some((rect.width, rect.height)),
            ..WmSizeHints::new()
        };
        size_hints
            .set_normal_hints(conn, window)
            .map_err(conn_err)?;
        conn.map_window(window).map_err(conn_err)?;
        // Some window managers ignore the initial position of new windows.
        let cfg = ConfigureWindowAux::new()
            .x(rect.x)
            .y(rect.y)
            .width(u32::from(w))
            .height(u32::from(h));
        conn.configure_window(window, &cfg).map_err(conn_err)?;
        Ok(window)
    }

    /// Diffs RandR outputs with our windows (startup and hot-plug).
    fn refresh_outputs(&mut self, initial: bool) -> Result<(), PlatformError> {
        self.outputs_dirty = false;
        let monitors = self.enumerate_monitors();
        let accepted = |m: &Monitor| self.config.outputs.accepts(&m.name);

        let mut i = 0;
        while i < self.outputs.len() {
            let keep = monitors
                .iter()
                .any(|m| m.name == self.outputs[i].info.name && accepted(m));
            if keep {
                i += 1;
                continue;
            }
            let o = self.outputs.remove(i);
            tracing::info!(output = %o.info.name, "output removed");
            self.exclude.retain(|w| *w != o.window);
            self.graveyard.push(o.window);
            self.queue.push(Event::OutputRemoved(o.id));
        }

        for m in monitors.iter().filter(|m| accepted(m)) {
            if let Some(o) = self.outputs.iter_mut().find(|o| o.info.name == m.name) {
                o.info.refresh_mhz = m.refresh_mhz;
                if o.rect == m.rect {
                    continue;
                }
                let cfg = ConfigureWindowAux::new()
                    .x(m.rect.x)
                    .y(m.rect.y)
                    .width(m.rect.width.max(1) as u32)
                    .height(m.rect.height.max(1) as u32);
                let _ = self.conn.configure_window(o.window, &cfg);
                let size = (m.rect.width.max(1) as u32, m.rect.height.max(1) as u32);
                if (o.rect.width, o.rect.height) != (m.rect.width, m.rect.height) {
                    self.queue.push(Event::Resized {
                        output: o.id,
                        physical_size: size,
                        logical_size: size,
                    });
                }
                if (o.rect.x, o.rect.y) != (m.rect.x, m.rect.y) {
                    self.queue.push(Event::Moved {
                        output: o.id,
                        position: (m.rect.x, m.rect.y),
                    });
                }
                o.rect = m.rect;
                o.info.position = (m.rect.x, m.rect.y);
                o.info.logical_size = size;
                o.info.physical_size = size;
                continue;
            }
            // parity: "requested output found: NAME -> x×y:w×h" (§5).
            tracing::info!(
                "requested output found: {} -> {}x{}:{}x{}",
                m.name,
                m.rect.x,
                m.rect.y,
                m.rect.width,
                m.rect.height
            );
            let window = self.create_window(m.rect)?;
            let id = OutputId(self.next_id);
            self.next_id += 1;
            let size = (m.rect.width.max(1) as u32, m.rect.height.max(1) as u32);
            let mut slot = FrameSlot::new(self.max_fps);
            slot.requested = true;
            self.exclude.push(window);
            self.outputs.push(X11Output {
                id,
                window,
                rect: m.rect,
                slot,
                paused: PauseReasons::NONE,
                obscured: false,
                last_pointer: None,
                info: OutputInfo {
                    id,
                    name: m.name.clone(),
                    description: None,
                    position: (m.rect.x, m.rect.y),
                    logical_size: size,
                    physical_size: size,
                    scale: 1.0,
                    refresh_mhz: m.refresh_mhz,
                },
            });
            self.queue.push(Event::OutputAdded(id));
        }

        if initial && self.outputs.is_empty() {
            return Err(PlatformError::NoMatchingOutput {
                requested: self.config.outputs.requested(),
                detected: monitors.into_iter().map(|m| m.name).collect(),
            });
        }
        self.infos.clear();
        self.infos
            .extend(self.outputs.iter().map(|o| o.info.clone()));
        crate::sort_outputs(&mut self.infos);
        self.pause_dirty = true;
        Ok(())
    }

    /// Re-reads `_NET_ACTIVE_WINDOW` and its fullscreen state, on focus changes and on geometry changes while
    /// it is fullscreen. No `PropertyChange` mask is ever put on foreign windows: applications rewrite their
    /// properties (user time, title, opaque region...) far too often for that to stay cheap.
    fn refresh_active(&mut self) {
        self.active_dirty = false;
        let conn = &*self.conn;
        let a = &self.atoms;
        let active = conn
            .get_property(
                false,
                self.root,
                a._NET_ACTIVE_WINDOW,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.next()))
            .unwrap_or(x11rb::NONE);
        self.active = active;
        self.active_fullscreen = None;
        if active == x11rb::NONE || self.is_ours(active) {
            self.pause_dirty = true;
            return;
        }
        let state = self.wm_state(active);
        if state.fullscreen && !state.hidden {
            let geom = conn.get_geometry(active).ok().and_then(|c| c.reply().ok());
            let pos = conn
                .translate_coordinates(active, self.root, 0, 0)
                .ok()
                .and_then(|c| c.reply().ok());
            if let (Some(g), Some(p)) = (geom, pos) {
                self.active_fullscreen = Some(Rect::new(
                    p.dst_x.into(),
                    p.dst_y.into(),
                    g.width.into(),
                    g.height.into(),
                ));
            }
        }
        self.pause_dirty = true;
    }

    /// Classifies a root child that could pause an output (a few requests, once per window).
    fn classify(&self, id: Window) -> Verdict {
        let conn = &*self.conn;
        let Some(attrs) = conn
            .get_window_attributes(id)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return Verdict::Ignored;
        };
        if attrs.class == WindowClass::INPUT_ONLY {
            return Verdict::Ignored;
        }
        // Same enumeration as the reference: windows QueryTree does not list (composite overlay) never count.
        let listed = conn
            .query_tree(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|t| t.children.contains(&id));
        if !listed {
            return Verdict::Ignored;
        }
        let desktop = conn
            .get_property(
                false,
                id,
                self.atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                0,
                32,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| {
                r.value32()
                    .map(|mut v| v.any(|t| t == self.atoms._NET_WM_WINDOW_TYPE_DESKTOP))
            })
            .unwrap_or(false);
        if desktop {
            Verdict::Ignored
        } else {
            Verdict::Counts
        }
    }

    /// Reads the EWMH state of a window (one round-trip, only on events).
    fn wm_state(&self, w: Window) -> WmState {
        let a = &self.atoms;
        let mut st = WmState::default();
        let reply = self
            .conn
            .get_property(false, w, a._NET_WM_STATE, AtomEnum::ATOM, 0, 64)
            .ok()
            .and_then(|c| c.reply().ok());
        if let Some(values) = reply.as_ref().and_then(|r| r.value32()) {
            let (mut vert, mut horz) = (false, false);
            for s in values {
                st.fullscreen |= s == a._NET_WM_STATE_FULLSCREEN;
                st.hidden |= s == a._NET_WM_STATE_HIDDEN;
                vert |= s == a._NET_WM_STATE_MAXIMIZED_VERT;
                horz |= s == a._NET_WM_STATE_MAXIMIZED_HORZ;
            }
            st.maximized = vert && horz;
        }
        st
    }

    fn recompute_pause(&mut self) {
        self.pause_dirty = false;
        let covered = self.config.pause_when_covered;
        let fullscreen = self.config.fullscreen.enabled;
        self.rects.clear();
        self.rects.extend(self.outputs.iter().map(|o| o.rect));

        let mut candidates = std::mem::take(&mut self.candidates);
        candidates.clear();
        candidates.extend(self.children.unclassified_candidates(
            &self.rects,
            &self.exclude,
            covered,
        ));
        for &id in &candidates {
            let verdict = self.classify(id);
            self.children.set_verdict(id, verdict);
        }
        // Windows that would pause an output: refresh their EWMH state (minimised windows may stay mapped
        // under Mutter/XWayland; maximised ones only hide their output). Usually zero to two windows.
        candidates.clear();
        candidates.extend(
            self.children
                .counted_candidates(&self.rects, &self.exclude, covered),
        );
        for &id in &candidates {
            let st = self.wm_state(id);
            self.children.set_wm_state(id, st.hidden, st.maximized);
        }
        self.candidates = candidates;

        self.scratch.clear();
        self.scratch.resize(self.outputs.len(), PauseReasons::NONE);
        self.children.reasons(
            &self.rects,
            &self.exclude,
            covered,
            fullscreen,
            &mut self.scratch,
        );
        if fullscreen
            && let Some(r) = self.active_fullscreen
            && let Some(i) = best_overlap(&r, self.rects.iter().copied())
        {
            self.scratch[i] |= PauseReasons::FULLSCREEN;
        }
        for (o, r) in self.outputs.iter().zip(self.scratch.iter_mut()) {
            if covered && o.obscured {
                *r |= PauseReasons::OCCLUDED;
            }
            if self.screensaver_on {
                *r |= PauseReasons::LOCKED;
            }
            if self.display_off {
                *r |= PauseReasons::DISPLAY_OFF;
            }
        }
        apply_scope(&mut self.scratch, self.config.fullscreen.scope);
        for (o, new) in self.outputs.iter_mut().zip(self.scratch.iter()) {
            if let Some(ev) = pause_transition(o.id, o.paused, *new) {
                self.queue.push(ev);
            }
            o.paused = *new;
        }
    }

    fn process_dirty(&mut self) {
        if self.outputs_dirty
            && let Err(e) = self.refresh_outputs(false)
        {
            tracing::error!("output refresh failed: {e}");
        }
        if self.active_dirty {
            self.refresh_active();
        }
        if self.pause_dirty {
            self.recompute_pause();
        }
    }

    /// Reads every buffered event. Returns `true` when at least one was handled.
    fn drain(&mut self) -> bool {
        let mut any = false;
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(ev)) => {
                    any = true;
                    self.handle_event(ev);
                }
                Ok(None) => return any,
                Err(e) => {
                    if self.lost.is_none() {
                        tracing::error!("X11 connection lost: {e}");
                        self.lost = Some(e.to_string());
                        self.queue.push(Event::CloseRequested);
                    }
                    return true;
                }
            }
        }
    }

    fn handle_event(&mut self, ev: XEvent) {
        tracing::trace!(event = ?ev, "x11 event");
        let root = self.root;
        match ev {
            XEvent::Expose(e) if e.count == 0 => {
                if let Some(o) = self.output_by_window(e.window) {
                    o.slot.requested = true;
                }
            }
            XEvent::CreateNotify(e) if e.parent == root => {
                self.children.created(
                    e.window,
                    Rect::new(e.x.into(), e.y.into(), e.width.into(), e.height.into()),
                );
            }
            XEvent::DestroyNotify(e) if e.event == root => {
                if self.children.destroyed(e.window) {
                    self.pause_dirty = true;
                }
                if e.window == self.active {
                    self.active_dirty = true;
                }
            }
            XEvent::MapNotify(e) if e.event == root => {
                self.pause_dirty |= self.children.set_mapped(e.window, true)
            }
            XEvent::UnmapNotify(e) if e.event == root => {
                self.pause_dirty |= self.children.set_mapped(e.window, false)
            }
            XEvent::ConfigureNotify(e) if e.event == root => {
                let rect = Rect::new(e.x.into(), e.y.into(), e.width.into(), e.height.into());
                self.pause_dirty |= self.children.configured(e.window, rect);
                if self.active_fullscreen.is_some() {
                    self.active_dirty = true;
                }
            }
            XEvent::ReparentNotify(e) => {
                if e.event == root {
                    if e.parent == root {
                        let size = self
                            .conn
                            .get_geometry(e.window)
                            .ok()
                            .and_then(|c| c.reply().ok());
                        let (w, h) = size.map_or((0, 0), |g| (g.width.into(), g.height.into()));
                        self.children
                            .created(e.window, Rect::new(e.x.into(), e.y.into(), w, h));
                    } else {
                        self.pause_dirty |= self.children.destroyed(e.window);
                    }
                } else if self.is_ours(e.window)
                    && e.parent != root
                    && !self.exclude.contains(&e.parent)
                {
                    // A reparenting window manager framed our window: its frame never counts as fullscreen.
                    self.exclude.push(e.parent);
                    self.pause_dirty = true;
                }
            }
            XEvent::PropertyNotify(e) => {
                if e.window == root && e.atom == self.atoms._NET_ACTIVE_WINDOW {
                    // Focus changes also cover minimise/restore, whose state lives in _NET_WM_STATE.
                    self.active_dirty = true;
                    self.pause_dirty = true;
                }
            }
            XEvent::VisibilityNotify(e) => {
                let obscured = e.state == Visibility::FULLY_OBSCURED;
                if let Some(o) = self.output_by_window(e.window)
                    && o.obscured != obscured
                {
                    o.obscured = obscured;
                    self.pause_dirty = true;
                }
            }
            XEvent::ButtonPress(e) | XEvent::ButtonRelease(e) => {
                let pressed = matches!(ev, XEvent::ButtonPress(_));
                let button = match e.detail {
                    1 => PointerButton::Left,
                    2 => PointerButton::Middle,
                    3 => PointerButton::Right,
                    4..=7 => return, // wheel
                    other => PointerButton::Other(other.into()),
                };
                if let Some(o) = self.outputs.iter().find(|o| o.window == e.event) {
                    self.queue.push(Event::Pointer(PointerEvent {
                        output: o.id,
                        x: e.event_x.into(),
                        y: e.event_y.into(),
                        kind: PointerKind::Button { button, pressed },
                    }));
                }
            }
            XEvent::RandrScreenChangeNotify(_) | XEvent::RandrNotify(_) => {
                self.outputs_dirty = true
            }
            XEvent::DpmsInfoNotify(e) => {
                self.display_off = e.state && e.power_level != dpms::DPMSMode::ON;
                self.pause_dirty = true;
            }
            XEvent::ScreensaverNotify(e) => {
                self.screensaver_on = e.state == screensaver::State::ON;
                self.pause_dirty = true;
            }
            XEvent::Error(e) => tracing::debug!("ignored X11 error: {e:?}"),
            _ => {}
        }
    }

    /// Pipelined `QueryPointer`: consumes the reply of the request sent at the previous frame and sends the
    /// next one. The reply has long arrived, so this never waits on the server.
    fn sample_pointer(&mut self) {
        let conn = &*self.conn;
        if let Some(seq) = self.pointer_seq.take() {
            match Cookie::<XCBConnection, QueryPointerReply>::new(conn, seq).reply() {
                Ok(r) if r.same_screen => self.cursor = Some((r.root_x.into(), r.root_y.into())),
                Ok(_) => {}
                Err(e) => tracing::debug!("QueryPointer failed: {e}"),
            }
        }
        if let Ok(cookie) = conn.query_pointer(self.root) {
            self.pointer_seq = Some(cookie.sequence_number());
            // Keep the reply pending (dropping the cookie would discard it); the cookie owns no memory.
            std::mem::forget(cookie);
        }
    }

    fn tick(&mut self, now: Instant) {
        let pointer = self.config.pointer != PointerMode::Disabled;
        let mut sampled = false;
        for i in 0..self.outputs.len() {
            let paused = !self.outputs[i].paused.is_empty();
            if !self.outputs[i].slot.fire_if_due(now, paused, true) {
                continue;
            }
            if pointer {
                if !sampled {
                    self.sample_pointer();
                    sampled = true;
                }
                let o = &mut self.outputs[i];
                if let Some((cx, cy)) = self.cursor {
                    // parity: position relative to the output, tracked over every window (§10.2, X11).
                    let local = (cx - o.rect.x, cy - o.rect.y);
                    if o.last_pointer != Some(local) {
                        o.last_pointer = Some(local);
                        self.queue.push(Event::Pointer(PointerEvent {
                            output: o.id,
                            x: local.0.into(),
                            y: local.1.into(),
                            kind: PointerKind::Motion,
                        }));
                    }
                }
            }
            self.queue.push(Event::Frame(self.outputs[i].id));
        }
    }

    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.outputs
            .iter()
            .filter_map(|o| o.slot.due(now, !o.paused.is_empty(), true))
            .min()
    }

    fn bury(&mut self) {
        if self.graveyard.is_empty() {
            return;
        }
        for w in self.graveyard.drain(..) {
            let _ = self.conn.destroy_window(w);
        }
        let _ = self.conn.flush();
    }
}

impl Drop for X11State {
    fn drop(&mut self) {
        self.graveyard.extend(self.outputs.iter().map(|o| o.window));
        self.bury();
    }
}

impl WallpaperSurface for X11Backend {
    fn backend(&self) -> BackendKind {
        BackendKind::X11
    }

    fn outputs(&self) -> &[OutputInfo] {
        &self.state.infos
    }

    fn raw_display_handle(&self) -> RawDisplayHandle {
        let ptr = NonNull::new(self.state.conn.get_raw_xcb_connection().cast::<c_void>());
        RawDisplayHandle::Xcb(XcbDisplayHandle::new(ptr, self.state.screen as i32))
    }

    fn raw_window_handle(&self, id: OutputId) -> Option<RawWindowHandle> {
        let o = self.state.outputs.iter().find(|o| o.id == id)?;
        let mut handle = XcbWindowHandle::new(NonZeroU32::new(o.window)?);
        handle.visual_id = NonZeroU32::new(self.state.root_visual);
        Some(RawWindowHandle::Xcb(handle))
    }

    fn request_frame(&mut self, id: OutputId) {
        if let Some(o) = self.state.outputs.iter_mut().find(|o| o.id == id) {
            o.slot.requested = true;
        }
    }

    fn pre_present(&mut self, _id: OutputId) {}

    fn set_max_fps(&mut self, fps: u32) {
        self.state.max_fps = fps;
        for o in &mut self.state.outputs {
            o.slot.clock.set_max_fps(fps);
        }
    }

    fn pause_reasons(&self, id: OutputId) -> PauseReasons {
        self.state
            .outputs
            .iter()
            .find(|o| o.id == id)
            .map_or(PauseReasons::NONE, |o| o.paused)
    }

    fn waker(&self) -> Waker {
        let ping = self.ping.clone();
        Waker::new(move || ping.ping())
    }

    fn wait(&mut self, wait: Wait) -> Result<(), PlatformError> {
        self.state.bury();
        let limit = wait.deadline();
        let mut dispatched = false;
        loop {
            self.state.drain();
            self.state.process_dirty();
            let now = Instant::now();
            self.state.tick(now);
            let _ = self.state.conn.flush();
            if !self.state.queue.is_empty() {
                return Ok(());
            }
            if let Some(reason) = self.state.lost.take() {
                return Err(PlatformError::ConnectionLost(reason));
            }
            if dispatched && limit.is_some_and(|l| now >= l) {
                return Ok(());
            }
            // Replies read above may have buffered events inside libxcb, invisible to epoll.
            if self.state.drain() {
                continue;
            }
            let timeout = timeout_until(min_deadline(self.state.next_deadline(now), limit), now);
            self.event_loop
                .dispatch(timeout, &mut self.state)
                .map_err(loop_error)?;
            dispatched = true;
        }
    }

    fn next_event(&mut self) -> Option<Event> {
        self.state.queue.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_refresh() {
        // 1920x1080@60: 148.5 MHz, 2200 x 1125.
        assert_eq!(
            refresh_mhz(148_500_000, 2200, 1125, false, false),
            Some(60_000)
        );
        assert_eq!(
            refresh_mhz(74_250_000, 2200, 1125, true, false),
            Some(60_000)
        );
        assert_eq!(
            refresh_mhz(148_500_000, 2200, 1125, false, true),
            Some(30_000)
        );
        assert_eq!(refresh_mhz(1, 0, 0, false, false), None);
    }
}
