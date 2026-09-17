//! Command for sending prompt scans to the daemon.

use std::io::Read as _;
use std::path::PathBuf;

use asc_daemon_protocol::{DaemonRequest, PromptScanParams, PromptScanWarmupParams, method};
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::output::PromptOutputFormat;

/// Environment variable that switches the L2 backend without a flag, so host
/// hooks can all share one backend without each passing `--model`.
const L2_MODEL_ENV: &str = "PROMPT_SCANNER_L2_MODEL";

/// Modes whose pipeline includes the L2 `ml_classifier` layer; an L2 model
/// override is inert in every other mode.
const L2_MODES: [&str; 2] = ["standard", "strict"];

/// Every scan mode the daemon serves; the CLI rejects anything else locally,
/// mirroring the V1 command that never sent an unknown mode over the wire.
const SUPPORTED_MODES: [&str; 4] = ["fast", "standard", "strict", "multi_turn"];

/// Local input failures of the scan-prompt command, reported as execution
/// failures rather than daemon errors.
///
/// Owned here instead of the shared [`crate::InputError`] because every
/// variant and its wording is specific to prompt scanning; the shared enum
/// wraps this one so the binary keeps a single error exit path.
#[derive(Debug, thiserror::Error)]
pub enum ScanPromptInputError {
    /// The selected scan mode is not one of `fast`, `standard`, `strict`,
    /// `multi_turn`.
    #[error("Error: Invalid mode '{0}'. Choose from: fast, standard, strict, multi_turn")]
    InvalidMode(String),
    /// The selected output format is neither `json` nor `text`.
    #[error("Error: Invalid format '{0}'. Choose from: json, text")]
    InvalidFormat(String),
    /// `--text`/`--input` cannot combine with `multi_turn`, which reads its
    /// JSON payload from stdin.
    #[error(
        "Error: --text and --input are not supported with multi_turn mode. \
         Pipe a JSON payload via stdin:\n  \
         echo '{{\"history\":[...],\"current_query\":\"...\",\"assistant_response\":\"...\"}}' | \
         agent-sec-cli scan-prompt --mode multi_turn"
    )]
    MultiTurnTextConflict,
    /// Stdin carried no input at all.
    #[error("Error: No input received from stdin.")]
    StdinEmpty,
    /// The `multi_turn` stdin payload is not valid JSON.
    #[error("Error: Invalid JSON: {0}")]
    InvalidJson(String),
    /// The `multi_turn` payload lacks a `history` list, a `current_query`
    /// string, or an `assistant_response` string.
    #[error(
        "Error: payload must include a 'history' list, a 'current_query' \
         string, and an 'assistant_response' string."
    )]
    InvalidPayload,
    /// The `multi_turn` payload's `current_query` is blank.
    #[error("Error: current_query is empty.")]
    EmptyCurrentQuery,
    /// The `--input` file exists but contains no scannable line.
    #[error("Error: File is empty: {0}")]
    FileEmpty(PathBuf),
    /// The `--input` file does not exist.
    #[error("Error: File not found: {0}")]
    FileNotFound(PathBuf),
    /// Reading stdin or an input file failed.
    #[error("cannot read scan-prompt input: {0}")]
    Read(#[from] std::io::Error),
    /// Serializing the scan request failed.
    #[error("invalid scan-prompt request input: {0}")]
    Json(#[from] serde_json::Error),
}

impl ScanPromptInputError {
    /// Whether the message already reads as a terminal usage error, so the
    /// binary prints it verbatim instead of behind the `agent-sec-cli:`
    /// prefix. Transport and encoding failures do not qualify.
    #[must_use]
    pub const fn is_usage_hint(&self) -> bool {
        matches!(
            self,
            Self::InvalidMode(_)
                | Self::InvalidFormat(_)
                | Self::MultiTurnTextConflict
                | Self::StdinEmpty
                | Self::InvalidJson(_)
                | Self::InvalidPayload
                | Self::EmptyCurrentQuery
                | Self::FileEmpty(_)
                | Self::FileNotFound(_)
        )
    }
}

/// Everything a `scan-prompt` invocation needs before it can run.
#[derive(Debug)]
pub struct PromptScanPlan {
    /// One request per input line or conversation payload; empty means
    /// nothing to scan (a whitespace `--text`), which exits successfully.
    pub requests: Vec<DaemonRequest>,
    /// Diagnostic warnings printed on stderr before the first request.
    pub warnings: Vec<String>,
    /// Selected presentation of each scan result.
    pub format: PromptOutputFormat,
    /// Whether the requests carry a conversation triple.
    pub is_multi_turn: bool,
    /// Whether the requests are readiness probes rather than scans; the
    /// renderer then follows the warmup message contract.
    pub is_warmup: bool,
    /// Normalized mode literal; a warmup's completion message depends on it.
    pub mode: String,
}

/// Subcommand of `scan-prompt` that probes readiness instead of scanning.
#[derive(Debug, Subcommand)]
pub(crate) enum ScanPromptAction {
    /// Check that Ollama can serve the models the selected mode requires.
    ///
    /// fast requires none, so there the check only covers the rule engine.
    /// Availability only: a model Ollama can serve is reported ready without
    /// being loaded into memory.
    Warmup {
        /// Detection mode to check: fast, standard, strict, `multi_turn`.
        #[arg(long, default_value = "standard")]
        mode: String,
        /// L2 backend model to check; overrides `PROMPT_SCANNER_L2_MODEL`.
        #[arg(long)]
        model: Option<String>,
    },
}

/// Scans prompts for injection or jailbreak attempts through `asc-daemon`.
///
/// Input priority is `--text` > `--input <file>` > stdin; `multi_turn` reads a
/// JSON conversation triple from stdin, and `--model` overrides the L2 backend.
/// The `warmup` subcommand probes model readiness without scanning.
#[derive(Debug, Args)]
pub(crate) struct ScanPromptCommand {
    /// Prompt text to scan directly. Takes precedence over --input and stdin.
    #[arg(long, allow_hyphen_values = true)]
    text: Option<String>,
    /// Path to a file containing prompts (one per line). If omitted, reads from stdin.
    #[arg(long)]
    input: Option<PathBuf>,
    /// Detection mode: fast (L1), standard (L1+L2), strict (L1+L2+L3 reserved), `multi_turn` (L4, reads JSON from stdin).
    #[arg(long, default_value = "standard")]
    mode: String,
    /// Output format: 'json' (default) or 'text' (human-readable).
    #[arg(long, default_value = "json")]
    format: String,
    /// Label for the input origin (e.g. `user_input`, `rag`, `tool_output`).
    #[arg(long, default_value = "")]
    source: String,
    /// L2 backend model; overrides `PROMPT_SCANNER_L2_MODEL`.
    #[arg(long)]
    model: Option<String>,
    #[command(subcommand)]
    action: Option<ScanPromptAction>,
}

impl ScanPromptCommand {
    /// Resolves the invocation into requests, warnings, and output format.
    ///
    /// Reading stdin or an input file happens here so the request path stays
    /// synchronous and each scan is submitted with fresh input.
    pub(crate) fn plan(&self) -> Result<PromptScanPlan, ScanPromptInputError> {
        // A warmup sub-command owns its mode and model and skips the scan
        // flow entirely, mirroring the V1 command whose callback returned
        // before the scan parameters were even validated.
        if let Some(ScanPromptAction::Warmup { mode, model }) = &self.action {
            return Self::warmup_plan(mode, model.as_deref());
        }
        let mode = self.mode.to_lowercase();
        if !SUPPORTED_MODES.contains(&mode.as_str()) {
            return Err(ScanPromptInputError::InvalidMode(mode));
        }
        let format = PromptOutputFormat::parse(&self.format)?;
        let model = resolve_l2_model(self.model.as_deref());
        let mut warnings = Vec::new();
        if let Some(warning) = inert_model_warning(&mode, model.as_deref(), self.model.as_deref()) {
            warnings.push(warning);
        }
        let source = (!self.source.is_empty()).then(|| self.source.clone());
        let requests = if mode == "multi_turn" {
            vec![self.multi_turn_request(&mode, source, model)?]
        } else {
            self.single_turn_requests(&mode, source, model)?
        };
        Ok(PromptScanPlan {
            requests,
            warnings,
            format,
            is_multi_turn: mode == "multi_turn",
            is_warmup: false,
            mode,
        })
    }

    /// Resolves the warmup probe into one readiness request.
    ///
    /// The probe reads nothing (no stdin, no files) and its message contract
    /// is fixed text, so `--format` never applies; the plan carries the
    /// normalized mode for the renderer's completion message.
    fn warmup_plan(
        mode: &str,
        model: Option<&str>,
    ) -> Result<PromptScanPlan, ScanPromptInputError> {
        let mode = mode.to_lowercase();
        if !SUPPORTED_MODES.contains(&mode.as_str()) {
            return Err(ScanPromptInputError::InvalidMode(mode));
        }
        let resolved = resolve_l2_model(model);
        let mut warnings = Vec::new();
        if let Some(warning) = inert_model_warning(&mode, resolved.as_deref(), model) {
            warnings.push(warning);
        }
        let request = DaemonRequest {
            trace_context: None,
            compatibility: None,
            method: method::ACTION_PROMPT_SCAN_WARMUP.to_owned(),
            params: serde_json::to_value(PromptScanWarmupParams {
                mode: Some(mode.clone()),
                model: resolved,
            })?,
        };
        Ok(PromptScanPlan {
            requests: vec![request],
            warnings,
            format: PromptOutputFormat::Json,
            is_multi_turn: false,
            is_warmup: true,
            mode,
        })
    }

    /// Builds one request per single-turn input using the input priority:
    /// `--text` (blank means nothing to scan), then `--input`, then stdin.
    fn single_turn_requests(
        &self,
        mode: &str,
        source: Option<String>,
        model: Option<String>,
    ) -> Result<Vec<DaemonRequest>, ScanPromptInputError> {
        if let Some(text) = self
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            return Ok(vec![Self::request(text, mode, source, model, None, None)?]);
        }
        // An explicitly blank `--text` means "nothing to scan" and succeeds
        // silently; falling through covers it because neither the
        // file nor stdin path is consulted when `--text` was given.
        if self.text.is_some() {
            return Ok(Vec::new());
        }
        if let Some(path) = &self.input {
            let file = std::fs::read_to_string(path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    ScanPromptInputError::FileNotFound(path.clone())
                } else {
                    ScanPromptInputError::Read(error)
                }
            })?;
            let lines: Vec<&str> = file
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            if lines.is_empty() {
                return Err(ScanPromptInputError::FileEmpty(path.clone()));
            }
            return lines
                .iter()
                .map(|line| Self::request(line, mode, source.clone(), model.clone(), None, None))
                .collect();
        }
        let raw = read_stdin()?;
        if raw.trim().is_empty() {
            return Err(ScanPromptInputError::StdinEmpty);
        }
        Ok(vec![Self::request(
            raw.trim(),
            mode,
            source,
            model,
            None,
            None,
        )?])
    }

    /// Reads the conversation triple from stdin and builds one L4 request.
    ///
    /// The payload must carry a `history` list, a `current_query` string, and
    /// an `assistant_response` string, and the query must be non-blank.
    fn multi_turn_request(
        &self,
        mode: &str,
        source: Option<String>,
        model: Option<String>,
    ) -> Result<DaemonRequest, ScanPromptInputError> {
        if self.text.is_some() || self.input.is_some() {
            return Err(ScanPromptInputError::MultiTurnTextConflict);
        }
        let raw = read_stdin()?;
        if raw.trim().is_empty() {
            return Err(ScanPromptInputError::StdinEmpty);
        }
        let payload: Value = serde_json::from_str(raw.trim())
            .map_err(|error| ScanPromptInputError::InvalidJson(error.to_string()))?;
        let history = match payload.get("history") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(_) => return Err(ScanPromptInputError::InvalidPayload),
        };
        let current_query = string_field(&payload, "current_query")?;
        let assistant_response = string_field(&payload, "assistant_response")?;
        if current_query.trim().is_empty() {
            return Err(ScanPromptInputError::EmptyCurrentQuery);
        }
        Self::request(
            &current_query,
            mode,
            source,
            model,
            Some(history),
            Some(assistant_response),
        )
    }

    /// Serializes one scan into a daemon request.
    fn request(
        text: &str,
        mode: &str,
        source: Option<String>,
        model: Option<String>,
        history: Option<Vec<Value>>,
        assistant_response: Option<String>,
    ) -> Result<DaemonRequest, ScanPromptInputError> {
        Ok(DaemonRequest {
            trace_context: None,
            compatibility: None,
            method: method::ACTION_PROMPT_SCAN.to_owned(),
            params: serde_json::to_value(PromptScanParams {
                text: text.to_owned(),
                mode: Some(mode.to_owned()),
                source,
                model,
                history,
                assistant_response,
            })?,
        })
    }
}

/// Reads all of stdin, blocking until end of input.
fn read_stdin() -> Result<String, ScanPromptInputError> {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .map_err(ScanPromptInputError::Read)?;
    Ok(raw)
}

/// Extracts a string field, returning an empty string when absent or null and
/// rejecting non-string values.
fn string_field(payload: &Value, field: &str) -> Result<String, ScanPromptInputError> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(ScanPromptInputError::InvalidPayload),
    }
}

/// Resolves the L2 backend override: `--model` > `PROMPT_SCANNER_L2_MODEL` >
/// the scanner's built-in default. A blank value at either layer means "not
/// set" and falls through.
fn resolve_l2_model(cli_model: Option<&str>) -> Option<String> {
    if let Some(model) = cli_model.map(str::trim).filter(|m| !m.is_empty()) {
        return Some(model.to_owned());
    }
    std::env::var(L2_MODEL_ENV).ok().and_then(|value| {
        let trimmed = value.trim().to_owned();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

/// Builds the warning for an L2 model override that is inert in `mode`:
/// fast runs L1 only and `multi_turn` a fixed L4 model, so the override would
/// silently do nothing; surfacing it keeps an operator from mistaking an
/// inert flag for a backend switch. `cli_model` is the raw flag value, which
/// decides whether the warning names `--model` or the environment variable.
fn inert_model_warning(
    mode: &str,
    resolved: Option<&str>,
    cli_model: Option<&str>,
) -> Option<String> {
    let model = resolved?;
    if L2_MODES.contains(&mode) {
        return None;
    }
    let origin = if cli_model
        .map(str::trim)
        .is_some_and(|model| !model.is_empty())
    {
        "--model"
    } else {
        L2_MODEL_ENV
    };
    Some(format!(
        "Warning: {origin} '{model}' is ignored in {mode} mode; \
         it only applies to standard/strict (L2)."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> ScanPromptCommand {
        ScanPromptCommand {
            text: None,
            input: None,
            mode: "standard".to_owned(),
            format: "json".to_owned(),
            source: String::new(),
            model: None,
            action: None,
        }
    }

    #[test]
    fn a_nonempty_prompt_builds_a_prompt_scan_request() {
        let mut command = command();
        command.text = Some("hello there".to_owned());
        command.mode = "fast".to_owned();
        command.source = "cli".to_owned();
        let plan = command.plan().expect("plan builds");
        assert_eq!(plan.requests.len(), 1);
        let request = &plan.requests[0];
        assert_eq!(request.method, method::ACTION_PROMPT_SCAN);
        assert_eq!(request.params["text"], "hello there");
        assert_eq!(request.params["mode"], "fast");
        assert_eq!(request.params["source"], "cli");
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn an_explicitly_blank_text_scans_nothing_successfully() {
        let mut command = command();
        command.text = Some("   ".to_owned());
        let plan = command.plan().expect("blank text is not an error");
        assert!(plan.requests.is_empty());
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn a_missing_source_is_omitted_from_the_request() {
        let mut command = command();
        command.text = Some("hello".to_owned());
        let plan = command.plan().expect("plan builds");
        assert!(plan.requests[0].params.get("source").is_none());
    }

    #[test]
    fn an_unknown_format_is_rejected() {
        let mut command = command();
        command.text = Some("hello".to_owned());
        command.format = "yaml".to_owned();
        let error = command.plan().expect_err("invalid format must fail");
        assert!(
            matches!(error, ScanPromptInputError::InvalidFormat(ref format) if format == "yaml")
        );
    }

    #[test]
    fn an_uppercase_mode_is_normalized() {
        let mut command = command();
        command.text = Some("hello".to_owned());
        command.mode = "FAST".to_owned();
        let plan = command.plan().expect("plan builds");
        assert_eq!(plan.requests[0].params["mode"], "fast");
    }

    #[test]
    fn a_model_override_reaches_the_request() {
        let mut command = command();
        command.text = Some("hello".to_owned());
        command.model = Some("modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF".to_owned());
        let plan = command.plan().expect("plan builds");
        assert_eq!(
            plan.requests[0].params["model"],
            "modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF"
        );
        // standard consumes the override, so no warning is due.
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn a_model_override_in_fast_mode_warns_but_still_scans() {
        let mut command = command();
        command.text = Some("hello".to_owned());
        command.mode = "fast".to_owned();
        command.model = Some("modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF".to_owned());
        let plan = command.plan().expect("plan builds");
        assert_eq!(
            plan.requests[0].params["model"],
            "modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF"
        );
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains("--model"));
        assert!(plan.warnings[0].contains("fast"));
    }

    #[test]
    fn multi_turn_with_text_or_input_is_rejected() {
        for conflict in [Some("hello".to_owned()), None] {
            let mut command = command();
            command.mode = "multi_turn".to_owned();
            if let Some(text) = conflict {
                command.text = Some(text);
            } else {
                command.input = Some(PathBuf::from("prompts.txt"));
            }
            let error = command
                .plan()
                .expect_err("multi_turn conflicts with text input");
            assert!(matches!(error, ScanPromptInputError::MultiTurnTextConflict));
        }
    }

    #[test]
    fn an_unknown_mode_is_rejected() {
        let mut command = command();
        command.text = Some("hello".to_owned());
        command.mode = "turbo".to_owned();
        let error = command.plan().expect_err("unknown mode must fail locally");
        assert!(matches!(error, ScanPromptInputError::InvalidMode(ref mode) if mode == "turbo"));
    }

    #[test]
    fn multi_turn_payload_validation_rejects_malformed_shapes() {
        // These cases read stdin, so they are covered by exercising the
        // helpers directly: the payload shape checks are pure functions of
        // the parsed JSON.
        let payload: Value =
            serde_json::from_str(r#"{"history": "not-a-list", "current_query": "q"}"#).unwrap();
        assert!(string_field(&payload, "current_query").is_ok());
        let history = payload.get("history");
        assert!(!matches!(history, Some(Value::Array(_))));
    }

    fn warmup_command(mode: &str, model: Option<&str>) -> ScanPromptCommand {
        ScanPromptCommand {
            action: Some(ScanPromptAction::Warmup {
                mode: mode.to_owned(),
                model: model.map(str::to_owned),
            }),
            ..command()
        }
    }

    #[test]
    fn a_warmup_builds_a_single_warmup_request() {
        let command = warmup_command("fast", None);
        let plan = command.plan().expect("warmup plan builds");
        assert_eq!(plan.requests.len(), 1);
        let request = &plan.requests[0];
        assert_eq!(request.method, method::ACTION_PROMPT_SCAN_WARMUP);
        assert_eq!(request.params["mode"], "fast");
        assert!(plan.is_warmup);
        assert_eq!(plan.mode, "fast");
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn a_warmup_mode_is_normalized_and_validated() {
        let command = warmup_command("STANDARD", None);
        let plan = command.plan().expect("warmup plan builds");
        assert_eq!(plan.requests[0].params["mode"], "standard");

        let command = warmup_command("turbo", None);
        let error = command
            .plan()
            .expect_err("unknown warmup mode must fail locally");
        assert!(matches!(error, ScanPromptInputError::InvalidMode(ref mode) if mode == "turbo"));
    }

    #[test]
    fn a_warmup_ignores_the_scan_level_options() {
        // The sub-command owns mode and model, mirroring the V1 callback that
        // returned before the scan parameters were validated or consumed.
        let mut command = warmup_command("fast", None);
        command.mode = "standard".to_owned();
        command.format = "yaml".to_owned();
        command.text = Some("hello".to_owned());
        let plan = command.plan().expect("warmup ignores scan options");
        assert_eq!(plan.requests[0].params["mode"], "fast");
        assert!(plan.requests[0].params.get("text").is_none());
    }

    #[test]
    fn a_warmup_in_fast_mode_warns_about_an_inert_model() {
        let command = warmup_command("fast", Some("qwen3-guard"));
        let plan = command.plan().expect("warmup plan builds");
        assert_eq!(plan.requests[0].params["model"], "qwen3-guard");
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains("--model"));
        assert!(plan.warnings[0].contains("fast"));
    }

    #[test]
    fn a_warmup_in_multi_turn_mode_warns_about_an_inert_model() {
        // multi_turn runs a fixed L4 model, so an L2 override is inert there
        // too; the resolved value still reaches the request. The environment
        // variable branch of the resolution cannot be tested here: mutating
        // the process environment requires `unsafe`, which this workspace
        // forbids.
        let command = warmup_command("multi_turn", Some("qwen3-guard"));
        let plan = command.plan().expect("warmup plan builds");
        assert_eq!(plan.requests[0].params["model"], "qwen3-guard");
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains("--model"));
        assert!(plan.warnings[0].contains("multi_turn"));
    }
}
