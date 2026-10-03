use std::borrow::Cow;

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use crate::process::{output_with_timeout, OutputError, MAX_PIPE_OUTPUT_BYTES};

use super::{Tool, ToolContext, ToolKind, ToolResult};

pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its output. Use this to run commands, scripts, and system utilities."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds (default: 30000)"
                }
            },
            "required": ["command"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ShellExec
    }

    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or("missing 'command' parameter")?;

        let timeout_ms = params
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(30_000);

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command).current_dir(&ctx.cwd);

        // Deadline-bounded execution with process-group cleanup: a bare
        // tokio::time::timeout would leak the child's process tree. Output
        // collection is size-capped so a runaway command cannot exhaust
        // memory before the deadline (issue #2841).
        let result =
            output_with_timeout(cmd, None, std::time::Duration::from_millis(timeout_ms)).await;

        match result {
            Ok(output) => {
                let truncated = output.stdout_truncated || output.stderr_truncated;
                // Each pipe is already capped at MAX_PIPE_OUTPUT_BYTES by the
                // capture layer; the context budget is applied uniformly by
                // the engine-level truncator after this tool returns. Bound the
                // decoded size too, so binary output cannot reintroduce the
                // exhaustion the byte cap just removed.
                let stdout = decode_within_limit(&output.stdout, MAX_PIPE_OUTPUT_BYTES);
                let stderr = decode_within_limit(&output.stderr, MAX_PIPE_OUTPUT_BYTES);
                let exit_code = output.status.code().unwrap_or(-1);

                // Sized up front: growing into a full pipe's worth of text
                // would otherwise keep the old and new buffers alive across
                // each doubling.
                let mut result_text = String::with_capacity(stdout.len() + stderr.len() + 512);
                if truncated {
                    // Placed first so the engine-level truncator (which keeps
                    // the head) cannot drop the explanation.
                    let pipe_name = if output.stdout_truncated && output.stderr_truncated {
                        "shell stdout and stderr"
                    } else if output.stdout_truncated {
                        "shell stdout"
                    } else {
                        "shell stderr"
                    };
                    result_text.push_str(&format!(
                        "[{pipe_name} exceeded the {MAX_PIPE_OUTPUT_BYTES}-byte \
                         limit; the process group was killed and only the \
                         beginning of the output is shown. Rerun the command \
                         with a narrower filter (for example `... | tail -c \
                         20000`) or redirect output to a file and read it in \
                         chunks.]\n\n"
                    ));
                }
                if !stdout.is_empty() {
                    result_text.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result_text.is_empty() {
                        result_text.push('\n');
                    }
                    result_text.push_str("[stderr]\n");
                    result_text.push_str(&stderr);
                }
                if result_text.is_empty() {
                    result_text = format!("(exit code: {exit_code})");
                }

                Ok(ToolResult {
                    output: result_text,
                    is_error: !output.status.success() || truncated,
                })
            }
            Err(OutputError::Timeout) => Ok(ToolResult::error(format!(
                "Command timed out after {timeout_ms}ms"
            ))),
            Err(e) => Err(format!("Failed to execute command: {e}")),
        }
    }
}

/// Decodes captured output as UTF-8 with lossy replacement, keeping the decoded
/// text within `max_bytes`.
///
/// Valid UTF-8 is borrowed, so the common case copies nothing and keeps every
/// captured byte. Invalid bytes each expand into a three-byte U+FFFD, so a pipe
/// full of binary data would otherwise triple while being decoded and undo the
/// capture-side cap; that path decodes only the prefix that fits the limit even
/// at the worst-case expansion. Bytes dropped there are orders of magnitude
/// past what the engine-level truncator keeps.
fn decode_within_limit(bytes: &[u8], max_bytes: usize) -> Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => {
            // U+FFFD is three bytes wide, so a third of the budget cannot
            // overflow it no matter how the input decodes. Splitting a
            // multibyte sequence here is harmless: the fragment becomes one
            // more replacement character.
            let head = &bytes[..bytes.len().min(max_bytes / 3)];
            String::from_utf8_lossy(head)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_ctx() -> ToolContext {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
        ToolContext::new(root.clone(), "test".to_string(), root)
    }

    #[tokio::test]
    async fn shell_echo() {
        let tool = ShellTool;
        let result = tool
            .invoke(serde_json::json!({"command": "echo hello"}), &test_ctx())
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn shell_exit_code() {
        let tool = ShellTool;
        let result = tool
            .invoke(serde_json::json!({"command": "false"}), &test_ctx())
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn shell_stderr() {
        let tool = ShellTool;
        let result = tool
            .invoke(serde_json::json!({"command": "echo err >&2"}), &test_ctx())
            .await
            .unwrap();
        assert!(result.output.contains("err"));
        assert!(result.output.contains("[stderr]"));
    }

    #[tokio::test]
    async fn shell_timeout() {
        let tool = ShellTool;
        let result = tool
            .invoke(
                serde_json::json!({"command": "sleep 60", "timeout_ms": 200}),
                &test_ctx(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("timed out"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_timeout_kills_process_group() {
        use crate::process::test_support::*;

        let _fixture_guard = exclusive_process_tree_test().await;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let pid_file = dir.path().join("pids");
        let command = leak_script(&marker, &pid_file);

        let tool = ShellTool;
        let result = tool
            .invoke(
                serde_json::json!({"command": command, "timeout_ms": 300}),
                &test_ctx(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("timed out"));

        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());
        for pid in &pids {
            assert_process_gone(*pid);
        }
        release_marker_probe(&marker);
        assert!(!marker.exists(), "grandchild survived the tool timeout");
    }

    #[tokio::test]
    async fn shell_output_over_limit_returns_notice_and_partial_output() {
        let tool = ShellTool;
        let result = tool
            .invoke(
                serde_json::json!({
                    "command": format!("head -c {} /dev/zero", MAX_PIPE_OUTPUT_BYTES + 1),
                    "timeout_ms": 30_000
                }),
                &test_ctx(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        let head: String = result.output.chars().take(200).collect();
        assert!(
            result.output.contains("exceeded the 33554432-byte limit"),
            "expected truncation notice, got: {head}"
        );
        assert!(result.output.contains("tail -c"));
        // The tool hands the engine the full collected head (each pipe is
        // capped at MAX_PIPE_OUTPUT_BYTES) and lets the engine-level
        // truncator apply the context budget uniformly.
        assert!(result.output.len() >= MAX_PIPE_OUTPUT_BYTES);
    }

    #[tokio::test]
    async fn shell_binary_output_over_limit_does_not_inflate() {
        // Companion to the /dev/zero case above, which is valid UTF-8 and so
        // never exercises lossy replacement. Random bytes make the decoder
        // substitute U+FFFD, the path that used to triple the capture.
        let tool = ShellTool;
        let result = tool
            .invoke(
                serde_json::json!({
                    "command": format!("head -c {} /dev/urandom", MAX_PIPE_OUTPUT_BYTES + 1),
                    "timeout_ms": 30_000
                }),
                &test_ctx(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("exceeded the 33554432-byte limit"));
        assert!(
            result.output.len() <= MAX_PIPE_OUTPUT_BYTES + 1024,
            "decoded output grew to {} bytes from a {MAX_PIPE_OUTPUT_BYTES}-byte cap",
            result.output.len()
        );
    }

    #[test]
    fn valid_utf8_is_borrowed_and_kept_whole() {
        let decoded = decode_within_limit(b"plain output\n", MAX_PIPE_OUTPUT_BYTES);
        assert!(matches!(decoded, Cow::Borrowed(_)));
        assert_eq!(decoded, "plain output\n");
    }

    #[test]
    fn invalid_bytes_cannot_expand_past_the_limit() {
        // 0xff is never a valid lead byte, so every byte would become a
        // three-byte U+FFFD if the decode were unbounded.
        let decoded = decode_within_limit(&[0xff; 300], 99);
        assert_eq!(decoded.len(), 99);
        assert!(decoded.chars().all(|c| c == '\u{fffd}'));
    }

    #[tokio::test]
    async fn shell_missing_command() {
        let tool = ShellTool;
        let result = tool.invoke(serde_json::json!({}), &test_ctx()).await;
        assert!(result.is_err());
    }
}
