use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::temp_output::TempOutput;
use super::{is_sensitive_target, strip_ansi};

const DEFAULT_STAGE_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;
const DEFAULT_OUTPUT_LIMIT_LINES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadonlyPipelinePlan {
    pub stages: Vec<ReadonlyPipelineStage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadonlyPipelineStage {
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadonlyPipelineConfig {
    pub stage_timeout: Duration,
    pub total_timeout: Duration,
    pub output_limit_bytes: usize,
    pub output_limit_lines: usize,
}

impl Default for ReadonlyPipelineConfig {
    fn default() -> Self {
        Self {
            stage_timeout: DEFAULT_STAGE_TIMEOUT,
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
            output_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
            output_limit_lines: DEFAULT_OUTPUT_LIMIT_LINES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadonlyPipelineOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadonlyPipelineError {
    pub reason: &'static str,
    pub detail: String,
}

pub fn validate_readonly_pipeline(
    command: &str,
) -> Result<ReadonlyPipelinePlan, ReadonlyPipelineError> {
    let stages = parse_pipeline(command)?;
    if stages.len() < 2 {
        return Err(error(
            "not-pipeline",
            "readonly pipeline requires at least two stages",
        ));
    }
    for (index, stage) in stages.iter().enumerate() {
        validate_stage(stage, index)?;
    }
    Ok(ReadonlyPipelinePlan {
        stages: stages
            .into_iter()
            .map(|argv| ReadonlyPipelineStage { argv })
            .collect(),
    })
}

pub fn run_readonly_pipeline(
    command: &str,
    config: &ReadonlyPipelineConfig,
) -> Result<ReadonlyPipelineOutput, ReadonlyPipelineError> {
    let plan = validate_readonly_pipeline(command)?;
    run_plan(&plan, config)
}

fn run_plan(
    plan: &ReadonlyPipelinePlan,
    config: &ReadonlyPipelineConfig,
) -> Result<ReadonlyPipelineOutput, ReadonlyPipelineError> {
    let deadline = Instant::now() + config.total_timeout;
    let mut input: Option<TempOutput> = None;
    let mut final_exit_code = None;
    let mut final_stderr = String::new();

    for stage in &plan.stages {
        if Instant::now() >= deadline {
            return Err(error("pipeline-timeout", "readonly pipeline timed out"));
        }

        let stdout = TempOutput::new()
            .map_err(|err| error("executor-io", format!("create stdout: {err}")))?;
        let mut stderr = TempOutput::new()
            .map_err(|err| error("executor-io", format!("create stderr: {err}")))?;
        let stdin = match input.as_mut() {
            Some(previous) => previous
                .rewind()
                .and_then(|()| previous.try_clone())
                .map(Stdio::from)
                .map_err(|err| error("executor-io", format!("open stdin: {err}")))?,
            None => Stdio::null(),
        };

        let mut command = Command::new(&stage.argv[0]);
        command
            .args(&stage.argv[1..])
            .stdin(stdin)
            .stdout(Stdio::from(stdout.try_clone().map_err(|err| {
                error("executor-io", format!("create stdout: {err}"))
            })?))
            .stderr(Stdio::from(stderr.try_clone().map_err(|err| {
                error("executor-io", format!("create stderr: {err}"))
            })?));

        let mut child = command
            .spawn()
            .map_err(|err| error("executor-spawn", format!("{}: {err}", stage.argv[0])))?;
        let stage_deadline = Instant::now()
            + config
                .stage_timeout
                .min(deadline.saturating_duration_since(Instant::now()));
        final_exit_code =
            wait_child_with_deadline(&mut child, stage_deadline, stage.argv.join(" "))?;

        final_stderr = read_limited_clean(
            &mut stderr,
            config.output_limit_bytes,
            config.output_limit_lines,
        )?;
        input = Some(stdout);
    }

    let stdout = input
        .as_mut()
        .map(|output| {
            read_limited_clean(output, config.output_limit_bytes, config.output_limit_lines)
        })
        .transpose()?
        .unwrap_or_default();
    Ok(ReadonlyPipelineOutput {
        exit_code: final_exit_code,
        stdout,
        stderr: final_stderr,
    })
}

fn parse_pipeline(command: &str) -> Result<Vec<Vec<String>>, ReadonlyPipelineError> {
    if command.trim().is_empty() {
        return Err(error("empty-command", "empty readonly pipeline"));
    }
    if command.contains('\0') {
        return Err(error("unsafe-binding", "NUL byte"));
    }

    let mut stages = Vec::new();
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote: Option<char> = None;
    let chars = command.chars().peekable();

    for ch in chars {
        if let Some(quote_ch) = quote {
            if ch == quote_ch {
                quote = None;
            } else {
                token.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ' ' | '\t' => push_token(&mut tokens, &mut token),
            '|' => {
                push_token(&mut tokens, &mut token);
                if tokens.is_empty() {
                    return Err(error("invalid-pipeline", "empty pipeline stage"));
                }
                stages.push(std::mem::take(&mut tokens));
            }
            ';' | '&' | '>' | '<' | '$' | '`' | '(' | ')' | '{' | '}' | '\n' | '\r' | '\\'
            | '*' | '?' | '[' | ']' => {
                return Err(error("unsupported-shell-syntax", ch.to_string()));
            }
            _ => token.push(ch),
        }
    }
    if quote.is_some() {
        return Err(error("parse-failed", "unterminated quote"));
    }
    push_token(&mut tokens, &mut token);
    if !tokens.is_empty() {
        stages.push(tokens);
    }
    Ok(stages)
}

fn validate_stage(argv: &[String], index: usize) -> Result<(), ReadonlyPipelineError> {
    let Some(program) = argv.first().map(|value| value.as_str()) else {
        return Err(error("empty-stage", "empty stage"));
    };
    if program.contains('/') {
        return Err(error("unsupported-command", program));
    }
    if matches!(program, "awk" | "sed" | "sh" | "bash" | "zsh" | "fish") {
        return Err(error("unsupported-command", program));
    }
    if argv.iter().any(|arg| is_sensitive_target(arg)) {
        return Err(error("sensitive-path", argv.join(" ")));
    }
    match program {
        "df" | "ps" => Ok(()),
        "git" if argv.get(1).is_some_and(|subcommand| subcommand == "status") => Ok(()),
        "git" => Err(error("unsupported-git-subcommand", argv.join(" "))),
        "grep" | "rg" => validate_search_stage(argv, index),
        "head" | "sort" | "uniq" | "cut" | "wc" => validate_stdin_filter_stage(argv, index),
        _ => Err(error("unsupported-command", program)),
    }
}

fn validate_search_stage(argv: &[String], index: usize) -> Result<(), ReadonlyPipelineError> {
    if index == 0 {
        return Err(error("stdin-stage-required", argv.join(" ")));
    }
    let positional = argv
        .iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .count();
    if positional > 1 {
        return Err(error("file-operand-not-allowed", argv.join(" ")));
    }
    Ok(())
}

fn validate_stdin_filter_stage(argv: &[String], index: usize) -> Result<(), ReadonlyPipelineError> {
    if index == 0 {
        return Err(error("stdin-stage-required", argv.join(" ")));
    }
    for arg in argv.iter().skip(1) {
        if !arg.starts_with('-') && !previous_arg_takes_value(argv, arg) {
            return Err(error("file-operand-not-allowed", argv.join(" ")));
        }
    }
    Ok(())
}

fn previous_arg_takes_value(argv: &[String], arg: &str) -> bool {
    argv.windows(2).any(|pair| {
        pair[1] == arg
            && matches!(
                pair[0].as_str(),
                "-n" | "-d" | "-f" | "-k" | "-t" | "-c" | "-m" | "--delimiter" | "--fields"
            )
    })
}

fn push_token(tokens: &mut Vec<String>, token: &mut String) {
    if !token.is_empty() {
        tokens.push(std::mem::take(token));
    }
}

/// Waits for a spawned child, killing it once `stage_deadline` passes.
/// Shared by the readonly pipeline and compound executors.
pub(crate) fn wait_child_with_deadline(
    child: &mut std::process::Child,
    stage_deadline: Instant,
    timeout_detail: impl Into<String>,
) -> Result<Option<i32>, ReadonlyPipelineError> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(exit_code_with_signal(status))),
            Ok(None) if Instant::now() >= stage_deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error("stage-timeout", timeout_detail));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(err) => return Err(error("executor-wait", err.to_string())),
        }
    }
}

/// Normalizes a finished child status to the shell exit-code contract: a
/// process ended by a signal reports `128 + signum` (e.g. SIGTERM → 143),
/// so list connectors (`&&`/`||`) evaluate the step as failed instead of
/// mistaking the missing code for success.
#[cfg(unix)]
fn exit_code_with_signal(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or_default())
}

#[cfg(not(unix))]
fn exit_code_with_signal(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or_default()
}

fn read_limited_clean(
    output: &mut TempOutput,
    byte_limit: usize,
    line_limit: usize,
) -> Result<String, ReadonlyPipelineError> {
    let bytes = output
        .read_all()
        .map_err(|err| error("executor-io", err.to_string()))?;
    Ok(limit_clean_text(&bytes, false, byte_limit, line_limit))
}

/// Canonical bounded-text shaping shared by the file-backed pipeline
/// capture and the pipe-backed compound capture: lossy UTF-8 within the
/// byte budget, ANSI stripped, line budget applied, and a single
/// `<truncated>` marker when either budget (or the caller's own
/// overflow signal) was exceeded.
pub(crate) fn limit_clean_text(
    bytes: &[u8],
    overflowed: bool,
    byte_limit: usize,
    line_limit: usize,
) -> String {
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(byte_limit)]).to_string();
    if overflowed || bytes.len() > byte_limit {
        text.push_str("\n<truncated>");
    }
    let mut text = strip_ansi(&text);
    let lines = text.lines().take(line_limit).collect::<Vec<_>>();
    if lines.len() < text.lines().count() {
        text = lines.join("\n");
        text.push_str("\n<truncated>");
    }
    text
}

pub(crate) fn error(reason: &'static str, detail: impl Into<String>) -> ReadonlyPipelineError {
    ReadonlyPipelineError {
        reason,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_pipeline_validates_diagnostic_pipeline() {
        let plan = validate_readonly_pipeline("ps aux | head -5").expect("valid pipeline");
        assert_eq!(plan.stages.len(), 2);
        assert_eq!(plan.stages[0].argv[0], "ps");
        assert_eq!(plan.stages[1].argv, vec!["head", "-5"]);
    }

    #[test]
    fn readonly_pipeline_rejects_shell_and_file_escape() {
        for command in [
            "ps aux | awk '{print $1}'",
            "ps aux > /tmp/out",
            "ps aux | grep foo /etc/passwd",
            "git log | head -5",
            "cat .env | head",
            "ps aux | grep .env.local",
            "ps aux | grep ~/.npmrc",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_err(),
                "{command} should be rejected"
            );
        }
    }

    #[test]
    fn readonly_pipeline_executor_runs_without_shell() {
        let output = run_readonly_pipeline(
            "ps aux | head -1",
            &ReadonlyPipelineConfig {
                output_limit_bytes: 4096,
                ..ReadonlyPipelineConfig::default()
            },
        )
        .expect("pipeline output");
        assert_eq!(output.exit_code, Some(0));
        assert!(!output.stdout.trim().is_empty());
    }

    #[test]
    fn readonly_pipeline_executor_applies_line_limit() {
        let output = run_readonly_pipeline(
            "ps aux | head -5",
            &ReadonlyPipelineConfig {
                output_limit_lines: 1,
                output_limit_bytes: 4096,
                ..ReadonlyPipelineConfig::default()
            },
        )
        .expect("pipeline output");
        assert!(output.stdout.lines().count() <= 2, "{}", output.stdout);
        assert!(output.stdout.contains("<truncated>"), "{}", output.stdout);
    }

    // Pipeline stage temp output must never appear at a predictable path
    // in the shared temp dir, and attacker-controlled bytes must never be
    // read back as pipeline output. The attacker polls the temp dir for
    // this process's historical `cosh-readonly-pipeline-<pid>-*` files;
    // on sighting one it swaps the file for a symlink to a sentinel
    // victim, which a path-based read-back would follow.
    #[test]
    fn readonly_pipeline_temp_output_is_not_observable_or_swappable() {
        use super::super::temp_output::test_support::SwapAttacker;
        use std::io::Write;
        use std::sync::atomic::Ordering;

        let temp_dir = std::env::temp_dir();
        // Zero sightings only proves something when the temp dir is
        // actually enumerable.
        std::fs::read_dir(&temp_dir).expect("temp dir must be enumerable");
        // NamedTempFile deletes the victim on drop, even when an assertion
        // panics mid-test.
        let mut victim_file = tempfile::NamedTempFile::new().expect("victim file");
        victim_file
            .write_all(b"READONLY_PIPELINE_INJECTED")
            .expect("write victim");
        victim_file.flush().expect("flush victim");

        let attacker = SwapAttacker::for_process(
            "cosh-readonly-pipeline",
            &temp_dir,
            victim_file.path().to_path_buf(),
        );

        let mut injected = false;
        for _ in 0..10 {
            let output = run_readonly_pipeline(
                "df -h | wc -l",
                &ReadonlyPipelineConfig {
                    output_limit_bytes: 4096,
                    ..ReadonlyPipelineConfig::default()
                },
            )
            .expect("pipeline run");
            if output.stdout.contains("READONLY_PIPELINE_INJECTED")
                || output.stderr.contains("READONLY_PIPELINE_INJECTED")
            {
                injected = true;
            }
        }
        let enum_errors = attacker.enum_errors.load(Ordering::Relaxed);
        let sightings = attacker.sightings.load(Ordering::Relaxed);
        let swaps = attacker.swaps.load(Ordering::Relaxed);
        attacker.finish();

        assert_eq!(enum_errors, 0, "temp dir enumeration must not fail");
        assert!(
            !injected,
            "attacker-controlled content was read back as pipeline output"
        );
        assert_eq!(
            sightings, 0,
            "pipeline temp files must never appear at predictable paths \
             (symlink swaps succeeded: {swaps})"
        );
    }
}
