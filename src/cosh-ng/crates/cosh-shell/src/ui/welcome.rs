use std::io::{self, Write};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{block::Padding, Block, BorderType, Paragraph, Widget},
};

use super::agent_render::{
    buffer_to_lines, buffer_to_styled_lines, display_width, wrap_plain_line,
};
use super::RatatuiInlineRenderer;
use crate::MessageId;

const LOGO: [&str; 6] = [
    "  ██████╗  ██████╗  ███████╗ ██╗  ██╗",
    " ██╔════╝ ██╔═══██╗ ██╔════╝ ██║  ██║",
    " ██║      ██║   ██║ ███████╗ ███████║",
    " ██║      ██║   ██║ ╚════██║ ██╔══██║",
    " ╚██████╗ ╚██████╔╝ ███████║ ██║  ██║",
    "  ╚═════╝  ╚═════╝  ╚══════╝ ╚═╝  ╚═╝",
];
const LOGO_COLORS: [u8; 6] = [33, 33, 39, 39, 117, 117];

pub(crate) struct WelcomeModel {
    pub compact: bool,
    pub context: Vec<String>,
    pub capability: String,
    pub notice: Option<String>,
    pub extra: Vec<String>,
}

impl RatatuiInlineRenderer {
    pub(crate) fn write_welcome<W: Write>(
        &self,
        output: &mut W,
        model: WelcomeModel,
    ) -> io::Result<()> {
        for line in self.welcome_lines(model) {
            writeln!(output, "{line}")?;
        }
        Ok(())
    }

    fn welcome_lines(&self, model: WelcomeModel) -> Vec<String> {
        let width = self.panel_standard_width().min(80);
        let i18n = self.i18n();
        let help = format!("{} /help", i18n.t(MessageId::StartupHelpLabel));
        let mut body = Vec::new();
        let inner_width = width.saturating_sub(4).max(1) as usize;
        let mut context = model
            .context
            .into_iter()
            .map(|text| crate::evidence::clean_terminal_control_sequences(&text))
            .collect::<Vec<_>>();
        let combined = context.join(" · ");
        if context.len() > 1 && display_width(&combined) <= inner_width {
            context = vec![combined];
        }
        for (text, color) in std::iter::once((model.capability, Color::Reset))
            .chain(context.into_iter().map(|text| (text, Color::Reset)))
            .chain(model.notice.map(|text| (text, Color::Yellow)))
            .chain(model.extra.into_iter().map(|text| (text, Color::Reset)))
        {
            let text = crate::evidence::clean_terminal_control_sequences(&text);
            for line in wrap_plain_line(&text, inner_width) {
                body.push(Line::styled(line, Style::default().fg(color)));
            }
        }
        if self.plain {
            let mut lines = vec![i18n.t(MessageId::StartupTitle).to_string()];
            lines.extend(body.iter().map(ToString::to_string));
            lines.push(help);
            return lines;
        }
        let height = body.len().saturating_add(2).min(u16::MAX as usize) as u16;
        let logo_height: u16 = if !model.compact && width >= 42 {
            (LOGO.len() + 1) as u16
        } else {
            0
        };
        let area = Rect::new(0, 0, width, height.saturating_add(logo_height));
        let panel_area = Rect::new(0, logo_height, width, height);
        let mut buffer = Buffer::empty(area);
        if logo_height > 0 {
            let logo = LOGO
                .iter()
                .zip(LOGO_COLORS)
                .map(|(line, color)| {
                    Line::styled(
                        *line,
                        Style::default()
                            .fg(Color::Indexed(color))
                            .add_modifier(Modifier::BOLD),
                    )
                })
                .collect::<Vec<_>>();
            Paragraph::new(Text::from(logo)).render(
                Rect::new(1, 0, width.saturating_sub(1), LOGO.len() as u16),
                &mut buffer,
            );
        }
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Indexed(245)))
            .padding(Padding::horizontal(1))
            .title_top(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    i18n.t(MessageId::StartupTitle),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]))
            .title_bottom(Line::from(vec![
                Span::styled(
                    format!(" {} ", i18n.t(MessageId::StartupHelpLabel)),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(
                    "/help",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]));
        let inner = block.inner(panel_area);
        block.render(panel_area, &mut buffer);
        Paragraph::new(Text::from(body)).render(inner, &mut buffer);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }
}

#[cfg(test)]
library_tests! {
    mod tests;
}
