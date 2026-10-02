//! LLM call trajectory recorder — captures each request/response pair and
//! exports the full session as an ATIF v1.7 document.
//!
//! Similar to how `~/.claude` or `~/.qoder` persist conversation histories,
//! this recorder saves the optimization analysis LLM interactions to disk
//! in ATIF format for debugging, auditing, and replay.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use agentsight_atif::{
    Agent, AtifTrajectory, FinalMetrics, Metrics, Observation, ObservationResult, Step, StepSource,
    SubagentTrajectoryRef, ToolCall, ATIF_SCHEMA_VERSION,
};
use chrono::Utc;

use super::types::ChatMessage;

/// A single recorded LLM call.
#[derive(Debug, Clone)]
pub struct RecordedCall {
    /// Input messages sent to the LLM.
    pub messages: Vec<ChatMessage>,
    /// The LLM response text.
    pub response: String,
    /// Hidden reasoning returned alongside the answer by reasoning models.
    /// Billed inside [`Self::output_tokens`], so without it a 12k-token step
    /// looks like a 300-token JSON blob.
    pub reasoning: Option<String>,
    /// Model used for this call.
    pub model: String,
    /// Input token count (from usage).
    pub input_tokens: u32,
    /// Output token count (from usage).
    pub output_tokens: u32,
    /// Input tokens served from the provider's prompt cache.
    pub cached_tokens: u32,
    /// Share of [`Self::output_tokens`] the provider attributes to reasoning.
    pub reasoning_tokens: u32,
    /// ISO 8601 timestamp when the request was sent.
    pub start_ts: String,
    /// ISO 8601 timestamp when the response was received.
    pub end_ts: String,
    /// Attribution label of a parallel fan-out call (e.g. "perf:prefix_cache").
    /// Labeled calls are grouped into subagent trajectories in the ATIF export.
    pub label: Option<String>,
}

/// Parameters for recording a single LLM call.
pub struct RecordParams<'a> {
    pub messages: &'a [ChatMessage],
    pub response: &'a str,
    /// Hidden reasoning (see [`RecordedCall::reasoning`]).
    pub reasoning: Option<&'a str>,
    pub model: &'a str,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// See [`RecordedCall::cached_tokens`].
    pub cached_tokens: u32,
    /// See [`RecordedCall::reasoning_tokens`].
    pub reasoning_tokens: u32,
    pub start_ts: &'a str,
    pub end_ts: &'a str,
    /// Attribution label (see [`RecordedCall::label`]).
    pub label: Option<&'a str>,
}

/// Thread-safe recorder that accumulates LLM calls during an analysis session.
#[derive(Debug, Default)]
pub struct TrajectoryRecorder {
    calls: Mutex<Vec<RecordedCall>>,
    model: String,
    session_label: String,
}

impl TrajectoryRecorder {
    /// Create a new recorder for a given analysis session.
    ///
    /// `session_label` identifies the analysis run (e.g. "perf-issues/session-abc123").
    pub fn new(model: impl Into<String>, session_label: impl Into<String>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            model: model.into(),
            session_label: session_label.into(),
        }
    }

    /// Record a completed LLM call.
    pub fn record(&self, params: RecordParams<'_>) {
        let call = RecordedCall {
            messages: params.messages.to_vec(),
            response: params.response.to_string(),
            reasoning: params.reasoning.map(str::to_string),
            model: params.model.to_string(),
            input_tokens: params.input_tokens,
            output_tokens: params.output_tokens,
            cached_tokens: params.cached_tokens,
            reasoning_tokens: params.reasoning_tokens,
            start_ts: params.start_ts.to_string(),
            end_ts: params.end_ts.to_string(),
            label: params.label.map(str::to_string),
        };
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }

    /// Number of recorded calls.
    pub fn len(&self) -> usize {
        self.calls.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Whether no calls have been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Export recorded calls as an ATIF v1.7 trajectory document.
    ///
    /// Unlabeled calls become root-level steps. Labeled calls (parallel
    /// fan-out, e.g. one per perf/cost strategy) are grouped by label into
    /// embedded `subagent_trajectories`; the root gains one dispatch step per
    /// label with a `ToolCall(Agent)` and a `subagent_trajectory_ref`, mirroring
    /// how coding agents record delegated subagent tasks.
    pub fn to_atif(&self) -> AtifTrajectory {
        let calls = self.calls.lock().map(|c| c.clone()).unwrap_or_default();

        let mut steps: Vec<Step> = Vec::new();
        let mut step_id: usize = 0;
        let mut total_prompt: u64 = 0;
        let mut total_completion: u64 = 0;
        let mut total_cached: u64 = 0;

        // Group labeled calls by label, preserving first-appearance order.
        let mut group_index: HashMap<String, usize> = HashMap::new();
        let mut groups: Vec<(String, Vec<&RecordedCall>)> = Vec::new();

        for call in &calls {
            total_prompt += call.input_tokens as u64;
            total_completion += call.output_tokens as u64;
            total_cached += call.cached_tokens as u64;

            let Some(label) = &call.label else {
                Self::append_call_steps(&mut steps, &mut step_id, call);
                continue;
            };
            match group_index.get(label) {
                Some(&i) => groups[i].1.push(call),
                None => {
                    group_index.insert(label.clone(), groups.len());
                    groups.push((label.clone(), vec![call]));
                    // Dispatch step: tool call + subagent trajectory reference.
                    step_id += 1;
                    let call_id = format!("dispatch-{label}");
                    steps.push(Step {
                        step_id,
                        timestamp: Some(call.start_ts.clone()),
                        source: StepSource::Agent,
                        message: String::new(),
                        model_name: Some(call.model.clone()),
                        reasoning_effort: None,
                        reasoning_content: None,
                        tool_calls: Some(vec![ToolCall {
                            tool_call_id: call_id.clone(),
                            function_name: "Agent".to_string(),
                            arguments: serde_json::json!({ "subagent_type": label }),
                            extra: None,
                        }]),
                        observation: Some(Observation {
                            results: vec![ObservationResult {
                                source_call_id: Some(call_id),
                                content: None,
                                subagent_trajectory_ref: Some(vec![SubagentTrajectoryRef {
                                    trajectory_id: Some(label.clone()),
                                    trajectory_path: None,
                                    session_id: None,
                                    extra: None,
                                }]),
                                extra: None,
                            }],
                        }),
                        metrics: None,
                        extra: None,
                        llm_call_count: None,
                        is_copied_context: None,
                    });
                }
            }
        }

        // Build one embedded subagent trajectory per label group.
        let subagents: Vec<AtifTrajectory> = groups
            .iter()
            .map(|(label, group_calls)| {
                let mut sub_steps: Vec<Step> = Vec::new();
                let mut sub_step_id: usize = 0;
                let mut sub_prompt: u64 = 0;
                let mut sub_completion: u64 = 0;
                let mut sub_cached: u64 = 0;
                for call in group_calls {
                    sub_prompt += call.input_tokens as u64;
                    sub_completion += call.output_tokens as u64;
                    sub_cached += call.cached_tokens as u64;
                    Self::append_call_steps(&mut sub_steps, &mut sub_step_id, call);
                }
                AtifTrajectory {
                    schema_version: ATIF_SCHEMA_VERSION.to_string(),
                    session_id: None,
                    trajectory_id: Some(label.clone()),
                    notes: None,
                    agent: Agent {
                        name: format!("agentsight-opt:{label}"),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        model_name: Some(self.model.clone()),
                        tool_definitions: None,
                        extra: None,
                    },
                    steps: sub_steps,
                    final_metrics: Some(FinalMetrics {
                        total_prompt_tokens: Some(sub_prompt),
                        total_completion_tokens: Some(sub_completion),
                        total_cached_tokens: Some(sub_cached),
                        total_cost_usd: None,
                        total_steps: Some(sub_step_id),
                        extra: None,
                    }),
                    continued_trajectory_ref: None,
                    subagent_trajectories: None,
                    extra: None,
                }
            })
            .collect();

        let mut doc_extra = HashMap::new();
        doc_extra.insert(
            "recorded_at".to_string(),
            serde_json::Value::String(Utc::now().to_rfc3339()),
        );
        doc_extra.insert("call_count".to_string(), serde_json::json!(calls.len()));

        AtifTrajectory {
            schema_version: ATIF_SCHEMA_VERSION.to_string(),
            session_id: Some(self.session_label.clone()),
            trajectory_id: None,
            notes: None,
            agent: Agent {
                name: "agentsight-opt".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                model_name: Some(self.model.clone()),
                tool_definitions: None,
                extra: None,
            },
            steps,
            final_metrics: Some(FinalMetrics {
                total_prompt_tokens: Some(total_prompt),
                total_completion_tokens: Some(total_completion),
                total_cached_tokens: Some(total_cached),
                total_cost_usd: None,
                total_steps: Some(step_id),
                extra: None,
            }),
            continued_trajectory_ref: None,
            subagent_trajectories: if subagents.is_empty() {
                None
            } else {
                Some(subagents)
            },
            extra: Some(doc_extra),
        }
    }

    /// Append the step group of one recorded call (system / user / agent) to
    /// `steps`, advancing `step_id`.
    fn append_call_steps(steps: &mut Vec<Step>, step_id: &mut usize, call: &RecordedCall) {
        // System prompt step (first system message, if any).
        if let Some(sys) = call.messages.iter().find(|m| m.role == "system") {
            *step_id += 1;
            steps.push(Step {
                step_id: *step_id,
                timestamp: Some(call.start_ts.clone()),
                source: StepSource::System,
                message: sys.content.clone(),
                model_name: None,
                reasoning_effort: None,
                reasoning_content: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                extra: None,
                llm_call_count: None,
                is_copied_context: None,
            });
        }

        // User messages (non-system) as user steps.
        for msg in call.messages.iter().filter(|m| m.role != "system") {
            *step_id += 1;
            let source = if msg.role == "assistant" {
                StepSource::Agent
            } else {
                StepSource::User
            };
            steps.push(Step {
                step_id: *step_id,
                timestamp: Some(call.start_ts.clone()),
                source,
                message: msg.content.clone(),
                model_name: if msg.role == "assistant" {
                    Some(call.model.clone())
                } else {
                    None
                },
                reasoning_effort: None,
                reasoning_content: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                extra: None,
                llm_call_count: None,
                is_copied_context: None,
            });
        }

        // Agent response step.
        *step_id += 1;
        let mut extra = HashMap::new();
        extra.insert(
            "start_timestamp".to_string(),
            serde_json::Value::String(call.start_ts.clone()),
        );
        steps.push(Step {
            step_id: *step_id,
            timestamp: Some(call.end_ts.clone()),
            source: StepSource::Agent,
            message: call.response.clone(),
            model_name: Some(call.model.clone()),
            reasoning_effort: None,
            reasoning_content: call.reasoning.clone(),
            tool_calls: None,
            observation: None,
            metrics: Some(Metrics {
                prompt_tokens: Some(call.input_tokens as u64),
                completion_tokens: Some(call.output_tokens as u64),
                cached_tokens: Some(call.cached_tokens as u64),
                cost_usd: None,
                logprobs: None,
                completion_token_ids: None,
                prompt_token_ids: None,
                // ATIF has no first-class reasoning-token field, but the split
                // is what makes `completion_tokens` add up against a short
                // answer, so it rides along in `extra`.
                extra: (call.reasoning_tokens > 0).then(|| {
                    HashMap::from([(
                        "reasoning_tokens".to_string(),
                        serde_json::json!(call.reasoning_tokens),
                    )])
                }),
            }),
            extra: Some(extra),
            llm_call_count: None,
            is_copied_context: None,
        });
    }

    /// Export and save the ATIF trajectory to a directory.
    ///
    /// File name: `{session_label}_{timestamp}.atif.json`
    /// Returns the path of the saved file.
    pub fn save_to_dir(&self, dir: &Path) -> std::io::Result<std::path::PathBuf> {
        std::fs::create_dir_all(dir)?;

        let ts = Utc::now().format("%Y%m%d_%H%M%S");
        // Sanitize session_label for file name.
        let safe_label: String = self
            .session_label
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let file_name = format!("{safe_label}_{ts}.atif.json");
        let path = dir.join(file_name);

        let json = self.to_atif_json()?;
        std::fs::write(&path, json)?;

        tracing::info!(
            "Saved opt LLM trajectory ({} calls) to {}",
            self.len(),
            path.display()
        );
        Ok(path)
    }

    /// Serialize the ATIF v1.7 document to a pretty JSON string.
    ///
    /// # Errors
    /// Returns an I/O error if serialization fails.
    pub fn to_atif_json(&self) -> std::io::Result<String> {
        let doc = self.to_atif();
        serde_json::to_string_pretty(&doc).map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// The session label identifying this analysis run.
    pub fn session_label(&self) -> &str {
        &self.session_label
    }

    /// The model used for LLM calls.
    pub fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_call(
        recorder: &TrajectoryRecorder,
        system: &str,
        user: &str,
        response: &str,
        tokens: (u32, u32),
        label: Option<&str>,
    ) {
        record_call_with_reasoning(
            recorder,
            system,
            user,
            response,
            None,
            (tokens.0, tokens.1, 0, 0),
            label,
        );
    }

    /// `usage` is (input, output, cached, reasoning) token counts.
    fn record_call_with_reasoning(
        recorder: &TrajectoryRecorder,
        system: &str,
        user: &str,
        response: &str,
        reasoning: Option<&str>,
        usage: (u32, u32, u32, u32),
        label: Option<&str>,
    ) {
        recorder.record(RecordParams {
            messages: &[ChatMessage::system(system), ChatMessage::user(user)],
            response,
            reasoning,
            model: "gpt-4o",
            input_tokens: usage.0,
            output_tokens: usage.1,
            cached_tokens: usage.2,
            reasoning_tokens: usage.3,
            start_ts: "2026-07-24T10:00:00+00:00",
            end_ts: "2026-07-24T10:00:05+00:00",
            label,
        });
    }

    #[test]
    fn test_recorder_saves_atif() {
        let recorder = TrajectoryRecorder::new("gpt-4o", "perf-issues/test-session");

        // Simulate two LLM calls.
        record_call(
            &recorder,
            "You are a perf analyzer.",
            "Analyze this trajectory.",
            r#"{"issues": []}"#,
            (1500, 42),
            None,
        );
        record_call(
            &recorder,
            "You are a cache analyzer.",
            "Check cache hits.",
            r#"{"applicable": true}"#,
            (800, 30),
            None,
        );

        assert_eq!(recorder.len(), 2);
        assert!(!recorder.is_empty());

        // Verify ATIF v1.7 export structure.
        let doc = recorder.to_atif();
        assert_eq!(doc.schema_version, "ATIF-v1.7");
        assert_eq!(doc.session_id.as_deref(), Some("perf-issues/test-session"));
        assert!(doc.steps.len() > 2); // at least system + user + agent per call
        assert!(doc.subagent_trajectories.is_none()); // unlabeled calls stay flat
        let metrics = doc.final_metrics.as_ref().unwrap();
        assert_eq!(metrics.total_prompt_tokens, Some(2300));
        assert_eq!(metrics.total_completion_tokens, Some(72));

        // Save to temp dir and verify file exists.
        let tmp_dir = std::env::temp_dir().join("agentsight-opt-test-trajectories");
        let path = recorder.save_to_dir(&tmp_dir).unwrap();
        assert!(path.exists());
        assert!(path.to_str().unwrap().ends_with(".atif.json"));

        // Verify the file is valid ATIF v1.7 JSON.
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["schema_version"], "ATIF-v1.7");
        assert_eq!(parsed["agent"]["name"], "agentsight-opt");
        assert!(!parsed["steps"].as_array().unwrap().is_empty());

        // Cleanup.
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&tmp_dir);

        println!(
            "ATIF v1.7 trajectory saved and verified at: {}",
            path.display()
        );
    }

    /// A reasoning model bills its hidden chain-of-thought in
    /// `completion_tokens`, so a 12k-token step can carry a 300-token answer.
    /// The reasoning has to land on the step or that gap is unexplainable.
    #[test]
    fn reasoning_lands_on_the_agent_step_of_its_own_subagent() {
        let recorder = TrajectoryRecorder::new("glm-5.2", "cost-waste/test-session");
        record_call_with_reasoning(
            &recorder,
            "Waste judge.",
            "Judge trial_error.",
            r#"{"detected":false,"findings":[]}"#,
            Some("Checked all 85 turns; every one advances the task."),
            (4128, 12412, 3000, 12100),
            Some("cost:trial_error"),
        );
        record_call_with_reasoning(
            &recorder,
            "Waste judge.",
            "Judge tool_output.",
            r#"{"worth_optimizing":true}"#,
            None,
            (1207, 1127, 0, 0),
            Some("cost:tool_output"),
        );

        let doc = recorder.to_atif();
        let subs = doc.subagent_trajectories.as_ref().unwrap();

        // Reasoning goes on the agent step (the one carrying the token
        // metrics), not on the system/user steps that precede it.
        let agent_steps: Vec<&Step> = subs[0]
            .steps
            .iter()
            .filter(|s| s.metrics.is_some())
            .collect();
        assert_eq!(agent_steps.len(), 1);
        assert_eq!(
            agent_steps[0].reasoning_content.as_deref(),
            Some("Checked all 85 turns; every one advances the task.")
        );
        assert!(subs[0]
            .steps
            .iter()
            .filter(|s| s.metrics.is_none())
            .all(|s| s.reasoning_content.is_none()));

        // Cached input and the reasoning share of the output are both carried,
        // so 12412 completion tokens against a 33-char answer adds up.
        let m = agent_steps[0].metrics.as_ref().unwrap();
        assert_eq!(m.cached_tokens, Some(3000));
        assert_eq!(m.extra.as_ref().unwrap()["reasoning_tokens"], 12100);
        assert_eq!(
            subs[0].final_metrics.as_ref().unwrap().total_cached_tokens,
            Some(3000)
        );

        // A non-reasoning response leaves both absent rather than empty, so the
        // ATIF stays free of null noise.
        assert!(subs[1].steps.iter().all(|s| s.reasoning_content.is_none()));
        let plain = subs[1]
            .steps
            .iter()
            .find_map(|s| s.metrics.as_ref())
            .unwrap();
        assert!(plain.extra.is_none());
        let json = recorder.to_atif_json().unwrap();
        assert_eq!(json.matches("reasoning_content").count(), 1);
        assert_eq!(json.matches("reasoning_tokens").count(), 1);
    }

    /// An attempt whose answer came back empty is discarded and retried, but it
    /// was still billed — and its reasoning is the only record of where those
    /// tokens went. It has to stay in the trajectory and in the totals.
    #[test]
    fn discarded_empty_attempt_stays_in_the_trajectory_and_totals() {
        let recorder = TrajectoryRecorder::new("glm-5.2", "cost-waste/test-session");
        record_call_with_reasoning(
            &recorder,
            "Waste judge.",
            "Judge trial_error.",
            "",
            Some("Spent the whole budget enumerating turns and never answered."),
            (4128, 16000, 0, 16000),
            Some("cost:trial_error"),
        );
        record_call_with_reasoning(
            &recorder,
            "Waste judge.",
            "Judge trial_error.",
            r#"{"detected":false}"#,
            Some("Shorter pass this time."),
            (4128, 900, 0, 700),
            Some("cost:trial_error"),
        );

        let doc = recorder.to_atif();
        let sub = &doc.subagent_trajectories.as_ref().unwrap()[0];

        // Both attempts present: 2 calls × (system + user + agent).
        assert_eq!(sub.steps.len(), 6);
        let answers: Vec<&str> = sub
            .steps
            .iter()
            .filter(|s| s.metrics.is_some())
            .map(|s| s.message.as_str())
            .collect();
        assert_eq!(answers, vec!["", r#"{"detected":false}"#]);

        // The empty attempt is billed, so it counts toward the totals; without
        // it the run would look ~16k output tokens cheaper than it was.
        let m = sub.final_metrics.as_ref().unwrap();
        assert_eq!(m.total_completion_tokens, Some(16900));
        assert_eq!(m.total_prompt_tokens, Some(8256));
    }

    #[test]
    fn test_labeled_calls_become_subagent_trajectories() {
        let recorder = TrajectoryRecorder::new("gpt-4o", "perf-issues/test-session");

        // One unlabeled call + two parallel strategy calls; the second
        // strategy needs a JSON-fix retry (same label, two calls).
        record_call(
            &recorder,
            "Orchestrator.",
            "Extract candidates.",
            "ok",
            (100, 10),
            None,
        );
        record_call(
            &recorder,
            "Cache strategy judge.",
            "Evaluate prefix_cache.",
            r#"{"applicable": true}"#,
            (500, 20),
            Some("perf:prefix_cache"),
        );
        record_call(
            &recorder,
            "Tool strategy judge.",
            "Evaluate fast_tool.",
            "bad json",
            (400, 15),
            Some("perf:fast_tool"),
        );
        record_call(
            &recorder,
            "Tool strategy judge.",
            "Fix your JSON.",
            r#"{"applicable": false}"#,
            (450, 18),
            Some("perf:fast_tool"),
        );

        let doc = recorder.to_atif();

        // Two label groups → two embedded subagent trajectories.
        let subs = doc.subagent_trajectories.as_ref().unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].trajectory_id.as_deref(), Some("perf:prefix_cache"));
        assert_eq!(subs[0].agent.name, "agentsight-opt:perf:prefix_cache");
        assert_eq!(subs[1].trajectory_id.as_deref(), Some("perf:fast_tool"));
        // Retry call folded into the same subagent: 2 calls × 3 steps.
        assert_eq!(subs[1].steps.len(), 6);
        let sub_metrics = subs[1].final_metrics.as_ref().unwrap();
        assert_eq!(sub_metrics.total_prompt_tokens, Some(850));
        assert_eq!(sub_metrics.total_completion_tokens, Some(33));

        // Root: 3 steps from the unlabeled call + 2 dispatch steps.
        assert_eq!(doc.steps.len(), 5);
        let dispatches: Vec<&Step> = doc
            .steps
            .iter()
            .filter(|s| s.tool_calls.is_some())
            .collect();
        assert_eq!(dispatches.len(), 2);
        let tc = &dispatches[0].tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.function_name, "Agent");
        assert_eq!(tc.arguments["subagent_type"], "perf:prefix_cache");
        let obs = dispatches[0].observation.as_ref().unwrap();
        let refs = obs.results[0].subagent_trajectory_ref.as_ref().unwrap();
        assert_eq!(refs[0].trajectory_id.as_deref(), Some("perf:prefix_cache"));

        // Root final_metrics aggregate the whole run (root + subagents).
        let metrics = doc.final_metrics.as_ref().unwrap();
        assert_eq!(metrics.total_prompt_tokens, Some(1450));
        assert_eq!(metrics.total_completion_tokens, Some(63));
    }
}
