use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::temp_output::TempOutput;
use super::{is_sensitive_target, strip_ansi};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedDiagnosticConfig {
    pub timeout: Duration,
    pub output_limit_bytes: usize,
}

impl Default for GuardedDiagnosticConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            output_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedDiagnosticPlan {
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedDiagnosticOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedDiagnosticError {
    pub reason: &'static str,
    pub detail: String,
}

pub fn validate_guarded_diagnostic(
    command: &str,
) -> Result<GuardedDiagnosticPlan, GuardedDiagnosticError> {
    let argv = parse_simple(command)?;
    let Some(program) = argv.first().map(String::as_str) else {
        return Err(error("empty-command", "empty diagnostic command"));
    };
    if program.contains('/') || !matches!(program, "df" | "ps" | "top") {
        return Err(error("unsupported-command", program));
    }
    if argv.iter().any(|arg| is_sensitive_target(arg)) {
        return Err(error("sensitive-path", argv.join(" ")));
    }
    Ok(GuardedDiagnosticPlan { argv })
}

pub fn run_guarded_diagnostic(
    command: &str,
    config: &GuardedDiagnosticConfig,
) -> Result<GuardedDiagnosticOutput, GuardedDiagnosticError> {
    let plan = validate_guarded_diagnostic(command)?;
    run_plan(&plan, config)
}

fn run_plan(
    plan: &GuardedDiagnosticPlan,
    config: &GuardedDiagnosticConfig,
) -> Result<GuardedDiagnosticOutput, GuardedDiagnosticError> {
    let mut stdout =
        TempOutput::new().map_err(|err| error("executor-io", format!("create stdout: {err}")))?;
    let mut stderr =
        TempOutput::new().map_err(|err| error("executor-io", format!("create stderr: {err}")))?;
    let mut child = Command::new(&plan.argv[0])
        .args(&plan.argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone().map_err(|err| {
            error("executor-io", format!("create stdout: {err}"))
        })?))
        .stderr(Stdio::from(stderr.try_clone().map_err(|err| {
            error("executor-io", format!("create stderr: {err}"))
        })?))
        .spawn()
        .map_err(|err| error("executor-spawn", format!("{}: {err}", plan.argv[0])))?;

    let deadline = Instant::now() + config.timeout;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error("diagnostic-timeout", plan.argv.join(" ")));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(err) => {
                return Err(error("executor-wait", err.to_string()));
            }
        }
    };

    Ok(GuardedDiagnosticOutput {
        exit_code,
        stdout: read_limited_clean(&mut stdout, config.output_limit_bytes)?,
        stderr: read_limited_clean(&mut stderr, config.output_limit_bytes)?,
    })
}

fn parse_simple(command: &str) -> Result<Vec<String>, GuardedDiagnosticError> {
    if command.trim().is_empty() {
        return Err(error("empty-command", "empty diagnostic command"));
    }
    if command.contains('\0') {
        return Err(error("unsafe-binding", "NUL byte"));
    }

    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote: Option<char> = None;
    for ch in command.chars() {
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
            ';' | '|' | '&' | '>' | '<' | '$' | '`' | '(' | ')' | '{' | '}' | '\n' | '\r'
            | '\\' => {
                return Err(error("unsupported-shell-syntax", ch.to_string()));
            }
            _ => token.push(ch),
        }
    }
    if quote.is_some() {
        return Err(error("parse-failed", "unterminated quote"));
    }
    push_token(&mut tokens, &mut token);
    Ok(tokens)
}

fn push_token(tokens: &mut Vec<String>, token: &mut String) {
    if !token.is_empty() {
        tokens.push(std::mem::take(token));
    }
}

fn read_limited_clean(
    output: &mut TempOutput,
    limit: usize,
) -> Result<String, GuardedDiagnosticError> {
    let bytes = output
        .read_all()
        .map_err(|err| error("executor-io", err.to_string()))?;
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]).to_string();
    if bytes.len() > limit {
        text.push_str("\n<truncated>");
    }
    Ok(strip_ansi(&text))
}

fn error(reason: &'static str, detail: impl Into<String>) -> GuardedDiagnosticError {
    GuardedDiagnosticError {
        reason,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_diagnostic_validates_safe_families_with_any_args() {
        for command in ["df -h /", "ps aux --sort=-%mem", "top -l 1 -n 5"] {
            let plan = validate_guarded_diagnostic(command).expect(command);
            assert!(matches!(plan.argv[0].as_str(), "df" | "ps" | "top"));
        }
    }

    #[test]
    fn guarded_diagnostic_rejects_shell_syntax_and_other_commands() {
        for command in [
            "ps aux | head",
            "rm -rf /tmp/nope",
            "cat .env",
            "df .env.local",
            "df ~/.npmrc",
            "/bin/ps aux",
        ] {
            assert!(
                validate_guarded_diagnostic(command).is_err(),
                "{command} should be rejected"
            );
        }
    }

    #[test]
    fn guarded_diagnostic_runs_without_shell() {
        let output = run_guarded_diagnostic(
            "df -h",
            &GuardedDiagnosticConfig {
                output_limit_bytes: 4096,
                ..GuardedDiagnosticConfig::default()
            },
        )
        .expect("diagnostic output");
        assert_eq!(output.exit_code, Some(0));
        assert!(!output.stdout.trim().is_empty());
    }

    // Diagnostic temp output must never appear at a predictable path in
    // the shared temp dir, and attacker-controlled bytes must never be
    // read back as diagnostic output. The attacker polls the temp dir for
    // this process's historical `cosh-guarded-diagnostic-<pid>-*` files;
    // on sighting one it swaps the file for a symlink to a sentinel
    // victim, which a path-based read-back would follow.
    #[test]
    fn guarded_diagnostic_temp_output_is_not_observable_or_swappable() {
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
            .write_all(b"GUARDED_DIAGNOSTIC_INJECTED")
            .expect("write victim");
        victim_file.flush().expect("flush victim");

        let attacker = SwapAttacker::for_process(
            "cosh-guarded-diagnostic",
            &temp_dir,
            victim_file.path().to_path_buf(),
        );

        let mut injected = false;
        for _ in 0..10 {
            let output = run_guarded_diagnostic(
                "df -h",
                &GuardedDiagnosticConfig {
                    output_limit_bytes: 4096,
                    ..GuardedDiagnosticConfig::default()
                },
            )
            .expect("diagnostic run");
            if output.stdout.contains("GUARDED_DIAGNOSTIC_INJECTED")
                || output.stderr.contains("GUARDED_DIAGNOSTIC_INJECTED")
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
            "attacker-controlled content was read back as diagnostic output"
        );
        assert_eq!(
            sightings, 0,
            "diagnostic temp files must never appear at predictable paths \
             (symlink swaps succeeded: {swaps})"
        );
    }
}
