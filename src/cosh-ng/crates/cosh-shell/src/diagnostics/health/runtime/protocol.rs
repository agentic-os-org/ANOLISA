use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::thread;

use serde::{Deserialize, Serialize};

use super::super::model::HealthProgress;
use super::job::{monotonic_ns, JobDeadline};
use super::plan::WorkerRequest;
use super::process::COMMAND_POLL_INTERVAL;

pub(super) const FRAME_LIMIT: usize = 32 * 1024;
pub(super) const TOTAL_LIMIT: usize = 256 * 1024;
pub(super) const FRAME_COUNT_LIMIT: u64 = 256;

#[derive(Debug)]
pub(super) enum ProtocolError {
    Io(io::Error),
    Invalid,
    TooLarge,
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub(super) enum Payload {
    Start(WorkerRequest),
    Progress(HealthProgress),
    Configuration(crate::config::HealthLoginConfig),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Frame {
    pub(super) version: u8,
    pub(super) nonce: String,
    pub(super) sequence: u64,
    pub(super) emitted_ns: u64,
    pub(super) payload: Payload,
}

pub(super) fn encode(
    nonce: &str,
    sequence: u64,
    payload: Payload,
) -> Result<Vec<u8>, ProtocolError> {
    let frame = Frame {
        version: 1,
        nonce: nonce.to_string(),
        sequence,
        emitted_ns: monotonic_ns().map_err(ProtocolError::Io)?,
        payload,
    };
    let mut body = LimitedBuffer {
        bytes: Vec::new(),
        exceeded: false,
    };
    if serde_json::to_writer(&mut body, &frame).is_err() {
        return Err(if body.exceeded {
            ProtocolError::TooLarge
        } else {
            ProtocolError::Invalid
        });
    }
    let body = body.bytes;
    let mut bytes = Vec::with_capacity(body.len() + 4);
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend(body);
    Ok(bytes)
}

struct LimitedBuffer {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > FRAME_LIMIT {
            self.exceeded = true;
            return Err(io::Error::from_raw_os_error(nix::libc::EFBIG));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Decoder {
    buffer: Vec<u8>,
    total: usize,
    frames: u64,
}

impl Decoder {
    pub(super) fn receive(
        &mut self,
        stream: &mut UnixStream,
        deadline: JobDeadline,
    ) -> Result<bool, ProtocolError> {
        let mut chunk = [0; 4096];
        loop {
            if deadline.remaining().map_err(ProtocolError::Io)?.is_zero() {
                return Err(ProtocolError::Timeout);
            }
            match stream.read(&mut chunk) {
                Ok(0) => return Ok(true),
                Ok(count) => {
                    if self.total.saturating_add(count) > TOTAL_LIMIT {
                        return Err(ProtocolError::TooLarge);
                    }
                    self.buffer.extend_from_slice(&chunk[..count]);
                    self.total += count;
                    self.frame_length()?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(ProtocolError::Io(error)),
            }
        }
    }

    pub(super) fn next(&mut self) -> Result<Option<Frame>, ProtocolError> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let length = self.frame_length()?.ok_or(ProtocolError::Invalid)?;
        if self.buffer.len() < length + 4 {
            return Ok(None);
        }
        let frame: Frame = serde_json::from_slice(&self.buffer[4..length + 4])
            .map_err(|_| ProtocolError::Invalid)?;
        if frame.version != 1
            || frame.nonce.len() != 32
            || !frame.nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ProtocolError::Invalid);
        }
        if self.frames >= FRAME_COUNT_LIMIT {
            return Err(ProtocolError::TooLarge);
        }
        self.buffer.drain(..length + 4);
        self.frames += 1;
        Ok(Some(frame))
    }
    pub(super) fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub(super) fn receive_buffered(
        &mut self,
        stream: &mut UnixStream,
    ) -> Result<(), ProtocolError> {
        let mut chunk = [0; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(count) => {
                    if self.total.saturating_add(count) > TOTAL_LIMIT {
                        return Err(ProtocolError::TooLarge);
                    }
                    self.buffer.extend_from_slice(&chunk[..count]);
                    self.total += count;
                    self.frame_length()?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(()),
                Err(error) => return Err(ProtocolError::Io(error)),
            }
        }
    }

    fn frame_length(&self) -> Result<Option<usize>, ProtocolError> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let length = u32::from_be_bytes([
            self.buffer[0],
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
        ]) as usize;
        if length > FRAME_LIMIT {
            return Err(ProtocolError::TooLarge);
        }
        if length == 0 {
            return Err(ProtocolError::Invalid);
        }
        Ok(Some(length))
    }
}

pub(super) fn send(
    stream: &mut UnixStream,
    bytes: &[u8],
    deadline: JobDeadline,
) -> Result<(), ProtocolError> {
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = deadline.remaining().map_err(ProtocolError::Io)?;
        if remaining.is_zero() {
            return Err(ProtocolError::Timeout);
        }
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err(ProtocolError::Invalid),
            Ok(count) => offset += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(COMMAND_POLL_INTERVAL.min(remaining))
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ProtocolError::Io(error)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
