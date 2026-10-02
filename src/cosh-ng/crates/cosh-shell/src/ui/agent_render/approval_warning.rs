//! Irrecoverable-consequence warning line for approval cards (#2064).
//! Rendered when the classifier verdict carries a system-control side
//! effect; the trigger is anchored on the assessment, never on
//! command-name matching in the renderer.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

/// Plain-text warning line: red `⚠ <consequence>` under the risk
/// metadata so the user sees the whole-machine blast radius before
/// approving.
pub(super) fn irrecoverable_warning_text(i18n: crate::I18n) -> String {
    format!(
        "\u{26a0} {}",
        i18n.t(crate::MessageId::ApprovalIrrecoverableWarningLine)
    )
}

/// Styled variant shared by the command-heading and generic card shapes.
pub(super) fn render_irrecoverable_warning(i18n: crate::I18n, area: Rect, buffer: &mut Buffer) {
    Paragraph::new(Line::from(Span::styled(
        irrecoverable_warning_text(i18n),
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )))
    .render(area, buffer);
}
