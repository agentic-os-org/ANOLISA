//! Converts a ratatui [`Buffer`] into the closed set of protocol [`DrawOp`]s.
//!
//! This adapter is the only place ratatui cell styling crosses into the worker
//! protocol. Unsupported colors or modifiers fail closed instead of being
//! converted to terminal escape sequences.

use ratatui::{
    buffer::{Buffer, Cell},
    style::{Color, Modifier},
};
use unicode_width::UnicodeWidthStr;

use super::worker_protocol::{
    recompute_geometry, validate_draw_op, DrawOp, DrawOpError, FrameKey, PreparedFrame,
    ProtocolLimits, RenderColor, RenderStyle, Viewport,
};

/// Typed conversion failures while translating a ratatui buffer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderSurfaceError {
    /// A zero-sized viewport cannot contain any addressable cursor position.
    EmptyViewport,
    /// The buffer dimensions differ from the Host-requested viewport.
    BufferGeometryMismatch {
        /// Host-requested dimensions.
        expected: Viewport,
        /// Buffer dimensions.
        actual: Viewport,
    },
    /// A ratatui color has no whitelisted [`RenderColor`] representation.
    UnsupportedColor(Color),
    /// A ratatui modifier has no whitelisted [`RenderStyle`] representation.
    UnsupportedModifier(Modifier),
    /// The buffer's public area/content fields disagree, so safe cell lookup
    /// cannot cover the advertised rectangle.
    MalformedBuffer,
    /// A cell symbol has zero width or would run beyond the buffer row.
    InvalidCellWidth {
        /// Cell column relative to the buffer.
        x: u16,
        /// Cell row relative to the buffer.
        y: u16,
        /// Terminal display width of the symbol.
        width: usize,
    },
    /// A generated op failed the same gate the Host applies to worker output.
    UnsafeDrawOp(DrawOpError),
}

impl std::fmt::Display for RenderSurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderSurfaceError::EmptyViewport => write!(f, "render viewport must be non-empty"),
            RenderSurfaceError::BufferGeometryMismatch { expected, actual } => write!(
                f,
                "buffer is {}x{}, expected {}x{}",
                actual.columns, actual.rows, expected.columns, expected.rows
            ),
            RenderSurfaceError::UnsupportedColor(color) => {
                write!(f, "ratatui color {color:?} is not in the wire palette")
            }
            RenderSurfaceError::UnsupportedModifier(modifier) => {
                write!(f, "ratatui modifier {modifier:?} is not a wire style")
            }
            RenderSurfaceError::MalformedBuffer => {
                write!(f, "buffer content does not cover its advertised area")
            }
            RenderSurfaceError::InvalidCellWidth { x, y, width } => {
                write!(f, "cell ({x}, {y}) has invalid display width {width}")
            }
            RenderSurfaceError::UnsafeDrawOp(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RenderSurfaceError {}

impl From<DrawOpError> for RenderSurfaceError {
    fn from(value: DrawOpError) -> Self {
        RenderSurfaceError::UnsafeDrawOp(value)
    }
}

/// A Host-sized surface that translates a complete ratatui buffer into typed
/// drawing primitives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderSurface {
    viewport: Viewport,
    limits: ProtocolLimits,
}

impl RenderSurface {
    /// Create a surface for the Host-requested `viewport` and approved limits.
    #[must_use]
    pub const fn new(viewport: Viewport, limits: ProtocolLimits) -> Self {
        Self { viewport, limits }
    }

    /// Host-requested viewport this surface renders into.
    #[must_use]
    pub const fn viewport(self) -> Viewport {
        self.viewport
    }

    /// Convert a complete ratatui buffer into whitelisted [`DrawOp`]s.
    ///
    /// The output starts from a reset style and clears the viewport, so it is a
    /// deterministic full repaint. Blank trailing cells are omitted because the
    /// preceding clear already establishes them. Styled blank cells are retained.
    ///
    /// # Errors
    /// Returns [`RenderSurfaceError`] when dimensions disagree, a cell uses an
    /// unsupported style, text contains terminal controls, a cell has invalid
    /// display width, or the generated op count exceeds protocol limits.
    pub fn draw_ops(&self, buffer: &Buffer) -> Result<Vec<DrawOp>, RenderSurfaceError> {
        self.validate_buffer_geometry(buffer)?;

        let mut ops = Vec::new();
        push_op(&mut ops, DrawOp::SetStyle(RenderStyle::Reset), &self.limits)?;
        push_op(
            &mut ops,
            DrawOp::ClearRect {
                x: 0,
                y: 0,
                width: self.viewport.columns,
                height: self.viewport.rows,
            },
            &self.limits,
        )?;

        let mut active_style = CellStyle::default();
        for y in 0..self.viewport.rows {
            let Some(last_x) = self.last_rendered_x(buffer, y)? else {
                continue;
            };
            push_op(&mut ops, DrawOp::MoveTo { x: 0, y }, &self.limits)?;
            self.render_row(buffer, y, last_x, &mut active_style, &mut ops)?;
        }
        if active_style != CellStyle::default() {
            push_op(&mut ops, DrawOp::SetStyle(RenderStyle::Reset), &self.limits)?;
        }

        for op in &ops {
            validate_draw_op(op, self.viewport, &self.limits)?;
        }
        Ok(ops)
    }

    /// Convert a buffer directly into a [`PreparedFrame`] carrying the
    /// Host-assigned key. Geometry is computed from the generated ops and can be
    /// recomputed independently by the receiving Host.
    ///
    /// Cursor visibility is deliberately not part of a worker frame: the worker
    /// shares the outer tty with the Bash prompt and every other presentation
    /// owner, so letting it show or hide the cursor would reach outside its own
    /// surface. The Host applies its cursor policy when it publishes.
    ///
    /// # Errors
    /// Returns [`RenderSurfaceError`] for any buffer/style/op violation.
    pub fn prepare_frame(
        &self,
        buffer: &Buffer,
        key: FrameKey,
    ) -> Result<PreparedFrame, RenderSurfaceError> {
        let draw_ops = self.draw_ops(buffer)?;
        let geometry = recompute_geometry(&draw_ops, self.viewport, &self.limits)?;
        Ok(PreparedFrame {
            worker_generation: key.worker_generation,
            frame_id: key.frame_id,
            capture_id: key.capture_id,
            draw_ops,
            geometry,
            full_repaint: true,
        })
    }

    fn validate_buffer_geometry(&self, buffer: &Buffer) -> Result<(), RenderSurfaceError> {
        if self.viewport.columns == 0 || self.viewport.rows == 0 {
            return Err(RenderSurfaceError::EmptyViewport);
        }
        let actual = Viewport {
            columns: buffer.area.width,
            rows: buffer.area.height,
        };
        if actual != self.viewport {
            return Err(RenderSurfaceError::BufferGeometryMismatch {
                expected: self.viewport,
                actual,
            });
        }
        Ok(())
    }

    fn last_rendered_x(&self, buffer: &Buffer, y: u16) -> Result<Option<u16>, RenderSurfaceError> {
        let mut last = None;
        for x in 0..self.viewport.columns {
            let cell = cell_at(buffer, x, y)?;
            let style = CellStyle::from_cell(cell)?;
            validate_cell_symbol(cell, x, y, self.viewport.columns, &self.limits)?;
            if (!cell.symbol().is_empty() && cell.symbol() != " ") || style != CellStyle::default()
            {
                last = Some(x);
            }
        }
        Ok(last)
    }

    fn render_row(
        &self,
        buffer: &Buffer,
        y: u16,
        last_x: u16,
        active_style: &mut CellStyle,
        ops: &mut Vec<DrawOp>,
    ) -> Result<(), RenderSurfaceError> {
        let mut x = 0u16;
        let mut run_style = None;
        let mut run = String::new();
        while x <= last_x {
            let cell = cell_at(buffer, x, y)?;
            let style = CellStyle::from_cell(cell)?;
            let symbol = if cell.symbol().is_empty() {
                " "
            } else {
                cell.symbol()
            };
            let width = UnicodeWidthStr::width(symbol);
            let end = usize::from(x).saturating_add(width);
            if width == 0 || end > usize::from(self.viewport.columns) {
                return Err(RenderSurfaceError::InvalidCellWidth { x, y, width });
            }

            if run_style != Some(style)
                || (!run.is_empty()
                    && run.len().saturating_add(symbol.len()) > self.limits.max_text_bytes)
            {
                flush_text_run(ops, &mut run, &self.limits)?;
                set_cell_style(ops, active_style, style, &self.limits)?;
                run_style = Some(style);
            }
            if symbol.len() > self.limits.max_text_bytes {
                return Err(RenderSurfaceError::UnsafeDrawOp(DrawOpError::TextTooLong {
                    len: symbol.len(),
                    max: self.limits.max_text_bytes,
                }));
            }
            run.push_str(symbol);
            x = u16::try_from(end).map_err(|_| RenderSurfaceError::InvalidCellWidth {
                x,
                y,
                width,
            })?;
        }
        flush_text_run(ops, &mut run, &self.limits)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CellStyle {
    fg: RenderColor,
    bg: RenderColor,
    bold: bool,
    dim: bool,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            fg: RenderColor::Default,
            bg: RenderColor::Default,
            bold: false,
            dim: false,
        }
    }
}

impl CellStyle {
    fn from_cell(cell: &Cell) -> Result<Self, RenderSurfaceError> {
        let supported = Modifier::BOLD | Modifier::DIM;
        if !supported.contains(cell.modifier) {
            return Err(RenderSurfaceError::UnsupportedModifier(cell.modifier));
        }
        Ok(Self {
            fg: render_color(cell.fg)?,
            bg: render_color(cell.bg)?,
            bold: cell.modifier.contains(Modifier::BOLD),
            dim: cell.modifier.contains(Modifier::DIM),
        })
    }
}

fn render_color(color: Color) -> Result<RenderColor, RenderSurfaceError> {
    match color {
        Color::Reset => Ok(RenderColor::Default),
        Color::Black | Color::DarkGray => Ok(RenderColor::Black),
        Color::Red => Ok(RenderColor::Red),
        Color::Green => Ok(RenderColor::Green),
        Color::Yellow => Ok(RenderColor::Yellow),
        Color::Blue => Ok(RenderColor::Blue),
        Color::Magenta => Ok(RenderColor::Magenta),
        Color::Cyan => Ok(RenderColor::Cyan),
        Color::Gray | Color::White => Ok(RenderColor::White),
        Color::LightRed
        | Color::LightGreen
        | Color::LightYellow
        | Color::LightBlue
        | Color::LightMagenta
        | Color::LightCyan
        | Color::Indexed(_)
        | Color::Rgb(_, _, _) => Err(RenderSurfaceError::UnsupportedColor(color)),
    }
}

fn cell_at(buffer: &Buffer, x: u16, y: u16) -> Result<&Cell, RenderSurfaceError> {
    let absolute_x = buffer
        .area
        .x
        .checked_add(x)
        .ok_or(RenderSurfaceError::MalformedBuffer)?;
    let absolute_y = buffer
        .area
        .y
        .checked_add(y)
        .ok_or(RenderSurfaceError::MalformedBuffer)?;
    buffer
        .cell((absolute_x, absolute_y))
        .ok_or(RenderSurfaceError::MalformedBuffer)
}

fn validate_cell_symbol(
    cell: &Cell,
    x: u16,
    y: u16,
    columns: u16,
    limits: &ProtocolLimits,
) -> Result<(), RenderSurfaceError> {
    let symbol = if cell.symbol().is_empty() {
        " "
    } else {
        cell.symbol()
    };
    validate_draw_op(
        &DrawOp::Text(symbol.to_owned()),
        Viewport {
            columns,
            rows: y + 1,
        },
        limits,
    )?;
    let width = UnicodeWidthStr::width(symbol);
    if width == 0 || usize::from(x).saturating_add(width) > usize::from(columns) {
        return Err(RenderSurfaceError::InvalidCellWidth { x, y, width });
    }
    Ok(())
}

fn set_cell_style(
    ops: &mut Vec<DrawOp>,
    active: &mut CellStyle,
    next: CellStyle,
    limits: &ProtocolLimits,
) -> Result<(), RenderSurfaceError> {
    if *active == next {
        return Ok(());
    }
    push_op(ops, DrawOp::SetStyle(RenderStyle::Reset), limits)?;
    if next.bold {
        push_op(ops, DrawOp::SetStyle(RenderStyle::Bold), limits)?;
    }
    if next.dim {
        push_op(ops, DrawOp::SetStyle(RenderStyle::Dim), limits)?;
    }
    if next.fg != RenderColor::Default {
        push_op(
            ops,
            DrawOp::SetStyle(RenderStyle::Foreground(next.fg)),
            limits,
        )?;
    }
    if next.bg != RenderColor::Default {
        push_op(
            ops,
            DrawOp::SetStyle(RenderStyle::Background(next.bg)),
            limits,
        )?;
    }
    *active = next;
    Ok(())
}

fn flush_text_run(
    ops: &mut Vec<DrawOp>,
    run: &mut String,
    limits: &ProtocolLimits,
) -> Result<(), RenderSurfaceError> {
    if run.is_empty() {
        return Ok(());
    }
    let text = std::mem::take(run);
    push_op(ops, DrawOp::Text(text), limits)
}

fn push_op(
    ops: &mut Vec<DrawOp>,
    op: DrawOp,
    limits: &ProtocolLimits,
) -> Result<(), RenderSurfaceError> {
    let next_count = ops.len().saturating_add(1);
    if next_count > limits.max_draw_ops {
        return Err(RenderSurfaceError::UnsafeDrawOp(
            DrawOpError::TooManyDrawOps {
                count: next_count,
                max: limits.max_draw_ops,
            },
        ));
    }
    ops.push(op);
    Ok(())
}
