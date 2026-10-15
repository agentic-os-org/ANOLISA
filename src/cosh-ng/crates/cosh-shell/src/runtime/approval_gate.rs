//! Host-owned acquisition and release of the approval surface rectangle.
//!
//! The worker paints a surface; only the Host knows where that surface sits on
//! the terminal. This module is the single place that decides the rectangle, so
//! the placement rule and its preconditions stay together and a worker frame can
//! never reach the tty without them.
//!
//! Acquisition is derived, not guessed. The Host positions the cursor on the
//! bottom viewport row and emits exactly `height` `CRLF`+`EL2` pairs. A `CRLF`
//! on the bottom row scrolls the viewport by one and leaves the cursor on that
//! same row, so after `height` repetitions the previous screen content has moved
//! into scrollback intact and the bottom `height` rows are known-blank. The
//! anchor row therefore follows from the viewport height the Host just read, and
//! no coordinate is inferred from terminal state the Host did not write.

use std::io::{self, Write};

use nix::libc;
use nix::pty::Winsize;

use crate::ui::surface_frame::{
    encode_surface_seal, encode_surface_withdraw, SurfaceRect, SLICE_COLUMNS,
};

/// A terminal the gate can measure and write to without userspace buffering.
///
/// `accepted` exists because a partial write has to be resumable from the exact
/// byte the kernel took: guessing where a truncated control sequence stopped
/// would leave the terminal in a state no rollback describes.
pub(crate) trait SurfaceTerminal: Write {
    /// Viewport size of the fd this terminal writes to.
    ///
    /// Implementations must report the size of their own descriptor. Falling
    /// back to another fd would compute the rectangle for a different viewport
    /// than the one being written.
    fn winsize(&mut self) -> io::Result<Winsize>;
    /// Bytes the kernel accepted since the last [`Self::reset_accepted`].
    fn accepted(&self) -> usize;
    /// Starts a new accepted-byte count for the next transaction.
    fn reset_accepted(&mut self);
}

/// The outer tty, written one `libc::write` at a time.
///
/// There is deliberately no `BufWriter` or `LineWriter` here. Buffered writers
/// make "flushed" mean "handed to a userspace buffer", which would let a
/// transaction report success for bytes the kernel never accepted.
pub(crate) struct TtySurface {
    fd: i32,
    accepted: usize,
}

impl TtySurface {
    /// The process's stdout descriptor, which the raw-mode guard has already
    /// pointed at the outer tty.
    pub(crate) fn stdout() -> Self {
        Self {
            fd: libc::STDOUT_FILENO,
            accepted: 0,
        }
    }
}

impl Write for TtySurface {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = unsafe { libc::write(self.fd, buf.as_ptr().cast(), buf.len()) };
        if written < 0 {
            let error = io::Error::last_os_error();
            return Err(if self.fd == libc::STDOUT_FILENO {
                crate::shell_host::tag_physical_error(error)
            } else {
                error
            });
        }
        if written == 0 && !buf.is_empty() {
            let error = io::Error::new(io::ErrorKind::WriteZero, "terminal wrote zero bytes");
            return Err(if self.fd == libc::STDOUT_FILENO {
                crate::shell_host::tag_physical_error(error)
            } else {
                error
            });
        }
        let written = written as usize;
        self.accepted += written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Nothing is buffered, so every byte already returned from `write` is
        // already accepted by the kernel.
        Ok(())
    }
}

impl SurfaceTerminal for TtySurface {
    fn winsize(&mut self) -> io::Result<Winsize> {
        let mut size: Winsize = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::ioctl(self.fd, libc::TIOCGWINSZ as libc::c_ulong, &mut size) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(size)
    }

    fn accepted(&self) -> usize {
        self.accepted
    }

    fn reset_accepted(&mut self) {
        self.accepted = 0;
    }
}

/// Why the surface could not be acquired or released.
#[derive(Debug)]
pub(crate) enum GateError {
    /// The viewport is narrower than the Host-owned surface width, so the card
    /// cannot be placed without relying on reflow.
    ViewportTooNarrow {
        /// Viewport columns actually reported.
        columns: u16,
    },
    /// The surface does not fit the viewport height.
    ViewportTooShort {
        /// Viewport rows actually reported.
        rows: u16,
        /// Rows the surface asked for.
        height: u16,
    },
    /// A rectangle was already acquired; releasing it first is the caller's job.
    AlreadyAcquired(SurfaceRect),
    /// No rectangle is acquired.
    NotAcquired,
    /// The viewport size could not be read.
    Winsize(io::Error),
    /// A terminal write failed after `accepted` bytes reached the kernel.
    Write {
        /// Bytes the kernel accepted before the failure.
        accepted: usize,
        /// The underlying error.
        source: io::Error,
    },
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ViewportTooNarrow { columns } => {
                write!(
                    f,
                    "terminal is {columns} columns wide, below {SLICE_COLUMNS}"
                )
            }
            Self::ViewportTooShort { rows, height } => {
                write!(f, "surface needs {height} rows, terminal has {rows}")
            }
            Self::AlreadyAcquired(rect) => write!(f, "surface already acquired at {rect:?}"),
            Self::NotAcquired => write!(f, "no surface is acquired"),
            Self::Winsize(error) => write!(f, "could not read the terminal size: {error}"),
            Self::Write { accepted, source } => {
                write!(f, "terminal write failed after {accepted} bytes: {source}")
            }
        }
    }
}

impl std::error::Error for GateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Winsize(error) | Self::Write { source: error, .. } => Some(error),
            _ => None,
        }
    }
}

/// Owns the approval surface for one Host session.
///
/// The gate owns its terminal so that "one physical writer" is a structural
/// property rather than a convention: nothing else can hold the same descriptor
/// and interleave with a surface transaction.
pub(crate) struct ApprovalSurfaceGate<T> {
    term: T,
    rect: Option<SurfaceRect>,
    /// Viewport exactly as reported when the rectangle was acquired.
    acquired_size: Option<(u16, u16)>,
    /// Latches on the first observed difference from `acquired_size`.
    ///
    /// Sticky on purpose. A terminal taken from 80 columns to 47 and back to 120
    /// reflows twice, and a comparison against a width floor sees nothing wrong
    /// at any step, yet the rows the rectangle names no longer hold the card.
    /// Once that has happened the recorded anchor can never be trusted again, so
    /// a later return to the original size must not revive it.
    geometry_untrusted: bool,
}

/// Whether an acquired rectangle's recorded coordinates can still be trusted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GeometryTrust {
    /// The viewport is exactly as it was at acquisition, so the rectangle still
    /// names the rows the card was painted on.
    Trusted,
    /// A dimension changed since acquisition. Erasing or repainting at the
    /// recorded anchor could blank the user's output, so the caller must
    /// relinquish without erasing and rebuild on a fresh region.
    Changed,
    /// The viewport could not be read. Nothing can be proven safe, and a caller
    /// that is about to write must treat this as untrusted rather than assume the
    /// old region is still theirs.
    Unknown,
}

pub(crate) fn encode_viewport_scrollout(rows: u16) -> Vec<u8> {
    let mut out = vec![0x18];
    out.extend_from_slice(format!("\x1b[{rows};1H").as_bytes());
    for _ in 0..rows {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\x1b[0m\x1b[?25h");
    out
}

#[cfg(test)]
impl ApprovalSurfaceGate<TtySurface> {
    pub(crate) fn claimed_closed_tty_for_test(rect: SurfaceRect) -> Self {
        Self {
            term: TtySurface {
                fd: -1,
                accepted: 0,
            },
            rect: Some(rect),
            acquired_size: Some((30, 80)),
            geometry_untrusted: false,
        }
    }
}

impl<T: SurfaceTerminal> ApprovalSurfaceGate<T> {
    pub(crate) const fn new(term: T) -> Self {
        Self {
            term,
            rect: None,
            acquired_size: None,
            geometry_untrusted: false,
        }
    }

    /// Currently acquired rectangle, if any.
    pub(crate) const fn rect(&self) -> Option<SurfaceRect> {
        self.rect
    }

    /// The terminal this gate writes to, for handing to the frame publisher.
    pub(crate) fn term_mut(&mut self) -> &mut T {
        &mut self.term
    }

    /// Scrolls the viewport by `height` rows and claims the blank rows it leaves
    /// at the bottom as the surface.
    ///
    /// # Errors
    /// Returns [`GateError::AlreadyAcquired`] when a rectangle is live,
    /// [`GateError::ViewportTooNarrow`] when the terminal is narrower than
    /// [`SLICE_COLUMNS`], [`GateError::ViewportTooShort`] when the surface does
    /// not fit, [`GateError::Winsize`] when the size cannot be read, or
    /// [`GateError::Write`] when the scroll-out was not fully accepted. Every
    /// error is returned before a rectangle is recorded, so a failed acquisition
    /// never leaves the gate claiming rows it does not own.
    pub(crate) fn acquire(&mut self, height: u16) -> Result<SurfaceRect, GateError> {
        if let Some(rect) = self.rect {
            return Err(GateError::AlreadyAcquired(rect));
        }
        let size = self.term.winsize().map_err(GateError::Winsize)?;
        if size.ws_col < SLICE_COLUMNS {
            return Err(GateError::ViewportTooNarrow {
                columns: size.ws_col,
            });
        }
        if height == 0 || size.ws_row == 0 || height > size.ws_row {
            return Err(GateError::ViewportTooShort {
                rows: size.ws_row,
                height,
            });
        }

        let mut out = Vec::new();
        out.extend_from_slice(format!("\x1b[{};1H", size.ws_row).as_bytes());
        for _ in 0..height {
            out.extend_from_slice(b"\r\n\x1b[2K");
        }
        self.write_tracked(&out)?;

        let rect = SurfaceRect {
            anchor_row: size.ws_row - height,
            columns: SLICE_COLUMNS,
            height,
        };
        self.rect = Some(rect);
        self.acquired_size = Some((size.ws_row, size.ws_col));
        self.geometry_untrusted = false;
        Ok(rect)
    }

    /// Whether the live rectangle is still the bottom `rows` of the viewport.
    ///
    /// `acquire` derives the anchor row from the viewport height at that moment,
    /// so a resize while a surface is live leaves the rectangle naming rows that
    /// are no longer at the bottom. Repainting there puts the new frame in the
    /// wrong place, and erasing there blanks whatever now occupies those rows —
    /// which after a reflow is the user's output, not the card.
    ///
    /// Both viewport dimensions are compared against the values actually reported
    /// at acquisition, and the verdict latches. A width floor such as
    /// `ws_col >= 40` is satisfied by 80, 47 and 120 alike, so a terminal taken
    /// through 80 -> 47 -> 120 would otherwise be reported unchanged despite two
    /// reflows; and a terminal resized away and back to the same size has still
    /// scrolled content through the rectangle's rows, so the old anchor must not
    /// be revived.
    ///
    /// # Errors
    /// Returns [`GateError::NotAcquired`] when no rectangle is live, or
    /// [`GateError::Winsize`] when the viewport could not be read. A failed read
    /// is an error rather than a `false`, so the caller cannot mistake an
    /// unreadable terminal for a resized one and repaint into a stale rectangle.
    pub(crate) fn rectangle_still_owns_the_bottom(
        &mut self,
        rows: u16,
        columns: u16,
    ) -> Result<bool, GateError> {
        let rect = self.rect.ok_or(GateError::NotAcquired)?;
        if self.geometry_untrusted {
            return Ok(false);
        }
        let size = self.term.winsize().map_err(GateError::Winsize)?;
        if Some((size.ws_row, size.ws_col)) != self.acquired_size {
            self.geometry_untrusted = true;
            return Ok(false);
        }
        Ok(rect.height == rows && rect.columns == columns && size.ws_row - rows == rect.anchor_row)
    }

    /// Whether the acquired rectangle's recorded coordinates can still be trusted.
    ///
    /// Unlike [`Self::rectangle_still_owns_the_bottom`] this does not need to know
    /// what height a repaint wants, so it can be asked before deciding whether a
    /// fallback is needed at all. [`GeometryTrust::Unknown`] is deliberately not
    /// folded into [`GeometryTrust::Trusted`]: a caller that is about to write
    /// cannot prove the old region is still theirs just because the size could not
    /// be read.
    pub(crate) fn observe_geometry(&mut self) -> GeometryTrust {
        let Some(acquired) = self.acquired_size else {
            return GeometryTrust::Trusted;
        };
        if self.geometry_untrusted {
            return GeometryTrust::Changed;
        }
        match self.term.winsize() {
            Ok(size) if (size.ws_row, size.ws_col) == acquired => GeometryTrust::Trusted,
            Ok(_) => {
                self.geometry_untrusted = true;
                GeometryTrust::Changed
            }
            Err(_) => GeometryTrust::Unknown,
        }
    }

    /// Gives up the rectangle without writing anything at its recorded rows.
    ///
    /// This is the only correct release once geometry or write ownership is
    /// untrusted: those rows may hold the user's output rather than the card, so
    /// the conservative choice is to leave them alone. Clearing the rectangle is
    /// what makes every later `withdraw`, `seal_to_scrollback` and `Drop` a no-op,
    /// since all three require a live rectangle, so no stale `CUP`/`EL2` can be
    /// emitted afterwards either.
    pub(crate) fn relinquish_without_erasing(&mut self) {
        self.rect = None;
        self.acquired_size = None;
    }

    /// Scrolls the whole current viewport into scrollback, leaving the cursor on a
    /// fresh blank bottom row.
    ///
    /// A conservative fallback must not leave the invalidated card in the viewport
    /// where it reads as the active one, and must not erase it either. Pushing the
    /// viewport out keeps it readable as history and gives the replacement card a
    /// region that provably holds nothing else. Only plain `CRLF` is emitted: no
    /// `EL2` on rows this gate no longer trusts, no `ED2`/`ED3`, no alternate
    /// screen, and no scrollback erasure. Called once per invalidation, never on
    /// an ordinary focus repaint.
    ///
    /// # Errors
    /// Returns [`GateError::Winsize`] when the size cannot be read, or
    /// [`GateError::Write`] when the scroll-out was not fully accepted.
    pub(crate) fn push_viewport_into_scrollback(&mut self) -> Result<(), GateError> {
        let size = self.term.winsize().map_err(GateError::Winsize)?;
        if size.ws_row == 0 {
            return Ok(());
        }
        self.write_tracked(&encode_viewport_scrollout(size.ws_row))
    }

    /// Erases the surface and hands the terminal back to ordinary output, with
    /// the cursor where the surface started so following output continues there.
    ///
    /// # Errors
    /// Returns [`GateError::NotAcquired`] when no rectangle is live, or
    /// [`GateError::Write`] when the erasure was not fully accepted.
    pub(crate) fn withdraw(&mut self) -> Result<(), GateError> {
        let rect = self.rect.ok_or(GateError::NotAcquired)?;
        self.write_tracked(&encode_surface_withdraw(rect))?;
        self.rect = None;
        Ok(())
    }

    /// Ends the surface while keeping its content readable as scrollback.
    ///
    /// Used once a decision receipt has been published: the rows are already
    /// hard rows, so nothing is erased. The cursor moves to a fresh row below
    /// the receipt, which is where later Bash output belongs.
    ///
    /// # Errors
    /// Returns [`GateError::NotAcquired`] when no rectangle is live, or
    /// [`GateError::Write`] when the seal was not fully accepted.
    pub(crate) fn seal_to_scrollback(&mut self) -> Result<(), GateError> {
        let rect = self.rect.ok_or(GateError::NotAcquired)?;
        self.write_tracked(&encode_surface_seal(rect))?;
        self.rect = None;
        Ok(())
    }

    /// Writes Host-owned presentation bytes with the same accepted-offset
    /// tracking as a surface transaction.
    ///
    /// # Errors
    /// Returns [`GateError::Write`] when the kernel stopped accepting bytes; the
    /// reported `accepted` count is where the transaction stopped.
    pub(crate) fn write_presentation(&mut self, bytes: &[u8]) -> Result<(), GateError> {
        self.write_tracked(bytes)
    }

    fn write_tracked(&mut self, bytes: &[u8]) -> Result<(), GateError> {
        self.term.reset_accepted();
        if let Err(source) = self.term.write_all(bytes) {
            return Err(GateError::Write {
                accepted: self.term.accepted(),
                source,
            });
        }
        // `write_all` only returns Ok after every byte was accepted, so a
        // mismatch would mean the terminal lied about its own count.
        if self.term.accepted() != bytes.len() {
            return Err(GateError::Write {
                accepted: self.term.accepted(),
                source: io::Error::other("terminal accepted a partial transaction"),
            });
        }
        Ok(())
    }
}
