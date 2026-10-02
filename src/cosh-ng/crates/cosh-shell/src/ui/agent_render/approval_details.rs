use std::io::{self, Write};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{block::Padding, Block, Paragraph, Widget, Wrap},
};

use super::{
    approval::{approval_content_width, audit_ref_line, audit_ref_rows, wrapped_preview_rows},
    buffer_to_lines, buffer_to_styled_lines, RatatuiInlineRenderer,
};

#[derive(Debug, Clone)]
pub struct ApprovalDetailsPanelModel<'a> {
    pub id: &'a str,
    pub run_id: &'a str,
    pub source: &'a str,
    pub kind: &'a str,
    pub status: &'a str,
    pub risk: &'a str,
    pub subject: &'a str,
    pub preview_label: &'a str,
    pub preview: &'a str,
    pub request_id: Option<&'a str>,
    pub tool_use_id: Option<&'a str>,
    pub execution_path: Option<&'a str>,
    pub command_block_id: Option<&'a str>,
    pub redaction_status: Option<&'a str>,
    pub assessment: Option<CommandAssessmentSummaryModel<'a>>,
    /// Audit event ID linked to this approval projection. Rendered only when
    /// present so the pull-based audit log never adds a placeholder row.
    pub audit_ref: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct CommandAssessmentSummaryModel<'a> {
    pub impact: &'a str,
    pub execution: &'a str,
    pub confidence: &'a str,
    pub primary_reason: &'a str,
    pub reason_trace: &'a str,
    pub auto_allow: Option<&'a str>,
    pub output_stability: &'a str,
    pub output_exposure: &'a str,
}

impl RatatuiInlineRenderer {
    pub fn write_approval_details_panel<W: Write>(
        &self,
        output: &mut W,
        model: ApprovalDetailsPanelModel<'_>,
    ) -> io::Result<usize> {
        let lines = self.approval_details_panel_write_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub fn approval_details_panel_lines(
        &self,
        model: ApprovalDetailsPanelModel<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_approval_details_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let i18n = self.i18n();
        let height = approval_details_panel_height(&model, width, &i18n);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_approval_details_panel(model, area, &mut buffer, &i18n);
        buffer_to_lines(&buffer, area)
    }

    fn approval_details_panel_write_lines(
        &self,
        model: ApprovalDetailsPanelModel<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_approval_details_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let i18n = self.i18n();
        let height = approval_details_panel_height(&model, width, &i18n);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_approval_details_panel(model, area, &mut buffer, &i18n);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }

    fn plain_approval_details_panel_lines(
        &self,
        model: ApprovalDetailsPanelModel<'_>,
    ) -> Vec<String> {
        let i18n = self.i18n();
        let risk = i18n.format(
            crate::MessageId::ApprovalRiskSuffix,
            &[("risk", model.risk)],
        );
        let mut lines = vec![
            format!(
                "{} {}",
                i18n.t(crate::MessageId::ApprovalDetailsTitle),
                model.id
            ),
            format!("{} - {} - {}", model.kind, model.status, risk),
            format!(
                "{}: {}  {}: {}",
                i18n.t(crate::MessageId::ApprovalDetailsSourceLabel),
                model.source,
                i18n.t(crate::MessageId::ApprovalDetailsRunLabel),
                model.run_id
            ),
            format!(
                "{}: {}  {}: {}",
                i18n.t(crate::MessageId::ApprovalDetailsExecutionLabel),
                model
                    .execution_path
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsPendingValue)),
                i18n.t(crate::MessageId::ApprovalDetailsCommandBlockLabel),
                model
                    .command_block_id
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue))
            ),
            format!(
                "{}: {}",
                i18n.t(crate::MessageId::ApprovalDetailsRedactionLabel),
                model
                    .redaction_status
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNotApplicableValue))
            ),
            format!(
                "{}: {}  {}: {}",
                i18n.t(crate::MessageId::ApprovalDetailsProviderRequestLabel),
                model
                    .request_id
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue)),
                i18n.t(crate::MessageId::ApprovalDetailsToolUseLabel),
                model
                    .tool_use_id
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue))
            ),
            i18n.t(crate::MessageId::ApprovalDetailsDefaultDenyLine)
                .to_string(),
            format!(
                "{}: {}",
                i18n.t(crate::MessageId::ApprovalDetailsRequestLabel),
                user_facing_subject(&i18n, model.subject)
            ),
            format!(
                "{}: {}",
                user_facing_preview_label(&i18n, &model),
                model.preview
            ),
            i18n.t(crate::MessageId::ApprovalExecutableToolPolicy)
                .to_string(),
        ];
        if let Some(assessment) = model.assessment {
            lines.insert(6, assessment_summary_line(&i18n, assessment));
        }
        if let Some(audit_ref) = model.audit_ref {
            lines.insert(6, audit_ref_line(audit_ref));
        }
        lines
    }
}

fn approval_details_panel_height(
    model: &ApprovalDetailsPanelModel<'_>,
    width: u16,
    i18n: &crate::I18n,
) -> u16 {
    let content_width = approval_content_width(width);
    let preview_rows = wrapped_preview_rows(model.preview, content_width, 6)
        .len()
        .max(1) as u16;
    let assessment_rows = model
        .assessment
        .map(|assessment| assessment_summary_rows(i18n, assessment, content_width).len() as u16)
        .unwrap_or(0);
    let audit_rows = audit_ref_rows(model.audit_ref, content_width).len() as u16;
    11 + preview_rows + assessment_rows + audit_rows
}

fn render_approval_details_panel(
    model: ApprovalDetailsPanelModel<'_>,
    area: Rect,
    buffer: &mut Buffer,
    i18n: &crate::I18n,
) {
    let border = if model.risk == "high" {
        Color::Red
    } else {
        Color::Yellow
    };
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(
                format!(" {} ", i18n.t(crate::MessageId::ApprovalDetailsTitle)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{} ", model.id)),
        ]))
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    block.render(area, buffer);

    let preview_rows =
        wrapped_preview_rows(model.preview, inner.width.saturating_sub(2) as usize, 6);
    let assessment_rows = model
        .assessment
        .map(|assessment| {
            assessment_summary_rows(i18n, assessment, inner.width.saturating_sub(2) as usize)
        })
        .unwrap_or_default();
    // Same content width as `approval_details_panel_height`, so a wrapped
    // reference can never exceed its allocated rows.
    let audit_rows = audit_ref_rows(model.audit_ref, inner.width as usize);
    let risk = i18n.format(
        crate::MessageId::ApprovalRiskSuffix,
        &[("risk", model.risk)],
    );
    let chunks = Layout::vertical(vec![
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(audit_rows.len() as u16),
        Constraint::Length(assessment_rows.len() as u16),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(preview_rows.len().max(1) as u16),
        Constraint::Length(1),
    ])
    .split(inner);

    Paragraph::new(Line::from(vec![
        Span::styled(model.kind.to_string(), Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::raw(model.status.to_string()),
        Span::raw("  "),
        Span::styled(risk, Style::default().fg(border)),
    ]))
    .render(chunks[0], buffer);
    Paragraph::new(Line::from(vec![
        Span::styled(
            format!("{}: ", i18n.t(crate::MessageId::ApprovalDetailsSourceLabel)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(model.source.to_string()),
        Span::raw("  "),
        Span::styled(
            format!("{}: ", i18n.t(crate::MessageId::ApprovalDetailsRunLabel)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(model.run_id.to_string()),
    ]))
    .render(chunks[1], buffer);
    Paragraph::new(Line::from(vec![
        Span::styled(
            format!(
                "{}: ",
                i18n.t(crate::MessageId::ApprovalDetailsExecutionLabel)
            ),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(
            model
                .execution_path
                .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsPendingValue))
                .to_string(),
        ),
        Span::raw("  "),
        Span::styled(
            format!(
                "{}: ",
                i18n.t(crate::MessageId::ApprovalDetailsCommandBlockLabel)
            ),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(
            model
                .command_block_id
                .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue))
                .to_string(),
        ),
    ]))
    .render(chunks[2], buffer);
    Paragraph::new(Line::from(vec![
        Span::styled(
            format!(
                "{}: ",
                i18n.t(crate::MessageId::ApprovalDetailsRedactionLabel)
            ),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(
            model
                .redaction_status
                .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNotApplicableValue))
                .to_string(),
        ),
    ]))
    .render(chunks[3], buffer);
    Paragraph::new(Line::from(vec![
        Span::styled(
            format!(
                "{}: ",
                i18n.t(crate::MessageId::ApprovalDetailsProviderRequestLabel)
            ),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(
            model
                .request_id
                .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue))
                .to_string(),
        ),
        Span::raw("  "),
        Span::styled(
            format!(
                "{}: ",
                i18n.t(crate::MessageId::ApprovalDetailsToolUseLabel)
            ),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(
            model
                .tool_use_id
                .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue))
                .to_string(),
        ),
    ]))
    .render(chunks[4], buffer);
    if !audit_rows.is_empty() {
        Paragraph::new(Text::from(
            audit_rows
                .into_iter()
                .map(|line| Line::from(Span::raw(line)))
                .collect::<Vec<_>>(),
        ))
        .render(chunks[5], buffer);
    }
    if !assessment_rows.is_empty() {
        Paragraph::new(Text::from(
            assessment_rows
                .into_iter()
                .map(|line| Line::from(Span::raw(line)))
                .collect::<Vec<_>>(),
        ))
        .render(chunks[6], buffer);
    }
    Paragraph::new(i18n.t(crate::MessageId::ApprovalDetailsDefaultDenyLine))
        .render(chunks[7], buffer);
    Paragraph::new(Line::from(vec![
        Span::styled(
            format!(
                "{}: ",
                i18n.t(crate::MessageId::ApprovalDetailsRequestLabel)
            ),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(user_facing_subject(i18n, model.subject)),
    ]))
    .render(chunks[8], buffer);
    Paragraph::new(Line::from(Span::styled(
        format!("{}:", user_facing_preview_label(i18n, &model)),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )))
    .render(chunks[9], buffer);
    Paragraph::new(Text::from(
        preview_rows
            .into_iter()
            .map(|line| Line::from(Span::raw(line)))
            .collect::<Vec<_>>(),
    ))
    .render(chunks[10], buffer);
    Paragraph::new(i18n.t(crate::MessageId::ApprovalExecutableToolPolicy))
        .wrap(Wrap { trim: true })
        .render(chunks[11], buffer);
}

fn assessment_summary_line(
    i18n: &crate::I18n,
    assessment: CommandAssessmentSummaryModel<'_>,
) -> String {
    i18n.format(
        crate::MessageId::ApprovalAssessmentSummaryLine,
        &[
            ("impact", assessment.impact),
            ("decision", assessment.execution),
            ("confidence", assessment.confidence),
        ],
    )
}

fn assessment_reason_line(
    i18n: &crate::I18n,
    assessment: CommandAssessmentSummaryModel<'_>,
) -> String {
    i18n.format(
        crate::MessageId::ApprovalAssessmentReasonLine,
        &[("reason", assessment.primary_reason)],
    )
}

fn assessment_summary_rows(
    i18n: &crate::I18n,
    assessment: CommandAssessmentSummaryModel<'_>,
    content_width: usize,
) -> Vec<String> {
    let mut rows =
        wrapped_preview_rows(&assessment_summary_line(i18n, assessment), content_width, 2);
    rows.extend(wrapped_preview_rows(
        &assessment_reason_line(i18n, assessment),
        content_width,
        2,
    ));
    rows
}

fn user_facing_subject(i18n: &crate::I18n, subject: &str) -> String {
    let subject = subject.trim();
    if subject.eq_ignore_ascii_case("tool Bash") || subject.eq_ignore_ascii_case("tool shell") {
        i18n.t(crate::MessageId::ApprovalDetailsBashCommandSubject)
            .to_string()
    } else if subject.eq_ignore_ascii_case("shell command") {
        i18n.t(crate::MessageId::ApprovalDetailsShellCommandSubject)
            .to_string()
    } else if let Some(tool) = subject.strip_prefix("tool ") {
        i18n.format(
            crate::MessageId::ApprovalDetailsToolSubject,
            &[("tool", tool)],
        )
    } else {
        subject.to_string()
    }
}
fn user_facing_preview_label(i18n: &crate::I18n, model: &ApprovalDetailsPanelModel<'_>) -> String {
    let subject = model.subject.to_ascii_lowercase();
    let en_tool_input =
        crate::I18n::new(crate::Language::EnUs).t(crate::MessageId::ApprovalToolInputLabel);
    if subject.contains("bash") || subject.contains("shell") {
        i18n.t(crate::MessageId::ApprovalCommandLabel).to_string()
    } else if model.preview_label.eq_ignore_ascii_case(en_tool_input)
        || model.preview_label == i18n.t(crate::MessageId::ApprovalToolInputLabel)
    {
        i18n.t(crate::MessageId::ApprovalDetailsInputLabel)
            .to_string()
    } else {
        model.preview_label.to_string()
    }
}
