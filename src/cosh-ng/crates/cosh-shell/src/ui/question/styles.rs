use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::{
    custom_option_label, custom_option_visual_label, render_wrapped_option, wrap_option_text,
    QuestionInputFeedback,
};

pub(super) fn option_heading_style(feedback: QuestionInputFeedback) -> Style {
    if feedback == QuestionInputFeedback::SelectionRequired {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

pub(super) fn option_marker_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn render_wrapped_option_with_ghost(
    prefix: &str,
    label: &str,
    ghost: &str,
    prefix_style: Style,
    feedback: QuestionInputFeedback,
    width: usize,
) -> Vec<Line<'static>> {
    let marker = "  › ";
    let text = format!("{label}{marker}{ghost}");
    let ghost_style = if matches!(
        feedback,
        QuestionInputFeedback::Required | QuestionInputFeedback::Invalid
    ) {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM)
    };
    let mut ghost_started = false;
    wrap_option_text(prefix, &text, width)
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let prefix_len = if idx == 0 {
                prefix.len().min(line.len())
            } else {
                0
            };
            let (line_prefix, rest) = line.split_at(prefix_len);
            let mut spans = vec![Span::styled(line_prefix.to_string(), prefix_style)];
            if ghost_started {
                spans.push(Span::styled(rest.to_string(), ghost_style));
            } else if let Some(at) = rest.find(marker) {
                spans.push(Span::raw(rest[..at].to_string()));
                spans.push(Span::styled(rest[at..].to_string(), ghost_style));
                ghost_started = true;
            } else {
                spans.push(Span::raw(rest.to_string()));
            }
            Line::from(spans)
        })
        .collect()
}

pub(super) fn render_custom_option_lines(
    idx: usize,
    selected: bool,
    i18n: crate::I18n,
    custom_answer: &str,
    feedback: QuestionInputFeedback,
    width: usize,
) -> Vec<Line<'static>> {
    let marker = if selected { ">" } else { " " };
    let prefix = format!("{marker} [{}] ", idx + 1);
    if selected && custom_answer.trim().is_empty() && feedback != QuestionInputFeedback::Disabled {
        let label = custom_option_label(i18n, custom_answer);
        let ghost = match feedback {
            QuestionInputFeedback::Required => i18n.t(crate::MessageId::QuestionRequiredGhost),
            QuestionInputFeedback::Invalid => i18n.t(crate::MessageId::QuestionInvalidGhost),
            _ => i18n.t(crate::MessageId::QuestionDefaultGhost),
        };
        return render_wrapped_option_with_ghost(
            &prefix,
            &label,
            ghost,
            option_marker_style(selected),
            feedback,
            width,
        );
    }
    let label = custom_option_visual_label(i18n, custom_answer, feedback, selected, false);
    render_wrapped_option(&prefix, &label, option_marker_style(selected), width)
}
