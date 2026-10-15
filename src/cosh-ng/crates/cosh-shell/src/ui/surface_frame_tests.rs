use crate::ui::surface_frame::{
    encode_surface_frame, encode_surface_seal, encode_surface_withdraw, SurfaceFrameError,
    SurfaceRect,
};
use crate::ui::worker_protocol::{DrawOp, RenderColor, RenderStyle};

const COLUMNS: u16 = 40;
const HEIGHT: u16 = 12;

/// A rectangle anchored below the top of the viewport, which is what the
/// acquisition algorithm produces: the surface occupies the bottom rows.
const fn rect() -> SurfaceRect {
    SurfaceRect {
        anchor_row: 13,
        columns: COLUMNS,
        height: HEIGHT,
    }
}

fn card_ops() -> Vec<DrawOp> {
    let mut ops = vec![
        DrawOp::SetStyle(RenderStyle::Reset),
        DrawOp::ClearRect {
            x: 0,
            y: 0,
            width: COLUMNS,
            height: HEIGHT,
        },
    ];
    for row in 0..HEIGHT {
        ops.push(DrawOp::MoveTo { x: 0, y: row });
        ops.push(DrawOp::SetStyle(RenderStyle::Foreground(RenderColor::Cyan)));
        ops.push(DrawOp::Text(format!("row {row}")));
    }
    ops
}

/// Every `CUP` target in the encoded bytes, as 1-based (row, column).
fn cup_targets(bytes: &[u8]) -> Vec<(u16, u16)> {
    let text = String::from_utf8_lossy(bytes);
    let mut targets = Vec::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("\x1b[") {
        rest = &rest[start + 2..];
        let end = rest
            .find(|ch: char| !ch.is_ascii_digit() && ch != ';')
            .unwrap_or(rest.len());
        let (params, final_byte) = (&rest[..end], rest[end..].chars().next());
        rest = &rest[end..];
        if final_byte == Some('H') {
            let mut parts = params.split(';');
            let row = parts.next().and_then(|value| value.parse().ok());
            let column = parts.next().and_then(|value| value.parse().ok());
            if let (Some(row), Some(column)) = (row, column) {
                targets.push((row, column));
            }
        }
    }
    targets
}

#[test]
fn frame_positions_are_anchored_to_the_host_rectangle() {
    let bytes = encode_surface_frame(
        &[DrawOp::MoveTo { x: 2, y: 0 }, DrawOp::Text("hi".to_owned())],
        rect(),
    )
    .expect("inside the rectangle");
    let text = String::from_utf8_lossy(&bytes);
    // anchor_row 13 is 0-based, so surface row 0 is viewport row 14.
    assert!(text.contains("\x1b[14;3H"), "{text:?}");
    assert!(
        !text.contains("\x1b[1;1H"),
        "viewport origin leaked: {text:?}"
    );
}

#[test]
fn no_position_leaves_the_rectangle() {
    let bytes = encode_surface_frame(&card_ops(), rect()).expect("card frame");
    let targets = cup_targets(&bytes);
    assert!(
        !targets.is_empty(),
        "a full repaint must position the cursor"
    );
    for &(row, column) in &targets {
        assert!(
            (14..=25).contains(&row),
            "row {row} outside 14..=25 in {targets:?}"
        );
        assert!(
            (1..=COLUMNS).contains(&column),
            "column {column} outside 1..={COLUMNS} in {targets:?}"
        );
    }
}

/// Writing into the last column leaves a pending wrap that only materialises on
/// the next character, so each row must end with an explicit `CR` before the
/// next absolute move; otherwise the card's 40-column rows scroll the viewport.
#[test]
fn every_painted_row_is_closed_with_a_carriage_return() {
    let bytes = encode_surface_frame(
        &[
            DrawOp::MoveTo { x: 0, y: 0 },
            DrawOp::Text("x".repeat(COLUMNS as usize)),
            DrawOp::MoveTo { x: 0, y: 1 },
            DrawOp::Text("second".to_owned()),
        ],
        rect(),
    )
    .expect("two full rows");
    let text = String::from_utf8_lossy(&bytes);
    let first_row_end = text.find("\x1b[15;1H").expect("second row move");
    assert_eq!(
        text.as_bytes()[first_row_end - 1],
        b'\r',
        "row 0 must be closed by CR before the next CUP: {text:?}"
    );
}

#[test]
fn transaction_tail_restores_style_cursor_and_sentinel() {
    let bytes = encode_surface_frame(&card_ops(), rect()).expect("card frame");
    let text = String::from_utf8_lossy(&bytes);
    // sentinel_row = anchor_row + height - 1 = 24 (0-based) = viewport row 25.
    assert!(
        text.ends_with("\x1b[0m\x1b[?25l\x1b[25;1H"),
        "tail must reset SGR, hide the cursor and park on the sentinel: {text:?}"
    );
}

#[test]
fn sentinel_row_is_the_last_surface_row_not_one_past_it() {
    // Parking one row past the surface would be row 26 of a 25-row viewport,
    // which scrolls the terminal and destroys the acquisition invariant.
    assert_eq!(rect().sentinel_row(), 24);
    assert_eq!(
        SurfaceRect {
            anchor_row: 0,
            columns: COLUMNS,
            height: 1,
        }
        .sentinel_row(),
        0
    );
}

#[test]
fn worker_cursor_visibility_is_rejected() {
    for visible in [true, false] {
        assert_eq!(
            encode_surface_frame(&[DrawOp::CursorVisible(visible)], rect()),
            Err(SurfaceFrameError::WorkerControlledCursor),
            "cursor policy belongs to the Host"
        );
    }
}

#[test]
fn text_crossing_the_right_edge_is_rejected() {
    let error = encode_surface_frame(
        &[
            DrawOp::MoveTo { x: 1, y: 0 },
            DrawOp::Text("x".repeat(COLUMNS as usize)),
        ],
        rect(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        SurfaceFrameError::TextOverflowsRow {
            x: 1,
            y: 0,
            width: COLUMNS as usize,
        }
    );
}

#[test]
fn ops_outside_the_rectangle_are_rejected() {
    assert_eq!(
        encode_surface_frame(&[DrawOp::MoveTo { x: COLUMNS, y: 0 }], rect()),
        Err(SurfaceFrameError::OutsideRectangle { x: COLUMNS, y: 0 })
    );
    assert_eq!(
        encode_surface_frame(&[DrawOp::MoveTo { x: 0, y: HEIGHT }], rect()),
        Err(SurfaceFrameError::OutsideRectangle { x: 0, y: HEIGHT })
    );
    assert_eq!(
        encode_surface_frame(
            &[DrawOp::ClearRect {
                x: 0,
                y: 0,
                width: COLUMNS,
                height: HEIGHT + 1,
            }],
            rect()
        ),
        Err(SurfaceFrameError::OutsideRectangle { x: 0, y: 0 })
    );
}

#[test]
fn text_without_a_position_is_rejected() {
    assert_eq!(
        encode_surface_frame(&[DrawOp::Text("hi".to_owned())], rect()),
        Err(SurfaceFrameError::TextWithoutPosition)
    );
}

/// A rejected frame must not be publishable: encoding is all-or-nothing, so a
/// cursor-visibility op anywhere in the frame discards the whole transaction
/// instead of leaving the caller with bytes it would then write.
#[test]
fn a_late_cursor_op_rejects_the_whole_frame() {
    let result = encode_surface_frame(
        &[
            DrawOp::MoveTo { x: 0, y: 0 },
            DrawOp::Text("ok".to_owned()),
            DrawOp::CursorVisible(true),
        ],
        rect(),
    );
    assert_eq!(result, Err(SurfaceFrameError::WorkerControlledCursor));
}

#[test]
fn clear_rect_erases_exactly_the_surface_rows() {
    let bytes = encode_surface_frame(
        &[DrawOp::ClearRect {
            x: 0,
            y: 0,
            width: COLUMNS,
            height: HEIGHT,
        }],
        rect(),
    )
    .expect("clear inside the rectangle");
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(
        text.matches(&format!("\x1b[{COLUMNS}X")).count(),
        HEIGHT as usize,
        "one erase per surface row: {text:?}"
    );
    let rows: Vec<u16> = cup_targets(&bytes).iter().map(|(row, _)| *row).collect();
    // One CUP per cleared row, then the transaction tail's sentinel CUP.
    let mut expected: Vec<u16> = (14..=25).collect();
    expected.push(25);
    assert_eq!(rows, expected, "{rows:?}");
}

#[test]
fn withdraw_erases_every_row_and_parks_at_the_anchor() {
    let bytes = encode_surface_withdraw(rect());
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(text.matches("\x1b[2K").count(), HEIGHT as usize, "{text:?}");
    assert!(
        text.ends_with("\x1b[0m\x1b[?25h\x1b[14;1H"),
        "withdraw must restore the caret and leave the cursor where the surface started: {text:?}"
    );
}

/// Sealing keeps the receipt readable, so it must not erase anything: the only
/// rows it touches are the sentinel and the fresh row below the surface.
#[test]
fn seal_keeps_the_receipt_and_steps_onto_a_fresh_row() {
    let bytes = encode_surface_seal(rect());
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("\x1b[2K"), "seal must not erase: {text:?}");
    assert_eq!(
        text, "\x1b[25;1H\x1b[0m\r\n\x1b[?25h",
        "park on the sentinel, reset, scroll once, restore the caret: {text:?}"
    );
    assert_eq!(cup_targets(&bytes), vec![(25, 1)]);
}

/// Wide characters occupy two columns, so counting bytes or chars would let a
/// CJK preview overrun the row and wrap into the next surface row.
#[test]
fn wide_characters_count_as_two_columns() {
    let twenty_wide = "汉".repeat(20);
    assert!(
        encode_surface_frame(
            &[
                DrawOp::MoveTo { x: 0, y: 0 },
                DrawOp::Text(twenty_wide.clone()),
            ],
            rect()
        )
        .is_ok(),
        "20 wide characters fit exactly in 40 columns"
    );
    assert_eq!(
        encode_surface_frame(
            &[
                DrawOp::MoveTo { x: 0, y: 0 },
                DrawOp::Text(format!("{twenty_wide}汉")),
            ],
            rect()
        ),
        Err(SurfaceFrameError::TextOverflowsRow {
            x: 0,
            y: 0,
            width: 42,
        })
    );
}
