use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolExecutionStatus {
    Executed,
    TimedOut,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolExecutionResult {
    pub(crate) status: ToolExecutionStatus,
    pub(crate) command: String,
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) reason: String,
}

pub(crate) fn run_shell_tool(command: &str, timeout: Option<Duration>) -> ToolExecutionResult {
    let label = "user-approved Bash tool";
    let child = match Command::new("bash")
        .args(["-lc", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return failed_result(command, format!("failed to start {label}: {err}")),
    };

    wait_for_tool(command, child, label, timeout, "executed through bash -lc")
}

pub(crate) fn run_tokenized_tool(
    command: &str,
    tokens: &[&str],
    timeout: Duration,
) -> ToolExecutionResult {
    let label = "approved read-only tool";
    let Some((program, args)) = tokens.split_first() else {
        return failed_result(command, "empty tokenized command".to_string());
    };
    let child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return failed_result(command, format!("failed to start {label}: {err}")),
    };

    wait_for_tool(
        command,
        child,
        label,
        Some(timeout),
        "executed directly without a shell",
    )
}

fn wait_for_tool(
    command: &str,
    mut child: std::process::Child,
    label: &str,
    timeout: Option<Duration>,
    success_detail: &str,
) -> ToolExecutionResult {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                let _ = child.kill();
                let output = child.wait_with_output();
                let (stdout, stderr) = output
                    .map(|output| {
                        (
                            decode_limited(&output.stdout),
                            decode_limited(&output.stderr),
                        )
                    })
                    .unwrap_or_else(|err| (String::new(), err.to_string()));
                return ToolExecutionResult {
                    status: ToolExecutionStatus::TimedOut,
                    command: command.to_string(),
                    exit_code: None,
                    stdout,
                    stderr,
                    reason: format!("{label} timed out"),
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(err) => {
                return failed_result(command, format!("failed while waiting for {label}: {err}"));
            }
        }
    }

    match child.wait_with_output() {
        Ok(output) => ToolExecutionResult {
            status: ToolExecutionStatus::Executed,
            command: command.to_string(),
            exit_code: output.status.code(),
            stdout: decode_limited(&output.stdout),
            stderr: decode_limited(&output.stderr),
            reason: format!("{label} {success_detail}"),
        },
        Err(err) => failed_result(command, format!("failed to collect {label} output: {err}")),
    }
}

fn failed_result(command: &str, reason: String) -> ToolExecutionResult {
    ToolExecutionResult {
        status: ToolExecutionStatus::Failed,
        command: command.to_string(),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        reason,
    }
}

fn decode_limited(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).to_string();
    if text.len() <= OUTPUT_LIMIT_BYTES {
        return text;
    }
    text.truncate(OUTPUT_LIMIT_BYTES);
    while !text.is_char_boundary(text.len()) {
        text.pop();
    }
    text.push_str("\n[truncated]");
    text
}
