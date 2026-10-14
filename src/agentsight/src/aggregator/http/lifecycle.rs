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
            state,
            continuation,
            reason,
            sse_summary,
        } = self;
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
                request.reassembled_body = Some(body_buffer);
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

impl HttpConnectionAggregator {
    pub(super) fn cleanup(&mut self, key: &ConnectionId) -> Option<Vec<u8>> {
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
        let sse_summary = self.sse_streams.pop(key).and_then(|stream| {
            let (events, count, timestamp) = stream.into_events();
            if let ConnectionState::SseActive { sse_events, .. } = &mut state {
                *sse_events = events;
                Some((count, timestamp))
            } else {
                None
            }
        });
        let continuation = self.cleanup(key);
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

    pub(super) fn ensure_room(&mut self) {
        if self.connections.len() == self.connections.cap().get() {
            if let Some((key, _)) = self.connections.peek_lru() {
                let key = *key;
                self.abandon(&key, RemoveReason::LruEvicted);
            }
        }
    }

    pub(super) fn insert(&mut self, key: ConnectionId, state: ConnectionState) {
        // State transitions pop the old value first. An occupied key here is
        // therefore a genuinely new request replacing unfinished evidence.
        if self.connections.contains(&key) {
            self.abandon(&key, RemoveReason::Replaced);
        }
        if self.connections.len() == self.connections.cap().get() {
            if let Some((key, _)) = self.connections.peek_lru() {
                let key = *key;
                self.abandon(&key, RemoveReason::LruEvicted);
            }
        }
        self.touch(&key);
        self.connections.put(key, state);
        self.enforce_limits();
    }

    pub(super) fn enforce_limits(&mut self) {
        let failed: Vec<_> = self
            .sse_errors
            .iter()
            .map(|(id, reason)| (*id, *reason))
            .collect();
        for (id, reason) in failed {
            self.abandon(&id, reason);
        }
        let oversized: Vec<_> = self
            .connections
            .iter()
            .filter_map(|(id, state)| {
                let over = match state {
                    ConnectionState::RequestBodyPending { body_buffer, .. } => {
                        body_buffer.len() > self.max_body_bytes
                    }
                    ConnectionState::SseActive { .. } => self.sse_streams.peek(id).map_or_else(
                        || {
                            state.sse_retained_bytes().saturating_add(
                                self.sse_continuation_buffers
                                    .peek(id)
                                    .map_or(0, Vec::capacity),
                            ) > self.max_sse_retained_bytes
                        },
                        |stream| stream.payload_retained_bytes() > self.max_sse_retained_bytes,
                    ),
                    _ => false,
                };
                over.then_some(*id)
            })
            .collect();
        for id in oversized {
            self.abandon(&id, RemoveReason::OverBudget);
        }
        while self.bytes_budget != 0 && self.metrics().connection_cache_bytes > self.bytes_budget {
            let Some((id, _)) = self.connections.peek_lru() else {
                break;
            };
            let id = *id;
            self.abandon(&id, RemoveReason::OverBudget);
        }
    }

    /// Run maintenance independently of incoming traffic.
    pub(crate) fn tick(&mut self, now: Instant) {
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
        self.enforce_limits();
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

    pub(crate) fn oldest_activity(&self) -> Option<Instant> {
        self.last_activity.peek_lru().map(|(_, time)| *time)
    }

    pub(crate) fn abandon_oldest(&mut self) -> bool {
        let Some((id, _)) = self.connections.peek_lru() else {
            return false;
        };
        let id = *id;
        self.abandon(&id, RemoveReason::OverBudget);
        true
    }

    pub(crate) fn drain_connections_for_pid(&mut self, pid: u32) -> Vec<RemovedConnection> {
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
        let pids: std::collections::HashSet<_> =
            self.connections.iter().map(|(key, _)| key.pid).collect();
        pids.into_iter()
            .filter(|pid| !crate::utils::procfs::proc_pid(*pid).exists())
            .flat_map(|pid| self.drain_connections_for_pid(pid))
            .collect()
    }

    /// Retire side caches after a normal result has taken ownership of its payload.
    pub(crate) fn cleanup_completed(&mut self, id: &ConnectionId) {
        if !self.connections.contains(id) {
            self.cleanup(id);
            self.sse_streams.pop(id);
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
