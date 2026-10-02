use std::time::Duration;

use nix::pty::Winsize;

#[derive(Debug, Clone)]
pub enum RawRelayAction {
    Write(Vec<u8>),
    Resize(Winsize),
    Wait(Duration),
}

impl RawRelayAction {
    pub fn write(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Write(bytes.into())
    }

    pub fn line(line: impl AsRef<str>) -> Self {
        let mut bytes = line.as_ref().as_bytes().to_vec();
        bytes.push(b'\n');
        Self::Write(bytes)
    }

    pub fn resize(rows: u16, cols: u16) -> Self {
        Self::Resize(Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        })
    }

    pub fn wait(duration: Duration) -> Self {
        Self::Wait(duration)
    }
}
