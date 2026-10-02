use std::path::Path;

use crate::adapter::{ShellEvidenceMetadata, ShellEvidenceResult};
use crate::evidence::model::OutputExcerptDirection;
use crate::evidence::output_policy::{
    bounded_output_excerpt_for_block, output_excerpt_status_for_block, parse_terminal_output_id,
    terminal_output_id,
};
use crate::runtime::prelude::{redact_provider_command_text, CoshApprovalMode};
use crate::types::CommandBlock;
use crate::types::CommandStatus;

const MAX_OUTPUT_LINES: usize = 300;
const MAX_OUTPUT_BYTES: usize = 12 * 1024;
const LIST_CURSOR_PREFIX: &str = "offset:";
pub(crate) const ALREADY_DELIVERED_RECENT_REASON: &str =
    "already_delivered_recent_shell_tool_output";

pub(crate) fn list_shell_evidence_commands(
    blocks: &[CommandBlock],
    limit: u16,
    cursor: Option<&str>,
) -> ShellEvidenceResult {
    let Some(offset) = parse_list_cursor(cursor) else {
        return list_unavailable(limit, cursor.unwrap_or("<none>"), "invalid_cursor");
    };
    let limit = usize::from(limit).clamp(1, 100);
    let page_end = offset.saturating_add(limit).min(blocks.len());
    let page = blocks.get(offset..page_end).unwrap_or_default();
    let next_cursor = if page_end < blocks.len() {
        Some(format!("{LIST_CURSOR_PREFIX}{page_end}"))
    } else {
        None
    };
    let mut lines = vec![
        "ShellEvidenceCommandIndex".to_string(),
        "scope: current_ledger".to_string(),
        format!("limit: {limit}"),
        format!("cursor: {}", cursor.unwrap_or("<none>")),
        format!(
            "next_cursor: {}",
            next_cursor.as_deref().unwrap_or("<none>")
        ),
        format!("command_count: {}", page.len()),
        format!("total_command_count: {}", blocks.len()),
    ];
    for block in page {
        let output_ref_status = output_excerpt_status_for_block(block);
        let output_available =
            block.output.terminal_output_ref.is_some() && output_ref_status != "expired";
        let output_id = if output_available {
            terminal_output_id(&block.session_id, &block.id)
        } else {
            "<none>".to_string()
        };
        lines.push(format!(
            "command\n\
             session_id: {session_id}\n\
             command_id: {command_id}\n\
             command: {command}\n\
             cwd: {cwd}\n\
             end_cwd: {end_cwd}\n\
             status: {status}\n\
             exit_code: {exit_code}\n\
             started_at_ms: {started_at_ms}\n\
             ended_at_ms: {ended_at_ms}\n\
             duration_ms: {duration_ms}\n\
             output_id: {output_id}\n\
             output_available: {output_available}\n\
             output_ref_status: {output_ref_status}\n\
             output_bytes: {output_bytes}",
            session_id = block.session_id,
            command_id = block.id,
            command = redact_provider_command_text(&block.command),
            cwd = block.cwd,
            end_cwd = block.end_cwd,
            status = command_status(block),
            exit_code = block.exit_code,
            started_at_ms = block.started_at_ms,
            ended_at_ms = block.ended_at_ms,
            duration_ms = block.duration_ms,
            output_ref_status = output_ref_status,
            output_bytes = block.output.terminal_output_bytes,
        ));
    }

    ShellEvidenceResult {
        llm_content: lines.join("\n"),
        return_display: None,
        metadata: ShellEvidenceMetadata {
            action: "list_commands".to_string(),
            scope: Some("current_ledger".to_string()),
            limit: Some(limit as u16),
            next_cursor,
            output_id: "<none>".to_string(),
            status: "included".to_string(),
            excerpt_status: "available".to_string(),
            reason: None,
            direction: "<none>".to_string(),
            lines: 0,
            command_count: Some(page.len()),
            provider_visible_byte_cap: MAX_OUTPUT_BYTES,
            truncated: false,
            truncated_by_lines: false,
            truncated_by_bytes: false,
            truncation_reason: "none".to_string(),
            is_error: false,
        },
    }
}

pub(crate) fn read_shell_evidence_output(
    blocks: &[CommandBlock],
    approval_mode: CoshApprovalMode,
    output_id: &str,
    direction: &str,
    lines: u16,
) -> ShellEvidenceResult {
    let Some(parsed) = parse_terminal_output_id(output_id) else {
        return unavailable(
            "read_output",
            output_id,
            direction,
            lines,
            "invalid_output_id",
        );
    };

    let session_seen = blocks
        .iter()
        .any(|block| block.session_id == parsed.shell_session_id);
    if !session_seen {
        return unavailable("read_output", output_id, direction, lines, "stale_session");
    }

    let Some(block) = blocks
        .iter()
        .find(|block| block.session_id == parsed.shell_session_id && block.id == parsed.command_id)
    else {
        return unavailable(
            "read_output",
            output_id,
            direction,
            lines,
            "not_in_current_ledger",
        );
    };

    if block.output.terminal_output_ref.is_none() {
        return unavailable("read_output", output_id, direction, lines, "unavailable");
    }

    let direction = match direction {
        "head" => OutputExcerptDirection::Head,
        _ => OutputExcerptDirection::Tail,
    };
    let lines = usize::from(lines).clamp(1, MAX_OUTPUT_LINES);
    let excerpt = bounded_output_excerpt_for_block(block, direction, lines, MAX_OUTPUT_BYTES);
    let direction_label = match direction {
        OutputExcerptDirection::Head => "head",
        OutputExcerptDirection::Tail => "tail",
    };

    if excerpt.text.is_none() {
        return unavailable(
            "read_output",
            output_id,
            direction_label,
            lines as u16,
            "expired",
        );
    }

    if approval_mode == CoshApprovalMode::Recommend || excerpt.confirmation_required {
        return unavailable(
            "read_output",
            output_id,
            direction_label,
            lines as u16,
            "redacted_confirmation_required",
        );
    }

    let status = command_status(block);
    let output_excerpt_status = output_excerpt_status_for_block(block);
    let text = excerpt.text.as_deref().unwrap_or_default();
    let truncation_reason =
        truncation_reason(excerpt.truncated_by_lines, excerpt.truncated_by_bytes);
    let llm_content = format!(
        "ShellEvidenceExcerpt\n\
         action: read_output\n\
         output_id: {output_id}\n\
         command_id: {command_id}\n\
         command: {command}\n\
         cwd: {cwd}\n\
         end_cwd: {end_cwd}\n\
         status: {status}\n\
         exit_code: {exit_code}\n\
         duration_ms: {duration_ms}\n\
         output_bytes: {output_bytes}\n\
         output_excerpt_status: {output_excerpt_status}\n\
         direction: {direction_label}\n\
         lines_requested: {lines}\n\
         provider_visible_byte_cap: {max_output_bytes}\n\
         provider_visible_bytes_truncated: {truncated}\n\
         truncated_by_lines: {truncated_by_lines}\n\
         truncated_by_bytes: {truncated_by_bytes}\n\
         truncation_reason: {truncation_reason}\n\
         excerpt_status: {excerpt_status}\n\
         redaction_status: {redaction_status}\n\
         bounded_output_excerpt:\n{text}",
        command_id = block.id,
        command = redact_provider_command_text(&block.command),
        cwd = block.cwd,
        end_cwd = block.end_cwd,
        exit_code = block.exit_code,
        duration_ms = block.duration_ms,
        output_bytes = block.output.terminal_output_bytes,
        max_output_bytes = MAX_OUTPUT_BYTES,
        truncated = excerpt.truncated,
        truncated_by_lines = excerpt.truncated_by_lines,
        truncated_by_bytes = excerpt.truncated_by_bytes,
        excerpt_status = excerpt.status,
        redaction_status = excerpt.redaction_status,
    );

    ShellEvidenceResult {
        llm_content,
        return_display: excerpt.text.clone(),
        metadata: ShellEvidenceMetadata {
            action: "read_output".to_string(),
            scope: None,
            limit: None,
            next_cursor: None,
            output_id: output_id.to_string(),
            status: excerpt.status.to_string(),
            excerpt_status: "available".to_string(),
            reason: None,
            direction: direction_label.to_string(),
            lines: lines as u16,
            command_count: None,
            provider_visible_byte_cap: MAX_OUTPUT_BYTES,
            truncated: excerpt.truncated,
            truncated_by_lines: excerpt.truncated_by_lines,
            truncated_by_bytes: excerpt.truncated_by_bytes,
            truncation_reason: truncation_reason.to_string(),
            is_error: false,
        },
    }
}

pub(crate) fn command_preview_for_terminal_output_id(
    blocks: &[CommandBlock],
    output_id: &str,
) -> Option<String> {
    let parsed = parse_terminal_output_id(output_id)?;
    blocks
        .iter()
        .find(|block| block.session_id == parsed.shell_session_id && block.id == parsed.command_id)
        .map(|block| redact_provider_command_text(&block.command))
}

pub(crate) fn shell_evidence_read_unavailable_guard(
    blocks: &[CommandBlock],
    approval_mode: CoshApprovalMode,
    output_id: &str,
    direction: &str,
    lines: u16,
) -> Option<ShellEvidenceResult> {
    let Some(parsed) = parse_terminal_output_id(output_id) else {
        return Some(unavailable(
            "read_output",
            output_id,
            direction,
            lines,
            "invalid_output_id",
        ));
    };

    let session_seen = blocks
        .iter()
        .any(|block| block.session_id == parsed.shell_session_id);
    if !session_seen {
        return Some(unavailable(
            "read_output",
            output_id,
            direction,
            lines,
            "stale_session",
        ));
    }

    let Some(block) = blocks
        .iter()
        .find(|block| block.session_id == parsed.shell_session_id && block.id == parsed.command_id)
    else {
        return Some(unavailable(
            "read_output",
            output_id,
            direction,
            lines,
            "not_in_current_ledger",
        ));
    };

    let Some(output_ref) = block.output.terminal_output_ref.as_deref() else {
        return Some(unavailable(
            "read_output",
            output_id,
            direction,
            lines,
            "unavailable",
        ));
    };

    if !Path::new(output_ref).is_file() {
        return Some(unavailable(
            "read_output",
            output_id,
            direction,
            lines,
            "expired",
        ));
    }

    if approval_mode == CoshApprovalMode::Recommend {
        return Some(unavailable(
            "read_output",
            output_id,
            direction,
            lines,
            "redacted_confirmation_required",
        ));
    }

    None
}

pub(crate) fn already_delivered_shell_evidence_result(
    output_id: &str,
    direction: &str,
    lines: u16,
) -> ShellEvidenceResult {
    ShellEvidenceResult {
        llm_content: format!(
            "ShellEvidenceExcerpt\n\
             action: read_output\n\
             output_id: {output_id}\n\
             direction: {direction}\n\
             lines_requested: {lines}\n\
             excerpt_status: already_delivered\n\
             reason: {ALREADY_DELIVERED_RECENT_REASON}"
        ),
        return_display: Some(ALREADY_DELIVERED_RECENT_REASON.to_string()),
        metadata: ShellEvidenceMetadata {
            action: "read_output".to_string(),
            scope: None,
            limit: None,
            next_cursor: None,
            output_id: output_id.to_string(),
            status: "already_delivered".to_string(),
            excerpt_status: "already_delivered".to_string(),
            reason: Some(ALREADY_DELIVERED_RECENT_REASON.to_string()),
            direction: direction.to_string(),
            lines,
            command_count: None,
            provider_visible_byte_cap: MAX_OUTPUT_BYTES,
            truncated: false,
            truncated_by_lines: false,
            truncated_by_bytes: false,
            truncation_reason: "none".to_string(),
            is_error: false,
        },
    }
}

fn unavailable(
    action: &str,
    output_id: &str,
    direction: &str,
    lines: u16,
    reason: &str,
) -> ShellEvidenceResult {
    ShellEvidenceResult {
        llm_content: format!(
            "ShellEvidenceExcerpt\n\
             action: {action}\n\
             output_id: {output_id}\n\
             excerpt_status: unavailable\n\
             reason: {reason}"
        ),
        return_display: Some(reason.to_string()),
        metadata: ShellEvidenceMetadata {
            action: action.to_string(),
            scope: None,
            limit: None,
            next_cursor: None,
            output_id: output_id.to_string(),
            status: "unavailable".to_string(),
            excerpt_status: "unavailable".to_string(),
            reason: Some(reason.to_string()),
            direction: direction.to_string(),
            lines,
            command_count: None,
            provider_visible_byte_cap: MAX_OUTPUT_BYTES,
            truncated: false,
            truncated_by_lines: false,
            truncated_by_bytes: false,
            truncation_reason: "none".to_string(),
            is_error: true,
        },
    }
}

fn list_unavailable(limit: u16, cursor: &str, reason: &str) -> ShellEvidenceResult {
    ShellEvidenceResult {
        llm_content: format!(
            "ShellEvidenceCommandIndex\n\
             scope: current_ledger\n\
             limit: {limit}\n\
             cursor: {cursor}\n\
             excerpt_status: unavailable\n\
             reason: {reason}"
        ),
        return_display: Some(reason.to_string()),
        metadata: ShellEvidenceMetadata {
            action: "list_commands".to_string(),
            scope: Some("current_ledger".to_string()),
            limit: Some(limit),
            next_cursor: None,
            output_id: "<none>".to_string(),
            status: "unavailable".to_string(),
            excerpt_status: "unavailable".to_string(),
            reason: Some(reason.to_string()),
            direction: "<none>".to_string(),
            lines: 0,
            command_count: None,
            provider_visible_byte_cap: MAX_OUTPUT_BYTES,
            truncated: false,
            truncated_by_lines: false,
            truncated_by_bytes: false,
            truncation_reason: "none".to_string(),
            is_error: true,
        },
    }
}

fn truncation_reason(truncated_by_lines: bool, truncated_by_bytes: bool) -> &'static str {
    match (truncated_by_lines, truncated_by_bytes) {
        (true, true) => "line_and_byte_cap",
        (true, false) => "line_cap",
        (false, true) => "byte_cap",
        (false, false) => "none",
    }
}

fn parse_list_cursor(cursor: Option<&str>) -> Option<usize> {
    let Some(cursor) = cursor else {
        return Some(0);
    };
    let offset = cursor.strip_prefix(LIST_CURSOR_PREFIX)?;
    offset.parse::<usize>().ok()
}

fn command_status(block: &CommandBlock) -> &'static str {
    match block.status {
        CommandStatus::Completed => "completed",
        CommandStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommandOrigin, OutputRefs};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn reads_available_shell_output_excerpt() {
        let file = output_file("line1\nline2\nline3\n");
        let block = command_block("raw-session-a", "cmd-1", Some(file.path()));
        let result = read_shell_evidence_output(
            &[block],
            CoshApprovalMode::Trust,
            "terminal-output://raw-session-a/cmd-1",
            "tail",
            2,
        );

        assert!(!result.metadata.is_error);
        assert_eq!(result.metadata.status, "truncated");
        assert_eq!(result.metadata.excerpt_status, "available");
        assert_eq!(result.metadata.provider_visible_byte_cap, 12 * 1024);
        assert!(result.metadata.truncated);
        assert!(result.llm_content.contains("line3"));
        assert!(!result.llm_content.contains("line1"));
    }

    #[test]
    fn already_delivered_shell_evidence_result_is_not_error_and_has_no_body() {
        let result = already_delivered_shell_evidence_result(
            "terminal-output://raw-session-a/cmd-1",
            "tail",
            120,
        );

        assert!(!result.metadata.is_error);
        assert_eq!(result.metadata.excerpt_status, "already_delivered");
        assert_eq!(
            result.metadata.reason.as_deref(),
            Some(ALREADY_DELIVERED_RECENT_REASON)
        );
        assert!(!result.llm_content.contains("bounded_output_excerpt:"));
    }

    #[test]
    fn read_unavailable_guard_rejects_expired_ref_before_recent_suppress() {
        let missing = std::env::temp_dir().join(format!(
            "cosh-shell-expired-guard-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let block = command_block("raw-session-a", "cmd-1", Some(&missing));

        let result = shell_evidence_read_unavailable_guard(
            &[block],
            CoshApprovalMode::Trust,
            "terminal-output://raw-session-a/cmd-1",
            "tail",
            120,
        )
        .expect("expired guard");

        assert!(result.metadata.is_error);
        assert_eq!(result.metadata.reason.as_deref(), Some("expired"));
    }

    #[test]
    fn read_unavailable_guard_rejects_recommend_mode_before_recent_suppress() {
        let file = output_file("safe output\n");
        let block = command_block("raw-session-a", "cmd-1", Some(file.path()));

        let result = shell_evidence_read_unavailable_guard(
            &[block],
            CoshApprovalMode::Recommend,
            "terminal-output://raw-session-a/cmd-1",
            "tail",
            120,
        )
        .expect("recommend guard");

        assert!(result.metadata.is_error);
        assert_eq!(
            result.metadata.reason.as_deref(),
            Some("redacted_confirmation_required")
        );
    }

    #[test]
    fn lists_command_facts_without_output_body() {
        let file = output_file("captured body should not appear\n");
        let block = command_block("raw-session-a", "cmd-1", Some(file.path()));
        let result = list_shell_evidence_commands(&[block], 20, None);

        assert!(!result.metadata.is_error);
        assert_eq!(result.metadata.action, "list_commands");
        assert_eq!(result.metadata.command_count, Some(1));
        assert!(result
            .llm_content
            .contains("output_id: terminal-output://raw-session-a/cmd-1"));
        assert!(result.llm_content.contains("output_available: true"));
        assert!(result.llm_content.contains("output_bytes: 12"));
        assert!(result.llm_content.contains("started_at_ms: 1"));
        assert!(result.llm_content.contains("ended_at_ms: 2"));
        assert!(!result
            .llm_content
            .contains("captured body should not appear"));
    }

    #[test]
    fn lists_command_facts_with_offset_cursor() {
        let blocks = vec![
            command_block("raw-session-a", "cmd-1", None),
            command_block("raw-session-a", "cmd-2", None),
            command_block("raw-session-a", "cmd-3", None),
        ];
        let result = list_shell_evidence_commands(&blocks, 2, None);

        assert!(!result.metadata.is_error);
        assert_eq!(result.metadata.scope.as_deref(), Some("current_ledger"));
        assert_eq!(result.metadata.limit, Some(2));
        assert_eq!(result.metadata.next_cursor.as_deref(), Some("offset:2"));
        assert_eq!(result.metadata.command_count, Some(2));
        assert!(result.llm_content.contains("command_id: cmd-1"));
        assert!(result.llm_content.contains("command_id: cmd-2"));
        assert!(!result.llm_content.contains("command_id: cmd-3"));

        let result = list_shell_evidence_commands(&blocks, 2, Some("offset:2"));
        assert!(!result.metadata.is_error);
        assert_eq!(result.metadata.next_cursor, None);
        assert_eq!(result.metadata.command_count, Some(1));
        assert!(result.llm_content.contains("command_id: cmd-3"));
    }

    #[test]
    fn list_commands_rejects_invalid_cursor() {
        let block = command_block("raw-session-a", "cmd-1", None);
        let result = list_shell_evidence_commands(&[block], 20, Some("cmd-1"));

        assert!(result.metadata.is_error);
        assert_eq!(result.metadata.reason.as_deref(), Some("invalid_cursor"));
        assert!(!result.llm_content.contains("command_id: cmd-1"));
    }

    #[test]
    fn list_commands_marks_missing_output_ref_expired() {
        let missing = std::env::temp_dir().join(format!(
            "cosh-shell-missing-output-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let block = command_block("raw-session-a", "cmd-1", Some(&missing));
        let result = list_shell_evidence_commands(&[block], 20, None);

        assert!(!result.metadata.is_error);
        assert!(result.llm_content.contains("output_available: false"));
        assert!(result.llm_content.contains("output_ref_status: expired"));
        assert!(!result
            .llm_content
            .contains("output_id: terminal-output://raw-session-a/cmd-1"));
    }

    #[test]
    fn stale_session_fails_closed_before_command_id_match() {
        let file = output_file("new output\n");
        let block = command_block("raw-session-new", "cmd-1", Some(file.path()));
        let result = read_shell_evidence_output(
            &[block],
            CoshApprovalMode::Trust,
            "terminal-output://raw-session/cmd-1",
            "tail",
            120,
        );

        assert!(result.metadata.is_error);
        assert_eq!(result.metadata.reason.as_deref(), Some("stale_session"));
        assert!(!result.llm_content.contains("new output"));
    }

    #[test]
    fn missing_command_in_current_session_is_not_in_current_ledger() {
        let file = output_file("output\n");
        let block = command_block("raw-session-a", "cmd-1", Some(file.path()));
        let result = read_shell_evidence_output(
            &[block],
            CoshApprovalMode::Trust,
            "terminal-output://raw-session-a/cmd-2",
            "tail",
            120,
        );

        assert!(result.metadata.is_error);
        assert_eq!(
            result.metadata.reason.as_deref(),
            Some("not_in_current_ledger")
        );
    }

    #[test]
    fn recommend_mode_requires_confirmation() {
        let file = output_file("safe output\n");
        let block = command_block("raw-session-a", "cmd-1", Some(file.path()));
        let result = read_shell_evidence_output(
            &[block],
            CoshApprovalMode::Recommend,
            "terminal-output://raw-session-a/cmd-1",
            "tail",
            120,
        );

        assert!(result.metadata.is_error);
        assert_eq!(
            result.metadata.reason.as_deref(),
            Some("redacted_confirmation_required")
        );
        assert!(!result.llm_content.contains("safe output"));
    }

    #[test]
    fn redacted_excerpt_requires_confirmation() {
        let file = output_file("-----BEGIN PRIVATE KEY-----\nsecret\n");
        let block = command_block("raw-session-a", "cmd-1", Some(file.path()));
        let result = read_shell_evidence_output(
            &[block],
            CoshApprovalMode::Trust,
            "terminal-output://raw-session-a/cmd-1",
            "tail",
            120,
        );

        assert!(result.metadata.is_error);
        assert_eq!(
            result.metadata.reason.as_deref(),
            Some("redacted_confirmation_required")
        );
        assert!(!result.llm_content.contains("BEGIN PRIVATE KEY"));
        assert!(!result.llm_content.contains("secret"));
    }

    #[test]
    fn home_path_redacted_excerpt_is_delivered() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/tester".to_string());
        let file = output_file(&format!(
            "USER PID COMMAND\nme 123 {home}/Applications/Codex.app/Contents/MacOS/Codex\n"
        ));
        let block = command_block("raw-session-a", "cmd-1", Some(file.path()));
        let result = read_shell_evidence_output(
            &[block],
            CoshApprovalMode::Trust,
            "terminal-output://raw-session-a/cmd-1",
            "tail",
            120,
        );

        assert!(!result.metadata.is_error);
        assert_eq!(result.metadata.excerpt_status, "available");
        assert!(!result.llm_content.contains(&home));
        assert!(result.llm_content.contains("~/"));
    }

    struct TempOutputFile {
        path: PathBuf,
    }

    impl TempOutputFile {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempOutputFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn output_file(contents: &str) -> TempOutputFile {
        let path = std::env::temp_dir().join(format!(
            "cosh-shell-evidence-test-{}-{}-{}",
            std::process::id(),
            unique_suffix(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, contents).unwrap();
        TempOutputFile { path }
    }

    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    }

    fn command_block(session_id: &str, id: &str, output_ref: Option<&Path>) -> CommandBlock {
        CommandBlock {
            id: id.to_string(),
            session_id: session_id.to_string(),
            command: "echo output".to_string(),
            origin: CommandOrigin::UserInteractive,
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            duration_ms: 1,
            exit_code: 0,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: output_ref.map(|path| path.to_string_lossy().into_owned()),
                terminal_output_bytes: 12,
            },
            shell_environment_generation: None,
            audit_identity: None,
        }
    }
}
