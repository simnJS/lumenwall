//! Windows transport: the host's standard input/output pipes. Handles are not sent through the
//! pipe; the main process duplicates them out of the host process (it owns the process handle).

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{HEADER_LEN, decode, decode_header, encode};

/// Writing half.
pub struct Sender {
    pipe: Box<dyn Write + Send>,
    buf: Vec<u8>,
}

impl std::fmt::Debug for Sender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sender").finish_non_exhaustive()
    }
}

impl Sender {
    pub fn new(pipe: impl Write + Send + 'static) -> Self {
        Self {
            pipe: Box::new(pipe),
            buf: Vec::new(),
        }
    }

    pub fn send<T: Serialize>(&mut self, message: &T) -> io::Result<()> {
        encode(message, 0, &mut self.buf)?;
        self.pipe.write_all(&self.buf)?;
        self.pipe.flush()
    }
}

/// Reading half.
pub struct Receiver {
    pipe: Box<dyn Read + Send>,
    body: Vec<u8>,
}

impl std::fmt::Debug for Receiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Receiver").finish_non_exhaustive()
    }
}

impl Receiver {
    pub fn new(pipe: impl Read + Send + 'static) -> Self {
        Self {
            pipe: Box::new(pipe),
            body: Vec::new(),
        }
    }

    /// Blocks for the next message. `Ok(None)` when the peer closed the pipe cleanly.
    pub fn recv<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
        let mut header = [0u8; HEADER_LEN];
        match self.pipe.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let (len, _) = decode_header(&header)?;
        self.body.resize(len, 0);
        self.pipe.read_exact(&mut self.body)?;
        decode(&self.body).map(Some)
    }
}

/// Host side: the IPC pipes are the process's standard input and output. Standard output is
/// then redirected to standard error so nothing else can write into the channel.
pub fn take_std_pipes() -> io::Result<(std::fs::File, std::fs::File)> {
    use std::os::windows::io::{AsHandle, OwnedHandle};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };

    let input: OwnedHandle = io::stdin().as_handle().try_clone_to_owned()?;
    let output: OwnedHandle = io::stdout().as_handle().try_clone_to_owned()?;
    // SAFETY: plain Win32 calls on the process's own standard handles.
    unsafe {
        let stderr = GetStdHandle(STD_ERROR_HANDLE);
        SetStdHandle(STD_OUTPUT_HANDLE, stderr);
    }
    Ok((std::fs::File::from(input), std::fs::File::from(output)))
}
