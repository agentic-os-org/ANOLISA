use std::io::{self, Write};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{block::Padding, Block, Paragraph, Widget, Wrap},
};

use super::{
    approval::{approval_content_width, audit_ref_line, audit_ref_rows, wrapped_preview_rows},
    approval_details::CommandAssessmentSummaryModel,
    buffer_to_lines, buffer_to_styled_lines, RatatuiInlineRenderer,
};

#[derive(Debug, Clone)]
pub struct ApprovalJournalEntryModel<'a> {
    pub id: &'a str,
    pub run_id: &'a str,
    pub source: &'a str,
    pub decision: &'a str,
    pub kind: &'a str,
    pub risk: &'a str,
    pub subject: &'a str,
    pub preview: &'a str,
    pub preview_hash: &'a str,
    pub request_id: Option<&'a str>,
    pub tool_use_id: Option<&'a str>,
    pub actor: &'a str,
    pub execution_path: Option<&'a str>,
    pub command_block_id: Option<&'a str>,
    pub redaction_status: Option<&'a str>,
    pub assessment: Option<CommandAssessmentSummaryModel<'a>>,
    /// Audit event ID linked to this approval entry. Rendered inside the owning
    /// entry only when present so the pull-based audit log adds no placeholder.
    pub audit_ref: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct ApprovalJournalPanelModel<'a> {
    pub entries: &'a [ApprovalJournalEntryModel<'a>],
}

impl RatatuiInlineRenderer {
    pub fn write_approval_journal_panel<W: Write>(
        &self,
        output: &mut W,
        model: ApprovalJournalPanelModel<'_>,
    ) -> io::Result<usize> {
        let lines = self.approval_journal_panel_write_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub fn approval_journal_panel_lines(
        &self,
        model: ApprovalJournalPanelModel<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_approval_journal_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let i18n = self.i18n();
        let height = approval_journal_panel_height(&model, width, &i18n);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_approval_journal_panel(model, area, &mut buffer, &i18n);
        buffer_to_lines(&buffer, area)
    }

    fn approval_journal_panel_write_lines(
        &self,
        model: ApprovalJournalPanelModel<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_approval_journal_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let i18n = self.i18n();
        let height = approval_journal_panel_height(&model, width, &i18n);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_approval_journal_panel(model, area, &mut buffer, &i18n);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }

    fn plain_approval_journal_panel_lines(
        &self,
        model: ApprovalJournalPanelModel<'_>,
    ) -> Vec<String> {
        let i18n = self.i18n();
        if model.entries.is_empty() {
            return vec![
                i18n.t(crate::MessageId::ApprovalJournalTitle).to_string(),
                i18n.t(crate::MessageId::ApprovalJournalEmptyBody)
                    .to_string(),
            ];
        }

        let decision_count = approval_journal_decision_count(&i18n, model.entries.len());
        let mut lines = vec![format!(
            "{} - {decision_count}",
            i18n.t(crate::MessageId::ApprovalJournalTitle)
        )];
        for entry in model.entries {
            let risk = i18n.format(
                crate::MessageId::ApprovalRiskSuffix,
                &[("risk", entry.risk)],
            );
            lines.push(format!(
                "{} {} - {} - {}",
                entry.id, entry.decision, entry.kind, risk
            ));
            lines.push(format!(
                "  {}: {}  {}: {}",
                i18n.t(crate::MessageId::ApprovalDetailsSourceLabel),
                entry.source,
                i18n.t(crate::MessageId::ApprovalDetailsRunLabel),
                entry.run_id
            ));
            lines.push(format!(
                "  {}: {}  {}: {}",
                i18n.t(crate::MessageId::ApprovalDetailsExecutionLabel),
                entry
                    .execution_path
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsPendingValue)),
                i18n.t(crate::MessageId::ApprovalDetailsCommandBlockLabel),
                entry
                    .command_block_id
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue))
            ));
            lines.push(format!(
                "  {}: {}",
                i18n.t(crate::MessageId::ApprovalDetailsRedactionLabel),
                entry
                    .redaction_status
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNotApplicableValue))
            ));
            if let Some(assessment) = entry.assessment {
                lines.push(format!(
                    "  {}",
                    approval_assessment_summary_line(&i18n, assessment)
                ));
            }
            lines.push(format!(
                "  {}: {}  {}: {}",
                i18n.t(crate::MessageId::ApprovalDetailsProviderRequestLabel),
                entry
                    .request_id
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue)),
                i18n.t(crate::MessageId::ApprovalDetailsToolUseLabel),
                entry
                    .tool_use_id
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue))
            ));
            if let Some(audit_ref) = entry.audit_ref {
                lines.push(format!("  {}", audit_ref_line(audit_ref)));
            }
            lines.push(format!(
                "  {}: {}",
                i18n.t(crate::MessageId::ApprovalJournalActorLabel),
                entry.actor
            ));
            lines.push(format!(
                "  {}: {}",
                i18n.t(crate::MessageId::ApprovalJournalPreviewHashLabel),
                entry.preview_hash
            ));
            lines.push(format!(
                "  {}: {}",
                i18n.t(crate::MessageId::ApprovalJournalSubjectLabel),
                entry.subject
            ));
            lines.push(format!(
                "  {}: {}",
                approval_journal_preview_label(&i18n, entry.subject),
                entry.preview
            ));
        }
        lines
    }
}

fn approval_journal_panel_height(
    model: &ApprovalJournalPanelModel<'_>,
    width: u16,
    i18n: &crate::I18n,
) -> u16 {
    if model.entries.is_empty() {
        return 4;
    }

    let content_width = approval_content_width(width);
    let row_count = model
        .entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let separator_rows = u16::from(idx > 0);
            let assessment_rows = entry
                .assessment
                .map(|assessment| {
                    approval_assessment_summary_rows(i18n, assessment, content_width).len() as u16
                })
                .unwrap_or(0);
            let preview_rows = wrapped_preview_rows(
                &format!(
                    "{}: {}",
                    approval_journal_preview_label(i18n, entry.subject),
                    entry.preview
                ),
                content_width,
                2,
            )
            .len()
            .max(1) as u16;
            let audit_rows = audit_ref_rows(entry.audit_ref, content_width).len() as u16;
            separator_rows + 8 + assessment_rows + preview_rows + audit_rows
        })
        .sum::<u16>();
    2 + row_count
}

fn render_approval_journal_panel(
    model: ApprovalJournalPanelModel<'_>,
    area: Rect,
    buffer: &mut Buffer,
    i18n: &crate::I18n,
) {
    let decision_count = approval_journal_decision_count(i18n, model.entries.len());
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(
                format!(" {} ", i18n.t(crate::MessageId::ApprovalJournalTitle)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{decision_count} ")),
        ]))
        .border_style(Style::default().fg(Color::Blue));
    let inner = block.inner(area);
    block.render(area, buffer);

    if model.entries.is_empty() {
        Paragraph::new(i18n.t(crate::MessageId::ApprovalJournalEmptyBody))
            .wrap(Wrap { trim: true })
            .render(inner, buffer);
        return;
    }

    let mut lines = Vec::new();
    for (idx, entry) in model.entries.iter().enumerate() {
        if idx > 0 {
            lines.push(Line::from(""));
        }
        let decision_style = if entry.decision == "approved" {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        };
        let risk = i18n.format(
            crate::MessageId::ApprovalRiskSuffix,
            &[("risk", entry.risk)],
        );
        lines.push(Line::from(vec![
            Span::styled(
                entry.id.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(entry.decision.to_string(), decision_style),
            Span::raw("  "),
            Span::styled(entry.kind.to_string(), Style::default().fg(Color::Cyan)),
            Span::raw("  "),
            Span::styled(risk, Style::default().fg(Color::Yellow)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", i18n.t(crate::MessageId::ApprovalDetailsSourceLabel)),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(entry.source.to_string()),
            Span::raw("  "),
            Span::styled(
                format!("{}: ", i18n.t(crate::MessageId::ApprovalDetailsRunLabel)),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(entry.run_id.to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{}: ",
                    i18n.t(crate::MessageId::ApprovalDetailsExecutionLabel)
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(
                entry
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
                entry
                    .command_block_id
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue))
                    .to_string(),
            ),
        ]));
        if let Some(assessment) = entry.assessment {
            for row in approval_assessment_summary_rows(
                i18n,
                assessment,
                inner.width.saturating_sub(2) as usize,
            ) {
                lines.push(Line::from(row));
            }
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{}: ",
                    i18n.t(crate::MessageId::ApprovalDetailsRedactionLabel)
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(
                entry
                    .redaction_status
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNotApplicableValue))
                    .to_string(),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{}: ",
                    i18n.t(crate::MessageId::ApprovalDetailsProviderRequestLabel)
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(
                entry
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
                entry
                    .tool_use_id
                    .unwrap_or(i18n.t(crate::MessageId::ApprovalDetailsNoneValue))
                    .to_string(),
            ),
        ]));
        // Same content width as `approval_journal_panel_height`, so wrapped
        // reference rows never push the trailing entry rows out of the panel.
        for row in audit_ref_rows(entry.audit_ref, inner.width as usize) {
            lines.push(Line::from(Span::raw(row)));
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", i18n.t(crate::MessageId::ApprovalJournalActorLabel)),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(entry.actor.to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{}: ",
                    i18n.t(crate::MessageId::ApprovalJournalPreviewHashLabel)
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(entry.preview_hash.to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{}: ",
                    i18n.t(crate::MessageId::ApprovalJournalSubjectLabel)
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(entry.subject.to_string()),
        ]));
        for row in wrapped_preview_rows(
            &format!(
                "{}: {}",
                approval_journal_preview_label(i18n, entry.subject),
                entry.preview
            ),
            inner.width.saturating_sub(2) as usize,
            2,
        ) {
            lines.push(Line::from(row));
        }
    }

    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: true })
        .render(inner, buffer);
}

fn approval_assessment_summary_line(
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

fn approval_assessment_reason_line(
    i18n: &crate::I18n,
    assessment: CommandAssessmentSummaryModel<'_>,
) -> String {
    i18n.format(
        crate::MessageId::ApprovalAssessmentReasonLine,
        &[("reason", assessment.primary_reason)],
    )
}

fn approval_assessment_summary_rows(
    i18n: &crate::I18n,
    assessment: CommandAssessmentSummaryModel<'_>,
    content_width: usize,
) -> Vec<String> {
    let mut rows = wrapped_preview_rows(
        &approval_assessment_summary_line(i18n, assessment),
        content_width,
        2,
    );
    rows.extend(wrapped_preview_rows(
        &approval_assessment_reason_line(i18n, assessment),
        content_width,
        2,
    ));
    rows
}

fn approval_journal_decision_count(i18n: &crate::I18n, count: usize) -> String {
    i18n.format(
        crate::MessageId::ApprovalJournalDecisionCount,
        &[("count", &count.to_string())],
    )
}

fn approval_journal_preview_label(i18n: &crate::I18n, subject: &str) -> &'static str {
    let subject = subject.to_ascii_lowercase();
    if subject.contains("bash") || subject.contains("shell") {
        i18n.t(crate::MessageId::ApprovalCommandLabel)
    } else {
        i18n.t(crate::MessageId::ApprovalJournalPreviewLabel)
    }
}
