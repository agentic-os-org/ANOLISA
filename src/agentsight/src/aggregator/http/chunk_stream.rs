//! Incremental transfer framing forwards data slices without retaining chunk payloads.

#[derive(Debug)]
enum State {
    Size,
    Data(usize),
    CrLf(usize),
    Trailers,
    Done,
}

#[derive(Debug)]
pub(super) struct ChunkStream {
    state: State,
    line: Vec<u8>,
}

impl Default for ChunkStream {
    fn default() -> Self {
        Self {
            state: State::Size,
            line: Vec::new(),
        }
    }
}

impl ChunkStream {
    pub(super) fn feed(
        &mut self,
        mut bytes: &[u8],
        mut consume: impl FnMut(&[u8]) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        while !bytes.is_empty() {
            match self.state {
                State::Size | State::Trailers => {
                    let len = bytes
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .map_or(bytes.len(), |end| end + 1);
                    if self.line.len().saturating_add(len) > 8192 {
                        return Err(std::io::Error::other("chunk framing line exceeds 8 KiB"));
                    }
                    self.line.extend_from_slice(&bytes[..len]);
                    bytes = &bytes[len..];
                    if self.line.last() != Some(&b'\n') {
                        continue;
                    }
                    let line = std::str::from_utf8(&self.line)
                        .map_err(std::io::Error::other)?
                        .trim_end_matches(['\r', '\n']);
                    self.state = match self.state {
                        State::Size => {
                            let size = usize::from_str_radix(
                                line.split(';').next().unwrap_or("").trim(),
                                16,
                            )
                            .map_err(std::io::Error::other)?;
                            if size == 0 {
                                State::Trailers
                            } else {
                                State::Data(size)
                            }
                        }
                        _ if line.is_empty() => State::Done,
                        _ => State::Trailers,
                    };
                    self.line.clear();
                }
                State::Data(remaining) => {
                    let len = remaining.min(bytes.len());
                    consume(&bytes[..len])?;
                    bytes = &bytes[len..];
                    self.state = if remaining == len {
                        State::CrLf(0)
                    } else {
                        State::Data(remaining - len)
                    };
                }
                State::CrLf(offset) => {
                    if bytes[0] != b"\r\n"[offset] {
                        return Err(std::io::Error::other("invalid chunk data terminator"));
                    }
                    bytes = &bytes[1..];
                    self.state = if offset == 0 {
                        State::CrLf(1)
                    } else {
                        State::Size
                    };
                }
                State::Done => break,
            }
        }
        Ok(())
    }

    pub(super) fn done(&self) -> bool {
        matches!(self.state, State::Done)
    }
    pub(super) fn retained_bytes(&self) -> usize {
        self.line.capacity()
    }
}
