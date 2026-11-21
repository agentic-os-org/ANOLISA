//! Track query reachability separately from evidence about actual collection.

use super::{now_ms, Observation, State, QUERY_BUDGET};
use crate::{rejected, runtime::Runtime, Result};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use super::super::{source_failure, Settings};

pub(super) const PROBE_INTERVAL: Duration = Duration::from_secs(2);
const MAX_TRANSITIONS: usize = 128;

impl Observation {
    pub(super) fn probe(
        &self,
        runtime: &Runtime,
        settings: &Settings,
        deadline: Instant,
    ) -> Result<()> {
        let deadline = deadline.min(
            Instant::now() + QUERY_BUDGET.min(Duration::from_millis(settings.query_timeout_ms)),
        );
        let result = settings
            .client()
            .and_then(|client| client.probe_until(deadline, settings.query_bytes));
        let mut state = self
            .state
            .lock()
            .map_err(|_| rejected("observation_state"))?;
        match result {
            Ok(_) => {
                let next = match state.metadata["source"]["state"].as_str() {
                    Some("ready") => "ready",
                    Some("recovered" | "lost" | "unsupported") => "recovered",
                    _ => "ready",
                };
                if state.metadata["query_state"] != "ready" {
                    state.metadata["query_state"] = json!("ready");
                    state.metadata["source_state"] = json!("unknown");
                    state.metadata["source_reason"] = json!("awaiting_native_identity");
                }
                self.coverage(runtime, &mut state, next, None);
            }
            Err(crate::agentsight::Error::Http(405 | 410 | 501)) => {
                self.lose_source(runtime, &mut state, "source_version");
            }
            Err(error) => {
                self.lose_source(runtime, &mut state, source_failure(&error));
            }
        }
        Ok(())
    }

    pub(super) fn lose_source(&self, runtime: &Runtime, state: &mut State, reason: &str) {
        if state.metadata["query_state"] == "ready" {
            state.source_epoch += 1;
        }
        state.metadata["query_state"] = json!("unavailable");
        state.metadata["source_state"] = json!("unavailable");
        state.metadata["source_reason"] = json!(reason);
        state.metadata["projection_revision"] = Value::Null;
        state.metadata["source_revision"] = Value::Null;
        state.metadata["evidence_gaps"] = Value::Null;
        state.latest = None;
        let next = if reason == "source_version" {
            "unsupported"
        } else {
            "lost"
        };
        self.coverage(runtime, state, next, Some(reason));
    }

    fn coverage(&self, runtime: &Runtime, state: &mut State, next: &str, reason: Option<&str>) {
        let source = &mut state.metadata["source"];
        if source["state"] == next && source["reason"] == json!(reason) {
            return;
        }
        let previous = source["state"].clone();
        source["state"] = json!(next);
        source["reason"] = json!(reason);
        source["changed_at_ms"] = json!(now_ms().ok());
        let mut gaps = vec!["collector_coverage_unmeasured", "ebpf_coverage_unmeasured"];
        gaps.extend(reason);
        source["gaps"] = json!(gaps);
        let record = json!({
            "phase":"coverage_changed", "event":"coverage.changed",
            "previous":previous,"source":state.metadata["source"],
        });
        if state.coverage_transitions == MAX_TRANSITIONS {
            state.metadata["coverage_transition_history_truncated"] = json!(true);
            self.record(
                runtime,
                state,
                json!({
                    "phase":"observation_gap", "reason":"coverage_transition_limit",
                }),
            );
        } else if state.coverage_transitions < MAX_TRANSITIONS {
            self.record(runtime, state, record);
        }
        if state.coverage_transitions <= MAX_TRANSITIONS {
            state.coverage_transitions += 1;
        }
    }
}
