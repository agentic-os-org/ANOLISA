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
    let program = argv[0].as_str();
    let positional = argv
        .iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .count();
    if positional > 1 {
        return Err(error("file-operand-not-allowed", argv.join(" ")));
    }
    for arg in argv.iter().skip(1) {
        if search_arg_carries_operand_file_or_program(arg, program) {
            return Err(error("option-operand-not-allowed", argv.join(" ")));
        }
    }
    Ok(())
}

fn validate_stdin_filter_stage(argv: &[String], index: usize) -> Result<(), ReadonlyPipelineError> {
    if index == 0 {
        return Err(error("stdin-stage-required", argv.join(" ")));
    }
    let program = argv[0].as_str();
    for arg in argv.iter().skip(1) {
        if !arg.starts_with('-') && !previous_arg_takes_value(argv, arg) {
            return Err(error("file-operand-not-allowed", argv.join(" ")));
        }
        if filter_arg_carries_path_operand(arg, program) {
            return Err(error("option-operand-not-allowed", argv.join(" ")));
        }
    }
    Ok(())
}

/// Long-option names whose operand is a path or a program, per filter
/// program. `sort` writes via `--output`/`--temporary-directory`, executes
/// `--compress-program`, and reads operand files via `--files0-from`/
/// `--random-source`; `wc` shares `--files0-from`. `cut`, `head`, and
/// `uniq` have none of these: cut's `--output-delimiter=` takes a
/// delimiter STRING, not a path, and must stay allowed. These options
/// take their operand inline (`--output=PATH`) or in the next token, so
/// the positional file-operand rejection never sees the path — they are
/// denied in every spelling, including unambiguous GNU abbreviations
/// (`--out=`, `--o=` are valid spellings of `--output` because no other
/// sort option starts with those prefixes).
fn filter_denied_long_options(program: &str) -> &'static [&'static str] {
    match program {
        "sort" => &[
            "output",
            "temporary-directory",
            "compress-program",
            "files0-from",
            "random-source",
        ],
        "wc" => &["files0-from"],
        _ => &[],
    }
}

/// True when `arg` is a long option (`--name` / `--name=value`) whose name
/// is, or unambiguously abbreviates, one of `denied`. A prefix test in
/// this direction never rejects a longer distinct option (`cut`'s
/// `--output-delimiter=` is not a prefix of `output` and stays allowed).
fn long_option_is_denied(arg: &str, denied: &[&str]) -> bool {
    let Some(option) = arg.strip_prefix("--") else {
        return false;
    };
    if option.is_empty() {
        return false;
    }
    let option = option.split('=').next().unwrap_or(option);
    denied.iter().any(|name| name.starts_with(option))
}

/// True when `arg` is a short-option cluster of the stdin-filter family
/// that reaches `o` (sort's output file) or `T` (sort's temporary
/// directory) as an option: `-oPATH`, `-o=PATH`, `-noPATH`, and a bare
/// `-o` whose value comes from the next token all redirect or write
/// outside the pipeline. `k`, `S`, and `t` also take values, so the scan
/// stops once one of them is reached — the rest of the cluster (or the
/// next token) is that option's value, not a further option. For `cut`,
/// `d` (the delimiter) is value-taking as well, so `cut -do -f1` must
/// stop at `d` and treat `o` as its value, while for `sort` `-d` is the
/// dictionary-order flag and `-do` still reaches `o`.
fn filter_short_cluster_has_path_option(arg: &str, program: &str) -> bool {
    let Some(cluster) = arg.strip_prefix('-') else {
        return false;
    };
    if cluster.is_empty() || cluster.starts_with('-') {
        return false;
    }
    for ch in cluster.chars() {
        match ch {
            'o' | 'T' => return true,
            'k' | 'S' | 't' => return false,
            'd' if program == "cut" => return false,
            _ => {}
        }
    }
    false
}

fn filter_arg_carries_path_operand(arg: &str, program: &str) -> bool {
    long_option_is_denied(arg, filter_denied_long_options(program))
        || filter_short_cluster_has_path_option(arg, program)
}

/// True when `arg` is a search-stage (`grep`/`rg`) option whose operand is
/// a pattern file or an executed program: `-f`/`--file` (pattern file —
/// attached `-fFILE`, bare `-f` with the value in the next token, or
/// inside a cluster like `-nf FILE`), rg's `--pre`/`--pre-glob`
/// preprocessor, and rg's `--hostname-bin`. The sibling readonly-rules
/// path already treats `rg --pre=cat` as a mutating form; the pipeline
/// validator must not be the weaker gate.
fn search_arg_carries_operand_file_or_program(arg: &str, program: &str) -> bool {
    if let Some(cluster) = arg.strip_prefix('-') {
        if !cluster.is_empty() && !cluster.starts_with('-') {
            for ch in cluster.chars() {
                match ch {
                    // grep/rg: pattern file; always value-taking, never a flag.
                    'f' => return true,
                    // Value-taking options of grep/rg (`-e`, `-g`, `-m`,
                    // `-A`/`-B`/`-C` context, `-d`/`-D` actions, `-j`,
                    // `-M`, rg's `-t` type): the rest of the cluster (or
                    // the next token) is the value, so no further option
                    // can hide behind them.
                    'e' | 'g' | 'm' | 't' | 'A' | 'B' | 'C' | 'd' | 'D' | 'j' | 'M' => {
                        return false;
                    }
                    _ => {}
                }
            }
            return false;
        }
    }
    let Some(option) = arg.strip_prefix("--") else {
        return false;
    };
    if option.is_empty() {
        return false;
    }
    let option = option.split('=').next().unwrap_or(option);
    // Each entry is the shortest input prefix that GNU resolves
    // unambiguously to the denied option, so abbreviations are denied
    // with the full name while the harmless same-prefix options stay
    // allowed. `--file` matches EXACTLY for both programs: grep has
    // `--files-with-matches`/`--files-without-match` and rg has
    // `--files`, so a prefix would over-block them (and `--fil` is
    // ambiguous in GNU anyway, never reaching the executor); grep's
    // `--exclude`/`--include` take a GLOB pattern (harmless), so the
    // boundary is "exclude-f"/"include-f"; rg's `--pre` is exact
    // because `--pretty` shares the prefix, `pre-` covers `--pre-glob`,
    // `hostname` separates `--hostname-bin` from `--heading`/`--hidden`,
    // and `--ignore-f` separates `--ignore-file` from
    // `--ignore-case`/`--ignore-vcs`.
    let grep_denied_exact = ["file"];
    let grep_denied_prefix = ["exclude-f", "include-f"];
    let rg_denied_exact = ["file", "pre"];
    let rg_denied_prefix = ["pre-", "hostname", "ignore-f"];
    let (exact, prefix): (&[&str], &[&str]) = if program == "rg" {
        (&rg_denied_exact, &rg_denied_prefix)
    } else {
        (&grep_denied_exact, &grep_denied_prefix)
    };
    exact.contains(&option) || prefix.iter().any(|p| option.starts_with(p))
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

    // Options that take a path or program operand inline (`--output=PATH`,
    // `-oPATH`) or in the next token (`-f FILE`) are invisible to the
    // positional file-operand check: sort would write the pipeline output
    // to an arbitrary path, execute `--compress-program`, spill temp files
    // into `--temporary-directory`, or read operand files via
    // `--files0-from`/`--random-source`; grep/rg would read a pattern
    // file via `-f`. GNU accepts unambiguous abbreviations, so `--out=`
    // and `--o=` are spellings of `--output=` and must be rejected too.
    // Verified against GNU coreutils 9.1: every `--output`/`--out`/`--o`/
    // `-oPATH`/`-noPATH` form below writes the named file.
    #[test]
    fn readonly_pipeline_rejects_filter_stage_path_option_operands() {
        for command in [
            "ps aux | sort --output=/tmp/cosh-pwned",
            "ps aux | sort --out=/tmp/cosh-pwned",
            "ps aux | sort --o=/tmp/cosh-pwned",
            "ps aux | sort -o/tmp/cosh-pwned",
            "ps aux | sort -o=/tmp/cosh-pwned",
            "ps aux | sort -no/tmp/cosh-pwned",
            "ps aux | sort -o -n",
            "ps aux | sort -o",
            "ps aux | sort -do /tmp/cosh-pwned",
            "ps aux | sort --compress-program=/usr/bin/id",
            "ps aux | sort --compress-p=/usr/bin/id",
            "ps aux | sort --temporary-directory=/tmp",
            "ps aux | sort --t=/tmp",
            "ps aux | sort --files0-from=-",
            "ps aux | sort --random-source=/dev/zero",
            "ps aux | wc --files0-from=-",
            "ps aux | uniq -o",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_err(),
                "{command} should be rejected"
            );
        }
    }

    // grep/rg read operand files through more pattern-file spellings than
    // `-f`: `--exclude-from=FILE`/`--include-from=FILE` (grep) and
    // `--ignore-file=FILE` (rg) all take a path inline or in the next
    // token — and GNU resolves unambiguous abbreviations, so the denied
    // boundary is the shortest prefix that separates the file option from
    // the harmless same-prefix GLOB options (`--exclude`/`--include` take
    // a pattern; `--ignore-case`/`--ignore-vcs` never take a file).
    // Verified against GNU grep 3.11 / ripgrep 14: every form below reads
    // the named file during the search stage.
    #[test]
    fn readonly_pipeline_rejects_search_stage_pattern_file_spellings() {
        for command in [
            "ps aux | grep --exclude-from=/etc/passwd root",
            "ps aux | grep --exclude-from /etc/passwd root",
            "ps aux | grep --exclude-f=/etc/passwd root",
            "ps aux | grep --exclude-fr /etc/passwd root",
            "ps aux | grep --include-from=/etc/passwd root",
            "ps aux | grep --include-f=/etc/passwd root",
            "ps aux | rg --ignore-file=/etc/passwd root",
            "ps aux | rg --ignore-file /etc/passwd root",
            "ps aux | rg --ignore-f=/etc/passwd root",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_err(),
                "{command} should be rejected"
            );
        }
    }

    // The reject list must stay narrow: ordinary filter flags, value-taking
    // options whose values are not paths (`-k`, `-S`, `-t`, `-d`), and
    // long options that merely start like a denied name of ANOTHER
    // program (`cut`'s `--output-delimiter=`, its `--f=1` spelling of
    // `--fields=1`; grep's `--exclude=`/`--include=` GLOB forms) keep the
    // auto-approve route — the deny tables are per program, and the
    // denied search boundaries start past the harmless same-prefix
    // options.
    #[test]
    fn readonly_pipeline_keeps_ordinary_filter_flags() {
        for command in [
            "ps aux | sort",
            "ps aux | sort -n",
            "ps aux | sort -k2,2 -u",
            "ps aux | sort -t: -k2",
            "ps aux | sort -S4M",
            "ps aux | sort --sort=general-numeric",
            "ps aux | sort -R",
            "ps aux | cut -d= -f1",
            "ps aux | cut -do -f1",
            "ps aux | cut --output-delimiter=, -f1",
            "ps aux | cut --f=1",
            "ps aux | cut --fields=2",
            "ps aux | grep --exclude=systemd root",
            "ps aux | grep --include=sshd root",
            "ps aux | grep --files-with-matches root",
            "ps aux | grep --files-without-match root",
            "ps aux | head -5",
            "ps aux | head -c 4096",
            "ps aux | wc -l",
            "ps aux | uniq -c",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_ok(),
                "{command} should stay allowed"
            );
        }
    }

    // `-f`/`--file` (pattern file) bypass the single-positional allowance:
    // `grep -f /etc/passwd` counts `/etc/passwd` as the pattern operand
    // while grep actually reads it as a pattern source, and the attached
    // form `-fFILE` (accepted by GNU grep) never even reaches the
    // positional count. rg's `--pre` preprocessor and `--hostname-bin`
    // execute a program; the readonly-rules broker path already denies
    // `rg --pre=cat`, and the pipeline gate must not be weaker.
    #[test]
    fn readonly_pipeline_rejects_search_stage_operand_options() {
        for command in [
            "ps aux | grep -f /etc/passwd",
            "ps aux | grep -f/etc/passwd",
            "ps aux | grep -nf /etc/passwd",
            "ps aux | grep -qf /etc/passwd",
            "ps aux | grep -e root -f/etc/passwd",
            "ps aux | rg -f /etc/passwd",
            "ps aux | rg --file /etc/passwd",
            "ps aux | rg --pre=/tmp/preprocess.sh root",
            "ps aux | rg --hostname-bin=/tmp/resolve.sh root",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_err(),
                "{command} should be rejected"
            );
        }
    }

    #[test]
    fn readonly_pipeline_keeps_ordinary_search_flags() {
        for command in [
            "ps aux | grep root",
            "ps aux | grep -i root",
            "ps aux | grep -F root",
            "ps aux | grep -A2 root",
            "ps aux | grep -m5 root",
            "ps aux | grep -dskip root",
            "ps aux | grep --color=auto root",
            "ps aux | rg root",
            "ps aux | rg -i root",
            "ps aux | rg --pretty root",
            "ps aux | rg --heading root",
        ] {
            assert!(
                validate_readonly_pipeline(command).is_ok(),
                "{command} should stay allowed"
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
