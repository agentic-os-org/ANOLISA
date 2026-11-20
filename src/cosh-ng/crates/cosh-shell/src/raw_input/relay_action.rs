use std::time::Duration;

use nix::pty::Winsize;

#[derive(Debug, Clone)]
pub enum RawRelayAction {
    Write(Vec<u8>),
    Resize(Winsize),
    Wait(Duration),
    /// Blocks until the assistance toggle would be accepted: the shell has
    /// painted a routable prompt and no draft occupies it. Scripted toggles
    /// use this instead of a fixed sleep, because how long the shell needs
    /// to repaint after a command depends on the runner's load.
    WaitForAssistanceToggle,
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

    pub fn wait_for_assistance_toggle() -> Self {
        Self::WaitForAssistanceToggle
    }
}
