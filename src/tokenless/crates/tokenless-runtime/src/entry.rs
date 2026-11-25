//! Operation-specific lifecycle services and protocol transport dispatch.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tokenless_ccr::{RecoveryMethod, StashStore, extract_hash, is_valid_hash, recovery_hashes};
use tokenless_compressors::JsonCompressionConfig;
use tokenless_protocol::{
    AppliedOperation, Attribution, BeforeModelRequest, BeforeModelResponse, ContentOrigin,
    Disposition, Operation, OutputOptimization, PostToolReplacementCapability, PostToolRequest,
    PostToolResponse, PreToolAction, PreToolRequest, PreToolResponse, Recoverability, Request,
    RequestEnvelope, Response, ResponseEnvelope, ResultKind, RetrieveRequest, RetrieveResponse,
    TOKENIZER_ID, ToolResultStatus, estimate_tokens,
};
use tokenless_schema::SchemaCompressor;
use tokenless_stats::rtk_outcome::OutcomeStore;
use tokenless_stats::{OperationType, StatsRecorder};

use crate::post_tool::{self, PostToolPipeline, PostToolPipelineConfig};
use crate::{
    MAX_INPUT_BYTES, MIN_TOON_CHARS, RESPONSE_PIPELINE_TIMEOUT, RuntimeError,
    finish_schema_compression, taxonomy,
};

const MIN_RESPONSE_CHARS: usize = 200;

mod native_output;

/// Per-call behavior resolved by a transport frontend.
#[derive(Debug, Clone)]
pub struct EntryOptions {
    /// Whether accepted candidates replace the original content.
    pub compression_enabled: bool,
    /// Whether API search records and non-file path lists may share paths.
    pub search_path_sharing_enabled: bool,
    /// Whether command-output Git diffs may omit context with original recovery. Enabled by default.
    pub diff_compression_enabled: bool,
    /// Whether complete HTML documents, and HTML inside JSON string values, are rendered as
    /// Markdown with original recovery. Enabled by default.
    pub html_extraction_enabled: bool,
    /// Whether browser accessibility snapshots are re-encoded as record tables
    /// with original recovery. Enabled by default.
    pub browser_snapshot_enabled: bool,
    /// Whether structured regions inside mixed tool outputs may be compressed
    /// individually. Disabled by default.
    pub mixed_content_enabled: bool,
    /// Whether lifecycle operations may use the attached stash.
    pub stash_enabled: bool,
    /// Tokenless executable whose `rtk` subcommand runs rewritten PreTool commands.
    pub rtk_launcher: Option<PathBuf>,
    /// Resolved state directory propagated to RTK commands.
    pub rtk_data_dir: Option<PathBuf>,
}

/// Runtime-only facts used for statistics recording.
pub struct EntryStats {
    pub(crate) operation: OperationType,
    pub(crate) input: String,
    pub(crate) measured_output: String,
    pub(crate) disposition: Disposition,
    pub(crate) content_type: Option<String>,
    pub(crate) content_origin: Option<String>,
    pub(crate) applied_operations: Vec<AppliedOperation>,
    pub(crate) recoverability: Recoverability,
    pub(crate) unrecoverable_truncations: Option<usize>,
}

/// One protocol response plus compression artifact facts.
pub struct EntryOutcome {
    /// Response to emit across the transport boundary.
    pub response: ResponseEnvelope,
    /// Compression measurement, absent for PreTool and Retrieve.
    pub stats: Option<EntryStats>,
    /// Successful stash writes still referenced by the response.
    pub stash_writes: Option<usize>,
    /// Failed stash operations.
    pub stash_errors: Option<usize>,
    /// Live stash entry count after the operation.
    pub stash_size: Option<usize>,
    /// Stash keys attributed to this lifecycle result.
    pub artifact_keys: Vec<String>,
}

/// Dispatches a v2 transport request to one typed lifecycle service.
///
/// PreTool runs RTK in the calling process, and RTK assumes that process is
/// short-lived: it caches trusted filters and project files for the process
/// lifetime and can wait on `git` without a deadline. This function sets no
/// deadline; `tokenless compress` ends the rewrite and the processes RTK
/// started after 5 seconds. A long-lived host runs PreTool through
/// `tokenless compress` instead, as the Python SDK does.
///
/// # Errors
///
/// Returns [`RuntimeError`] when the selected lifecycle operation fails.
pub fn dispatch_with_store(
    envelope: &RequestEnvelope,
    options: &EntryOptions,
    stash_store: Option<&Arc<dyn StashStore>>,
    stats_recorder: Option<&StatsRecorder>,
) -> Result<EntryOutcome, RuntimeError> {
    let (response, stats, stash_writes, stash_errors, stash_size, artifact_keys) =
        match &envelope.request {
            Request::BeforeModel(request) => {
                let outcome = before_model_with_store(request, options, stash_store)?;
                (
                    Response::BeforeModel(outcome.response),
                    Some(outcome.stats),
                    outcome.stash_writes,
                    outcome.stash_errors,
                    outcome.stash_size,
                    outcome.artifact_keys,
                )
            }
            Request::PreTool(request) => (
                Response::PreTool(pre_tool_with_rewriter(
                    request,
                    &envelope.attribution,
                    options.rtk_launcher.as_deref(),
                    options.rtk_data_dir.as_deref(),
                    |command| rtk_rewrite(options.rtk_launcher.as_deref(), command),
                )?),
                None,
                None,
                None,
                None,
                Vec::new(),
            ),
            Request::PostTool(request) => {
                let outcome =
                    post_tool_with_outcomes(request, options, stash_store, &envelope.attribution)?;
                let artifact_keys = outcome.response.stash_keys.clone();
                (
                    Response::PostTool(outcome.response),
                    Some(outcome.stats),
                    outcome.stash_writes,
                    outcome.stash_errors,
                    outcome.stash_size,
                    artifact_keys,
                )
            }
            Request::Retrieve(request) => (
                Response::Retrieve(retrieve_authorized_with_store(
                    request,
                    stash_store,
                    stats_recorder,
                    &envelope.attribution,
                    "cli",
                )?),
                None,
                None,
                None,
                stash_store.map(|store| store.len()),
                Vec::new(),
            ),
        };
    Ok(EntryOutcome {
        response: ResponseEnvelope {
            attribution: envelope.attribution.clone(),
            response,
        },
        stats,
        stash_writes,
        stash_errors,
        stash_size,
        artifact_keys,
    })
}

pub(crate) struct BeforeModelOutcome {
    pub(crate) response: BeforeModelResponse,
    pub(crate) stats: EntryStats,
    pub(crate) stash_writes: Option<usize>,
    pub(crate) stash_errors: Option<usize>,
    pub(crate) stash_size: Option<usize>,
    pub(crate) artifact_keys: Vec<String>,
}

pub(crate) fn before_model_with_store(
    request: &BeforeModelRequest,
    options: &EntryOptions,
    stash_store: Option<&Arc<dyn StashStore>>,
) -> Result<BeforeModelOutcome, RuntimeError> {
    let input = serde_json::to_string(&request.tools).map_err(RuntimeError::Serialize)?;
    if input.len() > MAX_INPUT_BYTES {
        return Err(RuntimeError::InputTooLarge {
            limit_mib: MAX_INPUT_BYTES / (1024 * 1024),
        });
    }
    let attached_store = if request.capabilities.replace_tools
        && options.compression_enabled
        && options.stash_enabled
        && matches!(request.capabilities.recovery, RecoveryMethod::Tool { .. })
    {
        stash_store
    } else {
        None
    };
    let mut compressor =
        SchemaCompressor::new().with_recovery(request.capabilities.recovery.clone());
    if let Some(store) = attached_store {
        compressor = compressor.with_stash_store(Arc::clone(store));
    }
    let mut pending_keys = Vec::new();
    let compression = if request.capabilities.replace_tools {
        let candidate = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| compressor.compress(tool))
                .collect(),
        );
        let candidate_text = serde_json::to_string(&candidate).map_err(RuntimeError::Serialize)?;
        pending_keys = compressor.stash_keys();
        finish_schema_compression(
            &input,
            candidate_text,
            options.compression_enabled,
            attached_store,
            &compressor,
        )
    } else {
        crate::CompressResult {
            output: input.clone(),
            compressed_output: input.clone(),
            disposition: Disposition::Passthrough,
            before_tokens: estimate_tokens(&input),
            after_tokens: estimate_tokens(&input),
            stash_writes: None,
            stash_errors: None,
            unrecoverable_truncations: None,
            stash_size: None,
        }
    };
    // Stash errors are fatal for BeforeModel, but only AFTER the
    // disposition ladder above has run: finish_schema_compression is what
    // rolls back the rows written before the failure. Returning earlier
    // would leak them with no marker ever emitted.
    if let Some(count) = compression.stash_errors.filter(|count| *count > 0) {
        return Err(RuntimeError::StashWrite { count });
    }
    let tools = serde_json::from_str::<Vec<Value>>(&compression.output)?;

    let mut markers = BTreeSet::new();
    collect_markers(
        &request.visible_context,
        &request.capabilities.recovery,
        &mut markers,
    );
    collect_markers(
        &Value::Array(tools.clone()),
        &request.capabilities.recovery,
        &mut markers,
    );
    let visible_markers = markers.into_iter().collect::<Vec<_>>();
    let measured = matches!(
        compression.disposition,
        Disposition::Applied | Disposition::DryRun
    );
    let emitted_keys = if compression.disposition == Disposition::Applied {
        pending_keys
    } else {
        Vec::new()
    };
    let recoverability = if emitted_keys.is_empty() {
        Recoverability::Lossless
    } else {
        Recoverability::Retrievable
    };
    Ok(BeforeModelOutcome {
        response: BeforeModelResponse {
            tools,
            visible_markers,
        },
        stats: EntryStats {
            operation: OperationType::CompressSchema,
            input,
            measured_output: if measured {
                compression.compressed_output
            } else {
                compression.output
            },
            disposition: compression.disposition,
            content_type: None,
            content_origin: None,
            applied_operations: (compression.disposition == Disposition::Applied)
                .then_some(vec![AppliedOperation::SchemaCompression])
                .unwrap_or_default(),
            recoverability,
            unrecoverable_truncations: None,
        },
        stash_writes: compression.stash_writes,
        stash_errors: compression.stash_errors,
        stash_size: compression.stash_size,
        artifact_keys: emitted_keys,
    })
}

fn collect_markers(value: &Value, recovery: &RecoveryMethod, markers: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => markers.extend(
            recovery_hashes(text, recovery)
                .into_iter()
                .map(str::to_ascii_lowercase),
        ),
        Value::Array(items) => {
            for item in items {
                collect_markers(item, recovery, markers);
            }
        }
        Value::Object(fields) => {
            for (name, item) in fields {
                markers.extend(
                    recovery_hashes(name, recovery)
                        .into_iter()
                        .map(str::to_ascii_lowercase),
                );
                collect_markers(item, recovery, markers);
            }
        }
        _ => {}
    }
}

/// Rewrites with the embedded RTK under the user's current RTK configuration.
///
/// The rewritten command runs `launcher` later, so a launcher that has left its
/// path passes the command through unchanged: once an upgrade or uninstall
/// replaces the running executable, Linux names it `<path> (deleted)`.
fn rtk_rewrite(launcher: Option<&Path>, command: &str) -> rtk::RewriteOutcome {
    if let Some(path) = launcher.filter(|path| !path.is_file()) {
        warn_launcher_gone(path);
        return rtk::RewriteOutcome::Passthrough;
    }
    rtk::rewrite(command, &rtk::RewriteOptions::load())
}

/// Reports once per process that rewriting has stopped, so the lost savings
/// are visible without repeating on every tool call.
fn warn_launcher_gone(path: &Path) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        crate::warn_soft(&format!(
            "tokenless: RTK launcher {} is no longer a file; commands pass through without rewriting",
            path.display()
        ));
    });
}

fn pre_tool_with_rewriter(
    request: &PreToolRequest,
    attribution: &Attribution,
    rtk_launcher: Option<&Path>,
    data_dir: Option<&Path>,
    rewrite: impl FnOnce(&str) -> rtk::RewriteOutcome,
) -> Result<PreToolResponse, RuntimeError> {
    let Some(arguments) = request.arguments.as_object() else {
        return Ok(pre_tool_passthrough(request));
    };
    let Some(command) = arguments
        .get(&request.command_field)
        .and_then(Value::as_str)
    else {
        return Ok(pre_tool_passthrough(request));
    };
    if !request.capabilities.replace_arguments && !request.capabilities.block_and_suggest {
        return Ok(pre_tool_passthrough(request));
    }
    // Preserve native execution according to the host's PostTool replacement capability.
    if keeps_command_native_for(command, request.capabilities.post_tool_replacement) {
        return Ok(pre_tool_passthrough(request));
    }
    let rtk_launcher = rtk_launcher.ok_or(RuntimeError::RtkLauncherUnavailable)?;
    let data_dir = data_dir.ok_or(RuntimeError::RtkDataDirectoryUnavailable)?;

    // Ask keeps the rewrite and leaves the prompt to the host, as exit 3 of
    // `rtk rewrite` does.
    let rewritten = match rewrite(command) {
        rtk::RewriteOutcome::Allow(rewritten) | rtk::RewriteOutcome::Ask(rewritten) => rewritten,
        rtk::RewriteOutcome::Deny | rtk::RewriteOutcome::Passthrough => {
            return Ok(pre_tool_passthrough(request));
        }
    };
    let rewritten = rewritten.trim();
    if rewritten.is_empty() || rewritten == command {
        return Ok(pre_tool_passthrough(request));
    }
    let anchored = if request.tool_name.eq_ignore_ascii_case("PowerShell") {
        let Some(anchored) =
            anchor_powershell_rtk(command, rewritten, rtk_launcher, attribution, data_dir)
        else {
            return Ok(pre_tool_passthrough(request));
        };
        anchored
    } else {
        anchor_rtk_prefix_with_outcomes(
            command,
            rewritten,
            rtk_launcher,
            attribution,
            data_dir,
            |expected, render| {
                if attribution
                    .session_id
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
                    || attribution
                        .tool_use_id
                        .as_deref()
                        .unwrap_or_default()
                        .is_empty()
                {
                    return None;
                }
                match OutcomeStore::open(data_dir, true).and_then(|mut store| {
                    store.begin(
                        &attribution.agent_id,
                        attribution.session_id.as_deref().unwrap_or_default(),
                        attribution.tool_use_id.as_deref().unwrap_or_default(),
                        expected,
                        command,
                        render,
                    )
                }) {
                    Ok(command) => command,
                    Err(error) => {
                        warn_rtk_outcome(&error);
                        None
                    }
                }
            },
        )
    };
    let mut rewritten_arguments = arguments.clone();
    rewritten_arguments.insert(request.command_field.clone(), Value::String(anchored));
    let action = if request.capabilities.replace_arguments {
        PreToolAction::ReplaceArguments
    } else {
        PreToolAction::BlockAndSuggest
    };
    Ok(PreToolResponse {
        arguments: Value::Object(rewritten_arguments),
        action,
        output_optimization: OutputOptimization::Rtk,
    })
}

fn pre_tool_passthrough(request: &PreToolRequest) -> PreToolResponse {
    PreToolResponse {
        arguments: request.arguments.clone(),
        action: PreToolAction::Passthrough,
        output_optimization: OutputOptimization::None,
    }
}

#[cfg(test)]
fn keeps_command_native(command: &str) -> bool {
    keeps_command_native_for(command, PostToolReplacementCapability::FullRetrievable)
}

fn keeps_command_native_for(command: &str, capability: PostToolReplacementCapability) -> bool {
    command_has_segment(command, |words| segment_keeps_native_for(words, capability))
}

/// Uses the same shell segments and transparent wrappers as PreTool ownership.
pub(crate) fn is_search_command(command: &str) -> bool {
    command_has_segment(command, native_output::is_search)
}

fn is_tokenless_retrieve_command(command: &str) -> bool {
    command_has_segment(command, |words| {
        let Some(unwrapped) = native_output::unwrap_transparent_prefix(words) else {
            return false;
        };
        if !matches!(unwrapped.executable(), "tokenless" | "tokenless.exe") {
            return false;
        }
        let mut index = 0;
        let args = unwrapped.args();
        while let Some(arg) = args.get(index) {
            if arg == "retrieve" {
                return true;
            }
            if matches!(arg.as_str(), "--data-dir" | "--stash-db" | "--stats-db") {
                index += 2;
            } else if arg.starts_with('-') {
                index += 1;
            } else {
                return false;
            }
        }
        false
    })
}

fn command_has_segment(command: &str, mut matches_segment: impl FnMut(&[String]) -> bool) -> bool {
    let mut words = Vec::new();
    let mut word = String::new();
    // Empty quoted words still consume wrapper arguments.
    let mut word_started = false;
    let mut quote = None;
    let mut escaped = false;

    for character in command.chars() {
        if escaped {
            if character != '\n' {
                word_started = true;
                word.push(character);
            }
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                word.push(character);
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            word_started = true;
            quote = Some(character);
        } else if character.is_whitespace() {
            if word_started {
                words.push(std::mem::take(&mut word));
                word_started = false;
            }
            if character == '\n' && matches_segment(&words) {
                return true;
            }
            if character == '\n' {
                words.clear();
            }
        } else if matches!(character, '&' | '|' | ';' | '(' | ')') {
            if word_started {
                words.push(std::mem::take(&mut word));
                word_started = false;
            }
            if matches_segment(&words) {
                return true;
            }
            words.clear();
        } else {
            word_started = true;
            word.push(character);
        }
    }
    if word_started {
        words.push(word);
    }
    matches_segment(&words)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandDomain {
    SourcePreview,
    Search,
    BuildTest,
    Other,
}

fn classify_command_domain(executable: &str, arguments: &[String]) -> CommandDomain {
    match executable {
        "head" | "tail" => CommandDomain::SourcePreview,
        "grep" | "rg" | "find" => CommandDomain::Search,
        "cargo" => {
            let arguments = &arguments[usize::from(
                arguments
                    .first()
                    .is_some_and(|argument| argument.starts_with('+')),
            )..];
            if matches!(
                arguments.first().map(String::as_str),
                Some("build" | "check" | "clippy" | "install" | "test")
            ) {
                CommandDomain::BuildTest
            } else {
                CommandDomain::Other
            }
        }
        "pytest" => CommandDomain::BuildTest,
        executable if is_python_executable(executable) => {
            if matches!(
                arguments,
                [module, runner, ..] if module == "-m" && runner == "pytest"
            ) {
                CommandDomain::BuildTest
            } else {
                CommandDomain::Other
            }
        }
        "npm" | "pnpm" => {
            if package_command_is_build_log_owned(arguments) {
                CommandDomain::BuildTest
            } else {
                CommandDomain::Other
            }
        }
        "npx" | "pnpx" => {
            if arguments.first().is_some_and(|runner| runner == "jest") {
                CommandDomain::BuildTest
            } else {
                CommandDomain::Other
            }
        }
        "jest" => CommandDomain::BuildTest,
        "go" => {
            if matches!(
                arguments.first().map(String::as_str),
                Some("build" | "test" | "vet")
            ) {
                CommandDomain::BuildTest
            } else {
                CommandDomain::Other
            }
        }
        "make" | "gmake" => CommandDomain::BuildTest,
        _ => CommandDomain::Other,
    }
}

fn segment_keeps_native_for(words: &[String], capability: PostToolReplacementCapability) -> bool {
    let Some(unwrapped) = native_output::unwrap_transparent_prefix(words) else {
        return false;
    };
    let domain = classify_command_domain(unwrapped.executable(), unwrapped.args());
    match capability {
        PostToolReplacementCapability::None => false,
        PostToolReplacementCapability::LosslessOnly => {
            matches!(domain, CommandDomain::SourcePreview | CommandDomain::Search)
        }
        PostToolReplacementCapability::FullRetrievable => matches!(
            domain,
            CommandDomain::SourcePreview | CommandDomain::Search | CommandDomain::BuildTest
        ),
    }
}

fn package_command_is_build_log_owned(arguments: &[String]) -> bool {
    let Some(action) = arguments.first().map(String::as_str) else {
        return false;
    };
    if action == "test" {
        return true;
    }
    if matches!(action, "exec" | "x" | "dlx") {
        return arguments.get(1).is_some_and(|runner| runner == "jest");
    }
    if !matches!(action, "run" | "run-script") {
        return false;
    }
    arguments
        .iter()
        .skip(1)
        .find(|word| !word.starts_with('-'))
        .is_some_and(|script| {
            matches!(script.as_str(), "build" | "jest" | "test")
                || script.starts_with("build:")
                || script.starts_with("test:")
        })
}

fn is_environment_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn command_basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

fn is_python_executable(executable: &str) -> bool {
    executable.strip_prefix("python").is_some_and(|suffix| {
        suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    })
}

/// Anchors a PowerShell rewrite to `rtk_launcher`, or returns `None` when an
/// inserted `rtk` is not where PowerShell starts a command.
///
/// Each inserted word becomes a call-operator invocation of the launcher's
/// `rtk` subcommand. The call stays native, so `$?` and `$LASTEXITCODE` keep
/// their meaning, and the script's pipelines, conditions, arguments and quoting
/// stay intact.
fn anchor_powershell_rtk(
    original: &str,
    rewritten: &str,
    rtk_launcher: &Path,
    attribution: &Attribution,
    data_dir: &Path,
) -> Option<String> {
    let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
    let offsets = inserted_rtk_offsets(original, rewritten);
    // The scanner follows shell quoting, so words it cannot see (after a
    // PowerShell backtick escape, for example) would run a missing `rtk`.
    if offsets.is_empty()
        || rtk_word_count(rewritten) != rtk_word_count(original) + offsets.len()
        || !offsets
            .iter()
            .all(|offset| starts_powershell_command(rewritten, *offset))
    {
        return None;
    }
    let invocation = format!("& {} rtk", quote(&rtk_launcher.to_string_lossy()));
    let mut body = String::with_capacity(rewritten.len() + offsets.len() * invocation.len());
    let mut copied_until = 0;
    for offset in offsets {
        body.push_str(&rewritten[copied_until..offset]);
        body.push_str(&invocation);
        copied_until = offset + 3;
    }
    body.push_str(&rewritten[copied_until..]);
    let values = [
        ("TOKENLESS_AGENT_ID", attribution.agent_id.as_str()),
        (
            "TOKENLESS_SESSION_ID",
            attribution.session_id.as_deref().unwrap_or_default(),
        ),
        (
            "TOKENLESS_TOOL_USE_ID",
            attribution.tool_use_id.as_deref().unwrap_or_default(),
        ),
        ("TOKENLESS_DATA_DIR", &data_dir.to_string_lossy()),
    ];
    let mut script = String::from("& { [CmdletBinding()] param(); ");
    for (index, (name, _)) in values.iter().enumerate() {
        script.push_str(&format!(
            "$tokenlessOld{index} = [Environment]::GetEnvironmentVariable('{name}', 'Process'); "
        ));
    }
    script.push_str("try { ");
    for (name, value) in values {
        script.push_str(&format!("$env:{name} = {}; ", quote(value)));
    }
    script.push_str(&body);
    script.push_str("\n} finally { $tokenlessOk = $?; $tokenlessExit = $LASTEXITCODE; ");
    for (index, (name, _)) in values.iter().enumerate() {
        script.push_str(&format!(
            "[Environment]::SetEnvironmentVariable('{name}', $tokenlessOld{index}, 'Process'); "
        ));
    }
    // $? is read-only. An advanced script block must report its own failure;
    // Ignore preserves that status without adding stderr or an $Error entry.
    script.push_str("$global:LASTEXITCODE = $tokenlessExit } if (-not $tokenlessOk) { $ErrorActionPreference = 'Ignore'; $PSCmdlet.WriteError([System.Management.Automation.ErrorRecord]::new([Exception]::new('Command failed'), 'TokenlessCommandFailed', [System.Management.Automation.ErrorCategory]::NotSpecified, $null)) } }");
    Some(script)
}

/// Whether the word at `offset` begins a PowerShell command.
///
/// A lone `&` is rejected: before a command it is the call operator, which
/// the inserted invocation would duplicate.
fn starts_powershell_command(script: &str, offset: usize) -> bool {
    let before = script[..offset].trim_end_matches([' ', '\t']);
    before.is_empty()
        || before.ends_with("&&")
        || before.ends_with([';', '|', '{', '(', '\n', '\r'])
}

/// Counts `rtk` occurrences delimited by characters that cannot extend a command name.
fn rtk_word_count(text: &str) -> usize {
    let extends = |character: char| character.is_alphanumeric() || matches!(character, '_' | '-');
    text.match_indices("rtk")
        .filter(|(index, _)| {
            !text[..*index].chars().next_back().is_some_and(extends)
                && !text[index + 3..].chars().next().is_some_and(extends)
        })
        .count()
}

#[cfg(test)]
fn anchor_rtk_prefix(
    original: &str,
    rewritten: &str,
    rtk_launcher: &Path,
    attribution: &Attribution,
    data_dir: &Path,
) -> String {
    anchor_rtk_prefix_with_outcomes(
        original,
        rewritten,
        rtk_launcher,
        attribution,
        data_dir,
        |_, _| None,
    )
}

fn anchor_rtk_prefix_with_outcomes(
    original: &str,
    rewritten: &str,
    rtk_launcher: &Path,
    attribution: &Attribution,
    data_dir: &Path,
    begin: impl FnOnce(usize, &dyn Fn(&str) -> String) -> Option<String>,
) -> String {
    let invocation = format!("{} rtk", shell_quote(&rtk_launcher.to_string_lossy()));
    let prefix = format!(
        "env TOKENLESS_AGENT_ID={} TOKENLESS_SESSION_ID={} TOKENLESS_TOOL_USE_ID={} TOKENLESS_DATA_DIR={} ",
        shell_quote(&attribution.agent_id),
        shell_quote(attribution.session_id.as_deref().unwrap_or_default()),
        shell_quote(attribution.tool_use_id.as_deref().unwrap_or_default()),
        shell_quote(&data_dir.to_string_lossy()),
    );
    let original_segments = bare_rtk_offsets_by_segment(original);
    let rewritten_segments = bare_rtk_offsets_by_segment(rewritten);
    let replacements = inserted_rtk_offsets(original, rewritten);

    // Opaque substitutions or existing RTK references may execute untracked wrappers.
    let all_tracked = original_segments
        .iter()
        .all(|(_, tokens)| tokens.is_empty())
        && !original.contains("rtk")
        && !original.contains('`')
        && !original.contains("$(")
        && rewritten_segments
            .iter()
            .map(|(_, tokens)| tokens.len())
            .sum::<usize>()
            == replacements.len();
    let render = |run: Option<&str>| {
        let mut anchored = String::with_capacity(
            rewritten.len() + replacements.len() * (prefix.len() + invocation.len()),
        );
        let mut copied_until = 0;
        for (ordinal, offset) in replacements.iter().copied().enumerate() {
            anchored.push_str(&rewritten[copied_until..offset]);
            anchored.push_str(&prefix);
            if let Some(run) = run {
                anchored.push_str(&format!(
                    "TOKENLESS_RTK_RUN_ID={} TOKENLESS_RTK_ORDINAL={ordinal} ",
                    shell_quote(run)
                ));
            }
            anchored.push_str(&invocation);
            copied_until = offset + 3;
        }
        anchored.push_str(&rewritten[copied_until..]);
        anchored
    };
    begin(if all_tracked { replacements.len() } else { 0 }, &|run| {
        render(Some(run))
    })
    .unwrap_or_else(|| render(None))
}

/// Offsets of the `rtk` words RTK inserted into `rewritten`.
///
/// RTK can preserve arbitrary configured transparent prefixes before its
/// wrapper. The first divergence in each rewritten segment locates the
/// inserted wrapper without replacing `rtk` arguments in that prefix.
/// Backtick and double-quoted command substitutions are left untouched
/// because they require the host parser.
fn inserted_rtk_offsets(original: &str, rewritten: &str) -> Vec<usize> {
    let original_segments = bare_rtk_offsets_by_segment(original);
    let mut offsets = Vec::new();
    for (segment_index, (rewritten_start, tokens)) in
        bare_rtk_offsets_by_segment(rewritten).iter().enumerate()
    {
        let original_segment = original_segments.get(segment_index);
        let original_count = original_segment.map_or(0, |(_, tokens)| tokens.len());
        if tokens.len() > original_count {
            let original_start = original_segment.map_or(original.len(), |(start, _)| *start);
            let original_suffix = &original[original_start..];
            let rewritten_suffix = &rewritten[*rewritten_start..];
            let original_trimmed = original_suffix.trim_start();
            let rewritten_trimmed = rewritten_suffix.trim_start();
            let common_prefix_len = original_trimmed
                .bytes()
                .zip(rewritten_trimmed.bytes())
                .take_while(|(original, rewritten)| original == rewritten)
                .count();
            let insertion_floor = rewritten_start
                + (rewritten_suffix.len() - rewritten_trimmed.len())
                + common_prefix_len;
            offsets.extend(
                tokens
                    .iter()
                    .copied()
                    .find(|offset| *offset + 3 > insertion_floor),
            );
        }
    }
    offsets
}

fn bare_rtk_offsets_by_segment(command: &str) -> Vec<(usize, Vec<usize>)> {
    let mut segments = Vec::new();
    let mut segment_start = 0;
    let mut current_offsets = Vec::new();
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut word_start = true;

    while index < command.len() {
        let Some(ch) = command[index..].chars().next() else {
            break;
        };
        let width = ch.len_utf8();

        if escaped {
            escaped = false;
            index += width;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            word_start = false;
            index += width;
            continue;
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            }
            index += width;
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            word_start = false;
            index += width;
            continue;
        }
        if ch.is_whitespace() {
            word_start = true;
            if ch == '\n' {
                segments.push((segment_start, current_offsets));
                segment_start = index + width;
                current_offsets = Vec::new();
            }
            index += width;
            continue;
        }
        if matches!(ch, '&' | '|' | ';' | '(') {
            segments.push((segment_start, current_offsets));
            segment_start = index + width;
            current_offsets = Vec::new();
            word_start = true;
            index += width;
            continue;
        }
        if word_start && command[index..].starts_with("rtk") {
            let next = command[index + 3..].chars().next();
            if next.is_none_or(|value| {
                value.is_whitespace() || matches!(value, '&' | '|' | ';' | '(' | ')')
            }) {
                current_offsets.push(index);
                index += 3;
                word_start = false;
                continue;
            }
        }
        word_start = false;
        index += width;
    }
    segments.push((segment_start, current_offsets));
    segments
}

fn shell_quote(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.'))
    {
        value.to_owned()
    } else {
        let quoted = format!("'{}'", value.replace('\'', "'\\''"));
        if cfg!(windows) {
            msys_harden_quoted(&quoted)
        } else {
            quoted
        }
    }
}

fn msys_harden_quoted(quoted: &str) -> String {
    // MSYS decodes backslash pairs in -c arguments before Bash parses quotes.
    quoted.replace(r"\\", r"\''\")
}

pub(crate) struct PostToolOutcome {
    pub(crate) response: PostToolResponse,
    pub(crate) stats: EntryStats,
    pub(crate) stash_writes: Option<usize>,
    pub(crate) stash_errors: Option<usize>,
    pub(crate) stash_size: Option<usize>,
}

fn warn_rtk_outcome(error: &tokenless_stats::rtk_outcome::OutcomeError) {
    crate::warn_soft(&format!(
        "tokenless: {error}; retaining RTK output ownership"
    ));
}

pub(crate) fn post_tool_with_outcomes(
    request: &PostToolRequest,
    options: &EntryOptions,
    stash_store: Option<&Arc<dyn StashStore>>,
    attribution: &Attribution,
) -> Result<PostToolOutcome, RuntimeError> {
    let mut original_command = None;
    if request.output_optimization == OutputOptimization::Rtk
        && let Some(command) = request.command.as_deref()
        && let Some(data_dir) = options.rtk_data_dir.as_deref()
        && OutcomeStore::path(data_dir).exists()
    {
        match OutcomeStore::open(data_dir, false).and_then(|mut store| {
            store.consume_unchanged(
                &attribution.agent_id,
                attribution.session_id.as_deref().unwrap_or_default(),
                attribution.tool_use_id.as_deref().unwrap_or_default(),
                command,
            )
        }) {
            Ok(value) => original_command = value,
            Err(error) => warn_rtk_outcome(&error),
        }
    }
    let refined;
    let request = if let Some(command) = original_command
        && request.status == ToolResultStatus::Success
    {
        refined = PostToolRequest {
            output_optimization: OutputOptimization::None,
            command: Some(command),
            ..request.clone()
        };
        &refined
    } else {
        request
    };
    post_tool_with_store(request, options, stash_store)
}

pub(crate) fn post_tool_with_store(
    request: &PostToolRequest,
    options: &EntryOptions,
    stash_store: Option<&Arc<dyn StashStore>>,
) -> Result<PostToolOutcome, RuntimeError> {
    // Refine the origin before routing so that thresholds, the pipeline and
    // the stats row all see the same classification.
    let refined;
    let request = if request.content_origin == ContentOrigin::CommandOutput
        && request
            .command
            .as_deref()
            .is_some_and(post_tool::file_read::prints_local_files)
    {
        refined = PostToolRequest {
            content_origin: ContentOrigin::FileRead,
            ..request.clone()
        };
        &refined
    } else {
        request
    };
    let before_tokens = estimate_tokens(&request.content) as u64;
    let routed = if request.result_kind == ResultKind::Retrieve
        || (request.content_origin == ContentOrigin::CommandOutput
            && request
                .command
                .as_deref()
                .is_some_and(is_tokenless_retrieve_command))
        || matches!(
            request.status,
            ToolResultStatus::Interrupted | ToolResultStatus::Denied
        )
        || request.output_optimization == OutputOptimization::Rtk
    {
        Some(PostToolResponse::passthrough(request, before_tokens))
    } else {
        None
    };
    let attached_store = request
        .capabilities
        .recovery
        .is_available()
        .then_some(stash_store)
        .flatten();

    let (
        mut response,
        candidate,
        operations,
        stash_writes,
        stash_errors,
        stash_size,
        unrecoverable,
    ) = if let Some(response) = routed {
        (response, None, Vec::new(), None, None, None, None)
    } else {
        let thresholds = taxonomy::thresholds_for(request.content_origin);
        let run = PostToolPipeline::run(
            request,
            &PostToolPipelineConfig {
                timeout: RESPONSE_PIPELINE_TIMEOUT,
                max_input_bytes: MAX_INPUT_BYTES,
                min_input_chars: MIN_RESPONSE_CHARS,
                error_min_input_chars: post_tool::DEFAULT_ERROR_MIN_CHARS,
                compression_enabled: options.compression_enabled,
                search_path_sharing_enabled: options.search_path_sharing_enabled,
                diff_compression_enabled: options.diff_compression_enabled,
                html_extraction_enabled: options.html_extraction_enabled,
                browser_snapshot_enabled: options.browser_snapshot_enabled,
                mixed_content_enabled: options.mixed_content_enabled,
                stash_enabled: options.stash_enabled,
                require_reversibility: true,
                force_json: false,
                allow_toon: true,
                min_toon_chars: MIN_TOON_CHARS,
                json: JsonCompressionConfig {
                    truncate_strings_at: thresholds.truncate_strings_at,
                    truncate_arrays_at: thresholds.truncate_arrays_at,
                    max_depth: thresholds.max_depth,
                    ..JsonCompressionConfig::default()
                },
            },
            attached_store,
        );
        if let Some(count) = run.stash_errors.filter(|count| *count > 0) {
            return Err(RuntimeError::StashWrite { count });
        }
        if let Some(violation) = run.fact_guard_violation {
            crate::warn_soft(&format!(
                "[tokenless] fact guard rolled back error compression: {}",
                violation.as_str(),
            ));
        }
        (
            run.response,
            run.candidate,
            run.operations,
            run.stash_writes,
            run.stash_errors,
            run.stash_size,
            run.unrecoverable_truncations,
        )
    };
    if request.status == ToolResultStatus::Error {
        response.additional_context = diagnose_tool_error(&request.tool_name, &request.content)
            .or_else(|| {
                request
                    .stderr
                    .as_deref()
                    .and_then(|stderr| diagnose_tool_error(&request.tool_name, stderr))
            });
        if matches!(
            response.disposition,
            Disposition::Passthrough
                | Disposition::NoSavings
                | Disposition::RecoverabilityUnavailable
        ) {
            response.disposition = Disposition::ToolError;
        }
    }
    let measured = matches!(
        response.disposition,
        Disposition::Applied | Disposition::DryRun
    );
    let measured_output = if measured {
        candidate.unwrap_or_else(|| request.content.clone())
    } else {
        request.content.clone()
    };
    let operation = if operations.contains(&AppliedOperation::Toon) {
        OperationType::CompressToon
    } else {
        OperationType::CompressResponse
    };
    Ok(PostToolOutcome {
        stats: EntryStats {
            operation,
            input: request.content.clone(),
            measured_output,
            disposition: response.disposition,
            content_type: response
                .content_type
                .map(|value| value.wire_str().to_owned()),
            content_origin: Some(request.content_origin.wire_str().to_owned()),
            applied_operations: response.applied_operations.clone(),
            recoverability: response.recoverability,
            unrecoverable_truncations: unrecoverable,
        },
        response,
        stash_writes,
        stash_errors,
        stash_size,
    })
}

fn diagnose_tool_error(tool_name: &str, content: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    let (category, hint) = if ["command not found", "not installed", "unable to locate"]
        .iter()
        .any(|pattern| lower.contains(pattern))
    {
        (
            "ENV_DEPENDENCY_MISSING",
            "Install the missing dependency or ask the user for guidance.",
        )
    } else if lower.contains("permission denied") || lower.contains("operation not permitted") {
        (
            "ENV_PERMISSION",
            "Check file or directory permissions and required access.",
        )
    } else if lower.contains("no such file or directory") || lower.contains("enoent") {
        (
            "ENV_FILE_MISSING",
            "Verify the path or create the required file or directory.",
        )
    } else if [
        "connection refused",
        "network is unreachable",
        "could not resolve host",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
    {
        (
            "ENV_NETWORK",
            "Check DNS, proxy, firewall, and network connectivity.",
        )
    } else if ["modulenotfounderror", "no module named", "importerror"]
        .iter()
        .any(|pattern| lower.contains(pattern))
    {
        (
            "ENV_PACKAGE_MISSING",
            "Install the required package or module.",
        )
    } else {
        return None;
    };
    Some(format!(
        "[tokenless:env] {tool_name} failed: {category} ({hint})."
    ))
}

pub(crate) fn retrieve_authorized_with_store(
    request: &RetrieveRequest,
    stash_store: Option<&Arc<dyn StashStore>>,
    recorder: Option<&StatsRecorder>,
    attribution: &Attribution,
    source: &str,
) -> Result<RetrieveResponse, RuntimeError> {
    let hash = normalize_hash(&request.hash_or_marker)?;
    let visible = request
        .visible_markers
        .iter()
        .filter_map(|marker| normalize_hash(marker).ok())
        .any(|visible_hash| visible_hash == hash);
    if !visible {
        return Err(RuntimeError::RetrieveUnauthorized { hash });
    }
    let store = stash_store
        .ok_or_else(|| RuntimeError::StashUnavailable("stash is not configured".to_string()))?;
    let result = store.retrieve(&hash);
    if let Some(recorder) = recorder {
        let (outcome, payload_tokens) = match &result {
            Ok(Some(payload)) => ("hit", Some(estimate_tokens(payload) as i64)),
            Ok(None) => ("miss", None),
            Err(_) => ("error", None),
        };
        let tokenizer_id = payload_tokens.is_some().then_some(TOKENIZER_ID);
        if let Err(e) = recorder.record_retrieve_event(
            &hash,
            outcome,
            source,
            payload_tokens,
            tokenizer_id,
            Some(&attribution.agent_id),
            attribution.session_id.as_deref(),
            attribution.tool_use_id.as_deref(),
        ) {
            // Fail-soft, and the warning itself must not be able to
            // fail the retrieval: warn_soft discards its own write
            // errors.
            crate::warn_soft(&format!(
                "[tokenless-stats] WARNING: failed to record retrieve event: {e}"
            ));
        }
    }
    match result {
        Ok(Some(payload)) => Ok(RetrieveResponse { hash, payload }),
        Ok(None) => Err(RuntimeError::StashEntryNotFound { hash }),
        Err(error) => Err(RuntimeError::StashRetrieve(error.to_string())),
    }
}

fn normalize_hash(hash_or_marker: &str) -> Result<String, RuntimeError> {
    let candidate = extract_hash(hash_or_marker).unwrap_or(hash_or_marker);
    if !is_valid_hash(candidate) {
        return Err(RuntimeError::InvalidHash {
            value: hash_or_marker.to_owned(),
        });
    }
    Ok(candidate.to_ascii_lowercase())
}

/// Returns the operation of an entry response.
#[must_use]
pub fn response_operation(outcome: &EntryOutcome) -> Operation {
    outcome.response.response.operation()
}

#[cfg(test)]
mod tests;
