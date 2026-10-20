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
    let persisted = store.complete_captured_batch(&output.events);
    // Recheck each sink's normal write gate so activation during persistence
    // cannot silently skip the already completed call.
    export_local_sinks(&output.events, exporters);
    persisted?;
    Ok(output.pending_response_id.as_ref().and_then(|_| {
        output.events.iter().find_map(|event| match event {
            GenAISemanticEvent::LLMCall(call) => Some(call.pid as u32),
            _ => None,
        })
    }))
}

fn export_local_sinks(events: &[GenAISemanticEvent], exporters: &[Box<dyn GenAIExporter>]) {
    for exporter in exporters {
        if exporter.name() != "sqlite" {
            exporter.export(events);
        }
    }
}

/// Only SQLite lock contention can recover without changing the captured call.
pub(super) fn is_retryable_sqlite_write(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(code, _))
            if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

impl PendingGenAiQueue {
    /// Transfer completion ownership under the same limits as deferred sessions.
    pub(super) fn queue_local_completion(
        &mut self,
        output: crate::genai::builder::BuildOutput,
        store: &GenAISqliteStore,
        limits: &crate::config::RuntimeLimits,
    ) -> Result<(), crate::genai::builder::BuildOutput> {
        let has_response_id = output.pending_response_id.is_some();
        let pid = output
            .events
            .iter()
            .find_map(|event| match event {
                GenAISemanticEvent::LLMCall(call) => Some(call.pid as u32),
                _ => None,
            })
            .unwrap_or(0);
        let pending = PendingGenAI {
            events: output.events.into(),
            response_id: output.pending_response_id.unwrap_or_default(),
            pid,
            created_at: std::time::Instant::now(),
        };
        let bytes = pending.estimated_bytes();
        if self.len() >= limits.pending_genai_max_count.max(1)
            || self.bytes.saturating_add(bytes) > limits.pending_genai_max_bytes.max(1)
        {
            metrics::counter!("agentsight_genai_sqlite_retry_total", "outcome" => "over_budget")
                .increment(1);
            log::warn!(
                "Completed local call cannot await SQLite: pending budget exhausted (count={}, bytes={}, incoming_bytes={bytes}, response_id={})",
                self.len(),
                self.bytes,
                pending.response_id
            );
            return Err(crate::genai::builder::BuildOutput {
                events: pending.events.into_events(),
                pending_response_id: has_response_id.then_some(pending.response_id),
            });
        }
        self.bytes += bytes;
        if self.local_retry_priority.is_none() {
            self.local_retry_priority = Some(store.prioritize_capture());
        }
        self.local_retries.push(pending);
        self.local_retry_after
            .get_or_insert_with(|| std::time::Instant::now() + Duration::from_millis(100));
        metrics::counter!("agentsight_genai_sqlite_retry_total", "outcome" => "queued")
            .increment(1);
        Ok(())
    }

    /// Retry one owned batch without repeating external delivery.
    pub(super) fn retry_local_completion(
        &mut self,
        store: &GenAISqliteStore,
        mapper: &ResponseSessionMapper,
        now: std::time::Instant,
        force: bool,
    ) -> Option<(Vec<GenAISemanticEvent>, Option<u32>)> {
        loop {
            if self.local_retries.is_empty()
                || (!force && self.local_retry_after.is_some_and(|next| now < next))
            {
                return None;
            }
            let mut pending = self.local_retries.remove(0);
            self.bytes = self.bytes.saturating_sub(pending.estimated_bytes());
            let session = mapper
                .get_session_by_response_id(&pending.response_id)
                .or_else(|| mapper.get_session_by_pid(pending.pid));
            if let Some(session) = session {
                pending.events.set_session_id(session);
            }
            let fixup =
                (session.is_none() && !pending.response_id.is_empty()).then_some(pending.pid);
            let events = pending.events.into_events();
            match store.complete_captured_batch(&events) {
                Ok(()) => {
                    self.local_retry_after = None;
                    if self.local_retries.is_empty() {
                        self.local_retry_priority = None;
                    }
                    metrics::counter!("agentsight_genai_sqlite_retry_total", "outcome" => "stored")
                        .increment(1);
                    return Some((events, fixup));
                }
                Err(error) if is_retryable_sqlite_write(error.as_ref()) => {
                    pending.events = events.into();
                    self.bytes += pending.estimated_bytes();
                    self.local_retries.insert(0, pending);
                    self.local_retry_after =
                        Some(std::time::Instant::now() + Duration::from_millis(100));
                    metrics::counter!("agentsight_genai_sqlite_retry_total", "outcome" => "busy")
                        .increment(1);
                    return None;
                }
                Err(error) => {
                    metrics::counter!("agentsight_genai_sqlite_retry_total", "outcome" => "failed")
                        .increment(1);
                    log::warn!(
                        "Completed local call cannot be retried in SQLite (response_id={}): {error}",
                        pending.response_id
                    );
                    self.local_retry_after = None;
                    if self.local_retries.is_empty() {
                        self.local_retry_priority = None;
                    }
                }
            }
        }
    }
}

impl AgentSight {
    fn complete_or_queue_local_genai(&mut self, output: crate::genai::builder::BuildOutput) {
        let Some(store) = self.genai_sqlite_store.as_ref().map(Arc::clone) else {
            return;
        };
        let _priority = store.prioritize_capture();
        let mut retry_limits = self.runtime_limits;
        retry_limits.pending_genai_max_bytes = retry_limits
            .pending_genai_max_bytes
            .saturating_sub(self.completions.bytes);
        retry_limits.pending_genai_max_count = retry_limits
            .pending_genai_max_count
            .saturating_sub(self.completions.len());
        let busy = if !self.pending_genai.local_retries.is_empty() {
            export_local_sinks(&output.events, &self.genai_exporters);
            true
        } else {
            match complete_local_genai(&output, &store, &self.genai_exporters) {
                Ok(Some(pid)) => {
                    self.register_retro_session_fixup(pid, &output.events);
                    false
                }
                Ok(None) => false,
                Err(error) if is_retryable_sqlite_write(error.as_ref()) => true,
                Err(error) => {
                    log::warn!("Failed to store completed local call: {error}");
                    false
                }
            }
        };
        if busy {
            if let Err(output) =
                self.pending_genai
                    .queue_local_completion(output, &store, &retry_limits)
            {
                // Keep the rejected call in flight; its sinks have already run.
                // Backpressure until earlier calls can commit without a larger queue.
                log::warn!("SQLite retry queue full; waiting with the current capture in flight");
                let deadline = std::time::Instant::now() + Duration::from_secs(1);
                while !self.pending_genai.local_retries.is_empty() {
                    if !self.running.load(Ordering::SeqCst) || std::time::Instant::now() >= deadline
                    {
                        log::error!(
                            "SQLite retry wait stopped or timed out; dropping in-flight capture with {} events",
                            output.events.len()
                        );
                        return;
                    }
                    self.retry_local_genai_until(true, deadline, true);
                    if !self.pending_genai.local_retries.is_empty() {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                loop {
                    if !self.running.load(Ordering::SeqCst) || std::time::Instant::now() >= deadline
                    {
                        log::error!(
                            "SQLite retry wait stopped or timed out; dropping in-flight capture with {} events",
                            output.events.len()
                        );
                        return;
                    }
                    match store.complete_captured_batch(&output.events) {
                        Ok(()) => break,
                        Err(error) if is_retryable_sqlite_write(error.as_ref()) => {
                            self.maybe_export_runtime_metrics();
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        Err(error) => {
                            log::error!("Failed to persist in-flight capture: {error}");
                            return;
                        }
                    }
                }
                if let Some(pid) = output.pending_response_id.as_ref().and_then(|_| {
                    output.events.iter().find_map(|event| match event {
                        GenAISemanticEvent::LLMCall(call) => Some(call.pid as u32),
                        _ => None,
                    })
                }) {
                    self.register_retro_session_fixup(pid, &output.events);
                }
                self.detect_and_store_interruptions(&output.events);
            }
            return;
        }
        self.detect_and_store_interruptions(&output.events);
    }

    /// Retry local SQLite writes without repeating completed external delivery.
    pub(super) fn retry_local_genai(&mut self, force: bool) {
        self.retry_local_genai_until(
            force,
            std::time::Instant::now() + Duration::from_secs(1),
            false,
        );
    }

    fn retry_local_genai_until(
        &mut self,
        force: bool,
        deadline: std::time::Instant,
        stop_on_shutdown: bool,
    ) {
        let Some(store) = self.genai_sqlite_store.as_ref().map(Arc::clone) else {
            return;
        };
        let budget = self.completions.budget.clone();
        loop {
            if std::time::Instant::now() >= deadline
                || (stop_on_shutdown && !self.running.load(Ordering::SeqCst))
            {
                break;
            }
            if self.pending_genai.local_retries.is_empty() {
                break;
            }
            let _permit = if let Some(budget) = &budget {
                let admission = if force {
                    budget.acquire_while(crate::utils::cpu_budget::WorkClass::Capture, || {
                        std::time::Instant::now() < deadline
                            && (!stop_on_shutdown || self.running.load(Ordering::SeqCst))
                    })
                } else {
                    budget.try_acquire(crate::utils::cpu_budget::WorkClass::Capture)
                };
                match admission {
                    Ok(Some(permit)) => Some(permit),
                    Ok(None) => break,
                    Err(error) => {
                        log::error!("CPU pacing failed during SQLite retry: {error}");
                        None
                    }
                }
            } else {
                None
            };
            let Some((events, fixup)) = self.pending_genai.retry_local_completion(
                &store,
                &self.response_mapper,
                std::time::Instant::now(),
                force,
            ) else {
                break;
            };
            if let Some(pid) = fixup {
                self.register_retro_session_fixup(pid, &events);
            }
            self.detect_and_store_interruptions(&events);
        }
        if force && !self.pending_genai.local_retries.is_empty() {
            log::warn!(
                "SQLite retry deferred: {} completed batches remain pending",
                self.pending_genai.local_retries.len()
            );
        }
    }

    pub(super) fn deliver_aggregated_results(
        &mut self,
        aggregated_results: Vec<crate::aggregator::AggregatedResult>,
    ) {
        for agg_result in aggregated_results {
            let mut analysis_results = self.analyzer.analyze_for_owned_delivery(
                agg_result,
                self.runtime_limits.retain_raw_sse
                    || (self.ffi_sender.is_none() && self.features.sqlite_storage_enabled)
                    || self
                        .ffi_sender
                        .as_ref()
                        .is_some_and(FfiEventSender::raw_https_enabled),
            );

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
                    self.complete_or_queue_local_genai(output);
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
