//! Unix transport: a stream socket pair, file descriptors passed with `SCM_RIGHTS`.

use std::collections::VecDeque;
use std::io::{self, IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{HEADER_LEN, decode, decode_header, encode};

/// Most descriptors carried by one message (DMA-BUF frames have at most 4 planes).
const MAX_FDS: usize = 8;
const READ_CHUNK: usize = 64 * 1024;

/// A connected socket pair (main end, host end).
pub fn pair() -> io::Result<(UnixStream, UnixStream)> {
    UnixStream::pair()
}

/// Writing half.
#[derive(Debug)]
pub struct Sender {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Sender {
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    /// Sends one message with descriptors attached (they are duplicated into the peer).
    pub fn send<T: Serialize>(&mut self, message: &T, fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        if fds.len() > MAX_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many descriptors",
            ));
        }
        encode(message, fds.len() as u8, &mut self.buf)?;
        if fds.is_empty() {
            return self.stream.write_all(&self.buf);
        }
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if !control.push(SendAncillaryMessage::ScmRights(fds)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "control buffer too small",
            ));
        }
        let sent = loop {
            match sendmsg(
                &self.stream,
                &[IoSlice::new(&self.buf)],
                &mut control,
                SendFlags::NOSIGNAL,
            ) {
                Ok(n) => break n,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
        };
        // The descriptors went with the first byte; the rest is plain data.
        self.stream.write_all(&self.buf[sent..])
    }
}

/// Reading half.
#[derive(Debug)]
pub struct Receiver {
    stream: UnixStream,
    buf: Vec<u8>,
    start: usize,
    scratch: Vec<u8>,
    fds: VecDeque<OwnedFd>,
}

impl Receiver {
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            start: 0,
            scratch: vec![0; READ_CHUNK],
            fds: VecDeque::new(),
        }
    }

    /// Blocks for the next message. `Ok(None)` when the peer closed the connection cleanly.
    pub fn recv<T: DeserializeOwned>(&mut self) -> io::Result<Option<(T, Vec<OwnedFd>)>> {
        loop {
            let available = &self.buf[self.start..];
            if available.len() >= HEADER_LEN {
                let header: [u8; HEADER_LEN] =
                    available[..HEADER_LEN].try_into().expect("header length");
                let (len, count) = decode_header(&header)?;
                if available.len() >= HEADER_LEN + len {
                    // Descriptors arrive no later than the first byte of their message.
                    if self.fds.len() < usize::from(count) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "missing descriptors",
                        ));
                    }
                    let fds: Vec<OwnedFd> = self.fds.drain(..usize::from(count)).collect();
                    let message = decode(&available[HEADER_LEN..HEADER_LEN + len])?;
                    self.start += HEADER_LEN + len;
                    if self.start == self.buf.len() {
                        self.buf.clear();
                        self.start = 0;
                    }
                    return Ok(Some((message, fds)));
                }
            }
            if self.fill()? == 0 {
                return if self.start == self.buf.len() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated message",
                    ))
                };
            }
        }
    }

    fn fill(&mut self) -> io::Result<usize> {
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let flags = RecvFlags::CMSG_CLOEXEC;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let flags = RecvFlags::empty();
        let received = loop {
            match recvmsg(
                &self.stream,
                &mut [IoSliceMut::new(&mut self.scratch)],
                &mut control,
                flags,
            ) {
                Ok(r) => break r,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
        };
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = message {
                self.fds.extend(fds);
            }
        }
        self.buf.extend_from_slice(&self.scratch[..received.bytes]);
        Ok(received.bytes)
    }
}

/// Host side: takes the socket the main process passed as standard input and points standard
/// input at `/dev/null`, so browser subprocesses never inherit the channel.
pub fn take_stdin_socket() -> io::Result<UnixStream> {
    // SAFETY: descriptor 0 is open for the whole process lifetime; it is only borrowed here.
    let stdin = unsafe { BorrowedFd::borrow_raw(0) };
    let socket = rustix::io::fcntl_dupfd_cloexec(stdin, 3)?;
    let null = std::fs::File::open("/dev/null")?;
    rustix::stdio::dup2_stdin(&null)?;
    Ok(UnixStream::from(socket))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{FrameInfo, HostCommand, HostEvent, PixelFormat, PlaneInfo};
    use std::io::{Read, Seek};
    use std::os::fd::AsFd;

    #[test]
    fn messages_and_descriptors_cross_the_socket() {
        let (a, b) = pair().unwrap();
        let mut tx = Sender::new(a);
        let mut rx = Receiver::new(b);

        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"plane data").unwrap();
        let frame = HostEvent::Frame(FrameInfo {
            id: 3,
            width: 4,
            height: 2,
            visible: [0, 0, 4, 2],
            format: PixelFormat::Bgra8,
            modifier: 0,
            planes: vec![PlaneInfo {
                offset: 0,
                stride: 16,
                size: 32,
            }],
            handle: 0,
            memory: false,
        });
        let writer = std::thread::spawn(move || {
            tx.send(&HostEvent::Started { pid: 1 }, &[]).unwrap();
            tx.send(&frame, &[file.as_fd()]).unwrap();
            // A large message split across many reads.
            tx.send(
                &HostCommand::Audio {
                    values: vec![0.123_456; 40_000],
                },
                &[],
            )
            .unwrap();
            drop(tx);
            frame
        });

        let (m, fds): (HostEvent, _) = rx.recv().unwrap().unwrap();
        assert_eq!(m, HostEvent::Started { pid: 1 });
        assert!(fds.is_empty());
        let (m, mut fds): (HostEvent, _) = rx.recv().unwrap().unwrap();
        assert_eq!(fds.len(), 1);
        let mut received = std::fs::File::from(fds.pop().unwrap());
        received.rewind().unwrap();
        let mut text = String::new();
        received.read_to_string(&mut text).unwrap();
        assert_eq!(text, "plane data");
        let (big, _): (HostCommand, _) = rx.recv().unwrap().unwrap();
        assert!(matches!(big, HostCommand::Audio { values } if values.len() == 40_000));
        assert!(rx.recv::<HostEvent>().unwrap().is_none());
        assert_eq!(writer.join().unwrap(), m);
    }
}
