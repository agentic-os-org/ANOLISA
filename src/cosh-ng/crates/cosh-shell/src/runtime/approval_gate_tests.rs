use std::io::{self, Write};

use nix::pty::Winsize;
use ratatui::{buffer::Buffer, layout::Rect};

use super::approval_gate::{ApprovalSurfaceGate, GateError, GeometryTrust, SurfaceTerminal};
use crate::ui::approval_surface::render_approval_view;
use crate::ui::render_surface::RenderSurface;
use crate::ui::surface_frame::{encode_surface_frame, SurfaceRect};
use crate::ui::worker_protocol::{
    ApprovalActionView, ApprovalCardView, ProtocolLimits, RenderView, UiLanguage, Viewport,
};

const ROWS: u16 = 24;
const COLS: u16 = 40;

const LIMITS: ProtocolLimits = ProtocolLimits {
    max_payload_bytes: 1_048_576,
    max_draw_ops: 4096,
    max_text_bytes: 16_384,
};

/// A terminal emulator rather than a byte recorder: the whole point of the
/// rectangle algorithm is what a real VT does with the scroll-out, and asserting
/// on emitted bytes would only restate the encoder.
struct VtTerminal {
    parser: vt100::Parser,
    size: Winsize,
    accepted: usize,
    fail_after: Option<usize>,
}

impl VtTerminal {
    fn new(rows: u16, cols: u16) -> Self {
        Self::with_scrollback(rows, cols, 0)
    }

    /// Builds a terminal that keeps `scrollback` rows of history.
    ///
    /// `vt100` does model scrollback; a zero-length fixture simply cannot tell
    /// "pushed into history" apart from "destroyed", which is the entire claim the
    /// safe fresh region makes. Tests that assert preservation need a real budget.
    fn with_scrollback(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, scrollback),
            size: Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
            accepted: 0,
            fail_after: None,
        }
    }

    /// The viewport as exactly `ws_row` strings of `ws_col` characters.
    ///
    /// `Screen::contents` trims trailing blank rows, which is precisely what
    /// this suite must observe: a blank surface row and a missing one look the
    /// same there. Reading cells keeps blank rows addressable.
    fn rows(&self) -> Vec<String> {
        let screen = self.parser.screen();
        (0..self.size.ws_row)
            .map(|row| {
                (0..self.size.ws_col)
                    .map(|col| {
                        screen
                            .cell(row, col)
                            .and_then(|cell| cell.contents().chars().next())
                            .unwrap_or(' ')
                    })
                    .collect()
            })
            .collect()
    }

    fn cursor(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// Fills every viewport row with a distinct, identifiable line.
    fn fill(&mut self) {
        let mut out = String::new();
        for index in 0..self.size.ws_row {
            out.push_str(&format!("line {index:02}"));
            if index + 1 < self.size.ws_row {
                out.push_str("\r\n");
            }
        }
        self.parser.process(out.as_bytes());
    }

    /// Reports a new viewport without repainting, which is what a resize looks
    /// like to the gate: it only ever asks for `winsize`.
    fn set_size(&mut self, rows: u16, cols: u16) {
        self.size.ws_row = rows;
        self.size.ws_col = cols;
    }

    /// Puts distinct content on `height` rows starting at `anchor_row`.
    ///
    /// A reflow leaves the user's own output at the rows a rectangle recorded, so
    /// a test that wants to tell "erased" apart from "left alone" has to put
    /// something there first. Without it the acquired rows are the blank ones the
    /// scroll just exposed and both behaviours look identical.
    fn stamp_foreign_rows(&mut self, anchor_row: u16, height: u16) {
        let mut out = format!("\x1b[{};1H", anchor_row + 1);
        for offset in 0..height {
            out.push_str(&format!("user {offset:02}"));
            if offset + 1 < height {
                out.push_str("\r\n");
            }
        }
        self.parser.process(out.as_bytes());
    }
}

impl Write for VtTerminal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(limit) = self.fail_after {
            if self.accepted + buf.len() > limit {
                let take = limit.saturating_sub(self.accepted);
                self.parser.process(&buf[..take]);
                self.accepted += take;
                return Err(io::Error::other("injected write failure"));
            }
        }
        self.parser.process(buf);
        self.accepted += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SurfaceTerminal for VtTerminal {
    fn winsize(&mut self) -> io::Result<Winsize> {
        Ok(Winsize {
            ws_row: self.size.ws_row,
            ws_col: self.size.ws_col,
            ws_xpixel: 0,
            ws_ypixel: 0,
        })
    }

    fn accepted(&self) -> usize {
        self.accepted
    }

    fn reset_accepted(&mut self) {
        self.accepted = 0;
    }
}

fn card() -> ApprovalCardView {
    ApprovalCardView {
        revision: 3,
        language: UiLanguage::En,
        display_id: "req-1".to_string(),
        kind: "Bash".to_string(),
        risk: "high".to_string(),
        reason: "write redirection".to_string(),
        subject: "slice approval request".to_string(),
        preview: "printf '%s\\n' slice_effect >> '/tmp/effect.txt'".to_string(),
        label: "shell".to_string(),
        actions: vec![
            ApprovalActionView::Approve,
            ApprovalActionView::Deny,
            ApprovalActionView::Details,
        ],
        selected: ApprovalActionView::Approve,
        expanded: false,
        irrecoverable: false,
    }
}

/// Runs the real renderer and the real surface encoder, so this is the same
/// byte stream a worker frame would produce.
fn card_frame_bytes(rect: SurfaceRect) -> Vec<u8> {
    let viewport = Viewport {
        columns: rect.columns,
        rows: rect.height,
    };
    let mut buffer = Buffer::empty(Rect::new(0, 0, viewport.columns, viewport.rows));
    render_approval_view(&RenderView::ApprovalCard(card()), &mut buffer);
    let draw_ops = RenderSurface::new(viewport, LIMITS)
        .draw_ops(&buffer)
        .expect("card draw ops");
    encode_surface_frame(&draw_ops, rect).expect("card frame encodes inside the rectangle")
}

#[test]
fn acquire_scrolls_the_screen_up_by_exactly_the_surface_height() {
    let mut term = VtTerminal::new(ROWS, COLS);
    term.fill();
    let mut gate = ApprovalSurfaceGate::new(term);

    let height = 6;
    let rect = gate.acquire(height).expect("acquisition fits the viewport");

    assert_eq!(
        rect,
        SurfaceRect {
            anchor_row: ROWS - height,
            columns: COLS,
            height,
        }
    );
    assert_eq!(gate.rect(), Some(rect));

    let rows = gate.term_mut().rows();
    assert_eq!(rows.len(), ROWS as usize);
    // Every surviving line moved up by `height`, which is the observable proof
    // that the previous screen went into scrollback instead of being erased.
    let surviving = (ROWS - height) as usize;
    for (offset, row) in rows.iter().enumerate().take(surviving) {
        assert!(
            row.starts_with(&format!("line {:02}", offset + height as usize)),
            "row {offset} should hold line {:02}, got {row:?}",
            offset + height as usize
        );
    }
    for (offset, row) in rows.iter().enumerate().skip(surviving) {
        assert!(
            row.trim().is_empty(),
            "surface row {offset} must start blank, got {row:?}"
        );
    }
    assert_eq!(gate.term_mut().cursor(), (ROWS - 1, 0));
}

/// A narrow terminal cannot host the card, and the refusal has to happen before
/// anything is written: scrolling first and then discovering the width would
/// already have damaged the screen.
#[test]
fn acquire_refuses_a_narrow_terminal_before_writing_anything() {
    let mut term = VtTerminal::new(ROWS, COLS - 1);
    term.fill();
    let before = term.rows();
    let mut gate = ApprovalSurfaceGate::new(term);

    let error = gate
        .acquire(6)
        .expect_err("39 columns cannot host a 40 column surface");

    assert!(
        matches!(error, GateError::ViewportTooNarrow { columns: 39 }),
        "{error}"
    );
    assert_eq!(gate.rect(), None);
    assert_eq!(gate.term_mut().accepted(), 0, "nothing may be written");
    assert_eq!(gate.term_mut().rows(), before, "screen must be untouched");
}

#[test]
fn acquire_refuses_a_surface_that_does_not_fit_the_viewport() {
    for (rows, height, expected) in [
        (4u16, 6u16, "surface needs 6 rows, terminal has 4"),
        (24, 0, "surface needs 0 rows, terminal has 24"),
        (24, 25, "surface needs 25 rows, terminal has 24"),
    ] {
        let mut term = VtTerminal::new(rows, COLS);
        term.fill();
        let mut gate = ApprovalSurfaceGate::new(term);

        let error = gate.acquire(height).expect_err("must not fit");

        assert_eq!(error.to_string(), expected, "rows={rows} height={height}");
        assert!(
            matches!(error, GateError::ViewportTooShort { .. }),
            "{error}"
        );
        assert_eq!(gate.rect(), None);
        assert_eq!(gate.term_mut().accepted(), 0, "nothing may be written");
    }
}

#[test]
fn a_second_acquisition_is_refused_while_a_rectangle_is_live() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    let first = gate.acquire(6).expect("first acquisition");
    let accepted_after_first = gate.term_mut().accepted();

    let error = gate.acquire(4).expect_err("two rectangles would overlap");

    assert!(
        matches!(error, GateError::AlreadyAcquired(live) if live == first),
        "{error}"
    );
    assert_eq!(gate.rect(), Some(first));
    assert_eq!(gate.term_mut().accepted(), accepted_after_first);
}

/// This is the end-to-end placement proof: the renderer, the surface encoder and
/// the gate's rectangle must agree inside a real emulator, and nothing outside
/// the rectangle may change.
#[test]
fn a_worker_frame_lands_only_on_the_acquired_rows() {
    let mut term = VtTerminal::new(ROWS, COLS);
    term.fill();
    let mut gate = ApprovalSurfaceGate::new(term);
    let height = 14;
    let rect = gate.acquire(height).expect("acquisition fits the viewport");
    let before = gate.term_mut().rows();

    let frame = card_frame_bytes(rect);
    gate.write_presentation(&frame).expect("frame is written");

    let rows = gate.term_mut().rows();
    let anchor = rect.anchor_row as usize;
    let surface: String = rows[anchor..].join("\n");
    assert!(surface.contains("Allow once"), "{surface}");
    assert!(surface.contains("Deny"), "{surface}");
    assert!(surface.contains("Details"), "{surface}");
    assert!(surface.contains("write redirection"), "{surface}");
    assert!(surface.contains("slice_effect"), "{surface}");
    // Rows above the rectangle still hold exactly what the scroll-out left
    // there, so the frame did not reach into the shell's own output.
    for (offset, (row, previous)) in rows.iter().zip(&before).enumerate().take(anchor) {
        assert_eq!(row, previous, "row {offset} above the rectangle changed");
    }
    // The transaction parks on the sentinel, the surface's last row.
    assert_eq!(gate.term_mut().cursor(), (ROWS - 1, 0));
}

#[test]
fn withdraw_erases_the_surface_and_restores_the_caret() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    let rect = gate.acquire(14).expect("acquisition fits the viewport");
    let frame = card_frame_bytes(rect);
    gate.write_presentation(&frame).expect("frame is written");
    assert!(
        gate.term_mut().rows().join("\n").contains("Allow once"),
        "the card must be on screen before withdrawing it"
    );

    gate.withdraw().expect("withdraw releases the rectangle");

    assert_eq!(gate.rect(), None);
    let rows = gate.term_mut().rows();
    for (offset, row) in rows.iter().enumerate().skip(rect.anchor_row as usize) {
        assert!(
            row.trim().is_empty(),
            "row {offset} must be erased, got {row:?}"
        );
    }
    assert_eq!(
        gate.term_mut().cursor(),
        (rect.anchor_row, 0),
        "output continues where the surface started"
    );
}

/// A decision receipt stays readable: sealing must not erase the rows, it only
/// moves below them so later Bash output does not repaint the receipt.
#[test]
fn seal_keeps_the_receipt_readable_and_steps_below_it() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    let rect = gate.acquire(14).expect("acquisition fits the viewport");
    let frame = card_frame_bytes(rect);
    gate.write_presentation(&frame).expect("frame is written");
    let before = gate.term_mut().rows();
    assert!(
        before.join("\n").contains("Allow once"),
        "the receipt must be on screen before it is sealed"
    );

    gate.seal_to_scrollback().expect("seal releases the lease");

    assert_eq!(gate.rect(), None);
    let after = gate.term_mut().rows();
    let screen = after.join("\n");
    assert!(
        screen.contains("Allow once"),
        "a sealed receipt must stay visible: {screen}"
    );
    // Sealing scrolls by exactly one row and erases nothing, so every row is
    // the one that was below it before, and the bottom row is the fresh one.
    for row in 0..ROWS as usize - 1 {
        assert_eq!(
            after[row],
            before[row + 1],
            "row {row} is not a pure scroll"
        );
    }
    assert!(
        after[ROWS as usize - 1].trim().is_empty(),
        "the fresh row below the receipt must be blank: {:?}",
        after[ROWS as usize - 1]
    );
    assert_eq!(gate.term_mut().cursor(), (ROWS - 1, 0));
}

#[test]
fn withdraw_and_seal_require_a_live_rectangle() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    assert!(matches!(gate.withdraw(), Err(GateError::NotAcquired)));
    assert!(matches!(
        gate.seal_to_scrollback(),
        Err(GateError::NotAcquired)
    ));
    assert_eq!(gate.term_mut().accepted(), 0);
}

/// A partial write has to report where it stopped; without the exact offset the
/// Host cannot tell whether a control sequence completed, and resuming from a
/// guess would corrupt the screen.
#[test]
fn a_partial_write_reports_the_exact_accepted_offset() {
    let mut term = VtTerminal::new(ROWS, COLS);
    term.fail_after = Some(5);
    let mut gate = ApprovalSurfaceGate::new(term);

    let error = gate.acquire(6).expect_err("the scroll-out cannot complete");

    assert!(
        matches!(error, GateError::Write { accepted: 5, .. }),
        "{error}"
    );
    assert_eq!(gate.rect(), None, "a failed acquisition claims no rows");
}

#[test]
fn a_terminal_that_cannot_report_its_size_refuses_acquisition() {
    struct NoSize(VtTerminal);
    impl Write for NoSize {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }
    impl SurfaceTerminal for NoSize {
        fn winsize(&mut self) -> io::Result<Winsize> {
            Err(io::Error::other("TIOCGWINSZ unavailable"))
        }
        fn accepted(&self) -> usize {
            self.0.accepted()
        }
        fn reset_accepted(&mut self) {
            self.0.reset_accepted();
        }
    }

    let mut gate = ApprovalSurfaceGate::new(NoSize(VtTerminal::new(ROWS, COLS)));
    let error = gate.acquire(6).expect_err("size is unknown");

    assert!(matches!(error, GateError::Winsize(_)), "{error}");
    assert_eq!(gate.rect(), None);
    assert_eq!(gate.term_mut().0.accepted(), 0, "nothing may be written");
}

#[test]
fn an_unchanged_viewport_still_owns_the_bottom() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    let rect = gate.acquire(8).expect("24 rows fit an 8 row surface");
    assert_eq!(rect.anchor_row, ROWS - 8);

    assert!(gate
        .rectangle_still_owns_the_bottom(8, COLS)
        .expect("readable"));
    // Asking again on an untouched viewport keeps offering the reuse and does not
    // latch distrust.
    assert!(gate
        .rectangle_still_owns_the_bottom(8, COLS)
        .expect("readable"));
    assert_eq!(gate.observe_geometry(), GeometryTrust::Trusted);
    // Extra width *is* a change. The surface is anchored at column 1 so the card
    // would still fit, but whatever reflowed the terminal to 100 columns moved
    // content through the recorded rows, so the anchor is not reusable.
    gate.term_mut().set_size(ROWS, 100);
    assert!(!gate
        .rectangle_still_owns_the_bottom(8, COLS)
        .expect("readable"));
}

#[test]
fn a_row_resize_stops_the_rectangle_owning_the_bottom() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    gate.acquire(8).expect("24 rows fit an 8 row surface");

    // Growing the viewport moves the bottom row down, so the recorded anchor no
    // longer names the last 8 rows: repainting would leave the old copy behind.
    gate.term_mut().set_size(30, COLS);
    assert!(!gate
        .rectangle_still_owns_the_bottom(8, COLS)
        .expect("readable"));

    // Shrinking it moves the bottom row up.
    gate.term_mut().set_size(20, COLS);
    assert!(!gate
        .rectangle_still_owns_the_bottom(8, COLS)
        .expect("readable"));
}

#[test]
fn reuse_reapplies_the_refusals_acquire_makes() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    gate.acquire(8).expect("24 rows fit an 8 row surface");

    // Too narrow for the surface: `acquire` refuses this, and reusing a
    // rectangle must not smuggle a 40 column frame into a 39 column viewport.
    gate.term_mut().set_size(ROWS, COLS - 1);
    assert!(!gate
        .rectangle_still_owns_the_bottom(8, COLS)
        .expect("readable"));

    // Too short for the surface.
    gate.term_mut().set_size(7, COLS);
    assert!(!gate
        .rectangle_still_owns_the_bottom(8, COLS)
        .expect("readable"));
}

#[test]
fn a_different_surface_height_is_not_a_reuse_of_this_rectangle() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    gate.acquire(8).expect("24 rows fit an 8 row surface");

    assert!(!gate
        .rectangle_still_owns_the_bottom(9, COLS)
        .expect("readable"));
    assert!(!gate
        .rectangle_still_owns_the_bottom(7, COLS)
        .expect("readable"));
}

#[test]
fn owning_the_bottom_requires_a_live_rectangle() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    let error = gate
        .rectangle_still_owns_the_bottom(8, COLS)
        .expect_err("nothing is acquired");
    assert!(matches!(error, GateError::NotAcquired), "{error}");
}

#[test]
fn a_width_floor_cannot_hide_a_reflow_and_back() {
    // The named miss: 80 -> 47 -> 120 satisfies `ws_col >= 40` at every step, so a
    // width floor reports no change despite two reflows. Comparing the actual
    // acquired width catches the first one.
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, 80));
    gate.acquire(8).expect("80 columns fit the surface");

    gate.term_mut().set_size(ROWS, 47);
    assert!(
        !gate
            .rectangle_still_owns_the_bottom(8, COLS)
            .expect("readable"),
        "47 columns is not the 80 the rectangle was acquired at"
    );

    // Returning to a wide viewport must not revive the old anchor.
    gate.term_mut().set_size(ROWS, 120);
    assert!(
        !gate
            .rectangle_still_owns_the_bottom(8, COLS)
            .expect("readable"),
        "a latched distrust survives a later size change"
    );
}

#[test]
fn geometry_distrust_is_sticky_even_when_the_size_returns() {
    let mut gate = ApprovalSurfaceGate::new(VtTerminal::new(ROWS, COLS));
    gate.acquire(8).expect("24 rows fit an 8 row surface");

    gate.term_mut().set_size(ROWS, COLS - 1);
    assert_eq!(gate.observe_geometry(), GeometryTrust::Changed);

    // 40 -> 39 -> 40: the content has still been reflowed through those rows, so
    // the recorded anchor cannot be trusted again.
    gate.term_mut().set_size(ROWS, COLS);
    assert_eq!(gate.observe_geometry(), GeometryTrust::Changed);
    assert!(
        !gate
            .rectangle_still_owns_the_bottom(8, COLS)
            .expect("readable"),
        "reuse must not be offered after a latched distrust"
    );
}

#[test]
fn an_untrusted_surface_is_relinquished_without_erasing_its_old_rows() {
    let mut term = VtTerminal::new(ROWS, COLS);
    term.fill();
    let mut gate = ApprovalSurfaceGate::new(term);
    let rect = gate.acquire(8).expect("24 rows fit an 8 row surface");
    assert_eq!(rect.anchor_row, ROWS - 8);

    // After a reflow the rectangle's rows may hold the user's output. Giving the
    // surface up must not touch them.
    gate.term_mut().set_size(30, COLS);
    assert_eq!(gate.observe_geometry(), GeometryTrust::Changed);
    gate.term_mut()
        .stamp_foreign_rows(rect.anchor_row, rect.height);
    let before = gate.term_mut().rows();
    assert!(
        before.iter().any(|row| row.starts_with("user ")),
        "the fixture must put foreign content on the recorded rows: {before:?}"
    );
    gate.relinquish_without_erasing();
    assert_eq!(gate.rect(), None, "relinquish clears the rectangle");
    let after = gate.term_mut().rows();
    assert_eq!(
        after, before,
        "relinquishing must not write to the terminal at all"
    );
    assert!(
        after.iter().any(|row| row.starts_with("user ")),
        "the foreign rows on the recorded rectangle were erased: {after:?}"
    );

    // And no later release may emit a stale CUP/EL2 either, because all of them
    // require a live rectangle.
    assert!(matches!(gate.withdraw(), Err(GateError::NotAcquired)));
    assert!(matches!(
        gate.seal_to_scrollback(),
        Err(GateError::NotAcquired)
    ));
}

#[test]
fn pushing_the_viewport_out_scrolls_without_erasing_any_row() {
    let mut term = VtTerminal::new(ROWS, COLS);
    term.fill();
    let mut gate = ApprovalSurfaceGate::new(term);
    let before = gate.term_mut().rows();
    gate.push_viewport_into_scrollback().expect("writes");

    // Every old row must have scrolled off the visible screen rather than being
    // blanked in place: `fill` writes a distinct `line NN` per row, so any survivor
    // means the viewport was not pushed out.
    let after = gate.term_mut().rows();
    assert_ne!(before, after);
    for row in &after {
        assert!(
            !row.trim().starts_with("line "),
            "old content survived the scroll: {after:?}"
        );
    }
    // The cursor is left on a blank bottom row, ready for a replacement card.
    // vt100 models a fixed screen with no scrollback, so "the old rows went into
    // scrollback rather than being destroyed" cannot be asserted here; what is
    // asserted is that nothing was blanked in place and no erase op was needed.
    assert_eq!(gate.term_mut().cursor().0, ROWS - 1);
    assert!(after[usize::from(ROWS) - 1].trim().is_empty(), "{after:?}");
}

/// The rows the scroll-out pushes off must still be readable as history.
///
/// This is the half a zero-scrollback fixture cannot assert, and it is what
/// separates "the user's output was moved aside" from "the user's output was
/// destroyed". Blanking in place would leave scrollback holding empty rows; not
/// scrolling at all would leave it empty.
#[test]
fn pushing_the_viewport_out_keeps_the_old_rows_readable_in_scrollback() {
    let mut term = VtTerminal::with_scrollback(ROWS, COLS, 4 * usize::from(ROWS));
    term.fill();
    let mut gate = ApprovalSurfaceGate::new(term);

    gate.push_viewport_into_scrollback().expect("writes");

    let term = gate.term_mut();
    // `scrollback()` reports the current view offset, not the depth of history, so
    // ask to scroll further back than any plausible amount and read the clamped
    // position: `set_scrollback` clamps to the real scrollback size.
    term.parser
        .screen_mut()
        .set_scrollback(4 * usize::from(ROWS));
    let available = term.parser.screen().scrollback();
    assert!(
        available >= usize::from(ROWS),
        "the whole viewport must have been pushed into history, got {available} rows"
    );
    let oldest = term.rows();
    assert!(
        oldest.iter().any(|row| row.starts_with("line 00")),
        "the user's first row must survive in scrollback: {oldest:?}"
    );
    assert!(
        oldest.iter().any(|row| row.starts_with("line 07")),
        "rows from the middle of the old viewport must survive too: {oldest:?}"
    );
}
