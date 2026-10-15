//! Safety and fidelity tests for ratatui-buffer to DrawOp conversion.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use crate::ui::{
    render_surface::{RenderSurface, RenderSurfaceError},
    worker_protocol::{
        validate_prepared_frame, DrawOp, DrawOpError, FrameKey, ProtocolLimits, RenderColor,
        RenderStyle, Viewport,
    },
};

const LIMITS: ProtocolLimits = ProtocolLimits {
    max_payload_bytes: 4096,
    max_draw_ops: 64,
    max_text_bytes: 16,
};
const VIEWPORT: Viewport = Viewport {
    columns: 8,
    rows: 2,
};

fn surface_buffer() -> Buffer {
    Buffer::empty(Rect::new(0, 0, VIEWPORT.columns, VIEWPORT.rows))
}

#[test]
fn render_surface_maps_buffer_to_whitelisted_draw_ops() {
    let mut buffer = surface_buffer();
    buffer.set_string(
        0,
        0,
        "ok",
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    );
    buffer.set_string(0, 1, "界", Style::default().bg(Color::Blue));

    let surface = RenderSurface::new(VIEWPORT, LIMITS);
    let ops = surface.draw_ops(&buffer).expect("safe buffer converts");

    assert_eq!(ops.first(), Some(&DrawOp::SetStyle(RenderStyle::Reset)));
    assert!(ops.contains(&DrawOp::ClearRect {
        x: 0,
        y: 0,
        width: 8,
        height: 2
    }));
    assert!(ops.contains(&DrawOp::SetStyle(RenderStyle::Foreground(
        RenderColor::Green
    ))));
    assert!(ops.contains(&DrawOp::SetStyle(RenderStyle::Bold)));
    assert!(ops.contains(&DrawOp::SetStyle(RenderStyle::Background(
        RenderColor::Blue
    ))));
    assert!(ops
        .iter()
        .any(|op| matches!(op, DrawOp::Text(text) if text.contains("ok"))));
    assert!(ops
        .iter()
        .any(|op| matches!(op, DrawOp::Text(text) if text.contains('界'))));
    assert!(ops.iter().all(|op| match op {
        DrawOp::Text(text) => !text.chars().any(char::is_control),
        _ => true,
    }));
}

#[test]
fn render_surface_prepares_host_recomputable_geometry() {
    let mut buffer = surface_buffer();
    buffer.set_string(2, 1, "yes", Style::default());
    let surface = RenderSurface::new(VIEWPORT, LIMITS);
    let frame = surface
        .prepare_frame(
            &buffer,
            FrameKey {
                worker_generation: 4,
                frame_id: 9,
                capture_id: Some(2),
            },
        )
        .expect("prepare frame");

    assert_eq!(frame.worker_generation, 4);
    assert_eq!(frame.frame_id, 9);
    assert_eq!(frame.capture_id, Some(2));
    assert!(frame.full_repaint);
    assert_eq!(validate_prepared_frame(&frame, VIEWPORT, &LIMITS), Ok(()));
}

#[test]
fn render_surface_rejects_unknown_color_style() {
    let mut buffer = surface_buffer();
    buffer.set_string(0, 0, "x", Style::default().fg(Color::Rgb(1, 2, 3)));
    let surface = RenderSurface::new(VIEWPORT, LIMITS);
    assert_eq!(
        surface.draw_ops(&buffer),
        Err(RenderSurfaceError::UnsupportedColor(Color::Rgb(1, 2, 3)))
    );
}

#[test]
fn render_surface_rejects_unknown_modifier_style() {
    let mut buffer = surface_buffer();
    buffer.set_string(
        0,
        0,
        "x",
        Style::default().add_modifier(Modifier::UNDERLINED),
    );
    let surface = RenderSurface::new(VIEWPORT, LIMITS);
    assert_eq!(
        surface.draw_ops(&buffer),
        Err(RenderSurfaceError::UnsupportedModifier(
            Modifier::UNDERLINED
        ))
    );
}

#[test]
fn render_surface_rejects_control_text() {
    let mut buffer = surface_buffer();
    buffer[(0, 0)].set_symbol("\u{1b}[6n");
    let surface = RenderSurface::new(VIEWPORT, LIMITS);
    assert!(matches!(
        surface.draw_ops(&buffer),
        Err(RenderSurfaceError::UnsafeDrawOp(_))
    ));
}

#[test]
fn render_surface_rejects_buffer_geometry_mismatch() {
    let buffer = Buffer::empty(Rect::new(0, 0, 7, 2));
    let surface = RenderSurface::new(VIEWPORT, LIMITS);
    assert_eq!(
        surface.draw_ops(&buffer),
        Err(RenderSurfaceError::BufferGeometryMismatch {
            expected: VIEWPORT,
            actual: Viewport {
                columns: 7,
                rows: 2
            },
        })
    );
}

#[test]
fn render_surface_applies_injected_draw_op_limit() {
    let buffer = surface_buffer();
    assert!(RenderSurface::new(VIEWPORT, LIMITS)
        .draw_ops(&buffer)
        .is_ok());

    let limits = ProtocolLimits {
        max_draw_ops: 1,
        ..LIMITS
    };
    assert!(matches!(
        RenderSurface::new(VIEWPORT, limits).draw_ops(&buffer),
        Err(RenderSurfaceError::UnsafeDrawOp(
            DrawOpError::TooManyDrawOps { max: 1, .. }
        ))
    ));
}

#[test]
fn prepare_frame_applies_injected_draw_op_limit() {
    let buffer = surface_buffer();
    let baseline_ops = RenderSurface::new(VIEWPORT, LIMITS)
        .draw_ops(&buffer)
        .expect("baseline draw ops");
    // One below the frame's own op count: the frame exceeds the injected limit
    // and must be refused rather than truncated.
    let limits = ProtocolLimits {
        max_draw_ops: baseline_ops.len() - 1,
        ..LIMITS
    };
    let key = FrameKey {
        worker_generation: 4,
        frame_id: 9,
        capture_id: Some(2),
    };

    assert!(matches!(
        RenderSurface::new(VIEWPORT, limits).prepare_frame(&buffer, key),
        Err(RenderSurfaceError::UnsafeDrawOp(
            DrawOpError::TooManyDrawOps { .. }
        ))
    ));

    // At the exact limit the same frame is accepted, so the rejection above is
    // about the op count and not about `prepare_frame` always failing.
    let exact = ProtocolLimits {
        max_draw_ops: baseline_ops.len(),
        ..LIMITS
    };
    assert!(RenderSurface::new(VIEWPORT, exact)
        .prepare_frame(&buffer, key)
        .is_ok());
}
