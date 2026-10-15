//! Host-side encoding of a worker surface frame into a terminal rectangle.
//!
//! Wire v2 makes every worker coordinate surface-relative: the worker paints a
//! `columns x height` surface and has no idea where the Host placed it. This
//! module is the only place that turns those relative ops into absolute
//! terminal positions, so a worker can never address scrollback or another part
//! of the viewport.
//!
//! Two terminal behaviours drive the encoding shape:
//!
//! - Writing into the last column leaves a *pending wrap* that only materialises
//!   on the next character. Every row therefore ends with an explicit `CR`
//!   before the next absolute `CUP`, so no row depends on auto-wrap.
//! - The transaction must leave the terminal in a known state even when it
//!   touched styles or cursor policy, so the tail resets SGR, applies the
//!   Host-owned cursor policy, and parks the cursor on the rectangle sentinel.

use unicode_width::UnicodeWidthStr;

use super::worker_protocol::{DrawOp, RenderColor, RenderStyle};

/// Host-owned surface width for the approval slice.
///
/// This is the renderer's minimum width, and the surface never grows with the
/// terminal: a card laid out for 40 columns is the only layout this slice
/// certifies. Activation refuses any other width rather than adapting, because a
/// wider surface would be a different layout that nothing has verified.
pub const SLICE_COLUMNS: u16 = 40;

/// The Host-owned rectangle a surface frame is painted into.
///
/// `anchor_row` is 0-based and absolute in the terminal viewport; the worker
/// never sees it. Rows `anchor_row .. anchor_row + height` are the only cells
/// this module may address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceRect {
    /// 0-based absolute viewport row of the surface's first row.
    pub anchor_row: u16,
    /// Surface width in columns; the slice pins this to the Host-owned width.
    pub columns: u16,
    /// Surface height in rows.
    pub height: u16,
}

impl SurfaceRect {
    /// Absolute 0-based row the cursor parks on after a transaction.
    ///
    /// This is the surface's *last* row, matching the acquisition sentinel: the
    /// Host scrolls the rectangle out from the bottom viewport row, so the
    /// sentinel stays inside the viewport and parking there cannot scroll.
    pub const fn sentinel_row(self) -> u16 {
        self.anchor_row
            .saturating_add(self.height.saturating_sub(1))
    }
}

/// Why a worker frame could not be encoded into the Host rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceFrameError {
    /// An op addressed a row or column outside the rectangle.
    OutsideRectangle {
        /// Offending surface-relative column.
        x: u16,
        /// Offending surface-relative row.
        y: u16,
    },
    /// A text run would have crossed the rectangle's right edge.
    TextOverflowsRow {
        /// Surface-relative starting column.
        x: u16,
        /// Surface-relative row.
        y: u16,
        /// Display columns the run needs.
        width: usize,
    },
    /// Text arrived without a preceding position, so the Host cannot prove
    /// which cells it would touch.
    TextWithoutPosition,
    /// The worker tried to control global cursor visibility. Cursor policy
    /// belongs to the Host: a worker that could show or hide the cursor would
    /// affect the Bash prompt and every other owner sharing the tty.
    WorkerControlledCursor,
}

impl std::fmt::Display for SurfaceFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutsideRectangle { x, y } => {
                write!(f, "draw op targets ({x},{y}), outside the surface")
            }
            Self::TextOverflowsRow { x, y, width } => write!(
                f,
                "text at ({x},{y}) needs {width} columns and crosses the surface edge"
            ),
            Self::TextWithoutPosition => write!(f, "text arrived without a position"),
            Self::WorkerControlledCursor => {
                write!(f, "worker frames must not control cursor visibility")
            }
        }
    }
}

impl std::error::Error for SurfaceFrameError {}

/// Encodes a validated worker frame as one absolute-positioned transaction
/// inside `rect`.
///
/// The caller must already have validated the frame against a viewport of
/// `rect.columns x rect.height`; this function re-checks the bounds because the
/// rectangle is the authority on what the Host is willing to write, and a stale
/// viewport must not widen it.
///
/// # Errors
/// Returns [`SurfaceFrameError`] without the caller publishing any bytes when an
/// op leaves the rectangle, when text has no proven position, or when the worker
/// sent a cursor-visibility op.
pub fn encode_surface_frame(
    draw_ops: &[DrawOp],
    rect: SurfaceRect,
) -> Result<Vec<u8>, SurfaceFrameError> {
    let mut encoded = Vec::new();
    // Surface-relative cursor, tracked so a text run's right edge is provable
    // and so `None` is a hard error rather than an assumed origin.
    let mut cursor: Option<(u16, u16)> = None;
    for op in draw_ops {
        match op {
            DrawOp::CursorVisible(_) => return Err(SurfaceFrameError::WorkerControlledCursor),
            DrawOp::ClearRect {
                x,
                y,
                width,
                height,
            } => {
                check_inside(rect, *x, *y)?;
                if x.saturating_add(*width) > rect.columns
                    || y.saturating_add(*height) > rect.height
                {
                    return Err(SurfaceFrameError::OutsideRectangle { x: *x, y: *y });
                }
                for offset in 0..*height {
                    let row = y.saturating_add(offset);
                    close_row(&mut encoded, &mut cursor);
                    move_to(&mut encoded, rect, *x, row);
                    encoded.extend_from_slice(format!("\x1b[{width}X").as_bytes());
                    // ECH erases without advancing, so the cursor stays at the
                    // clear's origin column on the row just cleared.
                    cursor = Some((*x, row));
                }
            }
            DrawOp::MoveTo { x, y } => {
                check_inside(rect, *x, *y)?;
                close_row(&mut encoded, &mut cursor);
                move_to(&mut encoded, rect, *x, *y);
                cursor = Some((*x, *y));
            }
            DrawOp::SetStyle(style) => encode_style(&mut encoded, *style),
            DrawOp::Text(text) => {
                let (x, y) = cursor.ok_or(SurfaceFrameError::TextWithoutPosition)?;
                let width = text.width();
                if x.saturating_add(width as u16) > rect.columns {
                    return Err(SurfaceFrameError::TextOverflowsRow { x, y, width });
                }
                encoded.extend_from_slice(text.as_bytes());
                cursor = Some((x.saturating_add(width as u16), y));
            }
        }
    }
    close_row(&mut encoded, &mut cursor);
    finish_transaction(&mut encoded, rect);
    Ok(encoded)
}

/// Encodes the erasure of `rect` as one transaction, leaving the cursor at the
/// rectangle's first row so whatever the Host writes next continues from where
/// the surface used to be.
///
/// Cursor visibility is restored here and hidden by [`finish_transaction`]:
/// while a surface is up the Host keeps the cursor hidden, and handing the
/// terminal back to ordinary output without restoring it would leave the Bash
/// prompt without a caret.
pub fn encode_surface_withdraw(rect: SurfaceRect) -> Vec<u8> {
    let mut encoded = Vec::new();
    for offset in 0..rect.height {
        move_to_absolute(&mut encoded, rect.anchor_row.saturating_add(offset), 0);
        encoded.extend_from_slice(b"\r\x1b[2K");
    }
    encoded.extend_from_slice(b"\x1b[0m\x1b[?25h");
    move_to_absolute(&mut encoded, rect.anchor_row, 0);
    encoded
}

/// Encodes the end of a surface whose content must stay readable.
///
/// Used after a decision receipt is published: every surface row is already a
/// hard row, so the Host does not erase anything. It parks on the sentinel,
/// scrolls once onto a fresh row below the surface, and restores the styles and
/// cursor that ordinary output expects. Later Bash output then appends below the
/// receipt without repainting or disturbing it.
pub fn encode_surface_seal(rect: SurfaceRect) -> Vec<u8> {
    let mut encoded = Vec::new();
    move_to_absolute(&mut encoded, rect.sentinel_row(), 0);
    encoded.extend_from_slice(b"\x1b[0m\r\n\x1b[?25h");
    encoded
}

/// Ends the row currently being painted.
///
/// `CR` is what clears a pending wrap left by writing into the last column; the
/// following absolute `CUP` for the next row then cannot be turned into a
/// spurious scroll by the emulator.
fn close_row(encoded: &mut Vec<u8>, cursor: &mut Option<(u16, u16)>) {
    if cursor.take().is_some() {
        encoded.push(b'\r');
    }
}

fn check_inside(rect: SurfaceRect, x: u16, y: u16) -> Result<(), SurfaceFrameError> {
    if x < rect.columns && y < rect.height {
        return Ok(());
    }
    Err(SurfaceFrameError::OutsideRectangle { x, y })
}

fn move_to(encoded: &mut Vec<u8>, rect: SurfaceRect, x: u16, y: u16) {
    move_to_absolute(encoded, rect.anchor_row.saturating_add(y), x);
}

/// Absolute 1-based `CUP`, the only positioning primitive the encoder emits.
fn move_to_absolute(encoded: &mut Vec<u8>, row0: u16, column0: u16) {
    encoded.extend_from_slice(
        format!(
            "\x1b[{};{}H",
            row0.saturating_add(1),
            column0.saturating_add(1)
        )
        .as_bytes(),
    );
}

/// Restores every mode the transaction touched and parks the cursor on the
/// sentinel row, so the terminal state after a publish does not depend on what
/// the worker painted.
fn finish_transaction(encoded: &mut Vec<u8>, rect: SurfaceRect) {
    encoded.extend_from_slice(b"\x1b[0m");
    // Host-owned cursor policy: the surface never shows a cursor, because a
    // visible cursor inside the card would compete with the Bash prompt's own
    // cursor once the surface is sealed into scrollback.
    encoded.extend_from_slice(b"\x1b[?25l");
    move_to_absolute(encoded, rect.sentinel_row(), 0);
}

fn encode_style(encoded: &mut Vec<u8>, style: RenderStyle) {
    let code = match style {
        RenderStyle::Reset => 0,
        RenderStyle::Bold => 1,
        RenderStyle::Dim => 2,
        RenderStyle::Foreground(color) => foreground_code(color),
        RenderStyle::Background(color) => background_code(color),
    };
    encoded.extend_from_slice(format!("\x1b[{code}m").as_bytes());
}

const fn foreground_code(color: RenderColor) -> u8 {
    match color {
        RenderColor::Default => 39,
        RenderColor::Black => 30,
        RenderColor::Red => 31,
        RenderColor::Green => 32,
        RenderColor::Yellow => 33,
        RenderColor::Blue => 34,
        RenderColor::Magenta => 35,
        RenderColor::Cyan => 36,
        RenderColor::White => 37,
    }
}

const fn background_code(color: RenderColor) -> u8 {
    match color {
        RenderColor::Default => 49,
        RenderColor::Black => 40,
        RenderColor::Red => 41,
        RenderColor::Green => 42,
        RenderColor::Yellow => 43,
        RenderColor::Blue => 44,
        RenderColor::Magenta => 45,
        RenderColor::Cyan => 46,
        RenderColor::White => 47,
    }
}
