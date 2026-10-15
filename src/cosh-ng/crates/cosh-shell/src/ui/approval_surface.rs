//! Pure renderer from a Host-owned typed view to surface cells.
//!
//! The worker owns no approval semantics: it receives an already redacted,
//! already decided projection and paints it. Nothing here reads input, touches
//! the terminal, or consults Host state, so the same function is exercised by
//! worker-process tests and by Host-side layout tests.

use ratatui::{buffer::Buffer, style::Style};

use crate::config::Language;
use crate::i18n::{I18n, MessageId};

use super::worker_protocol::{ApprovalActionView, ApprovalCardView, RenderView, UiLanguage};

/// Paints `view` into `buffer`, covering every cell so the frame is a canonical
/// full repaint of the surface the Host allocated.
pub(crate) fn render_approval_view(view: &RenderView, buffer: &mut Buffer) {
    let area = buffer.area;
    let width = area.width as usize;
    let lines = match view {
        RenderView::Empty(_) => Vec::new(),
        RenderView::ApprovalCard(card) => approval_card_lines(card, width),
    };
    for row in 0..area.height {
        let text = lines.get(row as usize).map_or("", String::as_str);
        buffer.set_string(area.x, area.y + row, text, Style::default());
        // Pad the remainder explicitly: a full repaint must define every cell
        // in columns x surface_rows, not leave stale content visible.
        let used = display_width(text).min(width);
        if used < width {
            buffer.set_string(
                area.x + used as u16,
                area.y + row,
                " ".repeat(width - used),
                Style::default(),
            );
        }
    }
}

/// Rows the Host must allocate for `view` at `columns`.
///
/// The Host and the worker share this function, which is what keeps the
/// rectangle the Host acquires and the surface the worker paints from drifting
/// apart: the Host derives the viewport height from here, and the worker's frame
/// is then validated against that same viewport. A worker reporting any other
/// height fails validation instead of being clipped or trusted.
///
/// Never returns 0, because a zero-row surface is not a valid viewport and an
/// empty view still needs one row to clear.
pub(crate) fn surface_rows_for(view: &RenderView, columns: u16) -> u16 {
    let lines = match view {
        RenderView::Empty(_) => 0,
        RenderView::ApprovalCard(card) => approval_card_lines(card, columns as usize).len(),
    };
    u16::try_from(lines).unwrap_or(u16::MAX).max(1)
}

/// On-screen label of an action. The descriptor name (`Approve`) and the label
/// a user reads (`Allow once`) differ; the label is what tests must match.
pub(crate) fn action_label(action: ApprovalActionView, language: UiLanguage) -> &'static str {
    match (action, language) {
        (ApprovalActionView::Approve, UiLanguage::En) => "Allow once",
        (ApprovalActionView::Deny, UiLanguage::En) => "Deny",
        (ApprovalActionView::Details, UiLanguage::En) => "Details",
        (ApprovalActionView::Approve, UiLanguage::Zh) => "仅此一次",
        (ApprovalActionView::Deny, UiLanguage::Zh) => "拒绝",
        (ApprovalActionView::Details, UiLanguage::Zh) => "详情",
    }
}

fn approval_card_lines(card: &ApprovalCardView, width: usize) -> Vec<String> {
    let i18n = I18n::new(match card.language {
        UiLanguage::En => Language::EnUs,
        UiLanguage::Zh => Language::ZhCn,
    });
    let risk_id = match card.risk.as_str() {
        "high" => Some(MessageId::ApprovalRiskLevelHigh),
        "medium" => Some(MessageId::ApprovalRiskLevelMedium),
        "low" => Some(MessageId::ApprovalRiskLevelLow),
        "unknown" => Some(MessageId::ApprovalRiskLevelUnknown),
        _ => None,
    };
    let risk = risk_id.map_or_else(
        || i18n.format(MessageId::ApprovalRiskSuffix, &[("risk", &card.risk)]),
        |id| i18n.t(id).to_string(),
    );
    let mut lines = Vec::new();
    lines.push(box_top(
        &format!("! {} {}", i18n.t(MessageId::ApprovalTitle), card.display_id),
        width,
    ));
    lines.push(box_row(&format!("{} · {risk}", card.kind), width));
    lines.push(box_row(
        &format!(
            "└ {}{}",
            i18n.t(MessageId::ApprovalRiskDetailLabel),
            card.reason
        ),
        width,
    ));
    // `request.preview` already carries the `$ ` marker that `tools/display.rs`
    // puts on a shell command, and the inline card wraps the bare command instead
    // (`command_preview_rows`), so adding another `$ ` here showed `$ $ printf …`.
    // Continuation rows keep the inline two-space indent under the command.
    for (index, segment) in wrap(&card.preview, inner_width(width).saturating_sub(2))
        .into_iter()
        .enumerate()
    {
        let indent = if index == 0 { "" } else { "  " };
        lines.push(box_row(&format!("{indent}{segment}"), width));
    }
    if card.expanded {
        for segment in wrap(&card.subject, inner_width(width)) {
            lines.push(box_row(&segment, width));
        }
    }
    if card.irrecoverable {
        let warning = match card.language {
            UiLanguage::En => "! presentation irrecoverable",
            UiLanguage::Zh => "! 呈现不可恢复",
        };
        lines.push(box_row(warning, width));
    }
    for group in action_rows(card, width) {
        lines.push(box_row(&group.to_string(), width));
    }
    lines.push(box_bottom(width));
    lines
}

struct ActionGroup {
    labels: String,
}

impl std::fmt::Display for ActionGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.labels)
    }
}

/// Lays actions out as `> [ label ] ` and `  [ label ] ` groups, starting a new
/// row when the next group would not fit inside the box.
///
/// The marker belongs to the selected action, not to its row, and both prefixes
/// are the same width — the convention the inline card already uses
/// (`agent_render::approval_actions::action_span`). A row-level marker leaves two
/// actions that share a row indistinguishable, so at the slice width, where
/// Approve and Deny wrap onto the same row, the card would keep showing the
/// Approve marker while Enter denies.
fn action_rows(card: &ApprovalCardView, width: usize) -> Vec<ActionGroup> {
    let mut rows: Vec<ActionGroup> = Vec::new();
    let mut current = String::new();
    let budget = inner_width(width);
    for action in &card.actions {
        let marker = if *action == card.selected { "> " } else { "  " };
        let label = format!("{marker}[ {} ] ", action_label(*action, card.language));
        let candidate = format!("{current}{label}");
        if !current.is_empty() && display_width(&candidate) > budget {
            rows.push(ActionGroup {
                labels: std::mem::take(&mut current),
            });
            current = label;
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        rows.push(ActionGroup { labels: current });
    }
    rows
}

fn inner_width(width: usize) -> usize {
    width.saturating_sub(4)
}

fn box_top(title: &str, width: usize) -> String {
    let title = truncate(title, width.saturating_sub(4));
    let mut line = format!("╭ {title} ");
    let fill = width.saturating_sub(display_width(&line) + 1);
    line.push_str(&"─".repeat(fill));
    line.push('╮');
    truncate_to_width(&line, width)
}

fn box_bottom(width: usize) -> String {
    let mut line = String::from("╰");
    line.push_str(&"─".repeat(width.saturating_sub(2)));
    line.push('╯');
    line
}

fn box_row(text: &str, width: usize) -> String {
    let inner = inner_width(width);
    let mut line = String::from("│ ");
    line.push_str(&truncate(text, inner));
    let pad = inner.saturating_sub(display_width(text));
    line.push_str(&" ".repeat(pad));
    line.push_str(" │");
    line
}

/// Character-based truncation that never exceeds `max` display columns.
fn truncate(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = char_width(ch);
        if used + w > max {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

fn truncate_to_width(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    truncate(text, max)
}

/// Greedy wrap on character boundaries; the preview is already redacted ASCII
/// in the slice, and wide characters are counted so a row never overflows.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = char_width(ch);
        if used + w > width {
            out.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(ch);
        used += w;
    }
    out.push(current);
    out
}

fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(ch: char) -> usize {
    if (ch as u32) < 0x20 || (ch as u32) == 0x7f {
        0
    } else if matches!(ch as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6)
    {
        2
    } else {
        1
    }
}
