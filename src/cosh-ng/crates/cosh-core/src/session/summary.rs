//! Bounded session summaries and picker-safe prompt previews.

use crate::provider::{MessageContent, MessageContentBlock};

use super::{PersistedSession, SessionHealth, SessionSummary};

pub(super) const MAX_PROMPT_PREVIEW_CHARS: usize = 160;
/// Maximum UTF-8 bytes retained from untrusted model metadata.
pub(super) const MAX_SUMMARY_MODEL_BYTES: usize = 256;
/// Maximum UTF-8 bytes retained from untrusted workspace metadata.
pub(super) const MAX_SUMMARY_WORKSPACE_BYTES: usize = 4096;

/// First line of the cosh-shell natural-language prompt envelope.
const SHELL_ENVELOPE_PREFIX: &str =
    "Handle this natural-language shell prompt request for a Shell-first assistant.\n";
/// Envelope marker that precedes the raw text the user typed.
const SHELL_ENVELOPE_INPUT_MARKER: &str = "\nuser_input: ";
/// Envelope section that always follows the raw user input.
const SHELL_ENVELOPE_RUNTIME_MARKER: &str = "\n\nruntime_frame:\n";
/// Trailing envelope section appended by the cosh-shell adapter.
const SHELL_ENVELOPE_CONTRACT_MARKER: &str = "\n\ncosh-shell Agent contract:\n";

pub(super) fn summary_from_session(
    session: &PersistedSession,
    health: SessionHealth,
) -> SessionSummary {
    let first_prompt = session
        .messages
        .iter()
        .filter(|message| message.role == "user")
        .find_map(|message| bounded_message_preview(&message.content));
    SessionSummary {
        session_id: session.session_id.clone(),
        workspace_scope: bounded_summary_text(
            &session.workspace_scope,
            MAX_SUMMARY_WORKSPACE_BYTES,
        ),
        created_at_ms: session.created_at_ms,
        updated_at_ms: session.updated_at_ms,
        model: (!session.model.is_empty())
            .then(|| bounded_summary_text(&session.model, MAX_SUMMARY_MODEL_BYTES)),
        message_count: session.messages.len(),
        first_prompt,
        schema_version: Some(session.schema_version),
        health,
    }
}

/// Truncates summary metadata without splitting a UTF-8 code point.
///
/// C0/C1 control characters are removed first so untrusted persisted
/// metadata can never inject terminal control sequences into a picker.
pub(crate) fn bounded_summary_text(value: &str, max_bytes: usize) -> String {
    let sanitized: String = value.chars().filter(|ch| !ch.is_control()).collect();
    if sanitized.len() <= max_bytes {
        return sanitized;
    }
    const ELLIPSIS: &str = "…";
    if max_bytes < ELLIPSIS.len() {
        return String::new();
    }
    let mut boundary = max_bytes - ELLIPSIS.len();
    while !sanitized.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}{}", &sanitized[..boundary], ELLIPSIS)
}

/// Projects a persisted user message onto the text worth showing in a picker.
///
/// cosh-shell hands cosh-core one `user` message whose fixed instruction
/// envelope wraps the text the user typed, so previewing the message verbatim
/// labels every session with the same envelope prefix. Text markers cannot
/// delimit arbitrary user input unambiguously, so this is a bounded
/// compatibility heuristic, not a parse: every anchor must be present in order,
/// otherwise the message is previewed as-is and plain cosh-core prompts keep
/// their behavior.
fn preview_source(text: &str) -> &str {
    shell_envelope_input(text).unwrap_or(text)
}

/// Recovers the raw input from a cosh-shell prompt envelope.
///
/// Both trailing sections are appended after the input, so their last
/// occurrences bound it: an input that quotes a marker keeps its full text, and
/// only untrusted evidence repeating the runtime marker can push the boundary
/// late — which pads the preview instead of losing what the user typed.
fn shell_envelope_input(text: &str) -> Option<&str> {
    if !text.starts_with(SHELL_ENVELOPE_PREFIX) {
        return None;
    }
    let contract_start = text.rfind(SHELL_ENVELOPE_CONTRACT_MARKER)?;
    let input_start = text
        .find(SHELL_ENVELOPE_INPUT_MARKER)
        .map(|offset| offset + SHELL_ENVELOPE_INPUT_MARKER.len())
        .filter(|start| *start <= contract_start)?;
    let input_end = text[input_start..contract_start]
        .rfind(SHELL_ENVELOPE_RUNTIME_MARKER)
        .map(|offset| input_start + offset)?;
    let input = text[input_start..input_end].trim();
    (!input.is_empty()).then_some(input)
}

fn bounded_message_preview(content: &MessageContent) -> Option<String> {
    let mut preview = String::new();
    let mut char_count = 0;
    match content {
        MessageContent::Text(text) => {
            append_preview_fragment(&mut preview, &mut char_count, preview_source(text));
        }
        MessageContent::Blocks(blocks) => {
            for block in blocks {
                let text = match block {
                    MessageContentBlock::Text { text }
                    | MessageContentBlock::ToolResult { content: text, .. } => text,
                };
                if append_preview_fragment(&mut preview, &mut char_count, preview_source(text)) {
                    break;
                }
            }
        }
    }
    if preview.is_empty() {
        return None;
    }
    if char_count > MAX_PROMPT_PREVIEW_CHARS {
        preview.pop();
        preview.pop();
        preview.push('…');
    }
    Some(preview)
}

fn append_preview_fragment(preview: &mut String, char_count: &mut usize, fragment: &str) -> bool {
    for word in fragment.split_whitespace() {
        if !preview.is_empty() {
            preview.push(' ');
            *char_count += 1;
            if *char_count > MAX_PROMPT_PREVIEW_CHARS {
                return true;
            }
        }
        // Control characters are dropped so persisted prompts cannot smuggle
        // terminal control bytes (BEL, BS, C1 CSI) into picker rows.
        for character in word.chars().filter(|character| !character.is_control()) {
            preview.push(character);
            *char_count += 1;
            if *char_count > MAX_PROMPT_PREVIEW_CHARS {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the envelope cosh-shell sends for a natural-language prompt.
    fn shell_envelope(input: &str) -> String {
        format!(
            "Handle this natural-language shell prompt request for a Shell-first assistant.\n\
             Decide based on user intent:\n\
             history_access: Recent shell history is not included by default.\n\
             Do not mention Claude Code, plan mode, implementation status, or internal workflow.\n\n\
             user_input: {input}\n\n\nruntime_frame:\ncwd: /root\n\ncosh-shell Agent contract:\n\
             - User modes: recommend and agent.\n\
             - Keep provider-specific names out of visible responses unless already shown by cosh-shell."
        )
    }

    fn preview(text: &str) -> Option<String> {
        bounded_message_preview(&MessageContent::Text(text.to_string()))
    }

    #[test]
    fn shell_envelope_preview_shows_raw_user_input() {
        let envelope = shell_envelope("查看当前目录下的文件");

        assert_eq!(preview(&envelope).as_deref(), Some("查看当前目录下的文件"));
    }

    #[test]
    fn shell_envelope_preview_stays_single_line_and_bounded() {
        let envelope = shell_envelope(&format!(
            "检查\u{7}nginx\t服务 {}",
            "界".repeat(MAX_PROMPT_PREVIEW_CHARS + 50)
        ));

        let preview = preview(&envelope).expect("bounded preview");

        assert!(preview.starts_with("检查nginx 服务 "));
        assert!(!preview.contains('\n') && !preview.contains('\t'));
        assert!(!preview.chars().any(char::is_control));
        assert_eq!(preview.chars().count(), MAX_PROMPT_PREVIEW_CHARS);
        assert!(preview.ends_with('…'));
        assert!(!preview.contains("runtime_frame"));
    }

    #[test]
    fn shell_envelope_preview_keeps_input_that_quotes_envelope_markers() {
        let envelope = shell_envelope(
            "user_input: still mine\n\ncosh-shell Agent contract: quoted by the user",
        );

        assert_eq!(
            preview(&envelope).as_deref(),
            Some("user_input: still mine cosh-shell Agent contract: quoted by the user")
        );
    }

    #[test]
    fn shell_envelope_preview_keeps_input_that_quotes_the_runtime_marker() {
        let envelope = shell_envelope("比较下面两段：\n\nruntime_frame:\ncwd: /tmp\n后一段更旧");

        assert_eq!(
            preview(&envelope).as_deref(),
            Some("比较下面两段： runtime_frame: cwd: /tmp 后一段更旧")
        );
    }

    #[test]
    fn envelope_without_runtime_marker_preview_falls_back_to_verbatim_text() {
        let without_runtime_frame =
            "Handle this natural-language shell prompt request for a Shell-first assistant.\n\
             Decide based on user intent:\n\n\
             user_input: 查看当前目录下的文件\n\ncosh-shell Agent contract:\n\
             - User modes: recommend and agent.";

        let preview = preview(without_runtime_frame).expect("verbatim preview");

        assert!(preview.starts_with("Handle this natural-language shell prompt request"));
    }

    #[test]
    fn plain_core_prompt_preview_is_unchanged() {
        assert_eq!(
            preview("查看当前目录下的文件").as_deref(),
            Some("查看当前目录下的文件")
        );
        assert_eq!(
            preview("user_input: not an envelope").as_deref(),
            Some("user_input: not an envelope")
        );
    }

    #[test]
    fn partial_envelope_preview_falls_back_to_verbatim_text() {
        // No trailing contract section, so the text is not a recognized envelope.
        let truncated =
            "Handle this natural-language shell prompt request for a Shell-first assistant.\n\
             Decide based on user intent:\n\nuser_input: 查看当前目录下的文件\n";

        let preview = preview(truncated).expect("verbatim preview");

        assert!(preview.starts_with("Handle this natural-language shell prompt request"));
    }

    #[test]
    fn shell_envelope_preview_applies_to_content_blocks() {
        let content = MessageContent::Blocks(vec![MessageContentBlock::Text {
            text: shell_envelope("帮我检查 nginx 服务状态"),
        }]);

        assert_eq!(
            bounded_message_preview(&content).as_deref(),
            Some("帮我检查 nginx 服务状态")
        );
    }
}
