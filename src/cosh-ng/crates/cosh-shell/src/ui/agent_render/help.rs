use std::io::{self, Write};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::reference_style::{
    reference_body_style, reference_emphasis_style, reference_muted_style, reference_section_style,
};
use super::wrap::{display_width, wrap_plain_line, wrap_plain_line_with_prefix};
use super::RatatuiInlineRenderer;
use crate::types::CardKind;

/// Command reference panel (`/help`) model.
///
/// Layout follows the shared reference-panel convention (`reference_style`):
/// group headers are bold + underlined, command names are highlighted,
/// arguments and scope tags are de-emphasized, and summaries sit on their
/// own indented line.
pub(crate) struct HelpPanelModel<'a> {
    pub(crate) title: &'a str,
    pub(crate) groups: Vec<HelpPanelGroup<'a>>,
    pub(crate) footer: String,
}

pub(crate) struct HelpPanelGroup<'a> {
    pub(crate) label: &'a str,
    pub(crate) entries: Vec<HelpPanelEntry<'a>>,
}

pub(crate) struct HelpPanelEntry<'a> {
    pub(crate) usage: &'a str,
    pub(crate) summary: &'a str,
    pub(crate) scope: &'a str,
}

const ENTRY_INDENT: &str = "  ";
const SUMMARY_INDENT: &str = "      ";

impl RatatuiInlineRenderer {
    pub(crate) fn write_help_panel<W: Write>(
        &self,
        output: &mut W,
        model: HelpPanelModel<'_>,
    ) -> io::Result<()> {
        if self.plain {
            let body = plain_help_lines(&model, self.content_width());
            return self.write_block(
                output,
                &CardKind::SlashCommand.title(model.title),
                body,
                None,
            );
        }
        let inner = usize::from(self.panel_standard_width().saturating_sub(4)).max(1);
        let title = CardKind::SlashCommand.title(model.title);
        let body = styled_help_lines(&model, inner);
        self.write_styled_block(output, &title, body)
    }
}

/// Splits a usage string into the command name and its argument tail.
fn split_usage(usage: &str) -> (&str, &str) {
    for (idx, _) in usage.match_indices(' ') {
        let rest = &usage[idx + 1..];
        if rest.starts_with('[') || rest.starts_with('<') {
            return (&usage[..idx], rest);
        }
    }
    (usage, "")
}

fn styled_help_lines(model: &HelpPanelModel<'_>, inner: usize) -> Vec<Line<'static>> {
    let header_style = reference_section_style();
    let name_style = reference_emphasis_style();
    let args_style = reference_muted_style();
    let summary_style = reference_body_style();

    let mut lines = Vec::new();
    for (group_index, group) in model.groups.iter().enumerate() {
        if group_index > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            group.label.to_string(),
            header_style,
        )));
        for entry in &group.entries {
            let (name, args) = split_usage(entry.usage);
            let usage_plain = if args.is_empty() {
                format!("{ENTRY_INDENT}{name}")
            } else {
                format!("{ENTRY_INDENT}{name} {args}")
            };
            let tag = format!("[{}]", entry.scope);
            let wrapped = wrap_plain_line(&usage_plain, inner);
            let last = wrapped.len().saturating_sub(1);
            for (index, segment) in wrapped.iter().enumerate() {
                let mut spans = usage_segment_spans(segment, index, name, name_style, args_style);
                if index == last {
                    let used = display_width(segment);
                    let tag_width = display_width(&tag);
                    if used + 2 + tag_width <= inner {
                        spans.push(Span::raw(" ".repeat(inner - used - tag_width)));
                        spans.push(Span::styled(tag.clone(), args_style));
                        lines.push(Line::from(spans));
                    } else {
                        lines.push(Line::from(spans));
                        let pad = inner.saturating_sub(tag_width);
                        lines.push(Line::from(vec![
                            Span::raw(" ".repeat(pad)),
                            Span::styled(tag.clone(), args_style),
                        ]));
                    }
                } else {
                    lines.push(Line::from(spans));
                }
            }
            for segment in
                wrap_plain_line_with_prefix(entry.summary, SUMMARY_INDENT, SUMMARY_INDENT, inner)
            {
                lines.push(Line::from(Span::styled(segment, summary_style)));
            }
        }
    }
    lines.push(Line::from(Span::styled(
        model.footer.clone(),
        summary_style,
    )));
    lines
}

fn usage_segment_spans(
    segment: &str,
    index: usize,
    name: &str,
    name_style: Style,
    args_style: Style,
) -> Vec<Span<'static>> {
    if index == 0 {
        if let Some(rest) = segment
            .strip_prefix(ENTRY_INDENT)
            .and_then(|rest| rest.strip_prefix(name))
        {
            let mut spans = vec![
                Span::raw(ENTRY_INDENT.to_string()),
                Span::styled(name.to_string(), name_style),
            ];
            if !rest.is_empty() {
                spans.push(Span::styled(rest.to_string(), args_style));
            }
            return spans;
        }
    }
    vec![Span::styled(segment.to_string(), args_style)]
}

fn plain_help_lines(model: &HelpPanelModel<'_>, width: usize) -> Vec<String> {
    let mut body = Vec::new();
    for (group_index, group) in model.groups.iter().enumerate() {
        if group_index > 0 {
            body.push(String::new());
        }
        body.push(format!("── {} ──", group.label));
        for entry in &group.entries {
            let usage_line = format!("{ENTRY_INDENT}{} [{}]", entry.usage, entry.scope);
            body.extend(wrap_plain_line(&usage_line, width));
            body.extend(wrap_plain_line_with_prefix(
                entry.summary,
                SUMMARY_INDENT,
                SUMMARY_INDENT,
                width,
            ));
        }
    }
    body.push(model.footer.clone());
    body
}
