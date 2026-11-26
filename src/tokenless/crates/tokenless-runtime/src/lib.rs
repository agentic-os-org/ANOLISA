//! Stateful application API shared by Tokenless frontends.
//!
//! The runtime owns lifecycle orchestration, including the PostTool pipeline,
//! reversible SQLite stash, and statistics, without depending on a
//! command-line or language-binding layer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokenless_ccr::{SqliteStore, StashStore, extract_hash, is_valid_hash};
use tokenless_compressors::{JsonCompressionConfig, numbers_are_exact};
use tokenless_protocol::{
    Attribution as ProtocolAttribution, BeforeModelRequest, BeforeModelResponse, ContentOrigin,
    OutputOptimization, PostToolCapabilities, PostToolRequest, PostToolResponse, ResultKind,
    RetrieveRequest, RetrieveResponse, ToolResultStatus,
};
use tokenless_schema::SchemaCompressor;
use tokenless_stats::{
    CompressionMode, OperationType, SlsWriter, StatsRecord, StatsRecorder, TokenlessConfig,
    ensure_state_dir, estimate_tokens, get_home_dir, resolve_data_dir, validate_data_dir,
    validate_database_path,
};

mod entry;
mod post_tool;
mod taxonomy;

pub use entry::{EntryOptions, EntryOutcome, dispatch_with_store, response_operation};
use entry::{before_model_with_store, post_tool_with_outcomes, retrieve_authorized_with_store};
use post_tool::{PostToolPipeline, PostToolPipelineConfig};

/// Why a compression attempt did or did not replace the input: the protocol
/// disposition vocabulary, re-exported verbatim so CLI, Runtime, and
/// language bindings share one set of names and wire strings (roadmap §5.6).
pub use tokenless_protocol::Disposition;

/// Maximum accepted response size, matching the standalone CLI input limit.
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Minimum payload length (in characters) before TOON encoding runs.
///
/// TOON on small JSON saves only a few characters (observed ~0.3% below
/// ~500 chars) while the per-event encode cost stays the same, so smaller
/// payloads pass through untouched. Keeping the threshold here lets the
/// `compress-toon` CLI and the [`TokenlessRuntime::compress_toon`] SDK
/// path behave exactly like the adapter hook layer, which skips payloads
/// under the same threshold before invoking the CLI.
pub const MIN_TOON_CHARS: usize = 500;

/// Runtime construction options for state and observability.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Explicit state directory. `None` uses `TOKENLESS_DATA_DIR`, then the
    /// passwd-backed home directory's `.tokenless` child.
    pub data_dir: Option<PathBuf>,
    /// Whether successful compression savings are stored in `stats.db`.
    pub stats_enabled: bool,
    /// Whether successful compression savings are emitted to the SLS writer.
    pub sls_enabled: bool,
    /// Whether compressed output is returned. Disabled mode calculates and
    /// records predicted savings but returns the original input.
    pub compression_enabled: bool,
    /// Whether search records and non-file path lists share paths. Enabled by default.
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
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            data_dir: None,
            stats_enabled: true,
            sls_enabled: false,
            compression_enabled: true,
            search_path_sharing_enabled: true,
            diff_compression_enabled: true,
            html_extraction_enabled: true,
            browser_snapshot_enabled: true,
            mixed_content_enabled: false,
        }
    }
}

/// Per-call response compression controls.
#[derive(Debug, Clone)]
pub struct CompressOptions {
    /// Maximum string character count before truncation.
    pub truncate_strings_at: Option<usize>,
    /// Maximum array item count before truncation.
    pub truncate_arrays_at: Option<usize>,
    /// Items preserved from the tail of truncated arrays.
    ///
    /// When set, truncated arrays keep this many trailing items in addition
    /// to the head, with a truncation marker between them. `None` uses the
    /// compressor default.
    pub array_tail_preserve: Option<usize>,
    /// Maximum JSON nesting depth before truncation.
    pub max_depth: Option<usize>,
    /// Whether reversible stash markers may be created.
    pub stash_enabled: bool,
    /// Preserve the original response when stash is unavailable or a write
    /// fails. Framework adapters should enable this to avoid lossy fallback.
    pub require_reversible: bool,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            truncate_strings_at: None,
            truncate_arrays_at: None,
            array_tail_preserve: None,
            max_depth: None,
            stash_enabled: true,
            require_reversible: false,
        }
    }
}

/// Identifiers attached to one compression statistics record.
#[derive(Debug, Clone)]
pub struct Attribution {
    /// Agent or integration identifier.
    pub agent_id: String,
    /// Optional conversation/session identifier.
    pub session_id: Option<String>,
    /// Optional tool-call identifier.
    pub tool_use_id: Option<String>,
}

impl Attribution {
    /// Create attribution for an agent without session or tool-call IDs.
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: None,
            tool_use_id: None,
        }
    }
}

/// Structured response from one compression attempt.
#[derive(Debug, Clone)]
pub struct CompressResult {
    /// Text that the caller should pass to the model.
    pub output: String,
    /// Compact candidate calculated before dry-run or fail-open policy; the
    /// original input when no candidate ran (passthrough, timeout, error).
    /// Retained as the legacy measurement channel — dry-run statistics
    /// record the predicted candidate from it — and scheduled for removal
    /// with the statistics migration (roadmap §5.5).
    pub compressed_output: String,
    /// Policy decision applied to the candidate.
    pub disposition: Disposition,
    /// Estimated tokens in the original input.
    pub before_tokens: usize,
    /// Estimated tokens in `compressed_output`.
    pub after_tokens: usize,
    /// Number of successful stash writes, or `None` when no store was attached.
    pub stash_writes: Option<usize>,
    /// Number of failed stash writes, or `None` when no store was attached.
    pub stash_errors: Option<usize>,
    /// Number of truncations without a retrievable marker, or `None` when no
    /// store was attached.
    pub unrecoverable_truncations: Option<usize>,
    /// Live stash entry count after compression, or `None` without a store.
    pub stash_size: Option<usize>,
}

impl CompressResult {
    /// Whether the caller-visible output is the compressed candidate.
    pub fn applied(&self) -> bool {
        self.disposition == Disposition::Applied
    }
}

/// Errors surfaced by the reusable runtime API.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RuntimeError {
    /// Input exceeded the public runtime limit.
    #[error("input exceeds {limit_mib} MiB limit")]
    InputTooLarge {
        /// Configured limit in mebibytes.
        limit_mib: usize,
    },
    /// Response input was not JSON.
    #[error("JSON parse error: {0}")]
    InvalidJson(#[from] serde_json::Error),
    /// State directory or database path violated the shared path policy.
    #[error("invalid Tokenless state path: {0}")]
    InvalidStatePath(#[from] tokenless_stats::path_policy::PathPolicyError),
    /// State directory creation failed.
    #[error("failed to create Tokenless state directory '{}': {source}", path.display())]
    CreateStateDirectory {
        /// Directory that could not be created.
        path: PathBuf,
        /// Filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// A response could not be serialized after compression.
    #[error("failed to serialize compressed response: {0}")]
    Serialize(serde_json::Error),
    /// A JSON value could not be encoded as TOON.
    #[error("TOON encode failed: {0}")]
    ToonEncode(String),
    /// Retrieve input was neither a marker nor a bare 24-character hash.
    #[error("invalid stash hash: {value:?} (expected 24 hex chars or a <<tokenless:HASH>> marker)")]
    InvalidHash {
        /// Rejected retrieve input.
        value: String,
    },
    /// The stash backend is unavailable.
    #[error("stash unavailable: {0}")]
    StashUnavailable(String),
    /// The requested stash entry does not exist or has expired.
    #[error("no stashed payload for hash: {hash}")]
    StashEntryNotFound {
        /// Normalized stash hash.
        hash: String,
    },
    /// The stash backend failed while retrieving an entry.
    #[error("stash retrieve failed: {0}")]
    StashRetrieve(String),
    /// One or more lifecycle Stash writes failed.
    #[error("{count} stash write operation(s) failed")]
    StashWrite {
        /// Number of failed writes in this operation.
        count: usize,
    },
    /// The requested marker is not visible to the calling model.
    #[error("stash hash is not authorized by a visible marker: {hash}")]
    RetrieveUnauthorized {
        /// Normalized hash rejected before the stash read.
        hash: String,
    },
    /// The PostTool domain compressor failed.
    #[error("{0}")]
    Pipeline(String),
    /// PreTool was requested without the executable that runs rewritten RTK commands.
    #[error("Tokenless executable for RTK commands is unavailable")]
    RtkLauncherUnavailable,
    /// PreTool was requested without the state directory required by RTK.
    #[error("RTK state directory is unavailable")]
    RtkDataDirectoryUnavailable,
}

/// Stateful Tokenless service for in-process callers.
pub struct TokenlessRuntime {
    config: RuntimeConfig,
    data_dir: PathBuf,
    stash_store: Option<Arc<dyn StashStore>>,
    stash_error: Option<String>,
    stats_recorder: Option<StatsRecorder>,
    stats_error: Option<String>,
}

impl TokenlessRuntime {
    /// Open reusable stash and statistics state.
    ///
    /// Invalid directory paths fail construction. SQLite open failures remain
    /// observable through `stash_error()` / `stats_error()` so compression can
    /// follow its configured fail-open policy.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the state directory violates the shared
    /// path policy or cannot be created.
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        let data_dir = resolve_runtime_data_dir(config.data_dir.as_deref())?;
        ensure_state_dir(&data_dir).map_err(|source| RuntimeError::CreateStateDirectory {
            path: data_dir.clone(),
            source,
        })?;

        let stash_path = validate_database_path(&data_dir.join("stash.db"), &[&data_dir])?;
        let (stash_store, stash_error) = match SqliteStore::new(&stash_path) {
            Ok(store) => (Some(Arc::new(store) as Arc<dyn StashStore>), None),
            Err(error) => (
                None,
                Some(format!(
                    "cannot open stash db at {}: {error}",
                    stash_path.display(),
                )),
            ),
        };

        let (stats_recorder, stats_error) = if config.stats_enabled {
            let stats_path = validate_database_path(&data_dir.join("stats.db"), &[&data_dir])?;
            match StatsRecorder::with_retention(
                &stats_path,
                TokenlessConfig::load().retention_policy(),
            ) {
                Ok(recorder) => (Some(recorder), None),
                Err(error) => (
                    None,
                    Some(format!(
                        "cannot open stats db at {}: {error}",
                        stats_path.display(),
                    )),
                ),
            }
        } else {
            (None, None)
        };

        Ok(Self {
            config,
            data_dir,
            stash_store,
            stash_error,
            stats_recorder,
            stats_error,
        })
    }

    /// Compress one JSON response and record any effective savings.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for oversized or invalid JSON input.
    pub fn compress_response(
        &self,
        input: &str,
        options: &CompressOptions,
        attribution: &Attribution,
    ) -> Result<CompressResult, RuntimeError> {
        let result = compress_response_with_store(
            input,
            options,
            self.config.compression_enabled,
            self.stash_store.as_ref(),
        )?;
        self.record_stats(OperationType::CompressResponse, input, &result, attribution);
        Ok(result)
    }

    /// Compress a Function Calling schema or top-level `tools` request with reversible markers.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for oversized or invalid JSON input, or if the
    /// compressed schema cannot be serialized.
    pub fn compress_schema(
        &self,
        input: &str,
        attribution: &Attribution,
    ) -> Result<CompressResult, RuntimeError> {
        let result = compress_schema_with_store(
            input,
            self.config.compression_enabled,
            self.stash_store.as_ref(),
        )?;
        self.record_stats(OperationType::CompressSchema, input, &result, attribution);
        Ok(result)
    }

    /// Encode one JSON value as TOON when doing so reduces estimated tokens.
    ///
    /// Payloads shorter than [`MIN_TOON_CHARS`] characters pass through
    /// unchanged, matching the adapter hook layer.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for oversized or invalid JSON input, or when
    /// the TOON encoder rejects the value.
    pub fn compress_toon(
        &self,
        input: &str,
        attribution: &Attribution,
    ) -> Result<CompressResult, RuntimeError> {
        let result = compress_toon(input, self.config.compression_enabled, MIN_TOON_CHARS)?;
        self.record_stats(OperationType::CompressToon, input, &result, attribution);
        Ok(result)
    }

    /// Runs the BeforeModel lifecycle service.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for invalid tool declarations, state errors,
    /// or a conflicting Retrieve declaration.
    pub fn before_model(
        &self,
        request: &BeforeModelRequest,
        attribution: &ProtocolAttribution,
    ) -> Result<BeforeModelResponse, RuntimeError> {
        let options = EntryOptions {
            compression_enabled: self.config.compression_enabled,
            search_path_sharing_enabled: self.config.search_path_sharing_enabled,
            diff_compression_enabled: self.config.diff_compression_enabled,
            html_extraction_enabled: self.config.html_extraction_enabled,
            browser_snapshot_enabled: self.config.browser_snapshot_enabled,
            mixed_content_enabled: self.config.mixed_content_enabled,
            stash_enabled: true,
            rtk_launcher: None,
            rtk_data_dir: None,
        };
        let outcome = before_model_with_store(request, &options, self.stash_store.as_ref())?;
        record_entry_stats(
            attribution,
            &outcome.stats,
            outcome.stash_writes,
            outcome.stash_errors,
            outcome.stash_size,
            &outcome.artifact_keys,
            self.stats_recorder.as_ref(),
            self.config.sls_enabled,
        );
        Ok(outcome.response)
    }

    /// Runs PostTool routing and the content-domain pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the selected pipeline fails.
    pub fn post_tool(
        &self,
        request: &PostToolRequest,
        attribution: &ProtocolAttribution,
    ) -> Result<PostToolResponse, RuntimeError> {
        let options = EntryOptions {
            compression_enabled: self.config.compression_enabled,
            search_path_sharing_enabled: self.config.search_path_sharing_enabled,
            diff_compression_enabled: self.config.diff_compression_enabled,
            html_extraction_enabled: self.config.html_extraction_enabled,
            browser_snapshot_enabled: self.config.browser_snapshot_enabled,
            mixed_content_enabled: self.config.mixed_content_enabled,
            stash_enabled: true,
            rtk_launcher: None,
            rtk_data_dir: Some(self.data_dir.clone()),
        };
        let outcome =
            post_tool_with_outcomes(request, &options, self.stash_store.as_ref(), attribution)?;
        record_entry_stats(
            attribution,
            &outcome.stats,
            outcome.stash_writes,
            outcome.stash_errors,
            outcome.stash_size,
            &outcome.response.stash_keys,
            self.stats_recorder.as_ref(),
            self.config.sls_enabled,
        );
        Ok(outcome.response)
    }

    /// Restores a stash entry only when its marker is currently visible.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for malformed, unauthorized, unavailable, or
    /// missing stash entries.
    pub fn retrieve_authorized(
        &self,
        request: &RetrieveRequest,
        attribution: &ProtocolAttribution,
    ) -> Result<RetrieveResponse, RuntimeError> {
        retrieve_authorized_with_store(
            request,
            self.stash_store.as_ref(),
            self.stats_recorder.as_ref(),
            attribution,
            "embedded",
        )
    }

    /// Retrieve a payload by bare hash or text containing a marker.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for malformed input, unavailable state, a
    /// missing/expired entry, or a backend failure.
    pub fn retrieve(&self, hash_or_marker: &str) -> Result<String, RuntimeError> {
        let store = self.stash_store.as_deref().ok_or_else(|| {
            RuntimeError::StashUnavailable(
                self.stash_error
                    .clone()
                    .unwrap_or_else(|| "stash is not configured".to_string()),
            )
        })?;
        retrieve_recorded(
            store,
            hash_or_marker,
            self.stats_recorder.as_ref(),
            "embedded",
        )
    }

    /// Validated state directory used by this runtime.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Whether the SQLite stash opened successfully.
    pub fn stash_available(&self) -> bool {
        self.stash_store.is_some()
    }

    /// Stash initialization failure, if any.
    pub fn stash_error(&self) -> Option<&str> {
        self.stash_error.as_deref()
    }

    /// Whether statistics were requested and their SQLite recorder opened.
    pub fn stats_available(&self) -> bool {
        self.stats_recorder.is_some()
    }

    /// Statistics initialization failure, if any.
    pub fn stats_error(&self) -> Option<&str> {
        self.stats_error.as_deref()
    }

    fn record_stats(
        &self,
        operation: OperationType,
        input: &str,
        result: &CompressResult,
        attribution: &Attribution,
    ) {
        if !self.config.stats_enabled && !self.config.sls_enabled {
            return;
        }
        let (after, after_tokens) = match result.disposition {
            Disposition::Applied | Disposition::DryRun => {
                (result.compressed_output.as_str(), result.after_tokens)
            }
            _ => (input, result.before_tokens),
        };
        let before_tokens = result.before_tokens;
        if after_tokens >= before_tokens {
            return;
        }

        let mode = if self.config.compression_enabled {
            CompressionMode::Active
        } else {
            CompressionMode::DryRun
        };
        let mut record = StatsRecord::new(
            operation,
            attribution.agent_id.clone(),
            input.len(),
            before_tokens,
            after.len(),
            after_tokens,
        )
        .with_before_text(input.to_string())
        .with_after_text(after.to_string())
        .with_source_pid(std::process::id() as i64)
        .with_mode(mode)
        .with_stash(result.stash_writes, result.stash_errors, result.stash_size);
        if let Some(session_id) = &attribution.session_id {
            record = record.with_session_id(session_id.clone());
        }
        if let Some(tool_use_id) = &attribution.tool_use_id {
            record = record.with_tool_use_id(tool_use_id.clone());
        }

        if let Some(recorder) = &self.stats_recorder
            && let Err(e) = recorder.record(&record)
        {
            warn_soft(&format!(
                "[tokenless-stats] WARNING: failed to record stats entry: {e}",
            ));
        }
        if self.config.sls_enabled {
            SlsWriter::new().write(&record);
        }
    }
}

/// Compress a Function Calling schema or top-level `tools` request using an optional stash store.
///
/// # Errors
///
/// Returns [`RuntimeError`] for oversized or invalid JSON input, or if the
/// compressed schema cannot be serialized.
pub fn compress_schema_with_store(
    input: &str,
    compression_enabled: bool,
    stash_store: Option<&Arc<dyn StashStore>>,
) -> Result<CompressResult, RuntimeError> {
    validate_input_size(input)?;
    let value: serde_json::Value = serde_json::from_str(input)?;
    let attached_store = if compression_enabled {
        stash_store
    } else {
        None
    };
    let mut compressor = SchemaCompressor::new();
    if let Some(store) = attached_store {
        compressor = compressor.with_stash_store(Arc::clone(store));
    }
    let compressed = compressor.compress(&value);
    let compressed_output = serde_json::to_string(&compressed).map_err(RuntimeError::Serialize)?;
    Ok(finish_schema_compression(
        input,
        compressed_output,
        compression_enabled,
        attached_store,
        &compressor,
    ))
}

/// Applies the schema disposition ladder to an already-computed candidate:
/// no-savings by tokens, dry-run, reversibility-unavailable when no store is
/// attached or a stash write failed, then applied — rolling back or
/// committing the compressor's stash session accordingly. Shared by
/// [`compress_schema_with_store`] and the unified entry router so the
/// hand-rolled ladder exists once.
fn finish_schema_compression(
    input: &str,
    compressed_output: String,
    compression_enabled: bool,
    attached_store: Option<&Arc<dyn StashStore>>,
    compressor: &SchemaCompressor,
) -> CompressResult {
    let before_tokens = estimate_tokens(input);
    let after_tokens = estimate_tokens(&compressed_output);
    let compression_stash_errors = attached_store.map(|_| compressor.stash_errors());
    let disposition = if after_tokens >= before_tokens {
        Disposition::NoSavings
    } else if !compression_enabled {
        Disposition::DryRun
    } else if attached_store.is_none() || compression_stash_errors.is_some_and(|count| count > 0) {
        Disposition::RecoverabilityUnavailable
    } else {
        Disposition::Applied
    };
    let (stash_writes, stash_errors) = if disposition != Disposition::Applied {
        compressor.rollback_stash_writes();
        (
            attached_store.map(|_| compressor.stash_writes()),
            attached_store.map(|_| compressor.stash_errors()),
        )
    } else {
        let metrics = (
            attached_store.map(|_| compressor.stash_writes()),
            attached_store.map(|_| compressor.stash_errors()),
        );
        compressor.clear_stash_session();
        metrics
    };
    let stash_size = attached_store.map(|store| store.len());
    let output = if disposition == Disposition::Applied {
        compressed_output.clone()
    } else {
        input.to_string()
    };
    CompressResult {
        output,
        compressed_output,
        disposition,
        before_tokens,
        after_tokens,
        stash_writes,
        stash_errors,
        unrecoverable_truncations: None,
        stash_size,
    }
}

/// Encode JSON as TOON and apply the shared minimum-length gate,
/// numeric fidelity gate, no-savings, and dry-run policies.
///
/// Payloads with fewer than `min_toon_chars` characters pass through
/// unchanged as [`Disposition::Passthrough`] because TOON savings on small
/// JSON are near-zero (see [`MIN_TOON_CHARS`], the shared default used by
/// the CLI and the adapter hooks). Pass `0` to encode any payload that
/// yields token savings, restoring the pre-gate behavior. The input is
/// still validated as JSON, so short invalid payloads fail instead of
/// passing through.
///
/// A payload whose numbers TOON cannot carry also passes through unchanged
/// as [`Disposition::Passthrough`], mirroring the two guards the PostTool
/// JSON domain applies before TOON may replace exact JSON. Size alone never
/// authorizes a lossy encoding.
///
/// # Errors
///
/// Returns [`RuntimeError`] for oversized or invalid JSON input, and for
/// TOON encode failures on payloads that clear the minimum-length gate.
pub fn compress_toon(
    input: &str,
    compression_enabled: bool,
    min_toon_chars: usize,
) -> Result<CompressResult, RuntimeError> {
    validate_input_size(input)?;
    let before_tokens = estimate_tokens(input);
    // Parse before the minimum-length gate: invalid JSON must fail with
    // `RuntimeError::InvalidJson` (CLI exit code 2) regardless of payload
    // size. The gate only skips TOON encoding; it never exempts input from
    // JSON validation.
    let value: serde_json::Value = serde_json::from_str(input)?;
    // Character count, not byte length: the adapter hooks measure the same
    // threshold in Unicode code points, so CJK payloads gate identically.
    if input.chars().count() < min_toon_chars {
        return Ok(toon_passthrough(input, before_tokens));
    }
    // Numeric fidelity gate, first half: decline the payload before encoding
    // when a source literal cannot survive a `serde_json` round trip
    // (`1e-400` underflows, `1.234567890123456789` rounds). This is the very
    // predicate the PostTool JSON domain applies, shared through
    // `tokenless_compressors` so the two TOON entries cannot drift apart.
    // TOON spec v4.4 made keyed object maps and nested record arrays short
    // enough to win the size comparison, so such payloads now reach this
    // entry where 0.5.0 left them as JSON.
    if !numbers_are_exact(input) {
        return Ok(toon_passthrough(input, before_tokens));
    }
    let compressed_output = toon_format::encode_default(&value)
        .map_err(|error| RuntimeError::ToonEncode(error.to_string()))?
        .trim_end()
        .to_string();
    // Numeric fidelity gate, second half: a candidate that does not decode
    // back to the parsed value drops something TOON cannot represent
    // (`-0.0` loses its sign, `1e3` returns as an integer), so the original
    // JSON is emitted instead. An empty candidate stays on the no-savings
    // branch below, which already reports it without claiming an encoding.
    if !compressed_output.is_empty()
        && !toon_format::decode_default::<serde_json::Value>(&compressed_output)
            .is_ok_and(|decoded| decoded == value)
    {
        return Ok(toon_passthrough(input, before_tokens));
    }
    let after_tokens = estimate_tokens(&compressed_output);
    let disposition = if compressed_output.is_empty() || after_tokens >= before_tokens {
        Disposition::NoSavings
    } else if !compression_enabled {
        Disposition::DryRun
    } else {
        Disposition::Applied
    };
    let output = if disposition == Disposition::Applied {
        compressed_output.clone()
    } else {
        input.to_string()
    };
    Ok(CompressResult {
        output,
        compressed_output,
        disposition,
        before_tokens,
        after_tokens,
        stash_writes: None,
        stash_errors: None,
        unrecoverable_truncations: None,
        stash_size: None,
    })
}

/// Original-text result for a TOON attempt the policy declined to encode.
///
/// Both the minimum-length gate and the numeric fidelity gate report
/// [`Disposition::Passthrough`]; callers that need to tell them apart
/// compare the payload length against the same `min_toon_chars`.
fn toon_passthrough(input: &str, before_tokens: usize) -> CompressResult {
    CompressResult {
        output: input.to_string(),
        compressed_output: input.to_string(),
        disposition: Disposition::Passthrough,
        before_tokens,
        after_tokens: before_tokens,
        stash_writes: None,
        stash_errors: None,
        unrecoverable_truncations: None,
        stash_size: None,
    }
}

fn validate_input_size(input: &str) -> Result<(), RuntimeError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(RuntimeError::InputTooLarge {
            limit_mib: MAX_INPUT_BYTES / (1024 * 1024),
        });
    }
    Ok(())
}

/// Best-effort stderr warning whose own write failure cannot fail the
/// command. `eprintln!` panics when writing to stderr fails (a full
/// filesystem behind redirected logs, a closed descriptor), which would
/// turn an optional-state warning into a process failure —
/// optional state must stay invisible to the compression and
/// retrieval results. The write errors are discarded: there is no
/// fallback channel to report a failed warning on, and failing the
/// command here is exactly the regression to avoid.
pub(crate) fn warn_soft(message: &str) {
    use std::io::Write;
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(message.as_bytes());
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
}

/// Overall budget for one in-process PostTool compression.
const RESPONSE_PIPELINE_TIMEOUT: Duration = Duration::from_secs(10);

/// Compress a response using an optional caller-owned stash store.
///
/// CLI and embedded frontends use the same Runtime-owned JSON domain path.
/// The explicit API validates JSON at its boundary; optional compression
/// failures remain fail-open inside the pipeline.
///
/// # Errors
///
/// Returns [`RuntimeError`] for oversized or invalid JSON input.
pub fn compress_response_with_store(
    input: &str,
    options: &CompressOptions,
    compression_enabled: bool,
    stash_store: Option<&Arc<dyn StashStore>>,
) -> Result<CompressResult, RuntimeError> {
    validate_input_size(input)?;
    // This explicit API forces JSON domain dispatch, so validation here keeps
    // invalid input a structured boundary error for the CLI and bindings.
    serde_json::from_str::<serde::de::IgnoredAny>(input)?;

    let request = PostToolRequest {
        result_kind: ResultKind::Tool,
        tool_name: "compress-response".into(),
        content: input.into(),
        status: ToolResultStatus::Success,
        content_origin: ContentOrigin::ApiResponse,
        command: None,
        output_optimization: OutputOptimization::None,
        capabilities: PostToolCapabilities {
            replace_output: true,
            recovery: if options.stash_enabled && stash_store.is_some() {
                tokenless_protocol::RecoveryMethod::Shell
            } else {
                tokenless_protocol::RecoveryMethod::None
            },
            replace_with_text: false,
        },
        exit_code: None,
        stderr: None,
        original_command: None,
        cwd: None,
    };
    let run = PostToolPipeline::run(
        &request,
        &PostToolPipelineConfig {
            timeout: RESPONSE_PIPELINE_TIMEOUT,
            max_input_bytes: MAX_INPUT_BYTES,
            min_input_chars: 0,
            error_min_input_chars: post_tool::DEFAULT_ERROR_MIN_CHARS,
            compression_enabled,
            search_path_sharing_enabled: false,
            diff_compression_enabled: false,
            html_extraction_enabled: false,
            browser_snapshot_enabled: false,
            stash_enabled: options.stash_enabled,
            require_reversibility: options.require_reversible,
            force_json: true,
            allow_toon: false,
            min_toon_chars: MIN_TOON_CHARS,
            mixed_content_enabled: false,
            json: json_config(options),
        },
        stash_store,
    );

    // Legacy measurement channel (see `CompressResult::compressed_output`):
    // the candidate the adapter produced, or the original when none ran.
    let compressed_output = run.candidate.unwrap_or_else(|| input.to_string());
    // Both counts run the shared estimator locally, mirroring each other;
    // this also avoids converting the response's u64 counts back to usize.
    let before_tokens = estimate_tokens(input);
    let after_tokens = estimate_tokens(&compressed_output);

    let stash_attached = run.stash_writes.is_some();
    Ok(CompressResult {
        output: run.response.output,
        compressed_output,
        disposition: run.response.disposition,
        before_tokens,
        after_tokens,
        stash_writes: run.stash_writes,
        stash_errors: run.stash_errors,
        unrecoverable_truncations: stash_attached
            .then(|| run.unrecoverable_truncations.unwrap_or(0)),
        stash_size: run.stash_size,
    })
}

fn json_config(options: &CompressOptions) -> JsonCompressionConfig {
    let defaults = JsonCompressionConfig::default();
    JsonCompressionConfig {
        truncate_strings_at: options
            .truncate_strings_at
            .unwrap_or(defaults.truncate_strings_at),
        truncate_arrays_at: options
            .truncate_arrays_at
            .unwrap_or(defaults.truncate_arrays_at),
        array_tail_preserve: options
            .array_tail_preserve
            .unwrap_or(defaults.array_tail_preserve),
        max_depth: options.max_depth.unwrap_or(defaults.max_depth),
        ..defaults
    }
}

/// Retrieve from a caller-owned store using a bare hash or embedded marker.
///
/// # Errors
///
/// Returns [`RuntimeError`] for malformed input, a missing/expired entry, or a
/// backend failure.
pub fn retrieve_from_store(
    store: &dyn StashStore,
    hash_or_marker: &str,
) -> Result<String, RuntimeError> {
    retrieve_recorded(store, hash_or_marker, None, "")
}

/// Normalize retrieve input — a bare hash or text containing a
/// `<<tokenless:HASH>>` marker — to the lowercase stash key.
///
/// # Errors
///
/// Returns [`RuntimeError::InvalidHash`] when neither form is present.
pub fn normalize_hash(hash_or_marker: &str) -> Result<String, RuntimeError> {
    match extract_hash(hash_or_marker) {
        Some(hash) => Ok(hash.to_ascii_lowercase()),
        None if is_valid_hash(hash_or_marker) => Ok(hash_or_marker.to_ascii_lowercase()),
        None => Err(RuntimeError::InvalidHash {
            value: hash_or_marker.to_string(),
        }),
    }
}

/// The single trusted retrieval implementation with event recording, shared
/// by the CLI (`"cli"`) and [`TokenlessRuntime::retrieve`] (`"embedded"`).
/// Every store lookup records
/// one `retrieve_events` row — `hit`, `miss`, or `error` — when a recorder
/// is attached; invalid input never reaches the store and records nothing.
/// Recording is fail-silent and never affects the returned payload or error.
///
/// # Errors
///
/// Returns [`RuntimeError`] for malformed input, a missing/expired entry,
/// or a backend failure.
pub fn retrieve_recorded(
    store: &dyn StashStore,
    hash_or_marker: &str,
    recorder: Option<&StatsRecorder>,
    source: &str,
) -> Result<String, RuntimeError> {
    let hash = normalize_hash(hash_or_marker)?;
    let result = store.retrieve(&hash);
    if let Some(recorder) = recorder {
        let (outcome, payload_tokens) = match &result {
            Ok(Some(payload)) => ("hit", Some(estimate_tokens(payload) as i64)),
            Ok(None) => ("miss", None),
            Err(_) => ("error", None),
        };
        let tokenizer_id = payload_tokens
            .is_some()
            .then_some(tokenless_protocol::TOKENIZER_ID);
        if let Err(e) = recorder.record_retrieve_event(
            &hash,
            outcome,
            source,
            payload_tokens,
            tokenizer_id,
            None,
            None,
            None,
        ) {
            warn_soft(&format!(
                "[tokenless-stats] WARNING: failed to record retrieve event: {e}",
            ));
        }
    }
    match result {
        Ok(Some(payload)) => Ok(payload),
        Ok(None) => Err(RuntimeError::StashEntryNotFound { hash }),
        Err(error) => Err(RuntimeError::StashRetrieve(error.to_string())),
    }
}

/// Records compression facts from one v2 lifecycle transport operation.
pub fn record_compression(
    attribution: &ProtocolAttribution,
    outcome: &EntryOutcome,
    recorder: Option<&StatsRecorder>,
    sls_enabled: bool,
) {
    let Some(stats) = outcome.stats.as_ref() else {
        return;
    };
    record_entry_stats(
        attribution,
        stats,
        outcome.stash_writes,
        outcome.stash_errors,
        outcome.stash_size,
        &outcome.artifact_keys,
        recorder,
        sls_enabled,
    );
}

// These values come from independent runtime, stash, and observability
// services, so explicit parameters avoid inventing a shared policy DTO.
#[allow(clippy::too_many_arguments)]
fn record_entry_stats(
    attribution: &ProtocolAttribution,
    stats: &entry::EntryStats,
    stash_writes: Option<usize>,
    stash_errors: Option<usize>,
    stash_size: Option<usize>,
    stash_keys: &[String],
    recorder: Option<&StatsRecorder>,
    sls_enabled: bool,
) {
    if recorder.is_none() && !sls_enabled {
        return;
    }
    let before_tokens = estimate_tokens(&stats.input);
    let after_tokens = estimate_tokens(&stats.measured_output);
    if after_tokens >= before_tokens {
        return;
    }
    let mode = if stats.disposition == Disposition::DryRun {
        CompressionMode::DryRun
    } else {
        CompressionMode::Active
    };
    let applied_operations = (!stats.applied_operations.is_empty()).then(|| {
        stats
            .applied_operations
            .iter()
            .map(|operation| operation.wire_str().to_owned())
            .collect()
    });
    let recoverability = serde_json::to_value(stats.recoverability)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned));
    let mut record = StatsRecord::new(
        stats.operation.clone(),
        attribution.agent_id.clone(),
        stats.input.len(),
        before_tokens,
        stats.measured_output.len(),
        after_tokens,
    )
    .with_before_text(stats.input.clone())
    .with_after_text(stats.measured_output.clone())
    .with_source_pid(std::process::id() as i64)
    .with_mode(mode)
    .with_stash(stash_writes, stash_errors, stash_size)
    .with_entry_metadata(
        stats.content_type.clone(),
        stats.content_origin.clone(),
        applied_operations,
        recoverability,
        tokenless_protocol::TOKENIZER_ID,
        stats.unrecoverable_truncations.map(|count| count as i64),
    );
    if let Some(session_id) = &attribution.session_id {
        record = record.with_session_id(session_id.clone());
    }
    if let Some(tool_use_id) = &attribution.tool_use_id {
        record = record.with_tool_use_id(tool_use_id.clone());
    }

    if let Some(recorder) = recorder {
        match recorder.record(&record) {
            Ok(stats_id) => {
                if stats.disposition == Disposition::Applied && !stash_keys.is_empty() {
                    let artifact_kind = if stats
                        .applied_operations
                        .contains(&tokenless_protocol::AppliedOperation::SchemaCompression)
                    {
                        "schema_compression"
                    } else if stats
                        .applied_operations
                        .contains(&tokenless_protocol::AppliedOperation::JsonRecordReduction)
                    {
                        "json_record_reduction"
                    } else {
                        "json_truncation"
                    };
                    if let Err(e) = recorder.record_artifacts(stats_id, artifact_kind, stash_keys) {
                        warn_soft(&format!(
                            "[tokenless-stats] WARNING: failed to record stash artifacts: {e}"
                        ));
                    }
                }
            }
            Err(e) => {
                warn_soft(&format!(
                    "[tokenless-stats] WARNING: failed to record stats entry: {e}",
                ));
            }
        }
    }
    if sls_enabled {
        SlsWriter::new().write(&record);
    }
}

fn resolve_runtime_data_dir(explicit: Option<&Path>) -> Result<PathBuf, RuntimeError> {
    if let Some(path) = explicit {
        return validate_data_dir(path).map_err(RuntimeError::from);
    }
    let home = get_home_dir();
    let trusted_home = (!home.is_empty()).then(|| Path::new(&home));
    let environment = std::env::var("TOKENLESS_DATA_DIR").ok();
    resolve_data_dir(trusted_home, environment.as_deref()).map_err(RuntimeError::from)
}

#[cfg(test)]
mod tests;
