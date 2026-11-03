//! Optional link with Lumenwall's GNOME Shell extension (`packaging/gnome-extension`).
//!
//! On GNOME the X11 window tree only shows XWayland clients, and Present starvation ([`crate::starvation`])
//! only sees an output once the compositor paints nothing of it. A maximised Wayland window leaves the strip
//! under the top bar visible (transparent with Blur my Shell and similar), so it never pauses anything. No
//! public GNOME interface says which monitor shows a fullscreen or maximised window (GNOME 50:
//! `org.gnome.Shell.Introspect` is denied outside unsafe mode, no foreign-toplevel protocol), so a small Shell
//! extension publishes it on the session bus:
//!
//! * name [`BUS_NAME`], object [`OBJECT_PATH`], interface [`INTERFACE`];
//! * `GetMonitors() -> a(siiiiu)` and the signal `MonitorsChanged(a(siiiiu))`, one entry per monitor:
//!   connector, logical x, y, width, height, flags ([`FLAG_FULLSCREEN`], [`FLAG_MAXIMIZED`]).
//!
//! XWayland only sees the pointer above X11 windows, and a desktop-icons window (Desktop Icons NG, a Wayland
//! client covering the desktop) takes every click on the desktop. Version 2 of the extension also publishes the
//! pointer, only to the connections that ask for it with `WatchPointer(u flags, u max_rate)` ([`WATCH_MOTION`],
//! [`WATCH_BUTTONS`]; 0 stops) and only while they are on the bus:
//!
//! * `PointerMotion(d x, d y, s connector)`: global logical position, at most once per compositor frame and
//!   `max_rate` per second, only when it changed (the current one right after `WatchPointer`);
//! * `PointerButton(d x, d y, s connector, u button, b pressed)`: presses that land on the desktop (no window,
//!   or a desktop-type window) and their releases; 1 left, 2 middle, 3 right.
//!
//! Both are unicast signals. The backend asks for them while at least one output runs, and converts them to
//! output coordinates with [`to_x11`].
//!
//! This is a minimal D-Bus client (SASL EXTERNAL, `Hello`, two match rules, two methods) on a non-blocking
//! socket watched by the backend's event loop: no thread, no timer, no wake-up while nothing changes. Without
//! the extension the connection only ever receives `NameOwnerChanged` for that one name.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::time::Duration;

use crate::geometry::Rect;

/// Well-known name owned by the extension.
pub(crate) const BUS_NAME: &str = "io.github.simnjs.Lumenwall.Shell";
/// Object exported by the extension.
pub(crate) const OBJECT_PATH: &str = "/io/github/simnjs/Lumenwall/Shell";
/// Interface of that object.
pub(crate) const INTERFACE: &str = "io.github.simnjs.Lumenwall.Shell1";
/// A fullscreen window is on top of the monitor (Mutter's own "in fullscreen" state).
pub(crate) const FLAG_FULLSCREEN: u32 = 1;
/// A maximised normal window is shown on the monitor (active workspace, not minimised).
pub(crate) const FLAG_MAXIMIZED: u32 = 2;
/// `WatchPointer` flag: pointer position.
pub(crate) const WATCH_MOTION: u32 = 1;
/// `WatchPointer` flag: presses on the desktop and their releases.
pub(crate) const WATCH_BUTTONS: u32 = 2;

const MONITORS_SIG: &str = "a(siiiiu)";
const MOTION_SIG: &str = "dds";
const BUTTON_SIG: &str = "ddsub";
const DBUS_NAME: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";

const METHOD_CALL: u8 = 1;
const METHOD_RETURN: u8 = 2;
const ERROR: u8 = 3;
const SIGNAL: u8 = 4;
const NO_AUTO_START: u8 = 0x2;

/// Largest message accepted (the protocol allows 128 MiB; ours are a few hundred bytes).
const MAX_MESSAGE: usize = 1 << 20;
/// Monitors accepted in one state message.
const MAX_MONITORS: usize = 64;

/// One monitor as the extension reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellMonitor {
    /// Connector (`DP-1`); empty when the Shell does not expose it (geometry is used then).
    pub(crate) connector: String,
    /// Logical geometry in the Shell's stage.
    pub(crate) rect: Rect,
    /// `FLAG_*` bits.
    pub(crate) flags: u32,
}

/// Pause flags of the output named `connector` with geometry `rect` (0 when unknown).
pub(crate) fn flags_for(monitors: &[ShellMonitor], connector: &str, rect: Rect) -> u32 {
    monitors
        .iter()
        .find(|m| !m.connector.is_empty() && m.connector == connector)
        .or_else(|| {
            monitors
                .iter()
                .find(|m| m.connector.is_empty() && m.rect == rect)
        })
        .map_or(0, |m| m.flags)
}

/// Pointer input published by the extension, in the Shell's global logical coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShellPointer {
    pub(crate) x: f64,
    pub(crate) y: f64,
    /// Index in [`ShellLink::monitors`] of the monitor named by the signal (`None`: unknown connector).
    pub(crate) monitor: Option<usize>,
    /// `Some((button, pressed))` for `PointerButton` (1 left, 2 middle, 3 right), `None` for a motion.
    pub(crate) button: Option<(u32, bool)>,
}

/// Converts a Shell position to X11 root coordinates.
///
/// `outputs` are the X11 outputs (RandR name, root rectangle). The Shell monitor under the pointer (the one
/// the signal names, else the one containing it) is matched with an output by connector, then by geometry,
/// and the position is scaled from the monitor's logical size to the output's size (fractional scaling with
/// XWayland's native scaling makes them differ). Without a match the coordinates are kept (scale 1: XWayland's
/// root is the Shell's logical space).
pub(crate) fn to_x11<'a>(
    monitors: &[ShellMonitor],
    p: &ShellPointer,
    outputs: impl Iterator<Item = (&'a str, Rect)> + Clone,
) -> (f64, f64) {
    let inside = |r: &Rect| {
        p.x >= f64::from(r.x)
            && p.y >= f64::from(r.y)
            && p.x < f64::from(r.right())
            && p.y < f64::from(r.bottom())
    };
    let monitor = p
        .monitor
        .and_then(|i| monitors.get(i))
        .filter(|m| inside(&m.rect))
        .or_else(|| monitors.iter().find(|m| inside(&m.rect)));
    let Some(m) = monitor.filter(|m| !m.rect.is_empty()) else {
        return (p.x, p.y);
    };
    let output = outputs
        .clone()
        .find(|(name, _)| !m.connector.is_empty() && *name == m.connector)
        .or_else(|| outputs.clone().find(|(_, r)| *r == m.rect));
    match output {
        Some((_, o)) => (
            f64::from(o.x)
                + (p.x - f64::from(m.rect.x)) * f64::from(o.width) / f64::from(m.rect.width),
            f64::from(o.y)
                + (p.y - f64::from(m.rect.y)) * f64::from(o.height) / f64::from(m.rect.height),
        ),
        None => (p.x, p.y),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Wire format (D-Bus specification, "Message Protocol"): little-endian writer, both-endian reader.

/// Value of a header field.
#[derive(Clone, Copy)]
enum Field<'a> {
    Path(&'a str),
    Str(&'a str),
    Sig(&'a str),
    /// Only replies carry one (reply serial); the client never sends replies.
    #[cfg(test)]
    U32(u32),
}

#[derive(Default)]
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn pad(&mut self, align: usize) {
        let len = self.buf.len().next_multiple_of(align);
        self.buf.resize(len, 0);
    }

    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.pad(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[cfg(test)]
    fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }

    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    fn signature(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    fn patch_u32(&mut self, at: usize, v: u32) {
        self.buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// Builds a whole message. Header fields use the standard codes (1 path, 2 interface, 3 member, 4 error
/// name, 5 reply serial, 6 destination, 7 sender, 8 signature).
fn message(
    kind: u8,
    flags: u8,
    serial: u32,
    fields: &[(u8, Field<'_>)],
    body: impl FnOnce(&mut Writer),
) -> Vec<u8> {
    let mut w = Writer::default();
    w.buf.extend_from_slice(&[b'l', kind, flags, 1]);
    w.u32(0); // body length, patched below
    w.u32(serial);
    w.u32(0); // header fields length, patched below
    let start = w.buf.len();
    for &(code, value) in fields {
        w.pad(8);
        w.u8(code);
        match value {
            Field::Path(p) => {
                w.signature("o");
                w.string(p);
            }
            Field::Str(s) => {
                w.signature("s");
                w.string(s);
            }
            Field::Sig(g) => {
                w.signature("g");
                w.signature(g);
            }
            #[cfg(test)]
            Field::U32(v) => {
                w.signature("u");
                w.u32(v);
            }
        }
    }
    let fields_len = (w.buf.len() - start) as u32;
    w.patch_u32(12, fields_len);
    w.pad(8);
    let body_start = w.buf.len();
    body(&mut w);
    let body_len = (w.buf.len() - body_start) as u32;
    w.patch_u32(4, body_len);
    w.buf
}

/// Arguments of a method call.
#[derive(Clone, Copy)]
enum Args<'a> {
    None,
    Str(&'a str),
    U32x2(u32, u32),
}

/// Method call.
fn method_call(
    serial: u32,
    flags: u8,
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    args: Args<'_>,
) -> Vec<u8> {
    let mut fields = vec![
        (1, Field::Path(path)),
        (2, Field::Str(interface)),
        (3, Field::Str(member)),
        (6, Field::Str(destination)),
    ];
    match args {
        Args::None => {}
        Args::Str(_) => fields.push((8, Field::Sig("s"))),
        Args::U32x2(..) => fields.push((8, Field::Sig("uu"))),
    }
    message(METHOD_CALL, flags, serial, &fields, |w| match args {
        Args::None => {}
        Args::Str(a) => w.string(a),
        Args::U32x2(a, b) => {
            w.u32(a);
            w.u32(b);
        }
    })
}

#[derive(Clone)]
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
    big: bool,
}

impl<'a> Reader<'a> {
    fn align(&mut self, n: usize) -> Option<()> {
        let p = self.pos.next_multiple_of(n);
        (p <= self.b.len()).then(|| self.pos = p)
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.b.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.align(4)?;
        let a: [u8; 4] = self.bytes(4)?.try_into().ok()?;
        Some(if self.big {
            u32::from_be_bytes(a)
        } else {
            u32::from_le_bytes(a)
        })
    }

    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }

    fn f64(&mut self) -> Option<f64> {
        self.align(8)?;
        let a: [u8; 8] = self.bytes(8)?.try_into().ok()?;
        Some(if self.big {
            f64::from_be_bytes(a)
        } else {
            f64::from_le_bytes(a)
        })
    }

    fn boolean(&mut self) -> Option<bool> {
        match self.u32()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn string(&mut self) -> Option<&'a str> {
        let n = self.u32()? as usize;
        let s = self.bytes(n)?;
        (self.u8()? == 0).then_some(())?;
        std::str::from_utf8(s).ok()
    }

    fn signature(&mut self) -> Option<&'a str> {
        let n = usize::from(self.u8()?);
        let s = self.bytes(n)?;
        (self.u8()? == 0).then_some(())?;
        std::str::from_utf8(s).ok()
    }
}

/// Length of the first message in `buf`: `Ok(None)` when incomplete, `Err` when it cannot be a message.
fn message_len(buf: &[u8]) -> Result<Option<usize>, ()> {
    if buf.len() < 16 {
        return Ok(None);
    }
    let big = match buf[0] {
        b'l' => false,
        b'B' => true,
        _ => return Err(()),
    };
    let word = |at: usize| {
        let a: [u8; 4] = buf[at..at + 4].try_into().unwrap_or_default();
        (if big {
            u32::from_be_bytes(a)
        } else {
            u32::from_le_bytes(a)
        }) as usize
    };
    let total = 16usize
        .checked_add(word(12))
        .map(|h| h.next_multiple_of(8))
        .and_then(|h| h.checked_add(word(4)))
        .ok_or(())?;
    if total > MAX_MESSAGE {
        Err(())
    } else if buf.len() < total {
        Ok(None)
    } else {
        Ok(Some(total))
    }
}

/// Header of a received message, with a reader on its body.
struct Message<'a> {
    kind: u8,
    reply_serial: u32,
    path: &'a str,
    interface: &'a str,
    member: &'a str,
    sender: &'a str,
    signature: &'a str,
    body: Reader<'a>,
}

/// Parses one whole message (`buf` is exactly its bytes).
fn parse(buf: &[u8]) -> Option<Message<'_>> {
    let mut r = Reader {
        b: buf,
        pos: 0,
        big: *buf.first()? == b'B',
    };
    let _endianness = r.u8()?;
    let kind = r.u8()?;
    let _flags = r.u8()?;
    let _version = r.u8()?;
    let _body_len = r.u32()?;
    let _serial = r.u32()?;
    let end = 16usize.checked_add(r.u32()? as usize)?;
    let mut m = Message {
        kind,
        reply_serial: 0,
        path: "",
        interface: "",
        member: "",
        sender: "",
        signature: "",
        body: r.clone(),
    };
    while r.pos < end {
        r.align(8)?;
        let code = r.u8()?;
        match r.signature()? {
            "o" | "s" => {
                let v = r.string()?;
                match code {
                    1 => m.path = v,
                    2 => m.interface = v,
                    3 => m.member = v,
                    7 => m.sender = v,
                    _ => {}
                }
            }
            "g" => {
                let v = r.signature()?;
                if code == 8 {
                    m.signature = v;
                }
            }
            "u" => {
                let v = r.u32()?;
                if code == 5 {
                    m.reply_serial = v;
                }
            }
            _ => return None,
        }
    }
    r.pos = end;
    r.align(8)?;
    m.body = r;
    Some(m)
}

/// Body of `GetMonitors` / `MonitorsChanged`.
fn read_monitors(r: &mut Reader<'_>) -> Option<Vec<ShellMonitor>> {
    let len = r.u32()? as usize;
    r.align(8)?;
    let end = r.pos.checked_add(len)?;
    if end > r.b.len() {
        return None;
    }
    let mut out = Vec::new();
    while r.pos < end {
        r.align(8)?;
        let connector = r.string()?.to_owned();
        let (x, y, w, h) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
        let flags = r.u32()?;
        out.push(ShellMonitor {
            connector,
            rect: Rect::new(x, y, w, h),
            flags,
        });
        if out.len() > MAX_MONITORS {
            return None;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------------------
// Protocol state (socket-independent, unit-tested).

/// Pointer subscription sent with `WatchPointer`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Watch {
    /// `WATCH_*` bits; 0: none.
    flags: u32,
    /// Signals per second at most (0: once per compositor frame).
    rate: u32,
}

/// Pointer signals kept between two reads at most (a burst of motions beyond it is dropped, never a button).
const MAX_POINTER_EVENTS: usize = 256;

/// Conversation with the bus: outgoing bytes in `out`, received bytes through [`Protocol::feed`].
struct Protocol {
    serial: u32,
    /// Serial of the pending `GetMonitors` call (0: none).
    get_serial: u32,
    /// `None` until the first answer; then whether the extension runs.
    present: Option<bool>,
    monitors: Vec<ShellMonitor>,
    /// Unique bus name of the extension (pointer signals from any other connection are ignored).
    owner: String,
    /// Pointer subscription the backend wants.
    watch_want: Watch,
    /// Subscription sent to the current owner (`None`: nothing yet).
    watch_sent: Option<Watch>,
    /// Serial of the pending `WatchPointer` call (0: none).
    watch_serial: u32,
    /// `None` until the extension answered `WatchPointer`; `Some(false)`: it predates version 2.
    watch_ok: Option<bool>,
    /// Pointer signals not taken yet (reused, see [`ShellLink::pointer_events`]).
    pointer: Vec<ShellPointer>,
    rbuf: Vec<u8>,
    out: Vec<u8>,
}

impl Protocol {
    /// Starts the conversation: `Hello`, the two match rules, then the first `GetMonitors`.
    fn new() -> Self {
        let mut p = Self {
            serial: 0,
            get_serial: 0,
            present: None,
            monitors: Vec::new(),
            owner: String::new(),
            watch_want: Watch::default(),
            watch_sent: None,
            watch_serial: 0,
            watch_ok: None,
            pointer: Vec::with_capacity(16),
            rbuf: Vec::new(),
            out: Vec::new(),
        };
        p.call(DBUS_NAME, DBUS_PATH, DBUS_NAME, "Hello", Args::None);
        // Every signal of the interface: `MonitorsChanged` is broadcast; the pointer signals are sent to us
        // only, which the bus delivers without a rule (kept in the rule should a bus require it).
        let state = format!(
            "type='signal',sender='{BUS_NAME}',path='{OBJECT_PATH}',interface='{INTERFACE}'"
        );
        let owner = format!(
            "type='signal',sender='{DBUS_NAME}',path='{DBUS_PATH}',interface='{DBUS_NAME}',member='NameOwnerChanged',arg0='{BUS_NAME}'"
        );
        p.call(
            DBUS_NAME,
            DBUS_PATH,
            DBUS_NAME,
            "AddMatch",
            Args::Str(&state),
        );
        p.call(
            DBUS_NAME,
            DBUS_PATH,
            DBUS_NAME,
            "AddMatch",
            Args::Str(&owner),
        );
        p.request_monitors();
        p
    }

    fn call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        args: Args<'_>,
    ) -> u32 {
        self.serial = self.serial.wrapping_add(1).max(1);
        let msg = method_call(
            self.serial,
            NO_AUTO_START,
            destination,
            path,
            interface,
            member,
            args,
        );
        self.out.extend_from_slice(&msg);
        self.serial
    }

    fn request_monitors(&mut self) {
        self.get_serial = self.call(BUS_NAME, OBJECT_PATH, INTERFACE, "GetMonitors", Args::None);
    }

    /// Records the subscription the backend wants and sends it when the extension runs.
    fn set_watch(&mut self, want: Watch) {
        self.watch_want = want;
        self.sync_watch();
    }

    /// Sends `WatchPointer` when the extension runs, supports it, and was not told this already.
    fn sync_watch(&mut self) {
        if self.present != Some(true) || self.watch_ok == Some(false) {
            return;
        }
        let want = self.watch_want;
        if self.watch_sent == Some(want) || (self.watch_sent.is_none() && want.flags == 0) {
            return;
        }
        self.watch_serial = self.call(
            BUS_NAME,
            OBJECT_PATH,
            INTERFACE,
            "WatchPointer",
            Args::U32x2(want.flags, want.rate),
        );
        self.watch_sent = Some(want);
    }

    /// `WATCH_*` bits the extension currently delivers (0 without a version-2 extension).
    fn pointer_feed(&self) -> u32 {
        if self.present == Some(true) && self.watch_ok == Some(true) {
            self.watch_sent.map_or(0, |w| w.flags)
        } else {
            0
        }
    }

    /// The extension's connection changed (`new` empty: gone).
    fn new_owner(&mut self, new: &str) {
        new.clone_into(&mut self.owner);
        self.watch_sent = None;
        self.watch_serial = 0;
        self.watch_ok = None;
        self.pointer.clear();
    }

    /// Consumes received bytes. Returns `true` when the published state changed.
    fn feed(&mut self, data: &[u8]) -> Result<bool, ()> {
        self.rbuf.extend_from_slice(data);
        let mut changed = false;
        let mut start = 0;
        while let Some(n) = message_len(&self.rbuf[start..])? {
            let bytes = std::mem::take(&mut self.rbuf);
            changed |= self.handle(&bytes[start..start + n]);
            self.rbuf = bytes;
            start += n;
        }
        self.rbuf.drain(..start);
        // The extension (re)appeared or the wanted subscription changed meanwhile.
        self.sync_watch();
        Ok(changed)
    }

    fn handle(&mut self, bytes: &[u8]) -> bool {
        let Some(m) = parse(bytes) else { return false };
        let answer = self.get_serial != 0 && m.reply_serial == self.get_serial;
        let watch_answer = self.watch_serial != 0 && m.reply_serial == self.watch_serial;
        match m.kind {
            METHOD_RETURN if answer => {
                self.get_serial = 0;
                let mut body = m.body;
                let monitors = (m.signature == MONITORS_SIG)
                    .then(|| read_monitors(&mut body))
                    .flatten();
                match monitors {
                    Some(v) => {
                        if self.owner != m.sender {
                            self.new_owner(m.sender);
                        }
                        self.set(true, v)
                    }
                    None => self.set(false, Vec::new()),
                }
            }
            // ServiceUnknown / NameHasNoOwner / UnknownMethod: no (compatible) extension.
            ERROR if answer => {
                self.get_serial = 0;
                self.set(false, Vec::new())
            }
            METHOD_RETURN if watch_answer => {
                self.watch_serial = 0;
                self.watch_ok = Some(true);
                false
            }
            // UnknownMethod: version 1 of the extension (state only).
            ERROR if watch_answer => {
                self.watch_serial = 0;
                self.watch_ok = Some(false);
                false
            }
            SIGNAL
                if m.member == "MonitorsChanged"
                    && m.interface == INTERFACE
                    && m.path == OBJECT_PATH
                    && m.signature == MONITORS_SIG =>
            {
                if self.owner.is_empty() {
                    self.new_owner(m.sender);
                }
                let mut body = m.body;
                read_monitors(&mut body).is_some_and(|v| self.set(true, v))
            }
            SIGNAL
                if m.interface == INTERFACE
                    && m.path == OBJECT_PATH
                    && !self.owner.is_empty()
                    && m.sender == self.owner =>
            {
                let mut body = m.body;
                self.pointer_signal(m.member, m.signature, &mut body);
                false
            }
            SIGNAL
                if m.member == "NameOwnerChanged"
                    && m.sender == DBUS_NAME
                    && m.interface == DBUS_NAME
                    && m.signature == "sss" =>
            {
                let mut body = m.body;
                let (Some(name), Some(_old), Some(new)) =
                    (body.string(), body.string(), body.string())
                else {
                    return false;
                };
                if name != BUS_NAME {
                    false
                } else if new.is_empty() {
                    self.new_owner("");
                    self.set(false, Vec::new())
                } else {
                    self.new_owner(new);
                    self.request_monitors();
                    false
                }
            }
            _ => false,
        }
    }

    /// `PointerMotion` / `PointerButton`, kept only while subscribed to them.
    fn pointer_signal(&mut self, member: &str, signature: &str, body: &mut Reader<'_>) {
        let feed = self.watch_sent.map_or(0, |w| w.flags);
        let button = match (member, signature) {
            ("PointerMotion", MOTION_SIG) if feed & WATCH_MOTION != 0 => None,
            ("PointerButton", BUTTON_SIG) if feed & WATCH_BUTTONS != 0 => Some(()),
            _ => return,
        };
        let (Some(x), Some(y), Some(connector)) = (body.f64(), body.f64(), body.string()) else {
            return;
        };
        let button = match button {
            None => None,
            Some(()) => match (body.u32(), body.boolean()) {
                (Some(b), Some(pressed)) => Some((b, pressed)),
                _ => return,
            },
        };
        if !(x.is_finite() && y.is_finite())
            || (button.is_none() && self.pointer.len() >= MAX_POINTER_EVENTS)
        {
            return;
        }
        let monitor = self
            .monitors
            .iter()
            .position(|m| !connector.is_empty() && m.connector == connector);
        self.pointer.push(ShellPointer {
            x,
            y,
            monitor,
            button,
        });
    }

    fn set(&mut self, present: bool, monitors: Vec<ShellMonitor>) -> bool {
        let changed = self.present != Some(present) || self.monitors != monitors;
        self.present = Some(present);
        self.monitors = monitors;
        changed
    }
}

// ---------------------------------------------------------------------------------------------------------
// Socket.

/// Session-bus connection waiting for the extension.
pub(crate) struct ShellLink {
    stream: UnixStream,
    proto: Protocol,
}

impl ShellLink {
    /// Connects to the session bus and asks for the extension's state (answered asynchronously).
    pub(crate) fn connect() -> io::Result<Self> {
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .filter(|a| !a.is_empty())
            .or_else(|| {
                std::env::var("XDG_RUNTIME_DIR")
                    .ok()
                    .filter(|d| !d.is_empty())
                    .map(|d| format!("unix:path={d}/bus"))
            })
            .ok_or_else(|| io::Error::other("no session bus address"))?;
        let mut stream = connect_address(&address)?;
        // Only the handshake blocks, briefly; the bus answers in microseconds.
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        authenticate(&mut stream)?;
        let mut proto = Protocol::new();
        stream.write_all(&proto.out)?;
        proto.out.clear();
        stream.set_nonblocking(true)?;
        Ok(Self { stream, proto })
    }

    /// Another handle on the socket, for the event loop's readiness.
    pub(crate) fn watch_handle(&self) -> io::Result<UnixStream> {
        self.stream.try_clone()
    }

    /// Reads everything the bus sent. `Ok(true)` when the published state changed; `Err` when the
    /// connection is gone (drop the link).
    pub(crate) fn read(&mut self) -> io::Result<bool> {
        let mut buf = [0u8; 4096];
        let mut changed = false;
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    changed |= self.proto.feed(&buf[..n]).map_err(|()| {
                        io::Error::new(io::ErrorKind::InvalidData, "malformed D-Bus message")
                    })?
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.flush()?;
        Ok(changed)
    }

    fn flush(&mut self) -> io::Result<()> {
        while !self.proto.out.is_empty() {
            match self.stream.write(&self.proto.out) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.proto.out.drain(..n);
                }
                // A few hundred bytes never fill the socket buffer; if they did, the rest goes out with the
                // next read.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// `None` until the bus answered; then whether the extension runs.
    pub(crate) fn present(&self) -> Option<bool> {
        self.proto.present
    }

    /// Latest state published by the extension (empty when absent).
    pub(crate) fn monitors(&self) -> &[ShellMonitor] {
        &self.proto.monitors
    }

    /// Asks the extension for the pointer (`WATCH_*` bits, 0 stops; `rate`: signals per second at most, 0 for
    /// one per compositor frame). Sent now if the extension runs, else as soon as it appears.
    pub(crate) fn set_pointer_watch(&mut self, flags: u32, rate: u32) -> io::Result<()> {
        self.proto.set_watch(Watch { flags, rate });
        self.flush()
    }

    /// `WATCH_*` bits the extension currently delivers (0 without a version-2 extension).
    pub(crate) fn pointer_feed(&self) -> u32 {
        self.proto.pointer_feed()
    }

    /// `Some(false)` when the running extension predates the pointer signals.
    pub(crate) fn pointer_supported(&self) -> Option<bool> {
        self.proto.watch_ok
    }

    /// Pointer signals received since the last call, with the monitors their indices refer to.
    pub(crate) fn pointer_events(
        &mut self,
    ) -> (&[ShellMonitor], std::vec::Drain<'_, ShellPointer>) {
        (&self.proto.monitors, self.proto.pointer.drain(..))
    }
}

/// Decodes the `%XX` escapes of a D-Bus address value.
fn unescape(v: &str) -> Vec<u8> {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%')
            .then(|| b.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    out
}

/// Connects to the first usable `unix:path=` / `unix:abstract=` entry of a bus address.
fn connect_address(address: &str) -> io::Result<UnixStream> {
    let mut last = io::Error::other(format!("unsupported session bus address {address:?}"));
    for entry in address.split(';') {
        let Some(params) = entry.strip_prefix("unix:") else {
            continue;
        };
        for (key, value) in params.split(',').filter_map(|kv| kv.split_once('=')) {
            let value = unescape(value);
            let result = match key {
                "path" => UnixStream::connect(OsStr::from_bytes(&value)),
                "abstract" => SocketAddr::from_abstract_name(&value)
                    .and_then(|a| UnixStream::connect_addr(&a)),
                _ => continue,
            };
            match result {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
        }
    }
    Err(last)
}

/// SASL `EXTERNAL` with our uid (the bus checks it against the socket's peer credentials).
fn authenticate(stream: &mut UnixStream) -> io::Result<()> {
    let uid = rustix::process::getuid().as_raw().to_string();
    let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
    stream.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
    // The server sends nothing else before BEGIN, so reading byte by byte never over-reads.
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        if line.len() > 512 || stream.read(&mut byte)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        line.push(byte[0]);
    }
    if !line.starts_with(b"OK ") {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "D-Bus authentication refused: {}",
                String::from_utf8_lossy(&line).trim_end()
            ),
        ));
    }
    stream.write_all(b"BEGIN\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitors_body(w: &mut Writer, monitors: &[(&str, [i32; 4], u32)]) {
        w.u32(0);
        let len_at = w.buf.len() - 4;
        w.pad(8);
        let start = w.buf.len();
        for (name, [x, y, ww, h], flags) in monitors {
            w.pad(8);
            w.string(name);
            w.i32(*x);
            w.i32(*y);
            w.i32(*ww);
            w.i32(*h);
            w.u32(*flags);
        }
        let len = (w.buf.len() - start) as u32;
        w.patch_u32(len_at, len);
    }

    fn state_signal(monitors: &[(&str, [i32; 4], u32)]) -> Vec<u8> {
        message(
            SIGNAL,
            0,
            7,
            &[
                (1, Field::Path(OBJECT_PATH)),
                (2, Field::Str(INTERFACE)),
                (3, Field::Str("MonitorsChanged")),
                (7, Field::Str(":1.42")),
                (8, Field::Sig(MONITORS_SIG)),
            ],
            |w| monitors_body(w, monitors),
        )
    }

    fn reply(kind: u8, to: u32, monitors: Option<&[(&str, [i32; 4], u32)]>) -> Vec<u8> {
        let mut fields = vec![(5, Field::U32(to)), (7, Field::Str(":1.42"))];
        if monitors.is_some() {
            fields.push((8, Field::Sig(MONITORS_SIG)));
        }
        if kind == ERROR {
            fields.push((4, Field::Str("org.freedesktop.DBus.Error.ServiceUnknown")));
        }
        message(kind, 0, 9, &fields, |w| {
            if let Some(m) = monitors {
                monitors_body(w, m);
            }
        })
    }

    fn owner_changed(name: &str, new: &str) -> Vec<u8> {
        message(
            SIGNAL,
            0,
            3,
            &[
                (1, Field::Path(DBUS_PATH)),
                (2, Field::Str(DBUS_NAME)),
                (3, Field::Str("NameOwnerChanged")),
                (7, Field::Str(DBUS_NAME)),
                (8, Field::Sig("sss")),
            ],
            |w| {
                w.string(name);
                w.string("");
                w.string(new);
            },
        )
    }

    /// Unicast pointer signal from `sender`.
    fn pointer_msg(
        sender: &str,
        x: f64,
        y: f64,
        connector: &str,
        button: Option<(u32, bool)>,
    ) -> Vec<u8> {
        let (member, sig) = match button {
            None => ("PointerMotion", MOTION_SIG),
            Some(_) => ("PointerButton", BUTTON_SIG),
        };
        message(
            SIGNAL,
            0,
            11,
            &[
                (1, Field::Path(OBJECT_PATH)),
                (2, Field::Str(INTERFACE)),
                (3, Field::Str(member)),
                (6, Field::Str(":1.7")),
                (7, Field::Str(sender)),
                (8, Field::Sig(sig)),
            ],
            |w| {
                w.pad(8);
                w.buf.extend_from_slice(&x.to_le_bytes());
                w.buf.extend_from_slice(&y.to_le_bytes());
                w.string(connector);
                if let Some((b, pressed)) = button {
                    w.u32(b);
                    w.u32(u32::from(pressed));
                }
            },
        )
    }

    /// Members of the method calls queued in `out` (cleared), with their `uu` arguments when they have them.
    fn sent_calls(p: &mut Protocol) -> Vec<(String, Option<(u32, u32)>)> {
        let mut calls = Vec::new();
        let mut at = 0;
        while let Ok(Some(n)) = message_len(&p.out[at..]) {
            let m = parse(&p.out[at..at + n]).unwrap();
            let mut body = m.body;
            let args = (m.signature == "uu").then(|| (body.u32().unwrap(), body.u32().unwrap()));
            calls.push((m.member.to_owned(), args));
            at += n;
        }
        p.out.clear();
        calls
    }

    /// A protocol whose extension (`:1.42`) answered `GetMonitors` with two monitors.
    fn with_extension() -> Protocol {
        let mut p = Protocol::new();
        p.out.clear();
        let answer = reply(
            METHOD_RETURN,
            4,
            Some(&[
                ("DP-4", [2560, 0, 2560, 1440], 0),
                ("DP-5", [0, 0, 2560, 1440], 0),
            ]),
        );
        assert_eq!(p.feed(&answer), Ok(true));
        assert_eq!(p.owner, ":1.42");
        p
    }

    #[test]
    fn method_call_layout() {
        let m = method_call(
            3,
            NO_AUTO_START,
            DBUS_NAME,
            DBUS_PATH,
            DBUS_NAME,
            "AddMatch",
            Args::Str("type='signal'"),
        );
        assert_eq!(&m[..4], &[b'l', METHOD_CALL, NO_AUTO_START, 1]);
        assert_eq!(message_len(&m), Ok(Some(m.len())));
        assert_eq!(message_len(&m[..m.len() - 1]), Ok(None));
        let p = parse(&m).unwrap();
        assert_eq!(
            (p.kind, p.path, p.interface, p.member),
            (METHOD_CALL, DBUS_PATH, DBUS_NAME, "AddMatch")
        );
        assert_eq!(p.signature, "s");
        let mut body = p.body;
        assert_eq!(body.string(), Some("type='signal'"));
        assert_eq!(body.pos, m.len());
        // Body offset is 8-aligned and the declared body length matches.
        let body_len = u32::from_le_bytes(m[4..8].try_into().unwrap()) as usize;
        assert_eq!((m.len() - body_len) % 8, 0);
    }

    #[test]
    fn big_endian_header() {
        // `Hello` reply in big-endian: kind 2, serial 1, reply serial 1, signature "s", body ":1.5".
        let mut m = vec![b'B', METHOD_RETURN, 0, 1];
        m.extend_from_slice(&9u32.to_be_bytes()); // body: u32 len + ":1.5\0"
        m.extend_from_slice(&1u32.to_be_bytes());
        let fields_at = m.len();
        m.extend_from_slice(&0u32.to_be_bytes());
        m.extend_from_slice(&[5, 1, b'u', 0]);
        m.extend_from_slice(&1u32.to_be_bytes());
        m.extend_from_slice(&[8, 1, b'g', 0, 1, b's', 0]);
        let flen = (m.len() - 16) as u32;
        m[fields_at..fields_at + 4].copy_from_slice(&flen.to_be_bytes());
        m.resize(m.len().next_multiple_of(8), 0);
        m.extend_from_slice(&4u32.to_be_bytes());
        m.extend_from_slice(b":1.5\0");
        assert_eq!(message_len(&m), Ok(Some(m.len())));
        let p = parse(&m).unwrap();
        assert_eq!(
            (p.kind, p.reply_serial, p.signature),
            (METHOD_RETURN, 1, "s")
        );
        let mut b = p.body;
        assert_eq!(b.string(), Some(":1.5"));
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(message_len(b"xxxxxxxxxxxxxxxxxxxx"), Err(()));
        let mut huge = vec![b'l', SIGNAL, 0, 1];
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        huge.extend_from_slice(&[0; 8]);
        assert_eq!(message_len(&huge), Err(()));
        // A truncated body never reads out of bounds.
        let mut s = state_signal(&[("DP-4", [0, 0, 1, 1], 1)]);
        let body_len = u32::from_le_bytes(s[4..8].try_into().unwrap()) as usize;
        let len_at = s.len() - body_len; // the array length is the first word of the body
        s[len_at..len_at + 4].copy_from_slice(&1000u32.to_le_bytes());
        let p = parse(&s).unwrap();
        let mut b = p.body;
        assert_eq!(read_monitors(&mut b), None);
    }

    #[test]
    fn startup_asks_for_the_state() {
        let p = Protocol::new();
        let mut members = Vec::new();
        let mut at = 0;
        while let Ok(Some(n)) = message_len(&p.out[at..]) {
            let m = parse(&p.out[at..at + n]).unwrap();
            members.push(m.member.to_owned());
            at += n;
        }
        assert_eq!(at, p.out.len());
        assert_eq!(members, ["Hello", "AddMatch", "AddMatch", "GetMonitors"]);
        assert_eq!(p.get_serial, 4);
        assert_eq!(p.present, None);
    }

    #[test]
    fn extension_absent_then_appears_then_leaves() {
        let mut p = Protocol::new();
        p.out.clear();
        // Unrelated replies (Hello, AddMatch) change nothing.
        assert_eq!(p.feed(&reply(METHOD_RETURN, 1, None)), Ok(false));
        assert_eq!(p.feed(&reply(ERROR, 4, None)), Ok(true));
        assert_eq!(p.present, Some(false));
        assert_eq!(p.get_serial, 0);
        // Other names are ignored.
        assert_eq!(p.feed(&owner_changed("org.example", ":1.9")), Ok(false));
        assert!(p.out.is_empty());
        // The extension starts: ask again, then the answer arrives split in two reads.
        assert_eq!(p.feed(&owner_changed(BUS_NAME, ":1.42")), Ok(false));
        let ask = parse(&p.out).unwrap();
        assert_eq!((ask.member, ask.interface), ("GetMonitors", INTERFACE));
        let serial = p.get_serial;
        let answer = reply(
            METHOD_RETURN,
            serial,
            Some(&[
                ("DP-4", [2560, 0, 2560, 1440], FLAG_MAXIMIZED),
                ("DP-5", [0, 0, 2560, 1440], 0),
            ]),
        );
        let (a, b) = answer.split_at(answer.len() / 2);
        assert_eq!(p.feed(a), Ok(false));
        assert_eq!(p.feed(b), Ok(true));
        assert_eq!(p.present, Some(true));
        assert_eq!(p.monitors.len(), 2);
        assert_eq!(
            p.monitors[0],
            ShellMonitor {
                connector: "DP-4".into(),
                rect: Rect::new(2560, 0, 2560, 1440),
                flags: FLAG_MAXIMIZED
            }
        );
        // Two signals in one read; the second one repeats the state.
        let mut two = state_signal(&[("DP-4", [2560, 0, 2560, 1440], FLAG_FULLSCREEN)]);
        two.extend(state_signal(&[(
            "DP-4",
            [2560, 0, 2560, 1440],
            FLAG_FULLSCREEN,
        )]));
        assert_eq!(p.feed(&two), Ok(true));
        assert_eq!(p.monitors[0].flags, FLAG_FULLSCREEN);
        assert_eq!(
            p.feed(&state_signal(&[(
                "DP-4",
                [2560, 0, 2560, 1440],
                FLAG_FULLSCREEN
            )])),
            Ok(false)
        );
        assert!(p.rbuf.is_empty());
        // The Shell locks (extensions disabled) or the extension is turned off.
        assert_eq!(p.feed(&owner_changed(BUS_NAME, "")), Ok(true));
        assert_eq!(p.present, Some(false));
        assert!(p.monitors.is_empty());
        assert_eq!(p.feed(b"not a message at all"), Err(()));
    }

    #[test]
    fn monitors_map_by_connector_then_geometry() {
        let m = vec![
            ShellMonitor {
                connector: "DP-4".into(),
                rect: Rect::new(2560, 0, 2560, 1440),
                flags: FLAG_MAXIMIZED,
            },
            ShellMonitor {
                connector: String::new(),
                rect: Rect::new(0, 0, 2560, 1440),
                flags: FLAG_FULLSCREEN,
            },
        ];
        assert_eq!(flags_for(&m, "DP-4", Rect::new(0, 0, 1, 1)), FLAG_MAXIMIZED);
        assert_eq!(
            flags_for(&m, "DP-5", Rect::new(0, 0, 2560, 1440)),
            FLAG_FULLSCREEN
        );
        assert_eq!(flags_for(&m, "HDMI-1", Rect::new(9, 9, 9, 9)), 0);
        assert_eq!(flags_for(&[], "DP-4", Rect::new(2560, 0, 2560, 1440)), 0);
    }

    #[test]
    fn pointer_watch_follows_the_extension() {
        let mut p = Protocol::new();
        p.out.clear();
        // Wanted before the extension answered: nothing sent yet.
        p.set_watch(Watch {
            flags: WATCH_MOTION | WATCH_BUTTONS,
            rate: 30,
        });
        assert!(p.out.is_empty());
        assert_eq!(p.pointer_feed(), 0);
        let answer = reply(METHOD_RETURN, 4, Some(&[("DP-4", [0, 0, 2560, 1440], 0)]));
        assert_eq!(p.feed(&answer), Ok(true));
        assert_eq!(
            sent_calls(&mut p),
            [("WatchPointer".to_owned(), Some((3, 30)))]
        );
        // Not delivered before the extension accepted it.
        assert_eq!(p.pointer_feed(), 0);
        let serial = p.watch_serial;
        assert_eq!(p.feed(&reply(METHOD_RETURN, serial, None)), Ok(false));
        assert_eq!(p.pointer_feed(), WATCH_MOTION | WATCH_BUTTONS);
        // Same subscription again: nothing sent. Stop: one call with 0, then nothing more.
        p.set_watch(Watch { flags: 3, rate: 30 });
        assert!(p.out.is_empty());
        p.set_watch(Watch::default());
        assert_eq!(
            sent_calls(&mut p),
            [("WatchPointer".to_owned(), Some((0, 0)))]
        );
        assert_eq!(p.pointer_feed(), 0);
        p.set_watch(Watch::default());
        assert!(p.out.is_empty());
        // The extension restarts (disabled while the screen is locked, then enabled again): the new
        // connection gets the subscription again once it answers.
        p.set_watch(Watch {
            flags: WATCH_MOTION,
            rate: 0,
        });
        sent_calls(&mut p);
        assert_eq!(p.feed(&owner_changed(BUS_NAME, "")), Ok(true));
        assert_eq!(p.pointer_feed(), 0);
        assert_eq!(p.feed(&owner_changed(BUS_NAME, ":1.80")), Ok(false));
        assert_eq!(sent_calls(&mut p), [("GetMonitors".to_owned(), None)]);
        let get = p.get_serial;
        let mut answer = reply(METHOD_RETURN, get, Some(&[("DP-4", [0, 0, 2560, 1440], 0)]));
        // Answered by the new connection.
        let at = answer.windows(5).position(|w| w == b":1.42").unwrap();
        answer[at..at + 5].copy_from_slice(b":1.80");
        assert_eq!(p.feed(&answer), Ok(true));
        assert_eq!(p.owner, ":1.80");
        assert_eq!(
            sent_calls(&mut p),
            [("WatchPointer".to_owned(), Some((1, 0)))]
        );
    }

    #[test]
    fn old_extension_keeps_the_state_only() {
        let mut p = with_extension();
        p.set_watch(Watch { flags: 3, rate: 60 });
        assert_eq!(sent_calls(&mut p).len(), 1);
        let serial = p.watch_serial;
        // UnknownMethod from version 1.
        assert_eq!(p.feed(&reply(ERROR, serial, None)), Ok(false));
        assert_eq!(p.watch_ok, Some(false));
        assert_eq!(p.present, Some(true));
        assert_eq!(p.pointer_feed(), 0);
        // Never asked again while that extension runs.
        p.set_watch(Watch { flags: 1, rate: 60 });
        assert!(p.out.is_empty());
    }

    #[test]
    fn pointer_signals() {
        let mut p = with_extension();
        // Not subscribed: dropped.
        assert_eq!(
            p.feed(&pointer_msg(":1.42", 1.0, 2.0, "DP-4", None)),
            Ok(false)
        );
        assert!(p.pointer.is_empty());
        p.set_watch(Watch {
            flags: WATCH_MOTION | WATCH_BUTTONS,
            rate: 0,
        });
        sent_calls(&mut p);
        let serial = p.watch_serial;
        p.feed(&reply(METHOD_RETURN, serial, None)).unwrap();
        let mut bytes = pointer_msg(":1.42", 3000.5, 20.25, "DP-4", None);
        bytes.extend(pointer_msg(":1.42", 10.0, 1439.0, "DP-5", Some((1, true))));
        bytes.extend(pointer_msg(":1.42", 10.0, 1439.0, "", Some((3, false))));
        // Another connection cannot inject input.
        bytes.extend(pointer_msg(":1.99", 5.0, 5.0, "DP-5", Some((1, true))));
        assert_eq!(p.feed(&bytes), Ok(false));
        let (monitors, events) = (p.monitors.clone(), p.pointer.clone());
        assert_eq!(monitors.len(), 2);
        assert_eq!(
            events,
            [
                ShellPointer {
                    x: 3000.5,
                    y: 20.25,
                    monitor: Some(0),
                    button: None
                },
                ShellPointer {
                    x: 10.0,
                    y: 1439.0,
                    monitor: Some(1),
                    button: Some((1, true))
                },
                ShellPointer {
                    x: 10.0,
                    y: 1439.0,
                    monitor: None,
                    button: Some((3, false))
                },
            ]
        );
        // Motions only: buttons are dropped after the subscription changed.
        p.pointer.clear();
        p.set_watch(Watch {
            flags: WATCH_MOTION,
            rate: 0,
        });
        assert_eq!(
            p.feed(&pointer_msg(":1.42", 1.0, 1.0, "DP-5", Some((1, true)))),
            Ok(false)
        );
        assert!(p.pointer.is_empty());
        // A flood of motions is capped; buttons always pass.
        p.set_watch(Watch { flags: 3, rate: 0 });
        let mut flood = Vec::new();
        for i in 0..300 {
            flood.extend(pointer_msg(":1.42", f64::from(i), 0.0, "DP-5", None));
        }
        flood.extend(pointer_msg(":1.42", 1.0, 1.0, "DP-5", Some((1, true))));
        p.feed(&flood).unwrap();
        assert_eq!(p.pointer.len(), MAX_POINTER_EVENTS + 1);
        assert_eq!(p.pointer.last().unwrap().button, Some((1, true)));
        // Gone: nothing is delivered any more.
        p.feed(&owner_changed(BUS_NAME, "")).unwrap();
        assert!(p.pointer.is_empty());
        assert_eq!(p.pointer_feed(), 0);
    }

    #[test]
    fn shell_to_x11_coordinates() {
        let monitors = vec![
            ShellMonitor {
                connector: "DP-5".into(),
                rect: Rect::new(0, 0, 2560, 1440),
                flags: 0,
            },
            ShellMonitor {
                connector: "DP-4".into(),
                rect: Rect::new(2560, 0, 2560, 1440),
                flags: 0,
            },
        ];
        let at = |x, y, monitor| ShellPointer {
            x,
            y,
            monitor,
            button: None,
        };
        let same = [
            ("DP-5", Rect::new(0, 0, 2560, 1440)),
            ("DP-4", Rect::new(2560, 0, 2560, 1440)),
        ];
        assert_eq!(
            to_x11(&monitors, &at(3000.5, 7.0, Some(1)), same.iter().copied()),
            (3000.5, 7.0)
        );
        // Fractional scaling with XWayland native scaling: 125 % makes the X11 outputs 3200×1800 pixels.
        let scaled = [
            ("DP-5", Rect::new(0, 0, 3200, 1800)),
            ("DP-4", Rect::new(3200, 0, 3200, 1800)),
        ];
        assert_eq!(
            to_x11(
                &monitors,
                &at(2560.0 + 1280.0, 720.0, Some(1)),
                scaled.iter().copied()
            ),
            (4800.0, 900.0)
        );
        // Unknown connector: the monitor containing the point, then its geometry when names differ.
        assert_eq!(
            to_x11(&monitors, &at(100.0, 100.0, None), scaled.iter().copied()),
            (125.0, 125.0)
        );
        let renamed = [("XWAYLAND0", Rect::new(0, 0, 2560, 1440))];
        assert_eq!(
            to_x11(&monitors, &at(100.0, 50.0, None), renamed.iter().copied()),
            (100.0, 50.0)
        );
        // A stale index pointing at another monitor is not trusted.
        assert_eq!(
            to_x11(
                &monitors,
                &at(100.0, 100.0, Some(1)),
                scaled.iter().copied()
            ),
            (125.0, 125.0)
        );
        // Outside every monitor, or no match: unchanged.
        assert_eq!(
            to_x11(&monitors, &at(-5.0, 3.0, None), scaled.iter().copied()),
            (-5.0, 3.0)
        );
        assert_eq!(
            to_x11(&[], &at(1.0, 2.0, None), scaled.iter().copied()),
            (1.0, 2.0)
        );
    }

    #[test]
    fn click_from_the_shell_reaches_the_tracker() {
        use crate::mouse::PointerTracker;
        use crate::{OutputId, PointerButton, PointerEvent, PointerKind};
        // Two 2560×1440 monitors; Lumenwall shows DP-4 (right) as output 7, rendered at 1440 px high.
        let mut p = with_extension();
        p.set_watch(Watch {
            flags: WATCH_MOTION | WATCH_BUTTONS,
            rate: 30,
        });
        sent_calls(&mut p);
        let serial = p.watch_serial;
        p.feed(&reply(METHOD_RETURN, serial, None)).unwrap();
        // A click on a desktop icon window, shorter than a frame: motion, press, release in one read.
        let mut bytes = pointer_msg(":1.42", 3200.0, 1300.0, "DP-4", None);
        bytes.extend(pointer_msg(
            ":1.42",
            3200.0,
            1300.0,
            "DP-4",
            Some((1, true)),
        ));
        bytes.extend(pointer_msg(
            ":1.42",
            3200.0,
            1300.0,
            "DP-4",
            Some((1, false)),
        ));
        p.feed(&bytes).unwrap();
        let outputs = [("DP-4", Rect::new(2560, 0, 2560, 1440))];
        let mut tracker = PointerTracker::new(true);
        let monitors = p.monitors.clone();
        for ev in p.pointer.drain(..) {
            let (x, y) = to_x11(&monitors, &ev, outputs.iter().copied());
            let kind = match ev.button {
                None => PointerKind::Motion,
                Some((_, pressed)) => PointerKind::Button {
                    button: PointerButton::Left,
                    pressed,
                },
            };
            let local = PointerEvent {
                output: OutputId(7),
                x: x - 2560.0,
                y,
                kind,
            };
            tracker.handle(&local, 1440.0);
        }
        // The next frame sees the button held at the click position (GL: from the bottom), the one after
        // sees it released: scripts get cursorDown, then cursorUp and cursorClick.
        let s = tracker.state(OutputId(7), (2560, 1440));
        assert_eq!((s.x, s.y, s.left), (640.0, 140.0, true));
        assert!(!tracker.state(OutputId(7), (2560, 1440)).left);
    }

    #[test]
    fn address_unescaping() {
        assert_eq!(unescape("/run/user/1000/bus"), b"/run/user/1000/bus");
        assert_eq!(unescape("/tmp/a%20b%2c"), b"/tmp/a b,");
        assert_eq!(unescape("50%"), b"50%");
        assert!(connect_address("tcp:host=localhost,port=1").is_err());
    }
}
