//! Host-side validation and geometry recomputation for typed DrawOps.

use unicode_width::UnicodeWidthStr;

use super::{DrawOp, FrameGeometry, PreparedFrame, ProtocolLimits, Viewport};

/// Reasons a [`DrawOp`] or [`PreparedFrame`] is rejected by the Host's safety
/// gate. The worker cannot smuggle raw terminal control through typed ops.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrawOpError {
    /// Text contained an ASCII/Unicode control character (ESC, NUL, CSI, …).
    /// This is what blocks OSC/DSR/DA and other escape-sequence injection.
    ControlCharacter,
    /// Text exceeded [`ProtocolLimits::max_text_bytes`].
    TextTooLong {
        /// Actual UTF-8 byte length.
        len: usize,
        /// Enforced maximum.
        max: usize,
    },
    /// A coordinate or rectangle fell outside the requested viewport.
    OutOfBounds {
        /// Offending left/target column.
        x: u16,
        /// Offending top/target row.
        y: u16,
        /// Viewport width.
        columns: u16,
        /// Viewport height.
        rows: u16,
    },
    /// The frame carried more draw ops than [`ProtocolLimits::max_draw_ops`].
    TooManyDrawOps {
        /// Actual op count.
        count: usize,
        /// Enforced maximum.
        max: usize,
    },
    /// Reported geometry did not match the requested viewport, i.e. the worker
    /// tried to forge a different surface than the Host asked for.
    GeometryMismatch {
        /// Viewport the Host requested.
        expected: Viewport,
        /// Geometry the worker reported.
        actual: FrameGeometry,
    },
}

impl std::fmt::Display for DrawOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DrawOpError::ControlCharacter => {
                write!(f, "draw op text contains a control character")
            }
            DrawOpError::TextTooLong { len, max } => {
                write!(f, "draw op text is {len} bytes, over the {max}-byte limit")
            }
            DrawOpError::OutOfBounds {
                x,
                y,
                columns,
                rows,
            } => {
                write!(
                    f,
                    "draw op ({x}, {y}) is outside the {columns}x{rows} viewport"
                )
            }
            DrawOpError::TooManyDrawOps { count, max } => {
                write!(f, "frame carries {count} draw ops, over the {max} limit")
            }
            DrawOpError::GeometryMismatch { expected, actual } => write!(
                f,
                "reported geometry {}x{} does not match requested viewport {}x{}",
                actual.columns, actual.rows, expected.columns, expected.rows
            ),
        }
    }
}

impl std::error::Error for DrawOpError {}

/// Validate a single [`DrawOp`] against the requested `viewport`.
///
/// This is the whitelist gate: text may not contain control characters (which
/// blocks ESC/OSC/DSR/DA injection) or exceed the byte limit, and coordinates
/// must stay inside the viewport. Styles are a closed enum, so any forged style
/// is already rejected at JSON decode time.
///
/// # Errors
/// Returns a [`DrawOpError`] describing the first violation found.
pub fn validate_draw_op(
    op: &DrawOp,
    viewport: Viewport,
    limits: &ProtocolLimits,
) -> Result<(), DrawOpError> {
    match op {
        DrawOp::ClearRect {
            x,
            y,
            width,
            height,
        } => {
            let right = x.checked_add(*width);
            let bottom = y.checked_add(*height);
            match (right, bottom) {
                (Some(right), Some(bottom))
                    if *width > 0
                        && *height > 0
                        && *x < viewport.columns
                        && *y < viewport.rows
                        && right <= viewport.columns
                        && bottom <= viewport.rows =>
                {
                    Ok(())
                }
                _ => Err(DrawOpError::OutOfBounds {
                    x: *x,
                    y: *y,
                    columns: viewport.columns,
                    rows: viewport.rows,
                }),
            }
        }
        DrawOp::MoveTo { x, y } => {
            if *x < viewport.columns && *y < viewport.rows {
                Ok(())
            } else {
                Err(DrawOpError::OutOfBounds {
                    x: *x,
                    y: *y,
                    columns: viewport.columns,
                    rows: viewport.rows,
                })
            }
        }
        DrawOp::SetStyle(_) => Ok(()),
        DrawOp::Text(text) => validate_text(text, limits),
        DrawOp::CursorVisible(_) => Ok(()),
    }
}

/// Reject control characters first, then oversize text.
fn validate_text(text: &str, limits: &ProtocolLimits) -> Result<(), DrawOpError> {
    if text.chars().any(char::is_control) {
        return Err(DrawOpError::ControlCharacter);
    }
    let len = text.len();
    let max = limits.max_text_bytes;
    if len > max {
        return Err(DrawOpError::TextTooLong { len, max });
    }
    Ok(())
}

/// Recompute frame geometry solely from Host-trusted `viewport` and validated
/// draw ops. The worker's reported geometry is never trusted as input.
///
/// Text advances by terminal display columns, not UTF-8 byte count. Newlines
/// and every other control character are rejected by [`validate_draw_op`], so
/// text cannot move the cursor vertically or trigger terminal behavior.
///
/// # Errors
/// Returns [`DrawOpError`] if an op is invalid or text would advance beyond the
/// right edge of the viewport.
pub fn recompute_geometry(
    draw_ops: &[DrawOp],
    viewport: Viewport,
    limits: &ProtocolLimits,
) -> Result<FrameGeometry, DrawOpError> {
    use unicode_width::UnicodeWidthStr;

    let mut cursor_x = 0usize;
    let mut cursor_y = 0u16;
    for op in draw_ops {
        validate_draw_op(op, viewport, limits)?;
        match op {
            DrawOp::MoveTo { x, y } => {
                cursor_x = usize::from(*x);
                cursor_y = *y;
            }
            DrawOp::Text(text) => {
                cursor_x = cursor_x.saturating_add(UnicodeWidthStr::width(text.as_str()));
                if cursor_x > usize::from(viewport.columns) {
                    return Err(DrawOpError::OutOfBounds {
                        x: u16::try_from(cursor_x).unwrap_or(u16::MAX),
                        y: cursor_y,
                        columns: viewport.columns,
                        rows: viewport.rows,
                    });
                }
            }
            DrawOp::ClearRect { .. } | DrawOp::SetStyle(_) | DrawOp::CursorVisible(_) => {}
        }
    }
    // The range check above proves the conversion cannot truncate.
    let cursor_x = u16::try_from(cursor_x).map_err(|_| DrawOpError::OutOfBounds {
        x: u16::MAX,
        y: cursor_y,
        columns: viewport.columns,
        rows: viewport.rows,
    })?;
    Ok(FrameGeometry {
        columns: viewport.columns,
        rows: viewport.rows,
        cursor_x,
        cursor_y,
    })
}

/// Validate a whole [`PreparedFrame`] against the requested `viewport`.
///
/// Enforces the op-count ceiling, recomputes geometry from the typed ops, and
/// rejects any worker-reported geometry that differs from the recomputed value.
///
/// # Errors
/// Returns the first [`DrawOpError`] encountered.
pub fn validate_prepared_frame(
    frame: &PreparedFrame,
    viewport: Viewport,
    limits: &ProtocolLimits,
) -> Result<(), DrawOpError> {
    if frame.draw_ops.len() > limits.max_draw_ops {
        return Err(DrawOpError::TooManyDrawOps {
            count: frame.draw_ops.len(),
            max: limits.max_draw_ops,
        });
    }
    let recomputed = recompute_geometry(&frame.draw_ops, viewport, limits)?;
    if frame.geometry != recomputed {
        return Err(DrawOpError::GeometryMismatch {
            expected: viewport,
            actual: frame.geometry,
        });
    }
    Ok(())
}
