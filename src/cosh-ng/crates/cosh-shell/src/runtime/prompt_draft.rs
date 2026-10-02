//! Multi-line prompt draft card runtime (#1721 D13-D15).
//!
//! Consumes the `component == "prompt_draft"` lifecycle events forwarded by
//! the shell host (open/changed/submit/cancel), keeps the card state on
//! [`InlineState`], and redraws the editor in place. General drafts use the
//! V2 rounded card; Agent composition uses a borderless `◆` prompt so input
//! ownership is visible before submission.

use std::io::Write;

use unicode_width::UnicodeWidthChar;

use crate::agent::composer::{
    ComposerCompletion, ComposerReferenceRejection, RejectedComposerReference,
};
use crate::i18n::MessageId;
use crate::runtime::state::InlineState;
use crate::types::{InputOwner, ShellEvent, ShellEventKind};

/// Active draft card bookkeeping (viewport snapshot + rendered height).
#[derive(Debug, Clone, Default)]
pub(crate) struct PromptDraftCardState {
    pub(crate) id: String,
    pub(crate) kind: PromptDraftKind,
    pub(crate) runtime: String,
    pub(crate) workspace_cwd: Option<String>,
    pub(crate) skill_names: Vec<String>,
    pub(crate) completions: Vec<ComposerCompletion>,
    pub(crate) selected_completion: usize,
    pub(crate) text: String,
    pub(crate) rows: Vec<String>,
    pub(crate) hidden_above: usize,
    pub(crate) hidden_below: usize,
    pub(crate) cursor: (usize, usize),
    pub(crate) panel_height: usize,
    /// First paint opens on a fresh line below the bash prompt (relay
    /// path); the slash path starts at a fresh column already (#1932).
    pub(crate) line_break_before: bool,
}

impl PromptDraftCardState {
    pub(crate) fn capture(&self) -> crate::raw_input::RawInputCapture {
        crate::raw_input::RawInputCapture::PromptDraft {
            id: self.id.clone(),
            initial_text: self.text.clone().into_boxed_str(),
            completion: self
                .completions
                .get(self.selected_completion)
                .map(|completion| {
                    Box::new((
                        completion.replacement.clone(),
                        (self.hidden_above + self.cursor.0, self.cursor.1),
                    ))
                }),
            agent_composer: self.kind == PromptDraftKind::AgentComposer,
            workspace_cwd: self.workspace_cwd.as_deref().map(Into::into),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingAgentComposerSubmission {
    pub(crate) text: String,
    pub(crate) workspace_cwd: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PromptDraftKind {
    #[default]
    Draft,
    AgentComposer,
}

/// Discoverability state for the multi-line prompt entries: the one-time
/// soft-newline tip (#1721 T-c) and the failure-insight entry hint (#1932).
#[derive(Default)]
pub(crate) struct PromptEntryHints {
    /// A soft-newline shortcut was observed on the bash-owned passthrough
    /// path; surface a one-time tip at the next prompt-ready boundary.
    pub(crate) pending_soft_newline_tip: bool,
    pub(crate) shown_soft_newline_tip: bool,
    /// A multi-line bracketed paste was relayed straight to bash this
    /// prompt cycle: arms the failure-insight multi-line entry hint.
    pub(crate) multiline_paste_observed: bool,
    /// Session-scoped cap: at most two nudges per session.
    pub(crate) multiline_entry_hint_count: u8,
}

/// Panel width aligned with every other card: the renderer's standard
/// width follows the terminal (clamped), same as approval/question panels.
fn card_width() -> usize {
    (crate::ui::RatatuiInlineRenderer::for_terminal().panel_standard_width() as usize).max(32)
}

#[derive(Clone, Copy, PartialEq)]
enum CardPhase {
    Editing,
    Submitted,
    ControlSubmitted,
    Cancelled,
}

/// Opens a fresh prompt-draft card immediately (#1932) for relay-driven
/// multiline input. The pending card capture picks the draft up on the next
/// controller pass.
/// `line_break_before` distinguishes the relay path (cursor still sits on
/// the bash prompt line, the card must open on a fresh line) from the
/// slash path (the dispatcher already left the cursor at a fresh column).
pub(crate) fn open_prompt_draft<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    text: String,
    line_break_before: bool,
    runtime: &str,
) -> std::io::Result<()> {
    open_prompt_editor(
        state,
        output,
        text,
        line_break_before,
        runtime,
        None,
        Vec::new(),
        PromptDraftKind::Draft,
    )
}

pub(crate) fn open_agent_composer<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    runtime: &str,
    workspace_cwd: Option<&str>,
    skill_names: Vec<String>,
) -> std::io::Result<()> {
    open_prompt_editor(
        state,
        output,
        String::new(),
        false,
        runtime,
        workspace_cwd,
        skill_names,
        PromptDraftKind::AgentComposer,
    )
}

fn open_prompt_editor<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    text: String,
    line_break_before: bool,
    runtime: &str,
    workspace_cwd: Option<&str>,
    skill_names: Vec<String>,
    kind: PromptDraftKind,
) -> std::io::Result<()> {
    state.prompt_draft_seq += 1;
    let id = format!("draft-{}", state.prompt_draft_seq);
    // The capture side owns the editor; mirror its initial viewport so the
    // first paint happens before any Changed snapshot arrives.
    let editor = crate::raw_input::PromptDraftEditor::from_text(&text);
    let view = editor.viewport();
    let mut card = PromptDraftCardState {
        id,
        kind,
        runtime: runtime.to_string(),
        workspace_cwd: workspace_cwd.map(str::to_string),
        skill_names,
        completions: Vec::new(),
        selected_completion: 0,
        text,
        rows: view.rows,
        hidden_above: view.hidden_above,
        hidden_below: view.hidden_below,
        cursor: view.cursor,
        panel_height: 0,
        line_break_before,
    };
    draw_card(&mut card, state, output, CardPhase::Editing)?;
    state.prompt_draft = Some(card);
    Ok(())
}

pub(crate) fn handle_prompt_draft_events<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    runtime: &str,
) -> std::io::Result<()> {
    for event in events {
        if event.kind != ShellEventKind::UserInputIntercepted
            || event.component.as_deref() != Some("prompt_draft")
        {
            continue;
        }
        let payload = event.input.as_deref().unwrap_or("{}");
        let value: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
        match event.message.as_deref() {
            Some("open") => {
                let text = value["text"].as_str().unwrap_or_default().to_string();
                open_prompt_draft(state, output, text, true, runtime)?;
            }
            Some("changed") => {
                let Some(mut card) = state.prompt_draft.take() else {
                    continue;
                };
                if value["id"].as_str() != Some(card.id.as_str()) {
                    state.prompt_draft = Some(card);
                    continue;
                }
                card.text = value["text"].as_str().unwrap_or_default().to_string();
                card.rows = value["rows"]
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|row| row.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                card.hidden_above = value["hidden_above"].as_u64().unwrap_or(0) as usize;
                card.hidden_below = value["hidden_below"].as_u64().unwrap_or(0) as usize;
                card.cursor = (
                    value["cursor_row"].as_u64().unwrap_or(0) as usize,
                    value["cursor_col"].as_u64().unwrap_or(0) as usize,
                );
                if card.kind == PromptDraftKind::AgentComposer {
                    let cursor_row =
                        value["first_row"].as_u64().unwrap_or(0) as usize + card.cursor.0;
                    card.completions = crate::agent::composer::completions(
                        &card.text,
                        cursor_row,
                        card.cursor.1,
                        card.workspace_cwd.as_deref(),
                        &card.skill_names,
                    );
                }
                card.selected_completion = (value["selected_completion"].as_u64().unwrap_or(0)
                    as usize)
                    .min(card.completions.len().saturating_sub(1));
                draw_card(&mut card, state, output, CardPhase::Editing)?;
                state.prompt_draft = Some(card);
            }
            Some("submit") => {
                let Some(mut card) = state.prompt_draft.take() else {
                    continue;
                };
                let slash = value["slash"].as_bool().unwrap_or(false);
                if card.kind == PromptDraftKind::AgentComposer && !slash {
                    state.pending_agent_composer_submission =
                        Some(PendingAgentComposerSubmission {
                            text: value["text"]
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| card.text.clone()),
                            workspace_cwd: card.workspace_cwd.clone(),
                        });
                }
                if slash {
                    state.pending_agent_composer_submission = None;
                    state.agent_run.needs_prompt_after_run = true;
                    state.agent_run.native_prompt_after_run = false;
                }
                card.completions.clear();
                // The corresponding intercept dispatches after the editor
                // releases input; its command may open another capture.
                let phase = if slash {
                    CardPhase::ControlSubmitted
                } else {
                    CardPhase::Submitted
                };
                draw_card(&mut card, state, output, phase)?;
            }
            Some("cancel") => {
                let Some(mut card) = state.prompt_draft.take() else {
                    continue;
                };
                card.completions.clear();
                draw_card(&mut card, state, output, CardPhase::Cancelled)?;
                state.pending_agent_composer_submission = None;
                // D15: restore the bash prompt after cancelling composition.
                state.trigger_pty_prompt = true;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn render_agent_composer_rejections<W: Write>(
    state: &InlineState,
    output: &mut W,
    rejected: &[RejectedComposerReference],
) -> std::io::Result<()> {
    if rejected.is_empty() {
        return Ok(());
    }

    let i18n = state.i18n();
    let body = rejected
        .iter()
        .map(|reference| {
            let message = match reference.reason {
                ComposerReferenceRejection::WorkspaceUnavailable => {
                    MessageId::AgentComposerRejectedWorkspaceUnavailableLine
                }
                ComposerReferenceRejection::InvalidPath => {
                    MessageId::AgentComposerRejectedInvalidPathLine
                }
                ComposerReferenceRejection::UnavailablePath => {
                    MessageId::AgentComposerRejectedUnavailablePathLine
                }
                ComposerReferenceRejection::OutsideWorkspace => {
                    MessageId::AgentComposerRejectedOutsideWorkspaceLine
                }
                ComposerReferenceRejection::LimitExceeded => {
                    MessageId::AgentComposerRejectedLimitLine
                }
            };
            let path = serde_json::to_string(&reference.path)
                .unwrap_or_else(|_| "\"<invalid>\"".to_string());
            i18n.format(message, &[("path", path.as_str())])
        })
        .collect();
    crate::ui::RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        crate::ui::NoticePanelModel {
            title: i18n.t(MessageId::AgentComposerRejectedTitle),
            body,
            footer: Some(i18n.t(MessageId::AgentComposerRejectedFooter)),
        },
    )
}

fn columns(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

/// One card content row: clipped to the budget, cursor cell inverted.
fn content_row(row: &str, cursor_col: Option<usize>, budget: usize, dim: bool) -> String {
    let mut cells: Vec<String> = Vec::new();
    let mut used = 0;
    let mut clipped = false;
    for (index, ch) in row.chars().enumerate() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width > budget.saturating_sub(1) {
            clipped = true;
            break;
        }
        if Some(index) == cursor_col {
            cells.push(format!("\x1b[7m{ch}\x1b[27m"));
        } else {
            cells.push(ch.to_string());
        }
        used += width;
    }
    let char_count = row.chars().count();
    if clipped {
        cells.push("…".to_string());
        used += 1;
    } else if cursor_col.is_some_and(|col| col >= char_count) && used < budget {
        // Cursor sits at end-of-line: render an inverted space cell.
        cells.push("\x1b[7m \x1b[27m".to_string());
        used += 1;
    }
    let body = cells.concat();
    let pad = " ".repeat(budget.saturating_sub(used));
    if dim {
        format!("\x1b[2m{body}{pad}\x1b[22m")
    } else {
        format!("{body}{pad}")
    }
}

fn draw_card<W: Write>(
    card: &mut PromptDraftCardState,
    state: &mut InlineState,
    output: &mut W,
    phase: CardPhase,
) -> std::io::Result<()> {
    if card.kind == PromptDraftKind::AgentComposer {
        return draw_agent_composer(card, state, output, phase);
    }

    let i18n = state.i18n();
    let title = i18n.t(MessageId::PromptDraftTitle);
    let footer = match phase {
        CardPhase::Editing => i18n.t(MessageId::PromptDraftFooterEditing),
        CardPhase::Submitted => i18n.t(MessageId::PromptDraftFooterSubmitted),
        CardPhase::ControlSubmitted => i18n.t(MessageId::PromptDraftFooterControlSubmitted),
        CardPhase::Cancelled => i18n.t(MessageId::PromptDraftFooterCancelled),
    };
    let border = match phase {
        CardPhase::Editing => "\x1b[36m",
        CardPhase::Submitted | CardPhase::ControlSubmitted | CardPhase::Cancelled => "\x1b[2m",
    };
    let reset = "\x1b[0m";
    let width = card_width();
    let budget = width - 4;
    let dim_body = phase != CardPhase::Editing;

    let mut lines: Vec<String> = Vec::new();
    let title_text = format!(" {title} ");
    let title_pad = width.saturating_sub(2 + columns(&title_text));
    lines.push(format!(
        "{border}╭{reset}\x1b[2m{title_text}\x1b[22m{border}{}╮{reset}",
        "─".repeat(title_pad)
    ));
    if card.hidden_above > 0 {
        let marker = format!("… ↑ {}", card.hidden_above);
        lines.push(format!(
            "{border}│{reset} \x1b[2m{marker}{}\x1b[22m {border}│{reset}",
            " ".repeat(budget.saturating_sub(columns(&marker)))
        ));
    }
    for (row_index, row) in card.rows.iter().enumerate() {
        let cursor_col = if phase == CardPhase::Editing && row_index == card.cursor.0 {
            Some(card.cursor.1)
        } else {
            None
        };
        let body = content_row(row, cursor_col, budget, dim_body);
        lines.push(format!("{border}│{reset} {body} {border}│{reset}"));
    }
    if card.hidden_below > 0 {
        let marker = format!("… ↓ {}", card.hidden_below);
        lines.push(format!(
            "{border}│{reset} \x1b[2m{marker}{}\x1b[22m {border}│{reset}",
            " ".repeat(budget.saturating_sub(columns(&marker)))
        ));
    }
    let footer_text = format!(" {footer} ");
    let footer_pad = width.saturating_sub(2 + columns(&footer_text));
    lines.push(format!(
        "{border}╰{reset}\x1b[2m{footer_text}\x1b[22m{border}{}╯{reset}",
        "─".repeat(footer_pad)
    ));

    repaint_editor(card, output, phase, &lines)
}

fn draw_agent_composer<W: Write>(
    card: &mut PromptDraftCardState,
    state: &mut InlineState,
    output: &mut W,
    phase: CardPhase,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let width = card_width();
    let budget = width.saturating_sub(2);
    let dim_body = phase != CardPhase::Editing;
    let mut lines = Vec::new();

    if card.hidden_above > 0 {
        lines.push(format!("  \x1b[2m… ↑ {}\x1b[22m", card.hidden_above));
    }
    for (row_index, row) in card.rows.iter().enumerate() {
        let cursor_col = if phase == CardPhase::Editing && row_index == card.cursor.0 {
            Some(card.cursor.1)
        } else {
            None
        };
        let prefix = if row_index == 0 {
            format!("{} ", InputOwner::Agent.symbol())
        } else {
            "  ".to_string()
        };
        let body = content_row(row, cursor_col, budget, dim_body);
        lines.push(format!("{prefix}{body}"));
    }
    if phase == CardPhase::Editing {
        let start = card.selected_completion / 6 * 6;
        for (index, completion) in card.completions.iter().enumerate().skip(start).take(6) {
            let marker = if index == card.selected_completion {
                "›"
            } else {
                " "
            };
            let suggestion = format!("{marker} {}", completion.display);
            let body = content_row(&suggestion, None, budget, index != card.selected_completion);
            lines.push(format!("  {body}"));
        }
    }
    if phase == CardPhase::Editing && card.completions.len() > 6 {
        lines.push(format!(
            "  {}/{}",
            card.selected_completion + 1,
            card.completions.len()
        ));
    }
    if card.hidden_below > 0 {
        lines.push(format!("  \x1b[2m… ↓ {}\x1b[22m", card.hidden_below));
    }

    let footer = match phase {
        CardPhase::Editing => i18n.t(MessageId::AgentComposerFooterEditing),
        CardPhase::Submitted => i18n.t(MessageId::PromptDraftFooterSubmitted),
        CardPhase::ControlSubmitted => i18n.t(MessageId::PromptDraftFooterControlSubmitted),
        CardPhase::Cancelled => i18n.t(MessageId::PromptDraftFooterCancelled),
    };
    let status = format!(
        "{} · {}: {}",
        i18n.t(MessageId::AgentComposerTitle),
        i18n.t(MessageId::PromptDraftRuntimeLabel),
        card.runtime
    );
    lines.push(format!("  {}", content_row(&status, None, budget, true)));
    lines.push(format!("  {}", content_row(footer, None, budget, true)));

    repaint_editor(card, output, phase, &lines)
}

fn repaint_editor<W: Write>(
    card: &mut PromptDraftCardState,
    output: &mut W,
    phase: CardPhase,
    lines: &[String],
) -> std::io::Result<()> {
    // Climb over the previous render, clear, and repaint. The editor may
    // shrink as viewport markers or completions disappear.
    if card.panel_height > 0 {
        write!(output, "\x1b[{}A", card.panel_height)?;
    } else if card.line_break_before {
        // Relay-driven drafts start with the cursor on the Shell prompt line.
        write!(output, "\x1b[?25l\r\n")?;
    } else {
        // Slash dispatch already left the cursor at a fresh column.
        write!(output, "\x1b[?25l")?;
    }
    let repaint_rows = card.panel_height.max(lines.len());
    for index in 0..repaint_rows {
        write!(output, "\r\x1b[2K")?;
        if let Some(line) = lines.get(index) {
            write!(output, "{line}")?;
        }
        write!(output, "\r\n")?;
    }
    if repaint_rows > lines.len() {
        write!(output, "\x1b[{}A", repaint_rows - lines.len())?;
    }
    if phase != CardPhase::Editing {
        write!(output, "\x1b[?25h")?;
    }
    output.flush()?;
    card.panel_height = lines.len();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_with_rows(rows: &[&str], cursor: (usize, usize)) -> PromptDraftCardState {
        PromptDraftCardState {
            id: "draft-1".to_string(),
            kind: PromptDraftKind::Draft,
            runtime: "fake".to_string(),
            workspace_cwd: None,
            skill_names: Vec::new(),
            completions: Vec::new(),
            selected_completion: 0,
            text: rows.join("\n"),
            rows: rows.iter().map(|row| row.to_string()).collect(),
            hidden_above: 0,
            hidden_below: 0,
            cursor,
            panel_height: 0,
            line_break_before: true,
        }
    }

    #[test]
    fn slash_first_paint_skips_the_extra_blank_line() {
        let mut state = InlineState::default();
        let mut card = card_with_rows(&[""], (0, 0));
        card.line_break_before = false;
        let mut out: Vec<u8> = Vec::new();
        draw_card(&mut card, &mut state, &mut out, CardPhase::Editing).expect("draw");
        let rendered = String::from_utf8(out).expect("utf8");
        assert!(
            rendered.starts_with("\x1b[?25l\r\x1b[2K"),
            "slash-opened card must not emit a leading blank line: {rendered:?}"
        );
        assert!(!rendered.contains("Runtime: fake"), "{rendered:?}");

        let mut composer = card_with_rows(&[""], (0, 0));
        composer.kind = PromptDraftKind::AgentComposer;
        composer.workspace_cwd = Some("/workspace/project".into());
        assert!(
            matches!(composer.capture(), crate::raw_input::RawInputCapture::PromptDraft {
            workspace_cwd: Some(cwd), ..
        } if cwd.as_ref() == "/workspace/project")
        );
        composer.line_break_before = false;
        let mut out: Vec<u8> = Vec::new();
        draw_card(&mut composer, &mut state, &mut out, CardPhase::Editing).expect("draw");
        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("Runtime: fake"), "{rendered:?}");
        assert!(rendered.contains("◆ "), "{rendered:?}");
        assert!(
            !rendered.contains("╭"),
            "composer input is borderless: {rendered:?}"
        );

        let mut relay_card = card_with_rows(&[""], (0, 0));
        let mut out: Vec<u8> = Vec::new();
        draw_card(&mut relay_card, &mut state, &mut out, CardPhase::Editing).expect("draw");
        let rendered = String::from_utf8(out).expect("utf8");
        assert!(
            rendered.starts_with("\x1b[?25l\r\n"),
            "relay-opened card keeps the fresh-line break: {rendered:?}"
        );
    }

    #[test]
    fn editing_card_renders_border_rows_and_cursor_cell() {
        let mut state = InlineState::default();
        let mut card = card_with_rows(&["请帮我分析系统负载", "给出优化建议"], (1, 6));
        let mut out: Vec<u8> = Vec::new();
        draw_card(&mut card, &mut state, &mut out, CardPhase::Editing).expect("draw");
        let rendered = String::from_utf8_lossy(&out).into_owned();
        assert!(rendered.contains("╭"), "top border: {rendered}");
        assert!(rendered.contains("请帮我分析系统负载"));
        assert!(rendered.contains("\x1b[7m"), "cursor cell must invert");
        assert!(rendered.contains("\x1b[36m"), "editing border is cyan");
        assert_eq!(card.panel_height, 4, "top + 2 rows + footer");
    }

    #[test]
    fn frozen_card_dims_and_restores_cursor() {
        let mut state = InlineState::default();
        let mut card = card_with_rows(&["草稿内容"], (0, 0));
        let mut out: Vec<u8> = Vec::new();
        draw_card(&mut card, &mut state, &mut out, CardPhase::Cancelled).expect("draw");
        let rendered = String::from_utf8_lossy(&out).into_owned();
        assert!(!rendered.contains("\x1b[36m"), "no cyan when frozen");
        assert!(rendered.contains("\x1b[?25h"), "cursor must come back");
    }

    #[test]
    fn hidden_line_markers_render_dimmed_counters() {
        let mut state = InlineState::default();
        let mut card = card_with_rows(&["视口行"], (0, 0));
        card.hidden_above = 2;
        card.hidden_below = 3;
        let mut out: Vec<u8> = Vec::new();
        draw_card(&mut card, &mut state, &mut out, CardPhase::Editing).expect("draw");
        let rendered = String::from_utf8_lossy(&out).into_owned();
        assert!(rendered.contains("… ↑ 2"), "above marker: {rendered}");
        assert!(rendered.contains("… ↓ 3"), "below marker: {rendered}");
    }

    #[test]
    fn composer_renders_bounded_completion_rows_inside_the_card() {
        let mut state = InlineState::default();
        let mut card = card_with_rows(&["review @Car"], (0, 11));
        card.kind = PromptDraftKind::AgentComposer;
        card.completions = vec![ComposerCompletion {
            display: "@Cargo.toml".to_string(),
            replacement: "@Cargo.toml ".to_string(),
        }];
        let mut out = Vec::new();

        draw_card(&mut card, &mut state, &mut out, CardPhase::Editing).expect("draw");

        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("◆ review @Car"), "{rendered}");
        assert!(rendered.contains("› @Cargo.toml"), "{rendered}");
        assert_eq!(card.panel_height, 4, "input + result + status + footer");
    }

    #[test]
    fn composer_rejection_notice_names_the_path_and_reason() {
        let state = InlineState::default();
        let rejected = [RejectedComposerReference {
            path: "../Cargo.toml".to_string(),
            reason: ComposerReferenceRejection::InvalidPath,
        }];
        let mut out = Vec::new();

        render_agent_composer_rejections(&state, &mut out, &rejected).expect("render notice");

        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("References skipped"), "{rendered}");
        assert!(rendered.contains("\"../Cargo.toml\""), "{rendered}");
        assert!(
            rendered.contains("invalid workspace-relative path"),
            "{rendered}"
        );
        assert!(
            rendered.contains("were not sent as Agent context"),
            "{rendered}"
        );
    }
}
