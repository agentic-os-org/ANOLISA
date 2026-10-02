//! Wake channels shared by the raw input driver and output relay.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;

use nix::libc;

pub(super) struct SignalWake(signal_hook::SigId);

impl SignalWake {
    fn register(signal: i32, wake: UnixStream) -> io::Result<Self> {
        signal_hook::low_level::pipe::register(signal, wake).map(Self)
    }
}

impl Drop for SignalWake {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.0);
    }
}

pub(super) struct RelayWake {
    reader: UnixStream,
    writer: UnixStream,
    resize_reader: UnixStream,
    resize_signal: SignalWake,
}

impl RelayWake {
    pub(super) fn new() -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        let (resize_reader, resize_writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        resize_reader.set_nonblocking(true)?;
        resize_writer.set_nonblocking(true)?;
        let resize_signal = SignalWake::register(libc::SIGWINCH, resize_writer)?;
        Ok(Self {
            reader,
            writer,
            resize_reader,
            resize_signal,
        })
    }

    pub(super) fn into_parts(self) -> (UnixStream, UnixStream, UnixStream, SignalWake) {
        (
            self.reader,
            self.writer,
            self.resize_reader,
            self.resize_signal,
        )
    }
}

pub(super) fn notify_relay(wake: &mut UnixStream) {
    match wake.write(&[1]) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Err(_) => {}
    }
}
