//! Owned removal keeps unfinished evidence observable and cleans every side cache.

use super::{
    ConnectionId, ConnectionState, HttpConnectionAggregator, pending_response::PendingResponse,
};
use crate::aggregator::{AggregatedResponse, AggregatedResult, HttpPair, RemoveReason};
use crate::parser::http::ParsedRequest;
use std::time::Instant;

pub(crate) struct RemovedConnection {
    pub(crate) id: ConnectionId,
    pub(crate) state: ConnectionState,
    continuation: Option<Vec<u8>>,
    reason: RemoveReason,
    sse_summary: Option<(usize, u64)>,
}

impl RemovedConnection {
    pub(crate) fn into_result(self) -> Option<AggregatedResult> {
        let Self {
            id,
            mut state,
            continuation,
            reason,
            sse_summary,
        } = self;
        if reason.metadata_only() {
            match &mut state {
                ConnectionState::RequestPending { request }
                | ConnectionState::RequestBodyPending { request, .. } => metadata_request(request),
                ConnectionState::SseActive {
                    request,
                    response_headers,
                    sse_events,
                    compressed_buffer,
                    zstd_decoder,
                    ..
                } => {
                    if let Some(request) = request {
                        metadata_request(request);
                    }
                    metadata_response(response_headers);
                    *sse_events = Vec::new();
                    *compressed_buffer = None;
                    *zstd_decoder = None;
                }
                ConnectionState::ResponsePending { request, assembly } => {
                    if let Some(request) = request {
                        metadata_request(request);
                    }
                    if let PendingResponse::Body { response, .. } = assembly {
                        metadata_response(response);
                    }
                }
                ConnectionState::Idle => {}
            }
            if let ConnectionState::RequestBodyPending { body_buffer, .. } = &mut state {
                *body_buffer = Vec::new();
            }
        }
        let partial = match state {
            ConnectionState::Idle => return None,
            ConnectionState::RequestPending { request } => AggregatedResult::RequestOnly {
                connection_id: id,
                request,
            },
            ConnectionState::RequestBodyPending {
                mut request,
                body_buffer,
                ..
            } => {
                request.set_reassembled_body(body_buffer);
                AggregatedResult::RequestOnly {
                    connection_id: id,
                    request,
                }
            }
            ConnectionState::SseActive {
                request,
                response_headers,
                sse_events,
                compressed_buffer,
                content_encoding,
                zstd_decoder,
            } => {
                if let Some(buffer) = compressed_buffer {
                    let source = std::rc::Rc::clone(&response_headers.source_event);
                    HttpConnectionAggregator::finish_compressed_sse(
                        id,
                        request,
                        response_headers,
                        content_encoding,
                        buffer,
                        zstd_decoder,
                        &source,
                    )
                } else {
                    let mut response = AggregatedResponse::from_parsed(response_headers);
                    response.sse_events = sse_events;
                    if let Some((count, timestamp)) = sse_summary {
                        response.captured_sse_event_count = Some(count);
                        response.completion_timestamp_ns = Some(timestamp);
                    }
                    response.sse_continuation_bytes = continuation;
                    pair_or_response(id, request, response, true)
                }
            }
            ConnectionState::ResponsePending { request, assembly } => match assembly {
                PendingResponse::Body {
                    response,
                    end_timestamp_ns,
                    ..
                } => {
                    let mut response = AggregatedResponse::from_parsed(response);
                    response.completion_timestamp_ns = Some(end_timestamp_ns);
                    pair_or_response(id, request, response, false)
                }
                PendingResponse::Headers(_) => AggregatedResult::RequestOnly {
                    connection_id: id,
                    request: request?,
                },
            },
        };
        metrics::counter!("agentsight_http_abandoned_emitted_total", "reason" => reason.as_str())
            .increment(1);
        Some(AggregatedResult::Abandoned {
            reason,
            partial: Box::new(partial),
        })
    }
}

fn pair_or_response(
    id: ConnectionId,
    request: Option<ParsedRequest>,
    response: AggregatedResponse,
    sse: bool,
) -> AggregatedResult {
    if let Some(request) = request {
        let pair = HttpPair {
            connection_id: id,
            request,
            response,
        };
        if sse {
            AggregatedResult::SseComplete(pair)
        } else {
            AggregatedResult::HttpComplete(pair)
        }
    } else {
        AggregatedResult::ResponseOnly {
            connection_id: id,
            response,
        }
    }
}

/// Detach capture payloads and bound routing fields before queuing a diagnostic.
pub(super) fn metadata_request(request: &mut ParsedRequest) {
    request.method = crate::aggregator::result::metadata_text(&request.method);
    request.path = crate::aggregator::result::metadata_text(&request.path);
    request.headers = request
        .headers
        .iter()
        .filter(|(name, _)| crate::aggregator::result::metadata_header(name))
        .take(8)
        .map(|(name, value)| {
            (
                name.clone(),
                crate::aggregator::result::metadata_text(value),
            )
        })
        .collect();
    request.source_event = std::rc::Rc::new(request.source_event.metadata_only());
    request.reassembled_body = None;
    request.body_offset = 0;
    request.body_len = 0;
}

/// Drop response bytes without decompressing or serializing partial content.
pub(super) fn metadata_response(response: &mut crate::parser::http::ParsedResponse) {
    response.headers = response
        .headers
        .iter()
        .filter(|(name, _)| crate::aggregator::result::metadata_header(name))
        .take(8)
        .map(|(name, value)| {
            (
                name.clone(),
                crate::aggregator::result::metadata_text(value),
            )
        })
        .collect();
    response.reason = crate::aggregator::result::metadata_text(&response.reason);
    response.source_event = std::rc::Rc::new(response.source_event.metadata_only());
    response.body_offset = 0;
    response.body_len = 0;
}

impl HttpConnectionAggregator {
    pub(super) fn cleanup(&mut self, key: &ConnectionId) -> Option<Vec<u8>> {
        self.forget_retained(key);
        let continuation = self.sse_continuation_buffers.pop(key);
        self.sse_read_state.pop(key);
        self.last_activity.pop(key);
        self.idle_snapshotted.pop(key);
        self.sse_errors.pop(key);
        self.request_chunks.pop(key);
        continuation
    }

    fn remove_connection(
        &mut self,
        key: &ConnectionId,
        reason: RemoveReason,
    ) -> Option<RemovedConnection> {
        let mut state = self.connections.pop(key)?;
        if matches!(state, ConnectionState::SseActive { .. })
            && matches!(
                reason,
                RemoveReason::LruEvicted | RemoveReason::OverBudget | RemoveReason::ProtocolError
            )
        {
            self.retired_sse.put(*key, (Instant::now(), reason));
        }
        let sse_summary = self.sse_streams.pop(key).and_then(|stream| {
            if reason.metadata_only() {
                return None;
            }
            let (events, count, timestamp) = stream.into_events();
            if let ConnectionState::SseActive { sse_events, .. } = &mut state {
                *sse_events = events;
                Some((count, timestamp))
            } else {
                None
            }
        });
        let continuation = self.cleanup(key).filter(|_| !reason.metadata_only());
        if matches!(
            reason,
            RemoveReason::LruEvicted
                | RemoveReason::OverBudget
                | RemoveReason::HardTimeout
                | RemoveReason::IdleTimeout
        ) {
            self.eviction_count = self.eviction_count.saturating_add(1);
        }
        let kind = match &state {
            ConnectionState::Idle => "idle",
            ConnectionState::RequestPending { .. } => "request_pending",
            ConnectionState::RequestBodyPending { .. } => "request_body_pending",
            ConnectionState::ResponsePending { .. } => "response_pending",
            ConnectionState::SseActive { .. } => "sse_active",
        };
        metrics::counter!("agentsight_http_conn_removed_total", "reason" => reason.as_str(), "state" => kind).increment(1);
        Some(RemovedConnection {
            id: *key,
            state,
            continuation,
            reason,
            sse_summary,
        })
    }

    pub(super) fn abandon(&mut self, key: &ConnectionId, reason: RemoveReason) {
        if let Some(removed) = self.remove_connection(key, reason) {
            self.queue_removed(std::iter::once(removed));
        }
    }

    pub(crate) fn queue_removed(&mut self, removed: impl IntoIterator<Item = RemovedConnection>) {
        self.ready.extend(
            removed
                .into_iter()
                .filter_map(RemovedConnection::into_result),
        );
    }

    pub(crate) fn take_ready(&mut self) -> Vec<AggregatedResult> {
        std::mem::take(&mut self.ready)
    }

    pub(crate) fn drain_pending(&mut self, reason: RemoveReason) {
        let keys: Vec<_> = self.connections.iter().map(|(id, _)| *id).collect();
        for id in keys {
            self.abandon(&id, reason);
        }
    }

    pub(super) fn ensure_room(&mut self, id: &ConnectionId) -> bool {
        let now = Instant::now();
        self.expire_completed(now);
        // A returning keep-alive request consumes its own reserved slot. New
        // connections cannot steal that slot between a response and its next request.
        self.recently_completed.pop(id);
        if self.connections.len() + self.recently_completed.len() >= self.connections.cap().get() {
            let Some(key) = self.eviction_candidate(now) else {
                return false;
            };
            self.abandon(&key, RemoveReason::LruEvicted);
        }
        true
    }

    fn expire_completed(&mut self, now: Instant) {
        while self.recently_completed.peek_lru().is_some_and(|(_, time)| {
            now.saturating_duration_since(*time)
                >= crate::aggregator::result::ACTIVE_CONNECTION_GRACE
        }) {
            self.recently_completed.pop_lru();
        }
    }

    /// Protect recent traffic throughout request assembly, response waiting and streaming.
    pub(super) fn eviction_candidate(&self, now: Instant) -> Option<ConnectionId> {
        self.connections.iter().rev().find_map(|(id, state)| {
            let protected = !matches!(state, ConnectionState::Idle)
                && self.last_activity.peek(id).is_some_and(|time| {
                    now.saturating_duration_since(*time)
                        < crate::aggregator::result::ACTIVE_CONNECTION_GRACE
                });
            (!protected).then_some(*id)
        })
    }

    /// Refuse a new exchange while preserving all protected streams and the count cap.
    pub(super) fn reject_capacity(&mut self, id: ConnectionId, state: ConnectionState) {
        self.retired_sse
            .put(id, (Instant::now(), RemoveReason::CapacityRejected));
        metrics::counter!("agentsight_http_conn_removed_total", "reason" => "capacity_rejected", "state" => "admission").increment(1);
        self.queue_removed([RemovedConnection {
            id,
            state,
            continuation: None,
            reason: RemoveReason::CapacityRejected,
            sse_summary: None,
        }]);
    }

    pub(super) fn insert(&mut self, key: ConnectionId, state: ConnectionState) {
        // State transitions pop the old value first. An occupied key here is
        // therefore a genuinely new request replacing unfinished evidence.
        if self.connections.contains(&key) {
            self.abandon(&key, RemoveReason::Replaced);
        }
        if !self.ensure_room(&key) {
            self.reject_capacity(key, state);
            return;
        }
        self.touch(&key);
        self.connections.put(key, state);
        self.refresh_retained(&key);
        self.enforce_connection_limits(&key);
    }

    pub(super) fn enforce_connection_limits(&mut self, id: &ConnectionId) {
        if let Some(reason) = self.sse_errors.pop(id) {
            self.abandon(id, reason);
        }
        let over = match self.connections.peek(id) {
            Some(ConnectionState::RequestBodyPending { body_buffer, .. }) => {
                body_buffer.len() > self.max_body_bytes
            }
            Some(ConnectionState::SseActive { .. }) => self
                .retained
                .get(id)
                .is_some_and(|retained| retained.sse_bytes > self.max_sse_retained_bytes),
            _ => false,
        };
        if over {
            self.abandon(id, RemoveReason::OverBudget);
        }
        self.enforce_limits();
    }

    pub(super) fn enforce_limits(&mut self) {
        while self.bytes_budget != 0 && self.metrics().connection_cache_bytes > self.bytes_budget {
            let Some(id) = self
                .eviction_candidate(Instant::now())
                .or_else(|| self.connections.peek_lru().map(|(id, _)| *id))
            else {
                break;
            };
            self.abandon(&id, RemoveReason::OverBudget);
        }
    }

    /// Run maintenance independently of incoming traffic.
    pub(crate) fn tick(&mut self, now: Instant) {
        self.expire_completed(now);
        let retention = self
            .hard_timeout
            .max(self.idle_timeout)
            .max(super::DEFAULT_IDLE_TIMEOUT);
        let expired: Vec<_> = self
            .retired_sse
            .iter()
            .filter_map(|(key, (time, _))| {
                (now.saturating_duration_since(*time) >= retention).then_some(*key)
            })
            .collect();
        for key in expired {
            self.retired_sse.pop(&key);
        }
        let idle: Vec<_> = self
            .last_activity
            .iter()
            .filter_map(|(key, time)| {
                (matches!(self.connections.peek(key), Some(ConnectionState::Idle))
                    && now.saturating_duration_since(*time) >= self.idle_timeout)
                    .then_some(*key)
            })
            .collect();
        for key in idle {
            self.abandon(&key, RemoveReason::IdleTimeout);
        }
        if !self.hard_timeout.is_zero() {
            let expired: Vec<_> = self
                .last_activity
                .iter()
                .filter_map(|(key, time)| {
                    (now.saturating_duration_since(*time) >= self.hard_timeout).then_some(*key)
                })
                .collect();
            for key in expired {
                self.abandon(&key, RemoveReason::HardTimeout);
            }
        }
        let keys: Vec<_> = self.connections.iter().map(|(id, _)| *id).collect();
        for id in keys {
            self.refresh_retained(&id);
            self.enforce_connection_limits(&id);
        }
    }

    /// Preserve soft-timeout pending semantics without cloning connection payloads.
    pub(crate) fn snapshot_idle_connections<T>(
        &mut self,
        mut make: impl FnMut(&ConnectionId, &ParsedRequest) -> Option<T>,
    ) -> Vec<T> {
        let now = Instant::now();
        let mut snapshots = Vec::new();
        for (key, time) in self.last_activity.iter() {
            if now.saturating_duration_since(*time) < self.idle_timeout
                || self.idle_snapshotted.contains(key)
            {
                continue;
            }
            if let Some(request) = self
                .connections
                .peek(key)
                .and_then(ConnectionState::pending_request)
            {
                if let Some(snapshot) = make(key, request) {
                    self.idle_snapshotted.put(*key, ());
                    snapshots.push(snapshot);
                }
            }
        }
        snapshots
    }

    /// Rank unprotected entries ahead of the hard-byte fallback across HTTP versions.
    pub(crate) fn eviction_priority(&self) -> Option<(bool, Instant)> {
        if let Some(id) = self.eviction_candidate(Instant::now()) {
            return self.last_activity.peek(&id).map(|time| (false, *time));
        }
        self.last_activity.peek_lru().map(|(_, time)| (true, *time))
    }

    pub(crate) fn abandon_oldest(&mut self) -> bool {
        let Some(id) = self
            .eviction_candidate(Instant::now())
            .or_else(|| self.connections.peek_lru().map(|(id, _)| *id))
        else {
            return false;
        };
        self.abandon(&id, RemoveReason::OverBudget);
        true
    }

    pub(crate) fn drain_connections_for_pid(&mut self, pid: u32) -> Vec<RemovedConnection> {
        let completed: Vec<_> = self
            .recently_completed
            .iter()
            .filter_map(|(key, _)| (key.pid == pid).then_some(*key))
            .collect();
        for key in completed {
            self.recently_completed.pop(&key);
        }
        let retired: Vec<_> = self
            .retired_sse
            .iter()
            .filter_map(|(key, _)| (key.pid == pid).then_some(*key))
            .collect();
        for key in retired {
            self.retired_sse.pop(&key);
        }
        let keys: Vec<_> = self
            .connections
            .iter()
            .filter_map(|(key, _)| (key.pid == pid).then_some(*key))
            .collect();
        keys.into_iter()
            .filter_map(|key| self.remove_connection(&key, RemoveReason::PidExited))
            .collect()
    }

    pub(crate) fn drain_dead_pid_connections(&mut self) -> Vec<RemovedConnection> {
        let pids: std::collections::HashSet<_> = self
            .connections
            .iter()
            .map(|(key, _)| key.pid)
            .chain(self.retired_sse.iter().map(|(key, _)| key.pid))
            .chain(self.recently_completed.iter().map(|(key, _)| key.pid))
            .collect();
        pids.into_iter()
            .filter(|pid| !crate::utils::procfs::proc_pid(*pid).exists())
            .flat_map(|pid| self.drain_connections_for_pid(pid))
            .collect()
    }

    /// Retire side caches after a normal result has taken ownership of its payload.
    pub(crate) fn cleanup_completed(&mut self, id: &ConnectionId) {
        if !self.connections.contains(id) {
            let tracked = self.last_activity.contains(id);
            self.cleanup(id);
            self.sse_streams.pop(id);
            if tracked {
                self.recently_completed.put(*id, Instant::now());
            }
        }
    }

    pub(super) fn record_completed(&mut self, result: Option<&AggregatedResult>) {
        let (id, state) = match result {
            Some(AggregatedResult::SseComplete(pair)) => (pair.connection_id, "sse_active"),
            Some(AggregatedResult::HttpComplete(pair)) => (pair.connection_id, "response_pending"),
            Some(AggregatedResult::ResponseOnly {
                connection_id,
                response,
            }) => (
                *connection_id,
                if response.parsed.is_sse() {
                    "sse_active"
                } else {
                    "response_pending"
                },
            ),
            _ => return,
        };
        if self.last_activity.contains(&id) {
            metrics::counter!("agentsight_http_conn_removed_total", "reason" => "completed", "state" => state).increment(1);
        }
        self.cleanup_completed(&id);
    }
}
