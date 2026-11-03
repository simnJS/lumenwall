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
//! This is a minimal D-Bus client (SASL EXTERNAL, `Hello`, two match rules, one method) on a non-blocking
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

const MONITORS_SIG: &str = "a(siiiiu)";
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

/// Method call with an optional single string argument.
fn method_call(
    serial: u32,
    flags: u8,
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    arg: Option<&str>,
) -> Vec<u8> {
    let mut fields = vec![
        (1, Field::Path(path)),
        (2, Field::Str(interface)),
        (3, Field::Str(member)),
        (6, Field::Str(destination)),
    ];
    if arg.is_some() {
        fields.push((8, Field::Sig("s")));
    }
    message(METHOD_CALL, flags, serial, &fields, |w| {
        if let Some(a) = arg {
            w.string(a);
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

/// Conversation with the bus: outgoing bytes in `out`, received bytes through [`Protocol::feed`].
struct Protocol {
    serial: u32,
    /// Serial of the pending `GetMonitors` call (0: none).
    get_serial: u32,
    /// `None` until the first answer; then whether the extension runs.
    present: Option<bool>,
    monitors: Vec<ShellMonitor>,
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
            rbuf: Vec::new(),
            out: Vec::new(),
        };
        p.call(DBUS_NAME, DBUS_PATH, DBUS_NAME, "Hello", None);
        let state = format!(
            "type='signal',sender='{BUS_NAME}',path='{OBJECT_PATH}',interface='{INTERFACE}',member='MonitorsChanged'"
        );
        let owner = format!(
            "type='signal',sender='{DBUS_NAME}',path='{DBUS_PATH}',interface='{DBUS_NAME}',member='NameOwnerChanged',arg0='{BUS_NAME}'"
        );
        p.call(DBUS_NAME, DBUS_PATH, DBUS_NAME, "AddMatch", Some(&state));
        p.call(DBUS_NAME, DBUS_PATH, DBUS_NAME, "AddMatch", Some(&owner));
        p.request_monitors();
        p
    }

    fn call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        arg: Option<&str>,
    ) -> u32 {
        self.serial = self.serial.wrapping_add(1).max(1);
        let msg = method_call(
            self.serial,
            NO_AUTO_START,
            destination,
            path,
            interface,
            member,
            arg,
        );
        self.out.extend_from_slice(&msg);
        self.serial
    }

    fn request_monitors(&mut self) {
        self.get_serial = self.call(BUS_NAME, OBJECT_PATH, INTERFACE, "GetMonitors", None);
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
        Ok(changed)
    }

    fn handle(&mut self, bytes: &[u8]) -> bool {
        let Some(m) = parse(bytes) else { return false };
        let answer = self.get_serial != 0 && m.reply_serial == self.get_serial;
        match m.kind {
            METHOD_RETURN if answer => {
                self.get_serial = 0;
                let mut body = m.body;
                let monitors = (m.signature == MONITORS_SIG)
                    .then(|| read_monitors(&mut body))
                    .flatten();
                match monitors {
                    Some(v) => self.set(true, v),
                    None => self.set(false, Vec::new()),
                }
            }
            // ServiceUnknown / NameHasNoOwner / UnknownMethod: no (compatible) extension.
            ERROR if answer => {
                self.get_serial = 0;
                self.set(false, Vec::new())
            }
            SIGNAL
                if m.member == "MonitorsChanged"
                    && m.interface == INTERFACE
                    && m.path == OBJECT_PATH
                    && m.signature == MONITORS_SIG =>
            {
                let mut body = m.body;
                read_monitors(&mut body).is_some_and(|v| self.set(true, v))
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
                    self.set(false, Vec::new())
                } else {
                    self.request_monitors();
                    false
                }
            }
            _ => false,
        }
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

    #[test]
    fn method_call_layout() {
        let m = method_call(
            3,
            NO_AUTO_START,
            DBUS_NAME,
            DBUS_PATH,
            DBUS_NAME,
            "AddMatch",
            Some("type='signal'"),
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
    fn address_unescaping() {
        assert_eq!(unescape("/run/user/1000/bus"), b"/run/user/1000/bus");
        assert_eq!(unescape("/tmp/a%20b%2c"), b"/tmp/a b,");
        assert_eq!(unescape("50%"), b"50%");
        assert!(connect_address("tcp:host=localhost,port=1").is_err());
    }
}
