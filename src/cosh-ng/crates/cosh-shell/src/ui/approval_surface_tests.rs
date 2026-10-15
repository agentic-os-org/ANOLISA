use ratatui::{buffer::Buffer, layout::Rect};
use unicode_width::UnicodeWidthStr;

use crate::ui::approval_surface::render_approval_view;
use crate::ui::surface_frame::SLICE_COLUMNS;
use crate::ui::worker_protocol::{
    ApprovalActionView, ApprovalCardView, EmptyRenderView, RenderView, UiLanguage,
};

fn card() -> ApprovalCardView {
    ApprovalCardView {
        revision: 7,
        language: UiLanguage::En,
        display_id: "req-1".to_string(),
        kind: "Bash".to_string(),
        risk: "high".to_string(),
        reason: "write redirection".to_string(),
        subject: "slice approval request".to_string(),
        preview: "$ printf '%s\\n' slice_effect >> '/tmp/cosh-slice/effect.txt'".to_string(),
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

fn paint(view: &RenderView, rows: u16) -> String {
    let mut buffer = Buffer::empty(Rect::new(0, 0, SLICE_COLUMNS, rows));
    render_approval_view(view, &mut buffer);
    let mut text = String::new();
    for row in 0..rows {
        let mut column = 0;
        while column < SLICE_COLUMNS {
            let symbol = buffer.cell((column, row)).expect("cell").symbol();
            text.push_str(symbol);
            column += UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        text.push('\n');
    }
    text
}

fn rows_of(view: &RenderView, rows: u16) -> Vec<String> {
    let mut buffer = Buffer::empty(Rect::new(0, 0, SLICE_COLUMNS, rows));
    render_approval_view(view, &mut buffer);
    (0..rows)
        .map(|row| {
            (0..SLICE_COLUMNS)
                .map(|col| buffer.cell((col, row)).expect("cell").symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn approval_card_paints_actions_and_risk_within_40_columns() {
    let text = paint(&RenderView::ApprovalCard(card()), 12);
    assert!(text.contains("Allow once"), "{text}");
    assert!(text.contains("Deny"), "{text}");
    assert!(text.contains("Details"), "{text}");
    assert!(text.contains("write redirection"), "{text}");
    assert!(text.contains("slice_effect"), "{text}");
    assert!(text.contains("! Approval req-1"), "{text}");
}

#[test]
fn chinese_approval_card_localizes_static_labels_and_preserves_request_content() {
    let mut view = card();
    view.language = UiLanguage::Zh;
    view.irrecoverable = true;
    view.selected = ApprovalActionView::Deny;
    let view = RenderView::ApprovalCard(view);
    let height = crate::ui::approval_surface::surface_rows_for(&view, SLICE_COLUMNS);
    let text = paint(&view, height);
    for expected in [
        "! 审批 req-1",
        "Bash · 高风险",
        "└ 风险: write redirection",
        "! 呈现不可恢复",
        "仅此一次",
        "> [ 拒绝 ]",
        "详情",
        "$ printf",
        "slice_effect",
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
    for untranslated in [
        "Approval",
        "high risk",
        "Risk:",
        "presentation irrecoverable",
    ] {
        assert!(!text.contains(untranslated), "{text}");
    }
    for row in rows_of(&view, height) {
        assert_eq!(row.chars().count(), SLICE_COLUMNS as usize, "{row:?}");
        assert!(
            row.starts_with('╭') || row.starts_with('╰') || row.starts_with('│'),
            "{row:?}"
        );
    }
    let mut english = card();
    english.irrecoverable = true;
    let english = RenderView::ApprovalCard(english);
    let english_height = crate::ui::approval_surface::surface_rows_for(&english, SLICE_COLUMNS);
    assert_eq!(height, english_height);
    let english_text = paint(&english, english_height);
    assert!(
        english_text.contains("! presentation irrecoverable"),
        "{english_text}"
    );
    for (risk, expected) in [
        ("medium", "中风险"),
        ("low", "低风险"),
        ("unknown", "未知风险"),
        ("custom-risk", "custom-risk"),
    ] {
        let mut view = card();
        view.language = UiLanguage::Zh;
        view.risk = risk.to_string();
        let text = paint(&RenderView::ApprovalCard(view), 12);
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn approval_card_keeps_the_single_command_marker_the_preview_already_carries() {
    // `request.preview` arrives as `$ <command>` from `tools/display.rs`, so a
    // renderer that adds its own marker shows `$ $ <command>`.
    let text = paint(&RenderView::ApprovalCard(card()), 12);
    assert!(text.contains("$ printf"), "{text}");
    assert!(!text.contains("$ $"), "{text}");
}

#[test]
fn approval_card_repaints_every_cell_of_the_surface() {
    // A full repaint must define every cell in columns x surface_rows, so no
    // stale content can survive under or beside the card.
    for row in rows_of(&RenderView::ApprovalCard(card()), 12) {
        assert_eq!(row.chars().count(), SLICE_COLUMNS as usize, "{row:?}");
    }
    let blank = rows_of(&RenderView::Empty(EmptyRenderView { revision: 1 }), 4);
    assert!(
        blank.iter().all(|row| row.trim().is_empty()),
        "empty view must clear the surface: {blank:?}"
    );
}

#[test]
fn approval_card_marks_the_focused_action() {
    let text = paint(&RenderView::ApprovalCard(card()), 12);
    assert!(text.contains("> [ Allow once ]"), "{text}");
    assert!(!text.contains("> [ Deny ]"), "{text}");
    assert!(!text.contains("> [ Details ]"), "{text}");
}

#[test]
fn moving_focus_moves_the_marker_onto_the_newly_focused_action() {
    let mut denied = card();
    denied.selected = ApprovalActionView::Deny;
    let text = paint(&RenderView::ApprovalCard(denied.clone()), 12);
    assert!(text.contains("> [ Deny ]"), "{text}");
    assert!(!text.contains("> [ Allow once ]"), "{text}");

    let mut details = card();
    details.selected = ApprovalActionView::Details;
    let details_text = paint(&RenderView::ApprovalCard(details), 12);
    assert!(details_text.contains("> [ Details ]"), "{details_text}");

    // Approve and Deny share a row at the slice width, so a marker that belongs
    // to the row instead of the action renders these two cards identically: the
    // user would see "Allow once" focused while Enter denies.
    let approve_text = paint(&RenderView::ApprovalCard(card()), 12);
    assert_ne!(text, approve_text);
    assert_ne!(details_text, approve_text);
    assert_ne!(details_text, text);
}

#[test]
fn approval_card_wraps_a_long_preview_without_overflow() {
    let mut view = card();
    view.preview = "x".repeat(200);
    let rows = rows_of(&RenderView::ApprovalCard(view), 16);
    for row in &rows {
        assert_eq!(row.chars().count(), SLICE_COLUMNS as usize, "{row:?}");
    }
    // The card is shorter than the surface, so rows are matched by shape rather
    // than by position: a painted row is either a box corner or keeps both
    // rails, which is what proves no wrapped segment escaped the box.
    let painted: Vec<&String> = rows.iter().filter(|row| !row.trim().is_empty()).collect();
    assert!(
        painted.iter().filter(|row| row.contains('x')).count() > 1,
        "a 200-column preview must wrap onto several rows: {painted:?}"
    );
    for row in &painted {
        let first = row.chars().next().expect("painted row");
        if matches!(first, '╭' | '╰') {
            continue;
        }
        assert!(row.starts_with('│') && row.ends_with('│'), "{row:?}");
    }
}
