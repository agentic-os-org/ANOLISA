use std::io::{self, Write};

use tokio::io::{AsyncRead, AsyncReadExt};

const READ_CHUNK_BYTES: usize = 4096;

#[derive(Debug)]
pub(crate) enum FrameReadError {
    Empty,
    TooLarge,
    Io(io::Error),
}

pub(crate) async fn read_request_frame<R>(
    reader: &mut R,
    maximum_wire_bytes: usize,
) -> Result<Vec<u8>, FrameReadError>
where
    R: AsyncRead + Unpin,
{
    let mut payload = Vec::with_capacity(maximum_wire_bytes.min(READ_CHUNK_BYTES));
    let mut chunk = [0_u8; READ_CHUNK_BYTES];

    loop {
        let remaining_probe = maximum_wire_bytes
            .saturating_sub(payload.len())
            .saturating_add(1)
            .min(READ_CHUNK_BYTES);
        let read = reader
            .read(&mut chunk[..remaining_probe])
            .await
            .map_err(FrameReadError::Io)?;
        if read == 0 {
            return if payload.is_empty() {
                Err(FrameReadError::Empty)
            } else {
                Ok(payload)
            };
        }

        if let Some(delimiter) = chunk[..read].iter().position(|byte| *byte == b'\n') {
            let wire_size = payload
                .len()
                .checked_add(delimiter + 1)
                .ok_or(FrameReadError::TooLarge)?;
            if wire_size > maximum_wire_bytes {
                return Err(FrameReadError::TooLarge);
            }
            payload.extend_from_slice(&chunk[..delimiter]);
            return Ok(payload);
        }

        payload.extend_from_slice(&chunk[..read]);
        if payload.len() > maximum_wire_bytes {
            return Err(FrameReadError::TooLarge);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponseFrameError {
    Empty,
    TooLarge,
    ContainsDelimiter,
    BytesBeforeClose,
}

pub(crate) struct BoundedResponseBuffer {
    bytes: Vec<u8>,
    maximum_payload_bytes: usize,
    failure: Option<ResponseFrameError>,
}

impl BoundedResponseBuffer {
    pub(crate) fn new(maximum_wire_bytes: usize) -> Self {
        let maximum_payload_bytes = maximum_wire_bytes.saturating_sub(1);
        Self {
            bytes: Vec::with_capacity(maximum_payload_bytes.min(READ_CHUNK_BYTES)),
            maximum_payload_bytes,
            failure: None,
        }
    }

    pub(crate) fn finish_send(mut self) -> Result<Vec<u8>, ResponseFrameError> {
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        if self.bytes.is_empty() {
            return Err(ResponseFrameError::Empty);
        }
        self.bytes.push(b'\n');
        Ok(self.bytes)
    }

    pub(crate) fn finish_close(self) -> Result<(), ResponseFrameError> {
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(ResponseFrameError::BytesBeforeClose)
        }
    }

    pub(crate) const fn failure(&self) -> Option<ResponseFrameError> {
        self.failure
    }
}

impl Write for BoundedResponseBuffer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.contains(&b'\n') {
            self.failure = Some(ResponseFrameError::ContainsDelimiter);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response payload contains an LF delimiter",
            ));
        }
        let Some(next_length) = self.bytes.len().checked_add(buffer.len()) else {
            self.failure = Some(ResponseFrameError::TooLarge);
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "response frame limit exceeded",
            ));
        };
        if next_length > self.maximum_payload_bytes {
            self.failure = Some(ResponseFrameError::TooLarge);
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "response frame limit exceeded",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt as _;

    use super::*;

    #[tokio::test]
    async fn frame_limit_includes_the_lf_delimiter() {
        let (mut writer, mut reader) = tokio::io::duplex(32);
        writer.write_all(b"1234567\ntrailing").await.unwrap();
        drop(writer);

        assert_eq!(
            read_request_frame(&mut reader, 8).await.unwrap(),
            b"1234567"
        );
    }

    #[tokio::test]
    async fn eof_terminated_frame_can_fill_the_exact_limit() {
        let (mut writer, mut reader) = tokio::io::duplex(32);
        writer.write_all(b"12345678").await.unwrap();
        drop(writer);

        assert_eq!(
            read_request_frame(&mut reader, 8).await.unwrap(),
            b"12345678"
        );
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_unbounded_growth() {
        let (mut writer, mut reader) = tokio::io::duplex(32);
        writer.write_all(b"123456789").await.unwrap();
        drop(writer);

        assert!(matches!(
            read_request_frame(&mut reader, 8).await,
            Err(FrameReadError::TooLarge)
        ));
    }

    #[test]
    fn response_buffer_reserves_the_delimiter_inside_the_limit() {
        let mut exact = BoundedResponseBuffer::new(8);
        exact.write_all(b"1234567").unwrap();
        assert_eq!(exact.finish_send().unwrap(), b"1234567\n");

        let mut oversized = BoundedResponseBuffer::new(8);
        assert!(oversized.write_all(b"12345678").is_err());
        assert_eq!(oversized.finish_send(), Err(ResponseFrameError::TooLarge));
    }

    #[test]
    fn response_buffer_rejects_embedded_frame_delimiters() {
        let mut response = BoundedResponseBuffer::new(32);
        assert!(response.write_all(b"first\nsecond").is_err());
        assert_eq!(
            response.finish_send(),
            Err(ResponseFrameError::ContainsDelimiter)
        );
    }
}
