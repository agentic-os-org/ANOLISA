//! Runtime-owned PostTool content dispatch and arbitration.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokenless_ccr::{InMemoryStore, StashStore, StashWrite};
use tokenless_compressors::{
    BrowserSnapshotCompressor, BuildLogCompressor, BuildLogMode, BuildLogOperation, HtmlExtractor,
    JsonCompressionConfig, JsonCompressionContext, JsonCompressor, JsonError, JsonOperation,
    SearchResultsCompressor, TabularCompressor, TabularOperation,
};
use tokenless_protocol::{
    AppliedOperation, BYTE_ESTIMATOR_ID, ContentOrigin, ContentType, Disposition, PostToolRequest,
    PostToolResponse, Recoverability, TOKENIZER_ID, ToolResultStatus, estimate_tokens,
    estimate_tokens_from_bytes,
};

use super::arbitration::{ArbitrationInput, Verdict, decide};
use super::content::detect;
use super::mixed;
use super::stash_ledger::StashLedger;

/// Default minimum character threshold before error outputs enter Level E2 compression.
pub(crate) const DEFAULT_ERROR_MIN_CHARS: usize = 400;

fn is_hard_execution_exit(code: i32) -> bool {
    matches!(code, 126 | 127) || !(0..128).contains(&code)
}

/// Policy resolved by Runtime for one PostTool call.
#[derive(Debug, Clone)]
pub(crate) struct PostToolPipelineConfig {
    pub(crate) timeout: Duration,
    pub(crate) max_input_bytes: usize,
    pub(crate) min_input_chars: usize,
    pub(crate) error_min_input_chars: usize,
    pub(crate) compression_enabled: bool,
    pub(crate) search_path_sharing_enabled: bool,
    pub(crate) diff_compression_enabled: bool,
    pub(crate) html_extraction_enabled: bool,
    pub(crate) embedded_content_enabled: bool,
    pub(crate) browser_snapshot_enabled: bool,
    pub(crate) stash_enabled: bool,
    pub(crate) require_reversibility: bool,
    pub(crate) force_json: bool,
    pub(crate) allow_toon: bool,
    pub(crate) min_toon_chars: usize,
    pub(crate) mixed_content_enabled: bool,
    pub(crate) json: JsonCompressionConfig,
}

/// Protocol response plus Runtime-only measurement and artifact facts.
pub(crate) struct PostToolRun {
    pub(crate) response: PostToolResponse,
    pub(crate) candidate: Option<String>,
    pub(crate) operations: Vec<AppliedOperation>,
    pub(crate) stash_writes: Option<usize>,
    pub(crate) stash_errors: Option<usize>,
    pub(crate) stash_size: Option<usize>,
    pub(crate) unrecoverable_truncations: Option<usize>,
    pub(crate) fact_guard_violation: Option<super::fact_guard::FactViolation>,
}

/// Runtime-owned PostTool pipeline for statically dispatched content domains.
pub(crate) struct PostToolPipeline;

impl PostToolPipeline {
    pub(crate) fn run(
        request: &PostToolRequest,
        config: &PostToolPipelineConfig,
        stash_store: Option<&Arc<dyn StashStore>>,
    ) -> PostToolRun {
        let started = Instant::now();

        if request.content.len() > config.max_input_bytes {
            let mut run = passthrough(
                request,
                estimate_tokens_from_bytes(request.content.len()) as u64,
                ContentType::Unknown,
            );
            run.response.tokenizer_id = BYTE_ESTIMATOR_ID.to_owned();
            return run;
        }
        let before_tokens = estimate_tokens(&request.content) as u64;
        let detected = detect(&request.content);
        // The kill switch gates the wire label too: passthrough responses
        // carry the detected type, and strict parsers without a fallback
        // variant reject unknown values. Disabled means zero new wire values.
        let content_type =
            if !config.browser_snapshot_enabled && detected == ContentType::BrowserSnapshot {
                ContentType::PlainText
            } else {
                detected
            };
        let is_error = request.status == ToolResultStatus::Error;
        if is_error && request.exit_code.is_none() {
            return passthrough(request, before_tokens, content_type);
        }
        if is_error && request.exit_code.is_some_and(is_hard_execution_exit) {
            return passthrough(request, before_tokens, content_type);
        }
        let min_chars = if is_error {
            config.min_input_chars.max(config.error_min_input_chars)
        } else {
            config.min_input_chars
        };
        if !request.capabilities.replace_output
            || request.content_origin == ContentOrigin::FileContent
            || request.content.chars().count() < min_chars
        {
            return passthrough(request, before_tokens, content_type);
        }

        // Grep without line prefixes can return CSV matches classified as
        // Tabular. Shape detection does not remove the need to retain every
        // match; other domain compressors may drop rows or rewrite source text.
        // Compound commands have no per-command output boundaries. Protect the
        // complete result when any segment searches, even a quiet condition.
        let protect_search_output = request.tool_name == "Grep"
            || (request.content_origin == ContentOrigin::CommandOutput
                && request
                    .command
                    .as_deref()
                    .is_some_and(crate::entry::is_search_command));
        // Data printed by a shell file read (`cat build.log`) compresses like
        // command output; a page printed that way is source the agent may edit,
        // so only HTML keeps it verbatim. Read tool results were excluded above.
        let shell_output = matches!(
            request.content_origin,
            ContentOrigin::CommandOutput | ContentOrigin::FileRead
        );
        // On error status (`is_error`), restrict compression to domains whose
        // transforms are lossless (`Diff`, `Search` path sharing) or error-aware
        // (`BuildLog` in `DiagnosticFailure` mode, `JSON`); `HTML`, `BrowserSnapshot`,
        // `Tabular`, and `Mixed` stay excluded via `!is_error`, with
        // `FactInvariantGuard` at arbitration serving as the universal backstop.
        let diff_candidate = config.diff_compression_enabled
            && !protect_search_output
            && content_type == ContentType::Diff
            && shell_output
            && request.capabilities.replace_with_text;
        let html_candidate = config.html_extraction_enabled
            && !is_error
            && !protect_search_output
            && content_type == ContentType::Html
            && request.content_origin != ContentOrigin::FileRead
            && request.capabilities.replace_with_text;
        let browser_snapshot_candidate = config.browser_snapshot_enabled
            && !is_error
            && !protect_search_output
            && content_type == ContentType::BrowserSnapshot
            && request.content_origin != ContentOrigin::FileRead
            && request.capabilities.replace_with_text;
        let json_candidate = !protect_search_output
            && (config.force_json
                || content_type == ContentType::Json
                || is_wrapped_structured_json(&request.content));
        let build_log_candidate =
            !protect_search_output && content_type == ContentType::BuildLog && shell_output;
        let tabular_candidate = !is_error
            && !protect_search_output
            && content_type == ContentType::Tabular
            && request.capabilities.replace_with_text;
        let search_candidate = config.search_path_sharing_enabled
            && (protect_search_output
                || (content_type == ContentType::SearchResults
                    && (request.content_origin == ContentOrigin::ApiResponse
                        || (request.content_origin == ContentOrigin::CommandOutput
                            && SearchResultsCompressor.is_path_list(&request.content)))))
            && request.capabilities.replace_with_text;
        // Mixed-content routing fires only when no whole-content domain
        // matched: standalone documents keep their existing routes and
        // eligibility checks untouched. Per-region compression reuses the
        // same compressors, arbitration and stash lifecycle as those routes.
        // Tabular regions are recognized only inside explicit fences (a bare
        // CSV block has no verifiable boundary); bare regions are JSON only.
        // Search output stays out even when the search route itself is
        // switched off: path-sharing off means do-not-touch, not free rein.
        let mixed_candidate = config.mixed_content_enabled
            && !is_error
            && !protect_search_output
            && !diff_candidate
            && !html_candidate
            && !browser_snapshot_candidate
            && !json_candidate
            && !build_log_candidate
            && !tabular_candidate
            && !search_candidate
            && request.capabilities.replace_with_text;
        if !diff_candidate
            && !html_candidate
            && !browser_snapshot_candidate
            && !json_candidate
            && !build_log_candidate
            && !tabular_candidate
            && !search_candidate
            && (!mixed_candidate || !mixed::has_recognizable_regions(&request.content))
        {
            return passthrough(request, before_tokens, content_type);
        }

        let dry_run_store = (!config.compression_enabled
            && config.stash_enabled
            && request.capabilities.recovery.is_available()
            && stash_store.is_some())
        .then(InMemoryStore::new);
        let attached_store: Option<&dyn StashStore> =
            if config.stash_enabled && request.capabilities.recovery.is_available() {
                if config.compression_enabled {
                    stash_store.map(AsRef::as_ref)
                } else {
                    dry_run_store.as_ref().map(|store| store as &dyn StashStore)
                }
            } else {
                None
            };
        let candidate = if diff_candidate {
            // A failed stash write is a backend failure, not a compressor
            // failure: degrade to the unchanged candidate and count the
            // error so the lifecycle surfaces `RuntimeError::StashWrite`,
            // the taxonomy the JSON, build-log and tabular domains already
            // use for failed stash writes, instead of failing the whole
            // PostTool call as `RuntimeError::Pipeline`.
            let mut stash_errors = 0;
            let rendered = if let Some(store) = attached_store
                && let Some(view) = super::diff::render(&request.content)
            {
                match store.stash(&request.content) {
                    Ok(write) => Some((view, write)),
                    Err(_) => {
                        stash_errors = 1;
                        None
                    }
                }
            } else {
                None
            };
            match rendered {
                Some((view, write)) => {
                    let hint = tokenless_ccr::recovery_instruction(
                        &write.key,
                        &request.capabilities.recovery,
                    );
                    DomainCandidate {
                        output: format!("{hint}\n{view}"),
                        operations: vec![AppliedOperation::DiffReduction],
                        recoverability: tokenless_compressors::Recoverability::Retrievable,
                        stash_writes: vec![write],
                        stash_errors: 0,
                        unrecoverable_truncations: None,
                    }
                }
                None => DomainCandidate {
                    output: request.content.clone(),
                    operations: Vec::new(),
                    recoverability: tokenless_compressors::Recoverability::Lossless,
                    stash_writes: Vec::new(),
                    stash_errors,
                    unrecoverable_truncations: None,
                },
            }
        } else if html_candidate {
            // Same contract as the diff branch: the stash backend, not the
            // HTML extractor, owns this failure mode.
            let mut stash_errors = 0;
            let rendered = if let Some(store) = attached_store
                && let Some(view) = HtmlExtractor.render(&request.content)
            {
                match store.stash(&request.content) {
                    Ok(write) => Some((view, write)),
                    Err(_) => {
                        stash_errors = 1;
                        None
                    }
                }
            } else {
                None
            };
            match rendered {
                Some((view, write)) => {
                    let hint = tokenless_ccr::recovery_instruction(
                        &write.key,
                        &request.capabilities.recovery,
                    );
                    DomainCandidate {
                        output: format!("{hint}\n{}", view.output),
                        operations: vec![AppliedOperation::HtmlExtraction],
                        recoverability: tokenless_compressors::Recoverability::Retrievable,
                        stash_writes: vec![write],
                        stash_errors: 0,
                        unrecoverable_truncations: None,
                    }
                }
                None => DomainCandidate {
                    output: request.content.clone(),
                    operations: Vec::new(),
                    recoverability: tokenless_compressors::Recoverability::Lossless,
                    stash_writes: Vec::new(),
                    stash_errors,
                    unrecoverable_truncations: None,
                },
            }
        } else if browser_snapshot_candidate {
            // Same contract as the diff and HTML branches: the stash backend,
            // not the compressor, owns the stash-failure degradation.
            let mut stash_errors = 0;
            let rendered = if let Some(store) = attached_store
                && let Some(view) = BrowserSnapshotCompressor.compress(&request.content)
            {
                match store.stash(&request.content) {
                    Ok(write) => Some((view, write)),
                    Err(_) => {
                        stash_errors = 1;
                        None
                    }
                }
            } else {
                None
            };
            match rendered {
                Some((view, write)) => {
                    let hint = tokenless_ccr::recovery_instruction(
                        &write.key,
                        &request.capabilities.recovery,
                    );
                    DomainCandidate {
                        output: format!("{hint}\n{view}"),
                        operations: vec![AppliedOperation::BrowserSnapshotReduction],
                        recoverability: tokenless_compressors::Recoverability::Retrievable,
                        stash_writes: vec![write],
                        stash_errors: 0,
                        unrecoverable_truncations: None,
                    }
                }
                None => DomainCandidate {
                    output: request.content.clone(),
                    operations: Vec::new(),
                    recoverability: tokenless_compressors::Recoverability::Lossless,
                    stash_writes: Vec::new(),
                    stash_errors,
                    unrecoverable_truncations: None,
                },
            }
        } else if json_candidate {
            let context = JsonCompressionContext {
                recovery: &request.capabilities.recovery,
                stash: attached_store,
                allow_toon: config.allow_toon && request.capabilities.replace_with_text,
                min_toon_chars: config.min_toon_chars,
                allow_unrecoverable: !config.require_reversibility || !config.compression_enabled,
                // Rewriting a field keeps the document JSON, so text
                // replacement is not required. Data printed from a local
                // file (`file_read`) keeps only HTML verbatim like a printed
                // page while non-HTML embedded domains still route; error
                // results disable embedded HTML/Tabular and switch embedded
                // build logs to `DiagnosticFailure`. `file_content` was
                // already passed through at pipeline entry.
                embedded_html: config.html_extraction_enabled
                    && !is_error
                    && request.content_origin != ContentOrigin::FileRead,
                embedded_content: config.embedded_content_enabled,
                is_error,
            };
            let outcome = match JsonCompressor::new(config.json.clone())
                .compress(&request.content, &context)
            {
                Ok(outcome) => outcome,
                // The bracket sniff that routed this content here inspects
                // only the first and last non-whitespace bytes, so
                // concatenated documents (for example stream JSON printed
                // one object per line) reach the JSON compressor without
                // parsing. The compressor is the parsing authority; treat
                // the misroute as passthrough instead of failing the call.
                Err(JsonError::InvalidJson(_)) => {
                    return passthrough(request, before_tokens, content_type);
                }
            };
            DomainCandidate {
                output: outcome.output,
                operations: json_operations(&outcome.operations),
                recoverability: outcome.recoverability,
                stash_writes: outcome.stash_writes,
                stash_errors: outcome.metrics.stash_errors,
                unrecoverable_truncations: outcome
                    .operations
                    .contains(&JsonOperation::Truncation)
                    .then_some(outcome.metrics.unrecoverable_truncations),
            }
        } else if build_log_candidate {
            let mode = if is_error {
                BuildLogMode::DiagnosticFailure
            } else {
                BuildLogMode::Success
            };
            let outcome = BuildLogCompressor.compress_with_options(
                &request.content,
                attached_store,
                &request.capabilities.recovery,
                mode,
            );
            DomainCandidate {
                output: outcome.output,
                operations: build_log_operations(&outcome.operations),
                recoverability: outcome.recoverability,
                stash_writes: outcome.stash_writes,
                stash_errors: outcome.metrics.stash_errors,
                unrecoverable_truncations: None,
            }
        } else if search_candidate {
            let output = SearchResultsCompressor.compress(&request.content);
            DomainCandidate {
                operations: if output.is_some() {
                    vec![AppliedOperation::SearchPathSharing]
                } else {
                    Vec::new()
                },
                output: output.unwrap_or_else(|| request.content.clone()),
                recoverability: tokenless_compressors::Recoverability::Lossless,
                stash_writes: Vec::new(),
                stash_errors: 0,
                unrecoverable_truncations: None,
            }
        } else if tabular_candidate {
            let outcome = TabularCompressor.compress(&request.content);
            DomainCandidate {
                output: outcome.output,
                operations: outcome
                    .operations
                    .iter()
                    .map(|operation| match operation {
                        TabularOperation::Compaction => AppliedOperation::TabularCompaction,
                        TabularOperation::RowReduction => AppliedOperation::TabularRowReduction,
                    })
                    .collect(),
                recoverability: outcome.recoverability,
                stash_writes: outcome.stash_writes,
                stash_errors: outcome.metrics.stash_errors,
                unrecoverable_truncations: None,
            }
        } else {
            // Mixed-content regions. Region recognition ran inside
            // `mixed::compress_regions`; every rejected or unsupported region
            // stays byte-for-byte in the candidate. `None` from
            // `compress_regions` means no region changed, no tentative
            // Stash write was produced, and no Stash error occurred. An
            // unchanged result carrying writes or any storage failure
            // reaches the arbiter without operations, so the ledger rolls
            // back all writes before the runtime reports the error count.
            match mixed::compress_regions(
                &request.content,
                config,
                attached_store,
                &request.capabilities.recovery,
                config.allow_toon && request.capabilities.replace_with_text,
                config.html_extraction_enabled && request.content_origin != ContentOrigin::FileRead,
                config.embedded_content_enabled,
            ) {
                Some(outcome) => DomainCandidate {
                    output: outcome.output,
                    operations: outcome.operations,
                    recoverability: outcome.recoverability,
                    stash_writes: outcome.stash_writes,
                    stash_errors: outcome.stash_errors,
                    unrecoverable_truncations: outcome.unrecoverable_truncations,
                },
                None => return passthrough(request, before_tokens, content_type),
            }
        };

        let mut ledger = StashLedger::default();
        for write in candidate.stash_writes {
            ledger.record(write);
        }
        let fact_guard_violation = if is_error && !candidate.operations.is_empty() {
            super::fact_guard::verify(&request.content, &candidate.output).err()
        } else {
            None
        };
        let verdict = if fact_guard_violation.is_some() {
            Verdict::Reject(Disposition::NoSavings)
        } else {
            decide(&ArbitrationInput {
                original: &request.content,
                candidate: &candidate.output,
                has_operations: !candidate.operations.is_empty(),
                // Reject marginal Diff, HTML, and snapshot estimates after including wrapper and recovery text.
                min_token_savings: if diff_candidate || html_candidate || browser_snapshot_candidate
                {
                    16
                } else {
                    1
                },
                recoverability: candidate.recoverability,
                require_reversibility: config.require_reversibility && config.compression_enabled,
                dry_run: !config.compression_enabled,
                timed_out: started.elapsed() > config.timeout,
            })
        };

        let store = attached_store;
        let (output, disposition, stash_keys) = match verdict {
            Verdict::Apply => {
                let keys = ledger.commit(&candidate.output, store, &request.capabilities.recovery);
                (candidate.output.clone(), Disposition::Applied, keys)
            }
            Verdict::DryRun => {
                ledger.rollback(store);
                (request.content.clone(), Disposition::DryRun, Vec::new())
            }
            Verdict::Reject(disposition) => {
                ledger.rollback(store);
                (request.content.clone(), disposition, Vec::new())
            }
        };
        let selected = matches!(verdict, Verdict::Apply | Verdict::DryRun);
        let after_tokens = if selected {
            estimate_tokens(&candidate.output) as u64
        } else {
            before_tokens
        };
        let response_operations = if matches!(verdict, Verdict::Apply) {
            candidate.operations.clone()
        } else {
            Vec::new()
        };
        let recoverability = if matches!(verdict, Verdict::Apply) {
            protocol_recoverability(candidate.recoverability)
        } else {
            Recoverability::Lossless
        };
        let unrecoverable_truncations = if !config.compression_enabled {
            None
        } else {
            candidate
                .unrecoverable_truncations
                .filter(|_| attached_store.is_some() || selected)
        };
        let persistent_store_attached = config.compression_enabled && attached_store.is_some();
        PostToolRun {
            response: PostToolResponse {
                output,
                disposition,
                content_type: Some(if json_candidate {
                    ContentType::Json
                } else {
                    content_type
                }),
                applied_operations: response_operations,
                recoverability,
                before_tokens,
                after_tokens,
                stash_keys,
                tokenizer_id: TOKENIZER_ID.to_owned(),
                additional_context: None,
            },
            candidate: Some(candidate.output),
            operations: candidate.operations,
            stash_writes: persistent_store_attached.then(|| ledger.live_writes()),
            stash_errors: persistent_store_attached
                .then(|| candidate.stash_errors + ledger.errors()),
            stash_size: if persistent_store_attached {
                attached_store.map(StashStore::len)
            } else {
                None
            },
            unrecoverable_truncations,
            fact_guard_violation,
        }
    }
}

struct DomainCandidate {
    output: String,
    operations: Vec<AppliedOperation>,
    recoverability: tokenless_compressors::Recoverability,
    stash_writes: Vec<StashWrite>,
    stash_errors: usize,
    unrecoverable_truncations: Option<usize>,
}

fn is_wrapped_structured_json(content: &str) -> bool {
    let Ok(Value::String(inner)) = serde_json::from_str(content) else {
        return false;
    };
    matches!(
        serde_json::from_str::<Value>(&inner),
        Ok(Value::Object(_) | Value::Array(_))
    )
}

fn passthrough(
    request: &PostToolRequest,
    before_tokens: u64,
    content_type: ContentType,
) -> PostToolRun {
    let mut response = PostToolResponse::passthrough(request, before_tokens);
    response.content_type = Some(content_type);
    PostToolRun {
        response,
        candidate: None,
        operations: Vec::new(),
        stash_writes: None,
        stash_errors: None,
        stash_size: None,
        unrecoverable_truncations: None,
        fact_guard_violation: None,
    }
}

/// Maps JSON-domain operations to their wire operation names.
pub(super) fn json_operations(operations: &[JsonOperation]) -> Vec<AppliedOperation> {
    operations
        .iter()
        .map(|operation| match operation {
            JsonOperation::Cleanup => AppliedOperation::JsonCleanup,
            JsonOperation::RecordReduction => AppliedOperation::JsonRecordReduction,
            JsonOperation::Truncation => AppliedOperation::JsonTruncation,
            // The response reports the JSON content type, which distinguishes
            // an embedded view from a standalone document.
            JsonOperation::EmbeddedHtml => AppliedOperation::HtmlExtraction,
            JsonOperation::EmbeddedBuildLog => AppliedOperation::BuildLogReduction,
            JsonOperation::EmbeddedTabular => AppliedOperation::TabularCompaction,
            JsonOperation::Toon => AppliedOperation::Toon,
        })
        .collect()
}

fn build_log_operations(operations: &[BuildLogOperation]) -> Vec<AppliedOperation> {
    operations
        .iter()
        .map(|operation| match operation {
            BuildLogOperation::TerminalCleanup => AppliedOperation::TerminalCleanup,
            BuildLogOperation::Reduction => AppliedOperation::BuildLogReduction,
        })
        .collect()
}

fn protocol_recoverability(
    recoverability: tokenless_compressors::Recoverability,
) -> Recoverability {
    match recoverability {
        tokenless_compressors::Recoverability::Lossless => Recoverability::Lossless,
        tokenless_compressors::Recoverability::Retrievable => Recoverability::Retrievable,
        tokenless_compressors::Recoverability::Unrecoverable => Recoverability::Unrecoverable,
    }
}

#[cfg(test)]
mod tests;
