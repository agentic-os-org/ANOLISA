//! Delivery consumes completed evidence before building the next call.

use super::*;

/// External consumers need the resolved session before receiving a call.
pub(super) fn can_complete_locally(
    has_ffi_sender: bool,
    has_sqlite_store: bool,
    exporters: &[Box<dyn GenAIExporter>],
) -> bool {
    !has_ffi_sender
        && has_sqlite_store
        && exporters
            .iter()
            .all(|exporter| exporter.name() == "sqlite" || !exporter.is_active())
}

/// Local rows can be repaired later while retaining only their call IDs.
pub(super) fn complete_local_genai(
    output: &crate::genai::builder::BuildOutput,
    store: &GenAISqliteStore,
    exporters: &[Box<dyn GenAIExporter>],
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let persisted = output
        .events
        .iter()
        .try_for_each(|event| store.complete_captured(event));
    // Recheck each sink's normal write gate so activation during persistence
    // cannot silently skip the already completed call.
    for exporter in exporters {
        if exporter.name() != "sqlite" {
            exporter.export(&output.events);
        }
    }
    persisted?;
    Ok(output.pending_response_id.as_ref().and_then(|_| {
        output.events.iter().find_map(|event| match event {
            GenAISemanticEvent::LLMCall(call) => Some(call.pid as u32),
            _ => None,
        })
    }))
}

impl AgentSight {
    pub(super) fn process_aggregated_results(
        &mut self,
        aggregated_results: Vec<crate::aggregator::AggregatedResult>,
    ) {
        for agg_result in aggregated_results {
            let mut analysis_results = self.analyzer.analyze_for_delivery(
                &agg_result,
                self.runtime_limits.retain_raw_sse
                    || (self.ffi_sender.is_none() && self.features.sqlite_storage_enabled)
                    || self
                        .ffi_sender
                        .as_ref()
                        .is_some_and(FfiEventSender::raw_https_enabled),
            );

            drop(agg_result);

            // Backfill TokenRecord.agent from pid_agent_name_cache, falling back
            // to the *process* comm (`<procfs root>/<pid>/comm`) and only then
            // the event's per-event thread comm (which may be e.g. "HTTP client").
            for ar in &mut analysis_results {
                if let crate::analyzer::AnalysisResult::Token(t) = ar {
                    if t.agent.is_none() {
                        t.agent = self
                            .pid_agent_name_cache
                            .get(&t.pid)
                            .cloned()
                            .or_else(|| crate::discovery::scanner::read_comm(t.pid))
                            .or_else(|| Some(t.comm.clone()));
                    }
                }
            }

            // Persist the HTTP view before transferring its strings into the call.
            if self.ffi_sender.is_none() {
                for result in &analysis_results {
                    if self.should_persist_analysis_result(result) {
                        if let Err(error) = self.storage.store(result) {
                            log::warn!("Failed to store analysis result: {error}");
                        }
                    }
                }
            }

            let local_delivery = can_complete_locally(
                self.ffi_sender.is_some(),
                self.genai_sqlite_store.is_some(),
                &self.genai_exporters,
            );
            let (mut output, pending_info) = self.genai_builder.build_for_owned_delivery(
                &analysis_results,
                &self.response_mapper,
                &self.pid_agent_name_cache,
                self.genai_sqlite_store.is_some() && !local_delivery,
            );

            // FFI fallback: when every built LLM event is semantically empty
            // (the underlying body format could not be parsed into
            // `AgentsightLLMData`), skip the useless LLM event and fall through
            // to raw-HTTP (`AgentsightHttpsData`) reporting instead. Only in FFI
            // mode; requires *all* events to be empty LLM calls so we never drop
            // a meaningful event.
            let ffi_https_fallback =
                self.ffi_sender.is_some() && events_are_empty_llm(&output.events);

            if !output.events.is_empty() && !ffi_https_fallback {
                output.take_raw_bodies(&mut analysis_results);
                analysis_results.clear();
            } else if self.ffi_sender.is_some() {
                analysis_results.retain(|result| {
                        matches!(result, crate::analyzer::AnalysisResult::Http(_))
                            || matches!(result, crate::analyzer::AnalysisResult::HttpContext(context) if context.retain_raw)
                    });
            } else {
                analysis_results.clear();
            }

            if !output.events.is_empty() && !ffi_https_fallback {
                if local_delivery {
                    if let Some(store) = &self.genai_sqlite_store {
                        match complete_local_genai(&output, store, &self.genai_exporters) {
                            Ok(Some(pid)) => {
                                self.register_retro_session_fixup(pid, &output.events);
                            }
                            Ok(None) => {}
                            Err(error) => {
                                log::warn!("Failed to store completed local call: {error}");
                            }
                        }
                    }
                    self.detect_and_store_interruptions(&output.events);
                    continue;
                }
                if let Some(response_id) = output.pending_response_id {
                    // Session_id not yet resolved — queue for deferred resolution.
                    // Write a pending row NOW so crash detection can see this call
                    // during the deferral window (up to PENDING_SESSION_TIMEOUT).
                    if let Some(info) = pending_info {
                        if let Some(sqlite_store) = self.genai_sqlite_store.as_ref() {
                            if let Err(e) = sqlite_store.insert_pending(&info) {
                                log::warn!(
                                    "Failed to insert deferred pending call {}: {}",
                                    info.call_id,
                                    e
                                );
                            }
                        }
                    } else if self.genai_sqlite_store.is_some() {
                        log::warn!(
                            "Deferred GenAI call queued without pending_info (response_id={response_id}), crash detection blind spot remains"
                        );
                    }
                    let pid = output
                        .events
                        .iter()
                        .find_map(|event| match event {
                            GenAISemanticEvent::LLMCall(call) => Some(call.pid as u32),
                            _ => None,
                        })
                        .unwrap_or(0);
                    self.pending_genai.push(PendingGenAI {
                        events: output.events.into(),
                        response_id,
                        pid,
                        created_at: std::time::Instant::now(),
                    });
                    self.enforce_pending_genai_limits();
                    log::debug!("GenAI events queued for deferred session_id resolution");
                } else {
                    // Session_id resolved (or no response_id) — export immediately.
                    // For SQLite: write pending first, then complete_pending;
                    // for other exporters: normal export.
                    if let Some(info) = pending_info {
                        if let Some(sqlite_store) = self.genai_sqlite_store.as_ref() {
                            if let Err(e) = sqlite_store.insert_pending(&info) {
                                log::warn!("Failed to insert pending call {}: {}", info.call_id, e);
                            }
                            drop(info);
                            for event in &output.events {
                                if let Err(e) = sqlite_store.complete_pending(event) {
                                    log::warn!("Failed to complete pending call: {e}");
                                }
                            }
                            // Export to non-SQLite exporters only (SQLite already written)
                            for exporter in &self.genai_exporters {
                                if exporter.name() != "sqlite" {
                                    exporter.export(&output.events);
                                    log::debug!(
                                        "Exported {} GenAI events via '{}'",
                                        output.events.len(),
                                        exporter.name()
                                    );
                                }
                            }
                        } else {
                            self.export_genai_events(&output.events);
                        }
                    } else {
                        self.export_genai_events(&output.events);
                    }

                    // ── Online interruption detection ─────────────────────────────
                    // Run after export so the call is already persisted.
                    self.detect_and_store_interruptions(&output.events);
                    self.send_ffi_events(output.events);
                }
            } else if let Some(ref sender) = self.ffi_sender {
                // Either no LLM event was produced, or all LLM events were
                // semantically empty (fallback). The FFI sender suppresses
                // raw HTTPS before enqueueing when it is disabled.
                for ar in analysis_results.drain(..) {
                    let record = match ar {
                        crate::analyzer::AnalysisResult::Http(record) => record,
                        crate::analyzer::AnalysisResult::HttpContext(context)
                            if context.retain_raw =>
                        {
                            context.http
                        }
                        _ => continue,
                    };
                    // Resolved here rather than in the FFI layer because the cache
                    // lives on `self` and outlives the process, so an exited agent
                    // still resolves. Same ladder as the LLM path in
                    // `GenAICallBuilder::build` so both report the same value.
                    //
                    // Passed as a closure so `send_https` can skip it entirely when
                    // raw HTTPS is disabled — the default — instead of paying two
                    // `/proc` reads per non-LLM exchange for a value it drops.
                    let pid = record.pid;
                    let comm = record.comm.clone();
                    sender.send_https(record, || {
                        Some(
                            GenAIBuilder::resolve_agent_name(
                                &comm,
                                pid,
                                &self.pid_agent_name_cache,
                            )
                            .unwrap_or(comm),
                        )
                    });
                }
            }
        }
    }
}
