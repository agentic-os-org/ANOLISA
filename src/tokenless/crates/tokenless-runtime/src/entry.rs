//! Operation-specific lifecycle services and protocol transport dispatch.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokenless_ccr::{RecoveryMethod, StashStore, extract_hash, is_valid_hash, recovery_hashes};
use tokenless_compressors::JsonCompressionConfig;
use tokenless_protocol::{
    AppliedOperation, Attribution, BeforeModelRequest, BeforeModelResponse, Disposition, Operation,
    OutputOptimization, PostToolRequest, PostToolResponse, PreToolAction, PreToolRequest,
    PreToolResponse, Recoverability, Request, RequestEnvelope, Response, ResponseEnvelope,
    ResultKind, RetrieveRequest, RetrieveResponse, TOKENIZER_ID, ToolResultStatus, estimate_tokens,
};
use tokenless_schema::SchemaCompressor;
use tokenless_stats::{OperationType, StatsRecorder};

use crate::post_tool::{PostToolPipeline, PostToolPipelineConfig};
use crate::{
    MAX_INPUT_BYTES, MIN_TOON_CHARS, RESPONSE_PIPELINE_TIMEOUT, RuntimeError,
    finish_schema_compression, taxonomy,
};

const MIN_RESPONSE_CHARS: usize = 200;
const RTK_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-call behavior resolved by a transport frontend.
#[derive(Debug, Clone)]
pub struct EntryOptions {
    /// Whether accepted candidates replace the original content.
    pub compression_enabled: bool,
    /// Whether API search listings may share paths; independent of other domains.
    pub search_path_sharing_enabled: bool,
    /// Whether command-output Git diffs may omit context with original recovery. Disabled by default.
    pub diff_compression_enabled: bool,
    /// Whether complete HTML documents are rendered as Markdown with original recovery. Disabled by default.
    pub html_extraction_enabled: bool,
    /// Whether lifecycle operations may use the attached stash.
    pub stash_enabled: bool,
    /// Resolved RTK executable for PreTool.
    pub rtk_path: Option<PathBuf>,
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
                Response::PreTool(pre_tool_with_optional_rtk(
                    request,
                    &envelope.attribution,
                    options.rtk_path.as_deref(),
                    options.rtk_data_dir.as_deref(),
                    RTK_TIMEOUT,
                )?),
                None,
                None,
                None,
                None,
                Vec::new(),
            ),
            Request::PostTool(request) => {
                let outcome = post_tool_with_store(request, options, stash_store)?;
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

pub(crate) fn pre_tool_with_rtk(
    request: &PreToolRequest,
    attribution: &Attribution,
    rtk_path: &Path,
    data_dir: &Path,
) -> Result<PreToolResponse, RuntimeError> {
    pre_tool_with_optional_rtk(
        request,
        attribution,
        Some(rtk_path),
        Some(data_dir),
        RTK_TIMEOUT,
    )
}

fn pre_tool_with_optional_rtk(
    request: &PreToolRequest,
    attribution: &Attribution,
    rtk_path: Option<&Path>,
    data_dir: Option<&Path>,
    timeout: Duration,
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
    // RTK ownership would bypass the PostTool domain selected for native
    // build/test output, so the two optimizers remain mutually exclusive.
    if is_build_log_owned_command(command) {
        return Ok(pre_tool_passthrough(request));
    }
    let rtk_path = rtk_path.ok_or(RuntimeError::RtkUnavailable)?;
    let data_dir = data_dir.ok_or(RuntimeError::RtkDataDirectoryUnavailable)?;

    let mut child = Command::new(rtk_path);
    child
        .arg("rewrite")
        .arg(command)
        .env("TOKENLESS_AGENT_ID", &attribution.agent_id)
        .env("TOKENLESS_DATA_DIR", data_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(session_id) = &attribution.session_id {
        child.env("TOKENLESS_SESSION_ID", session_id);
    }
    if let Some(tool_use_id) = &attribution.tool_use_id {
        child.env("TOKENLESS_TOOL_USE_ID", tool_use_id);
    }
    let mut child = child.spawn().map_err(|source| RuntimeError::RtkSpawn {
        path: rtk_path.to_path_buf(),
        source,
    })?;
    let mut stdout_pipe = child.stdout.take().ok_or_else(|| {
        RuntimeError::RtkOutput(std::io::Error::other("RTK stdout pipe was not created"))
    })?;
    let stdout_reader = thread::spawn(move || {
        let mut stdout = String::new();
        stdout_pipe.read_to_string(&mut stdout)?;
        Ok::<_, std::io::Error>(stdout)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(RuntimeError::RtkWait)? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            return Err(RuntimeError::RtkTimeout);
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| {
            RuntimeError::RtkOutput(std::io::Error::other("RTK stdout reader terminated"))
        })?
        .map_err(RuntimeError::RtkOutput)?;
    let code = status.code().ok_or(RuntimeError::RtkTerminated)?;
    if matches!(code, 1 | 2) {
        return Ok(pre_tool_passthrough(request));
    }
    if !matches!(code, 0 | 3) {
        return Err(RuntimeError::RtkUnexpectedExit { code });
    }
    let rewritten = stdout.trim();
    if rewritten.is_empty() || rewritten == command {
        return Ok(pre_tool_passthrough(request));
    }
    let anchored = anchor_rtk_prefix(command, rewritten, rtk_path, attribution, data_dir);
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

fn is_build_log_owned_command(command: &str) -> bool {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;

    for character in command.chars() {
        if escaped {
            word.push(character);
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
            quote = Some(character);
        } else if character.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if character == '\n' && segment_is_build_log_owned(&words) {
                return true;
            }
            if character == '\n' {
                words.clear();
            }
        } else if matches!(character, '&' | '|' | ';' | '(' | ')') {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if segment_is_build_log_owned(&words) {
                return true;
            }
            words.clear();
        } else {
            word.push(character);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    segment_is_build_log_owned(&words)
}

fn segment_is_build_log_owned(words: &[String]) -> bool {
    let mut index = words
        .iter()
        .position(|word| !is_environment_assignment(word))
        .unwrap_or(words.len());
    if words.get(index).is_some_and(|word| word == "env") {
        index += 1;
        while words
            .get(index)
            .is_some_and(|word| is_environment_assignment(word))
        {
            index += 1;
        }
    }
    if matches!(
        words.get(index).map(|word| command_basename(word)),
        Some("command" | "exec")
    ) {
        index += 1;
    }

    let executable = words
        .get(index)
        .map(|word| command_basename(word))
        .unwrap_or_default();
    let arguments = &words[index.saturating_add(1).min(words.len())..];
    match executable {
        "cargo" => matches!(
            arguments.first().map(String::as_str),
            Some("build" | "check" | "clippy" | "install" | "test")
        ),
        "pytest" => true,
        executable if is_python_executable(executable) => {
            matches!(
                arguments,
                [module, runner, ..] if module == "-m" && runner == "pytest"
            )
        }
        "uv" => matches!(
            arguments,
            [run, runner, ..] if run == "run" && (runner == "pytest" || is_python_pytest(arguments, 1))
        ),
        "npm" | "pnpm" => package_command_is_build_log_owned(arguments),
        "npx" | "pnpx" => arguments.first().is_some_and(|runner| runner == "jest"),
        "jest" => true,
        "go" => matches!(
            arguments.first().map(String::as_str),
            Some("build" | "test" | "vet")
        ),
        "make" | "gmake" => true,
        _ => false,
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

fn is_python_pytest(arguments: &[String], start: usize) -> bool {
    matches!(
        arguments.get(start..),
        Some([python, module, runner, ..])
            if is_python_executable(command_basename(python))
                && module == "-m"
                && runner == "pytest"
    )
}

fn anchor_rtk_prefix(
    original: &str,
    rewritten: &str,
    rtk_path: &Path,
    attribution: &Attribution,
    data_dir: &Path,
) -> String {
    let quoted_path = shell_quote(&rtk_path.to_string_lossy());
    let prefix = format!(
        "env TOKENLESS_AGENT_ID={} TOKENLESS_SESSION_ID={} TOKENLESS_TOOL_USE_ID={} TOKENLESS_DATA_DIR={} {}",
        shell_quote(&attribution.agent_id),
        shell_quote(attribution.session_id.as_deref().unwrap_or_default()),
        shell_quote(attribution.tool_use_id.as_deref().unwrap_or_default()),
        shell_quote(&data_dir.to_string_lossy()),
        quoted_path,
    );
    // RTK can preserve arbitrary configured transparent prefixes before its
    // wrapper. The first divergence in each rewritten segment locates the
    // inserted wrapper without replacing `rtk` arguments in that prefix.
    // Backtick and double-quoted command substitutions are left untouched
    // because they require the host parser.
    let original_segments = bare_rtk_offsets_by_segment(original);
    let rewritten_segments = bare_rtk_offsets_by_segment(rewritten);
    let mut replacements = Vec::new();
    for (segment_index, (rewritten_start, tokens)) in rewritten_segments.iter().enumerate() {
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
            replacements.extend(
                tokens
                    .iter()
                    .copied()
                    .find(|offset| *offset + 3 > insertion_floor),
            );
        }
    }

    let mut anchored = String::with_capacity(rewritten.len() + replacements.len() * prefix.len());
    let mut copied_until = 0;
    for offset in replacements {
        anchored.push_str(&rewritten[copied_until..offset]);
        anchored.push_str(&prefix);
        copied_until = offset + 3;
    }
    anchored.push_str(&rewritten[copied_until..]);
    anchored
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
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(crate) struct PostToolOutcome {
    pub(crate) response: PostToolResponse,
    pub(crate) stats: EntryStats,
    pub(crate) stash_writes: Option<usize>,
    pub(crate) stash_errors: Option<usize>,
    pub(crate) stash_size: Option<usize>,
}

pub(crate) fn post_tool_with_store(
    request: &PostToolRequest,
    options: &EntryOptions,
    stash_store: Option<&Arc<dyn StashStore>>,
) -> Result<PostToolOutcome, RuntimeError> {
    let before_tokens = estimate_tokens(&request.content) as u64;
    let routed = if request.result_kind == ResultKind::Retrieve
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
                compression_enabled: options.compression_enabled,
                search_path_sharing_enabled: options.search_path_sharing_enabled,
                diff_compression_enabled: options.diff_compression_enabled,
                html_extraction_enabled: options.html_extraction_enabled,
                stash_enabled: options.stash_enabled,
                require_reversibility: true,
                force_json: false,
                preserve_top_level_shape: !request.capabilities.replace_with_text,
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
        )
        .map_err(|error| RuntimeError::Pipeline(error.to_string()))?;
        if let Some(count) = run.stash_errors.filter(|count| *count > 0) {
            return Err(RuntimeError::StashWrite { count });
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
        response.additional_context = diagnose_tool_error(&request.tool_name, &request.content);
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
        let _ = recorder.record_retrieve_event(
            &hash,
            outcome,
            source,
            payload_tokens,
            tokenizer_id,
            Some(&attribution.agent_id),
            attribution.session_id.as_deref(),
            attribution.tool_use_id.as_deref(),
        );
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
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use tempfile::tempdir;
    use tokenless_ccr::{InMemoryStore, StashError, StashStore, StashWrite};
    use tokenless_protocol::{
        BeforeModelCapabilities, ContentOrigin, ContentType, PostToolCapabilities,
        PreToolCapabilities,
    };

    use super::*;

    fn options() -> EntryOptions {
        EntryOptions {
            compression_enabled: true,
            search_path_sharing_enabled: true,
            diff_compression_enabled: false,
            html_extraction_enabled: false,
            stash_enabled: true,
            rtk_path: None,
            rtk_data_dir: Some(PathBuf::from("/tmp/tokenless-test")),
        }
    }

    fn write_executable(path: &Path, script: &str) {
        fs::write(path, script).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[derive(Default)]
    struct ReadCountingStore {
        inner: InMemoryStore,
        reads: AtomicUsize,
    }

    impl StashStore for ReadCountingStore {
        fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
            self.inner.stash(payload)
        }

        fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.inner.retrieve(hash)
        }

        fn len(&self) -> usize {
            self.inner.len()
        }

        fn evict_expired(&self) -> Result<usize, StashError> {
            self.inner.evict_expired()
        }

        fn delete(&self, hash: &str, generation: u64) -> Result<bool, StashError> {
            self.inner.delete(hash, generation)
        }
    }

    struct FailingStore;

    impl StashStore for FailingStore {
        fn stash(&self, _payload: &str) -> Result<StashWrite, StashError> {
            Err(StashError::Backend("simulated write failure".into()))
        }

        fn retrieve(&self, _hash: &str) -> Result<Option<String>, StashError> {
            Ok(None)
        }

        fn len(&self) -> usize {
            0
        }

        fn evict_expired(&self) -> Result<usize, StashError> {
            Ok(0)
        }

        fn delete(&self, _hash: &str, _generation: u64) -> Result<bool, StashError> {
            Ok(false)
        }
    }

    #[test]
    fn retrieve_authorization_precedes_store_read() {
        let concrete = Arc::new(ReadCountingStore::default());
        let write = concrete.stash("byte-exact\n").unwrap();
        let store: Arc<dyn StashStore> = concrete.clone();
        let denied = RetrieveRequest {
            hash_or_marker: write.key.clone(),
            visible_markers: vec![],
        };
        assert!(matches!(
            retrieve_authorized_with_store(
                &denied,
                Some(&store),
                None,
                &Attribution::new("test"),
                "test"
            ),
            Err(RuntimeError::RetrieveUnauthorized { .. })
        ));
        assert_eq!(concrete.reads.load(Ordering::Relaxed), 0);
        let allowed = RetrieveRequest {
            hash_or_marker: write.key.clone(),
            visible_markers: vec![write.key.clone()],
        };
        let restored = retrieve_authorized_with_store(
            &allowed,
            Some(&store),
            None,
            &Attribution::new("test"),
            "test",
        )
        .unwrap();
        assert_eq!(restored.payload.as_bytes(), b"byte-exact\n");
        assert_eq!(concrete.reads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn new_visible_references_authorize_only_the_current_static_tool() {
        let concrete = Arc::new(ReadCountingStore::default());
        let write = concrete.stash("恢复\n").unwrap();
        let store: Arc<dyn StashStore> = concrete.clone();
        let recovery = RecoveryMethod::tool("tenant_retrieve").unwrap();
        for (context, authorized) in [
            (write.key.clone(), false),
            (
                tokenless_ccr::recovery_instruction(
                    &write.key,
                    &RecoveryMethod::tool("other").unwrap(),
                ),
                false,
            ),
            (
                tokenless_ccr::recovery_instruction(&write.key, &recovery),
                true,
            ),
            (
                tokenless_ccr::recovery_instruction(&write.key, &RecoveryMethod::Shell),
                true,
            ),
            (tokenless_ccr::marker_for(&write.key), true),
        ] {
            let before = before_model_with_store(
                &BeforeModelRequest {
                    tools: vec![],
                    visible_context: json!({"messages": [context]}),
                    capabilities: BeforeModelCapabilities {
                        replace_tools: false,
                        recovery: recovery.clone(),
                    },
                },
                &options(),
                Some(&store),
            )
            .unwrap();
            let reads = concrete.reads.load(Ordering::Relaxed);
            let restored = retrieve_authorized_with_store(
                &RetrieveRequest {
                    hash_or_marker: write.key.clone(),
                    visible_markers: before.response.visible_markers,
                },
                Some(&store),
                None,
                &Attribution::new("test"),
                "test",
            );
            if authorized {
                assert_eq!(restored.unwrap().payload, "恢复\n");
                assert_eq!(concrete.reads.load(Ordering::Relaxed), reads + 1);
            } else {
                assert!(matches!(
                    restored,
                    Err(RuntimeError::RetrieveUnauthorized { .. })
                ));
                assert_eq!(concrete.reads.load(Ordering::Relaxed), reads);
            }
        }
    }

    #[test]
    fn shell_recovery_does_not_enable_schema_stash() {
        let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
        let request = BeforeModelRequest {
            tools: vec![json!({"name":"read", "description":"long description ".repeat(200)})],
            visible_context: json!([]),
            capabilities: BeforeModelCapabilities {
                replace_tools: true,
                recovery: RecoveryMethod::Shell,
            },
        };
        let result = before_model_with_store(&request, &options(), Some(&store)).unwrap();
        assert_eq!(result.response.tools, request.tools);
        assert!(result.response.visible_markers.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn retrieve_records_attribution_only_after_authorization() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("stats.db");
        let recorder = StatsRecorder::new(&database).unwrap();
        let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
        let write = store.stash("payload").unwrap();
        let attribution = Attribution {
            agent_id: "agent".into(),
            session_id: Some("session".into()),
            tool_use_id: Some("call".into()),
        };

        let denied = RetrieveRequest {
            hash_or_marker: write.key.clone(),
            visible_markers: Vec::new(),
        };
        assert!(
            retrieve_authorized_with_store(
                &denied,
                Some(&store),
                Some(&recorder),
                &attribution,
                "test"
            )
            .is_err()
        );
        assert_eq!(recorder.retrieve_totals().unwrap().hits, 0);

        let allowed = RetrieveRequest {
            hash_or_marker: write.key.clone(),
            visible_markers: vec![format!("<<tokenless:{}>>", write.key.to_ascii_uppercase())],
        };
        dispatch_with_store(
            &RequestEnvelope {
                attribution,
                request: Request::Retrieve(allowed),
            },
            &options(),
            Some(&store),
            Some(&recorder),
        )
        .unwrap();

        let connection = rusqlite::Connection::open(database).unwrap();
        let event: (String, String, String, String, String) = connection
            .query_row(
                "SELECT source, agent_id, session_id, tool_use_id, outcome FROM retrieve_events",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            event,
            (
                "cli".into(),
                "agent".into(),
                "session".into(),
                "call".into(),
                "hit".into()
            )
        );
    }

    #[test]
    fn pre_tool_applies_rtk_exit_zero_and_anchors_path() {
        let directory = tempdir().unwrap();
        let rtk = directory.path().join("fake rtk");
        write_executable(&rtk, "#!/bin/sh\nprintf 'rtk grep --count error log'\n");
        let response = pre_tool_with_rtk(
            &PreToolRequest {
                tool_name: "Bash".into(),
                arguments: json!({"command": "grep error log"}),
                command_field: "command".into(),
                capabilities: PreToolCapabilities {
                    replace_arguments: true,
                    block_and_suggest: false,
                },
            },
            &Attribution::new("test"),
            &rtk,
            directory.path(),
        )
        .unwrap();
        assert_eq!(response.action, PreToolAction::ReplaceArguments);
        assert_eq!(response.output_optimization, OutputOptimization::Rtk);
        assert!(
            response.arguments["command"]
                .as_str()
                .unwrap()
                .contains("fake rtk")
        );
    }

    /// Contract v1 pin for the wrapper form published in
    /// `docs/design/rtk-hook-placement.md`. Host risk classifiers unwrap this
    /// exact shape, so changing the key set, the key order, the quoting rule,
    /// or the payload is a contract break and must bump the documented version.
    #[test]
    fn pre_tool_rtk_wrapper_matches_published_contract_v1() {
        let directory = tempdir().unwrap();
        let rtk = directory.path().join("rtk");
        write_executable(&rtk, "#!/bin/sh\nprintf 'rtk git status'\n");
        // A literal directory with a space: the contract promises `shell_quote`
        // semantics for every value, and this value needs quoting.
        let data_dir = Path::new("/var/lib/tokenless state");
        let response = pre_tool_with_rtk(
            &PreToolRequest {
                tool_name: "Bash".into(),
                arguments: json!({"command": "git status"}),
                command_field: "command".into(),
                capabilities: PreToolCapabilities {
                    replace_arguments: true,
                    block_and_suggest: false,
                },
            },
            &Attribution {
                agent_id: "cosh".into(),
                session_id: Some("session 1".into()),
                tool_use_id: Some("call/1".into()),
            },
            &rtk,
            data_dir,
        )
        .unwrap();
        assert_eq!(response.action, PreToolAction::ReplaceArguments);
        assert_eq!(response.output_optimization, OutputOptimization::Rtk);
        // Fixed key order after `env`, unquoted values when every byte is
        // alphanumeric or one of `/ _ - .` (`call/1` stays bare), single-quoted
        // values otherwise, then the RTK payload with its bare `rtk` token
        // replaced by the wrapper.
        let quoted_rtk = shell_quote(&rtk.to_string_lossy());
        assert_eq!(
            response.arguments["command"].as_str().unwrap(),
            format!(
                "env TOKENLESS_AGENT_ID=cosh TOKENLESS_SESSION_ID='session 1' \
                 TOKENLESS_TOOL_USE_ID=call/1 \
                 TOKENLESS_DATA_DIR='/var/lib/tokenless state' {quoted_rtk} git status"
            )
        );
    }

    #[test]
    fn pre_tool_reserves_supported_build_and_test_commands_for_post_tool() {
        let commands = [
            "cargo test --workspace",
            "RUST_BACKTRACE=1 /usr/bin/cargo check",
            "cd crate && cargo clippy",
            "python3.12 -m pytest tests/",
            "uv run python -m pytest -q",
            "npm test",
            "npm run --silent test:unit",
            "pnpm exec jest --runInBand",
            "npx jest",
            "go test ./... 2>&1 | tail -40",
            "make -j8",
        ];
        for command in commands {
            assert!(
                is_build_log_owned_command(command),
                "expected BuildLog ownership for {command:?}"
            );
        }

        for command in [
            "cargo metadata",
            "cargo fmt --all",
            "go env",
            "npm run lint",
            "cat build.log",
            "echo 'cargo test'",
            "rg pytest README.md",
        ] {
            assert!(
                !is_build_log_owned_command(command),
                "unexpected BuildLog ownership for {command:?}"
            );
        }
    }

    #[test]
    fn pre_tool_build_log_owner_does_not_require_rtk() {
        let request = PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": "cargo test --workspace"}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: true,
                block_and_suggest: false,
            },
        };
        let response = pre_tool_with_optional_rtk(
            &request,
            &Attribution::new("test"),
            None,
            None,
            RTK_TIMEOUT,
        )
        .unwrap();
        assert_eq!(response.action, PreToolAction::Passthrough);
        assert_eq!(response.output_optimization, OutputOptimization::None);
        assert_eq!(response.arguments, request.arguments);
    }

    #[test]
    fn pre_tool_anchor_preserves_quoted_arguments_and_handles_subshells() {
        let path = Path::new("/opt/tokenless/rtk");
        let data_dir = Path::new("/tenant/tokenless");
        let attribution = Attribution {
            agent_id: "agent".into(),
            session_id: Some("session".into()),
            tool_use_id: Some("call".into()),
        };
        let prefix = "env TOKENLESS_AGENT_ID=agent TOKENLESS_SESSION_ID=session TOKENLESS_TOOL_USE_ID=call TOKENLESS_DATA_DIR=/tenant/tokenless /opt/tokenless/rtk";
        assert_eq!(
            anchor_rtk_prefix(
                "grep -E 'foo | rtk bar' src && git status",
                "rtk grep -E 'foo | rtk bar' src && rtk git status",
                path,
                &attribution,
                data_dir,
            ),
            format!("{prefix} grep -E 'foo | rtk bar' src && {prefix} git status")
        );
        assert_eq!(
            anchor_rtk_prefix(
                "echo $(git status)",
                "echo $(rtk git status)",
                path,
                &attribution,
                data_dir,
            ),
            format!("echo $({prefix} git status)")
        );
        assert_eq!(
            anchor_rtk_prefix(
                "echo `git status`",
                "echo `rtk git status`",
                path,
                &attribution,
                data_dir,
            ),
            "echo `rtk git status`"
        );
        assert_eq!(
            anchor_rtk_prefix(
                "sudo git status",
                "sudo rtk git status",
                path,
                &attribution,
                data_dir,
            ),
            format!("sudo {prefix} git status")
        );
        assert_eq!(
            anchor_rtk_prefix(
                "RUST_BACKTRACE=1 cargo test",
                "RUST_BACKTRACE=1 rtk cargo test",
                path,
                &attribution,
                data_dir,
            ),
            format!("RUST_BACKTRACE=1 {prefix} cargo test")
        );
        assert_eq!(
            anchor_rtk_prefix(
                "sudo noglob git status",
                "sudo noglob rtk git status",
                path,
                &attribution,
                data_dir,
            ),
            format!("sudo noglob {prefix} git status")
        );
        assert_eq!(
            anchor_rtk_prefix(
                "shadowenv exec -- git status",
                "shadowenv exec -- rtk git status",
                path,
                &attribution,
                data_dir,
            ),
            format!("shadowenv exec -- {prefix} git status")
        );
        assert_eq!(
            anchor_rtk_prefix(
                "git status | grep rtk",
                "rtk git status | grep rtk",
                path,
                &attribution,
                data_dir,
            ),
            format!("{prefix} git status | grep rtk")
        );
        assert_eq!(
            anchor_rtk_prefix(
                "docker exec rtk git status",
                "docker exec rtk rtk git status",
                path,
                &attribution,
                data_dir,
            ),
            format!("docker exec rtk {prefix} git status")
        );
        assert_eq!(
            anchor_rtk_prefix("rg error", "rtk rg error", path, &attribution, data_dir),
            format!("{prefix} rg error")
        );
    }

    #[test]
    fn pre_tool_no_op_does_not_require_rtk() {
        let requests = [
            PreToolRequest {
                tool_name: "Read".into(),
                arguments: json!({"path": "README.md"}),
                command_field: "command".into(),
                capabilities: PreToolCapabilities {
                    replace_arguments: true,
                    block_and_suggest: false,
                },
            },
            PreToolRequest {
                tool_name: "Bash".into(),
                arguments: Value::String("not an object".into()),
                command_field: "command".into(),
                capabilities: PreToolCapabilities {
                    replace_arguments: true,
                    block_and_suggest: false,
                },
            },
            PreToolRequest {
                tool_name: "Bash".into(),
                arguments: json!({"command": "git status"}),
                command_field: "command".into(),
                capabilities: PreToolCapabilities {
                    replace_arguments: false,
                    block_and_suggest: false,
                },
            },
        ];
        for request in requests {
            let outcome = dispatch_with_store(
                &RequestEnvelope {
                    attribution: Attribution::new("test"),
                    request: Request::PreTool(request.clone()),
                },
                &options(),
                None,
                None,
            )
            .unwrap();
            let Response::PreTool(response) = outcome.response.response else {
                unreachable!("the request operation fixes the response variant")
            };
            assert_eq!(response.action, PreToolAction::Passthrough);
            assert_eq!(response.arguments, request.arguments);
        }

        let applicable = PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": "git status"}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: true,
                block_and_suggest: false,
            },
        };
        assert!(matches!(
            pre_tool_with_optional_rtk(
                &applicable,
                &Attribution::new("test"),
                None,
                None,
                RTK_TIMEOUT
            ),
            Err(RuntimeError::RtkUnavailable)
        ));
    }

    #[test]
    fn pre_tool_honors_rtk_exit_contract_and_preserves_arguments() {
        let directory = tempdir().unwrap();
        let request = PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": "grep error log", "timeout": 30}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: true,
                block_and_suggest: false,
            },
        };
        for code in [1, 2] {
            let rtk = directory.path().join(format!("rtk-{code}"));
            write_executable(&rtk, &format!("#!/bin/sh\nprintf 'changed'\nexit {code}\n"));
            let response =
                pre_tool_with_rtk(&request, &Attribution::new("test"), &rtk, directory.path())
                    .unwrap();
            assert_eq!(response.action, PreToolAction::Passthrough);
            assert_eq!(response.arguments, request.arguments);
        }
        for (name, output) in [("empty", ""), ("unchanged", "grep error log")] {
            let rtk = directory.path().join(format!("rtk-{name}"));
            write_executable(&rtk, &format!("#!/bin/sh\nprintf '%s' '{output}'\n"));
            let response =
                pre_tool_with_rtk(&request, &Attribution::new("test"), &rtk, directory.path())
                    .unwrap();
            assert_eq!(response.action, PreToolAction::Passthrough);
            assert_eq!(response.arguments, request.arguments);
        }

        let rtk = directory.path().join("rtk-3");
        write_executable(&rtk, "#!/bin/sh\nprintf 'optimized command'\nexit 3\n");
        let response =
            pre_tool_with_rtk(&request, &Attribution::new("test"), &rtk, directory.path()).unwrap();
        assert_eq!(response.action, PreToolAction::ReplaceArguments);
        assert_eq!(response.output_optimization, OutputOptimization::Rtk);
        assert_eq!(response.arguments["command"], "optimized command");
        assert_eq!(response.arguments["timeout"], 30);
    }

    #[test]
    fn pre_tool_passes_attribution_and_rejects_unexpected_exit() {
        let directory = tempdir().unwrap();
        let request = PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": "original"}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: false,
                block_and_suggest: true,
            },
        };
        let rtk = directory.path().join("rtk-env");
        write_executable(
            &rtk,
            "#!/bin/sh\nprintf '%s:%s:%s:%s' \"$TOKENLESS_AGENT_ID\" \"$TOKENLESS_SESSION_ID\" \"$TOKENLESS_TOOL_USE_ID\" \"$TOKENLESS_DATA_DIR\"\n",
        );
        let attribution = Attribution {
            agent_id: "agent".into(),
            session_id: Some("session".into()),
            tool_use_id: Some("call".into()),
        };
        let response = pre_tool_with_rtk(&request, &attribution, &rtk, directory.path()).unwrap();
        assert_eq!(response.action, PreToolAction::BlockAndSuggest);
        assert_eq!(
            response.arguments["command"],
            format!("agent:session:call:{}", directory.path().display())
        );

        let unexpected = directory.path().join("rtk-9");
        write_executable(&unexpected, "#!/bin/sh\nexit 9\n");
        assert!(matches!(
            pre_tool_with_rtk(&request, &attribution, &unexpected, directory.path()),
            Err(RuntimeError::RtkUnexpectedExit { code: 9 })
        ));
        assert!(matches!(
            pre_tool_with_rtk(
                &request,
                &attribution,
                &directory.path().join("missing-rtk"),
                directory.path(),
            ),
            Err(RuntimeError::RtkSpawn { .. })
        ));
    }

    #[test]
    fn pre_tool_timeout_is_an_operation_error() {
        let directory = tempdir().unwrap();
        let rtk = directory.path().join("rtk-slow");
        write_executable(&rtk, "#!/bin/sh\nsleep 1\n");
        let request = PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": "original"}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: true,
                block_and_suggest: false,
            },
        };
        assert!(matches!(
            pre_tool_with_optional_rtk(
                &request,
                &Attribution::new("test"),
                Some(&rtk),
                Some(directory.path()),
                Duration::from_millis(20)
            ),
            Err(RuntimeError::RtkTimeout)
        ));
    }

    #[test]
    fn pre_tool_drains_large_rtk_output_before_exit() {
        let directory = tempdir().unwrap();
        let rtk = directory.path().join("rtk-large-output");
        write_executable(
            &rtk,
            &format!("#!/bin/sh\nprintf 'optimized {}'\n", "x".repeat(256 * 1024)),
        );
        let request = PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": "original"}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: true,
                block_and_suggest: false,
            },
        };
        let response = pre_tool_with_optional_rtk(
            &request,
            &Attribution::new("test"),
            Some(&rtk),
            Some(directory.path()),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(response.action, PreToolAction::ReplaceArguments);
        assert!(response.arguments["command"].as_str().unwrap().len() > 256 * 1024);
    }

    #[test]
    fn before_model_without_retrieve_capability_keeps_schema_unchanged() {
        let request = BeforeModelRequest {
            tools: vec![json!({
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "long description ".repeat(200),
                    "parameters": {"type": "object", "properties": {}}
                }
            })],
            visible_context: json!({"messages": []}),
            capabilities: BeforeModelCapabilities {
                replace_tools: true,
                recovery: tokenless_protocol::RecoveryMethod::None,
            },
        };
        let outcome = before_model_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.tools, request.tools);
        assert!(outcome.response.visible_markers.is_empty());
        assert_eq!(
            outcome.stats.disposition,
            Disposition::RecoverabilityUnavailable
        );
        assert!(outcome.stats.applied_operations.is_empty());
    }

    #[test]
    fn before_model_returns_sorted_markers_with_recovery_available() {
        let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
        let request = BeforeModelRequest {
            tools: vec![json!({
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "long description ".repeat(200),
                    "parameters": {"type": "object", "properties": {}}
                }
            })],
            visible_context: json!({
                "messages": [
                    "<<tokenless:ABCDEF0123456789ABCDEF01>>",
                    "<<tokenless:abcdef0123456789abcdef01>>"
                ]
            }),
            capabilities: BeforeModelCapabilities {
                replace_tools: true,
                recovery: RecoveryMethod::tool("tokenless_retrieve").unwrap(),
            },
        };
        let outcome = before_model_with_store(&request, &options(), Some(&store)).unwrap();
        assert!(
            outcome
                .response
                .visible_markers
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
        assert_eq!(
            outcome
                .response
                .visible_markers
                .iter()
                .filter(|hash| *hash == "abcdef0123456789abcdef01")
                .count(),
            1
        );
        assert!(!outcome.artifact_keys.is_empty());
    }

    #[test]
    fn before_model_obeys_replace_capability_without_owning_tool_names() {
        let tool = json!({
            "type": "function",
            "function": {
                "name": "tokenless_retrieve",
                "description": "description ".repeat(100),
                "parameters": {"type": "object"}
            }
        });
        let mut request = BeforeModelRequest {
            tools: vec![tool.clone()],
            visible_context: json!({}),
            capabilities: BeforeModelCapabilities {
                replace_tools: false,
                recovery: tokenless_protocol::RecoveryMethod::None,
            },
        };
        let outcome = before_model_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.tools, vec![tool]);
        assert_eq!(outcome.stats.disposition, Disposition::Passthrough);

        request.capabilities.recovery = RecoveryMethod::tool("tokenless_retrieve").unwrap();
        let outcome = before_model_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.tools, request.tools);
    }

    #[test]
    fn search_path_sharing_records_stats_and_respects_rtk_ownership() {
        let content = "crates/long_directory/src/file.rs:12:matching source text\n".repeat(20);
        let mut request = post_tool_request(&content);
        request.tool_name = "Grep".into();
        request.content_origin = ContentOrigin::ApiResponse;
        let outcome = post_tool_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::Applied);
        assert_eq!(
            outcome.stats.applied_operations,
            [AppliedOperation::SearchPathSharing]
        );
        assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
        assert!(outcome.response.stash_keys.is_empty());

        request.tool_name = "Bash".into();
        request.content_origin = ContentOrigin::CommandOutput;
        for optimization in [OutputOptimization::None, OutputOptimization::Rtk] {
            request.output_optimization = optimization;
            let outcome = post_tool_with_store(&request, &options(), None).unwrap();
            assert_eq!(outcome.response.disposition, Disposition::Passthrough);
            assert_eq!(outcome.response.output, content);
            assert!(outcome.stats.applied_operations.is_empty());
        }
    }

    #[test]
    fn disabling_search_path_sharing_keeps_other_tools_compressible() {
        let mut options = options();
        options.search_path_sharing_enabled = false;
        for tool_name in ["Grep", "SearchFiles"] {
            let content = "crates/long_directory/src/file.rs:12:matching source text\n".repeat(20);
            let mut request = post_tool_request(&content);
            request.tool_name = tool_name.into();
            request.content_origin = ContentOrigin::ApiResponse;
            let outcome = post_tool_with_store(&request, &options, None).unwrap();
            assert_eq!(outcome.response.output, content);
            assert_eq!(outcome.response.disposition, Disposition::Passthrough);
            assert!(outcome.stats.applied_operations.is_empty());
            assert!(outcome.response.stash_keys.is_empty());
        }
        let input = serde_json::json!({"debug": "x".repeat(1000), "value": 1}).to_string();
        let mut request = post_tool_request(&input);
        request.tool_name = "SearchFiles".into();
        request.content_origin = ContentOrigin::ApiResponse;
        let outcome = post_tool_with_store(&request, &options, None).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::Applied);
        assert!(!outcome.stats.applied_operations.is_empty());
    }

    #[test]
    fn rtk_and_retrieve_results_bypass_post_tool_pipeline() {
        for (kind, optimization) in [
            (ResultKind::Tool, OutputOptimization::Rtk),
            (ResultKind::Retrieve, OutputOptimization::None),
        ] {
            let request = PostToolRequest {
                result_kind: kind,
                tool_name: "Bash".into(),
                content: r#"{"debug":"remove me","value":1}"#.into(),
                status: ToolResultStatus::Success,
                content_origin: ContentOrigin::CommandOutput,
                output_optimization: optimization,
                capabilities: PostToolCapabilities {
                    replace_output: true,
                    recovery: tokenless_protocol::RecoveryMethod::None,
                    replace_with_text: true,
                },
            };
            let outcome = post_tool_with_store(&request, &options(), None).unwrap();
            assert_eq!(outcome.response.disposition, Disposition::Passthrough);
            assert!(outcome.response.applied_operations.is_empty());
        }
    }

    #[test]
    fn post_tool_routes_statuses_before_the_json_pipeline() {
        for status in [ToolResultStatus::Interrupted, ToolResultStatus::Denied] {
            let request = post_tool_request(r#"{"debug":"remove me","value":1}"#);
            let request = PostToolRequest { status, ..request };
            let outcome = post_tool_with_store(&request, &options(), None).unwrap();
            assert_eq!(outcome.response.disposition, Disposition::Passthrough);
            assert_eq!(outcome.response.output, request.content);
        }

        let request = PostToolRequest {
            status: ToolResultStatus::Error,
            content: "/bin/sh: jq: command not found".into(),
            ..post_tool_request("unused")
        };
        let outcome = post_tool_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::ToolError);
        assert_eq!(outcome.response.output, request.content);
        assert!(
            outcome
                .response
                .additional_context
                .unwrap()
                .contains("ENV_DEPENDENCY_MISSING")
        );
    }

    fn failing_build_log() -> String {
        let mut output = "$ cargo build\n".to_owned();
        for index in 0..30 {
            output.push_str(&format!(
                "Compiling package-{index:03} v0.1.{index} with extended progress output\n"
            ));
        }
        output.push_str("error: linker command not found\n");
        output
    }

    #[test]
    fn tool_error_build_log_stays_original_even_when_recovery_is_available() {
        let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
        let request = PostToolRequest {
            status: ToolResultStatus::Error,
            content: failing_build_log(),
            capabilities: PostToolCapabilities {
                replace_output: true,
                recovery: tokenless_protocol::RecoveryMethod::Shell,
                replace_with_text: true,
            },
            ..post_tool_request("unused")
        };

        let outcome = post_tool_with_store(&request, &options(), Some(&store)).unwrap();

        assert_eq!(outcome.response.disposition, Disposition::ToolError);
        assert_eq!(outcome.response.content_type, Some(ContentType::BuildLog));
        assert_eq!(outcome.response.output, request.content);
        assert!(outcome.response.applied_operations.is_empty());
        assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
        assert!(
            outcome
                .response
                .additional_context
                .as_deref()
                .unwrap()
                .contains("ENV_DEPENDENCY_MISSING")
        );
    }

    #[test]
    fn tool_error_without_recovery_keeps_the_original_build_log() {
        let request = PostToolRequest {
            status: ToolResultStatus::Error,
            content: failing_build_log(),
            ..post_tool_request("unused")
        };
        let outcome = post_tool_with_store(&request, &options(), None).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::ToolError);
        assert_eq!(outcome.response.output, request.content);
        assert!(outcome.response.applied_operations.is_empty());
    }

    #[test]
    fn post_tool_accepts_lossless_json_and_requires_retrieve_for_truncation() {
        let cleanup = post_tool_request(&format!(
            r#"{{"debug":"{}","value":"kept"}}"#,
            "noise".repeat(100)
        ));
        let outcome = post_tool_with_store(&cleanup, &options(), None).unwrap();
        assert_eq!(outcome.response.disposition, Disposition::Applied);
        assert_eq!(outcome.response.recoverability, Recoverability::Lossless);
        assert_eq!(
            outcome.response.applied_operations,
            vec![AppliedOperation::JsonCleanup]
        );

        let lossy =
            post_tool_request(&serde_json::to_string(&(0..300).collect::<Vec<_>>()).unwrap());
        let unavailable_store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
        let rejected = post_tool_with_store(&lossy, &options(), Some(&unavailable_store)).unwrap();
        assert_eq!(
            rejected.response.disposition,
            Disposition::RecoverabilityUnavailable
        );
        assert_eq!(rejected.response.output, lossy.content);
        assert!(unavailable_store.is_empty());

        let failing: Arc<dyn StashStore> = Arc::new(FailingStore);
        let failing_request = PostToolRequest {
            capabilities: PostToolCapabilities {
                recovery: tokenless_protocol::RecoveryMethod::Shell,
                ..lossy.capabilities
            },
            ..lossy.clone()
        };
        assert!(matches!(
            post_tool_with_store(&failing_request, &options(), Some(&failing)),
            Err(RuntimeError::StashWrite { count }) if count > 0
        ));

        let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
        let recoverable = PostToolRequest {
            capabilities: PostToolCapabilities {
                recovery: tokenless_protocol::RecoveryMethod::Shell,
                ..lossy.capabilities
            },
            ..lossy
        };
        let applied = post_tool_with_store(&recoverable, &options(), Some(&store)).unwrap();
        assert_eq!(applied.response.disposition, Disposition::Applied);
        assert_eq!(applied.response.recoverability, Recoverability::Retrievable);
        assert!(!applied.response.stash_keys.is_empty());
    }

    fn post_tool_request(content: &str) -> PostToolRequest {
        PostToolRequest {
            result_kind: ResultKind::Tool,
            tool_name: "Bash".into(),
            content: content.into(),
            status: ToolResultStatus::Success,
            content_origin: ContentOrigin::CommandOutput,
            output_optimization: OutputOptimization::None,
            capabilities: PostToolCapabilities {
                replace_output: true,
                recovery: tokenless_protocol::RecoveryMethod::None,
                replace_with_text: true,
            },
        }
    }
}
