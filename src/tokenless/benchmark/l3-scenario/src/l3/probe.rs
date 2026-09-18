// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Semantic probe: can a model still answer from the compressed conversation?
//!
//! Retention answers "is the error line still in the text". It cannot answer
//! "can the model read it", which is the question that decides whether
//! the reference's lossless table re-encoding is genuinely lossless in use, or
//! whether tokenless' truncation broke something a string match cannot see.
//!
//! # Scoring follows the reference evaluator
//!
//! the reference scores answers in `benchmarks/comprehensive_eval.py` with
//! `evaluate_answer`, returning `exact_match`, token-level `f1_score` and
//! `contains_answer`. This module mirrors that so the two products are judged by
//! the same rule, and takes `contains_answer` as the verdict: a model answers a
//! factual question in a sentence while the ground truth is a short fact, so
//! exact match would reject correct answers. F1 is reported alongside as the
//! graded view.
//!
//! An LLM judge is deliberately not used. It would add a second source of
//! variance on top of the answering model, and the gate here is a *difference*
//! between two conditions — noise in the judge inflates or hides that difference
//! with no way to tell which.
//!
//! # The gate
//!
//! The four-layer plan sets L3's gate as "probe success-rate drop < 5%". The
//! drop is measured per scenario against the *uncompressed* conversation, not
//! against an absolute target: a question the model gets wrong on the original
//! says nothing about the compressor, so those are excluded from the
//! denominator rather than counted as compression damage.

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::asset::{Message, Scenario};

/// One probe question with its expected answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    /// What to ask.
    pub prompt: String,
    /// Short fact the answer must contain.
    pub ground_truth: String,
    /// What kind of fact, for grouping in the report.
    pub kind: String,
}

/// How one question fared under one condition.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Verdict {
    /// Whether the answer contained the ground truth. The headline judgement.
    pub contains: bool,
    /// Token-level F1 against the ground truth, as the reference computes it.
    pub f1: f64,
    /// Whether the answer matched exactly after normalisation.
    pub exact: bool,
}

/// Probe outcome for one scenario.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProbeScore {
    /// Questions asked.
    pub asked: usize,
    /// Answered correctly from the uncompressed conversation.
    pub correct_uncompressed: usize,
    /// Of those, still answered correctly after compression.
    pub retained: usize,
    /// Mean F1 on the uncompressed conversation.
    pub f1_uncompressed: f64,
    /// Mean F1 after compression.
    pub f1_compressed: f64,
    /// Questions the original could answer and the compressed could not.
    pub lost: Vec<String>,
}

impl ProbeScore {
    /// Share of answerable questions still answerable after compression.
    ///
    /// Conditioned on the original being right, so a question the model simply
    /// cannot do is not charged to the compressor. `None` when the original
    /// answered nothing, since there is then no baseline to lose against.
    pub fn success_rate(&self) -> Option<f64> {
        if self.correct_uncompressed == 0 {
            return None;
        }
        Some(self.retained as f64 / self.correct_uncompressed as f64)
    }

    /// Drop in success rate, the quantity the L3 gate is defined on.
    pub fn drop(&self) -> Option<f64> {
        self.success_rate().map(|rate| 1.0 - rate)
    }
}

/// Gate threshold from the four-layer plan: a drop beyond this is a signal.
pub const MAX_DROP: f64 = 0.05;

/// Normalise text the way the reference's evaluator does before comparing.
fn normalize(text: &str) -> String {
    text.to_lowercase().trim().to_string()
}

/// Token-level F1, ported from the reference's `compute_f1`.
///
/// Set-based rather than multiset-based, matching the reference exactly: repeated
/// tokens count once. Kept identical on purpose so a score here means the same
/// thing as a score in its reports.
pub fn compute_f1(prediction: &str, ground_truth: &str) -> f64 {
    let pred: HashSet<&str> = prediction.split_whitespace().collect();
    let truth: HashSet<&str> = ground_truth.split_whitespace().collect();
    if pred.is_empty() || truth.is_empty() {
        return 0.0;
    }
    let common = pred.intersection(&truth).count();
    if common == 0 {
        return 0.0;
    }
    let precision = common as f64 / pred.len() as f64;
    let recall = common as f64 / truth.len() as f64;
    2.0 * precision * recall / (precision + recall)
}

/// Judge one answer, mirroring the reference's `evaluate_answer`.
pub fn evaluate_answer(prediction: &str, ground_truth: &str) -> Verdict {
    let pred = normalize(prediction);
    let truth = normalize(ground_truth);
    Verdict {
        contains: pred.contains(&truth),
        f1: compute_f1(&pred, &truth),
        exact: pred == truth,
    }
}

/// Derive probe questions from a scenario's own payload.
///
/// Ground truth comes from the payload rather than a hand-maintained list, so a
/// regenerated asset cannot silently invalidate the questions. Only facts that
/// can be located unambiguously become questions; anything vaguer would measure
/// the model rather than the compressor.
pub fn questions(scenario: &Scenario) -> Vec<Question> {
    let mut out = Vec::new();

    // The date CacheAligner exists to stabilise.
    if scenario.messages.iter().any(|m| {
        m.get("content")
            .and_then(Value::as_str)
            .is_some_and(|t| t.contains("2025-01-06"))
    }) {
        out.push(Question {
            prompt: "What is the current date according to the system prompt? \
                     Answer with just the date in YYYY-MM-DD form."
                .to_string(),
            ground_truth: "2025-01-06".to_string(),
            kind: "date".to_string(),
        });
    }

    // Tool schemas: a model that cannot name the tool cannot call it.
    if let Some(tools) = &scenario.tools
        && let Some(first) = tools.first()
    {
        let function = first.get("function").unwrap_or(first);
        if let Some(name) = function.get("name").and_then(Value::as_str) {
            out.push(Question {
                prompt: "Name the first tool available to you. Answer with just \
                         the tool name."
                    .to_string(),
                ground_truth: name.to_string(),
                kind: "tool_name".to_string(),
            });
        }
    }

    for m in &scenario.messages {
        if m.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let Some(parsed) = m
            .get("content")
            .and_then(Value::as_str)
            .and_then(|c| serde_json::from_str::<Value>(c).ok())
        else {
            continue;
        };
        out.extend(payload_questions(&parsed));
        if out.len() >= MAX_QUESTIONS {
            break;
        }
    }

    out.truncate(MAX_QUESTIONS);
    out
}

/// Cap per scenario: enough signal to detect a real drop, few enough that a
/// full run stays affordable across 37 scenarios and two conditions.
const MAX_QUESTIONS: usize = 4;

/// Questions derivable from one parsed tool response.
fn payload_questions(value: &Value) -> Vec<Question> {
    let mut out = Vec::new();
    let Value::Array(array) = value else {
        // One level of nesting covers the "object holding several arrays" shape
        // without walking unboundedly deep.
        if let Value::Object(map) = value {
            for nested in map.values() {
                if nested.is_array() {
                    out.extend(payload_questions(nested));
                }
            }
        }
        return out;
    };

    // How many records there were. Truncation changes this answer, and a
    // summary that keeps a count is meaningfully better than one that does not.
    out.push(Question {
        prompt: "The tool returned a list of records. How many records were there \
                 in total? Answer with just the number."
            .to_string(),
        ground_truth: array.len().to_string(),
        kind: "record_count".to_string(),
    });

    // The first error, which is usually why the output was requested at all.
    // Detection reads the fields that declare a record to be an error rather
    // than scanning its whole serialization: an ordinary search result whose
    // prose happens to contain "error" is not an error, and asking about it
    // would test the model's reading of noise instead of the compressor.
    for entry in array {
        if !declares_error(entry) {
            continue;
        }
        if let Some(message) = error_message(entry) {
            out.push(Question {
                prompt: "Quote the error message reported in the tool output. \
                         Answer with just the message text."
                    .to_string(),
                ground_truth: message,
                kind: "error_message".to_string(),
            });
        }
        break;
    }

    out
}

/// Whether an entry declares itself to be an error.
///
/// Mirrors `retention::declares_error` so the deterministic and semantic checks
/// agree on what counts as an error record.
fn declares_error(entry: &Value) -> bool {
    match entry {
        Value::String(text) => looks_like_error(text),
        Value::Object(map) => ["level", "severity", "status", "message", "msg", "error"]
            .iter()
            .any(|key| {
                map.get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(looks_like_error)
            }),
        _ => false,
    }
}

/// Whether a field value names an error condition.
fn looks_like_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "error", "critical", "fatal", "failed", "failure", "denied", "timeout",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// The message field of an error-looking entry.
fn error_message(entry: &Value) -> Option<String> {
    if let Value::Object(map) = entry {
        for key in ["message", "msg", "error"] {
            if let Some(text) = map.get(key).and_then(Value::as_str)
                && !text.is_empty()
            {
                return Some(text.to_string());
            }
        }
        return None;
    }
    // A bare string entry is its own message.
    entry.as_str().map(str::to_string)
}

/// Client for the DashScope-compatible chat completions API.
#[derive(Debug)]
pub struct Probe {
    client: reqwest::blocking::Client,
    endpoint: String,
    api_key: String,
    model: String,
}

/// Failure modes of the probe.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// No API key, so the probe cannot run at all.
    #[error("no API key: set DASHSCOPE_API_KEY to enable the semantic probe")]
    NoApiKey,
    /// The HTTP client could not be built or the request failed.
    #[error("probe request failed: {0}")]
    Request(#[from] reqwest::Error),
    /// The response did not carry an answer where the API contract says it will.
    #[error("probe response had no answer content")]
    NoContent,
}

impl Probe {
    /// Build a probe from the environment.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError::NoApiKey`] when no key is set, which callers should
    /// record as a degradation rather than treat as a run failure.
    pub fn from_env() -> Result<Self, ProbeError> {
        let api_key = std::env::var("DASHSCOPE_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or(ProbeError::NoApiKey)?;
        let endpoint = std::env::var("L3_PROBE_ENDPOINT").unwrap_or_else(|_| {
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions".to_string()
        });
        let model = std::env::var("L3_PROBE_MODEL").unwrap_or_else(|_| "qwen3-max".to_string());
        let client = reqwest::blocking::Client::builder()
            // A large conversation plus a slow model can take a while; a short
            // timeout would turn a valid answer into a spurious loss.
            .timeout(Duration::from_secs(180))
            .build()?;
        Ok(Self {
            client,
            endpoint,
            api_key,
            model,
        })
    }

    /// Which model is answering, for the report.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Ask one question against one conversation.
    ///
    /// # Errors
    ///
    /// Propagates transport failures and a response missing its answer.
    pub fn ask(&self, messages: &[Message], question: &Question) -> Result<String, ProbeError> {
        // The conversation is handed over as context rather than replayed as
        // chat history: tool messages without their originating tool_calls are
        // rejected by strict providers, and rebuilding that structure would
        // change the very bytes under test.
        let context = messages
            .iter()
            .filter_map(|m| serde_json::to_string(&Value::Object(m.clone())).ok())
            .collect::<Vec<_>>()
            .join("\n");

        let body = serde_json::json!({
            "model": self.model,
            // Deterministic decoding: the gate is a difference between two
            // conditions, and sampling noise would show up as compression damage.
            "temperature": 0,
            "messages": [
                {
                    "role": "system",
                    "content": "Answer strictly from the provided conversation. \
                                Reply with the shortest possible answer and no \
                                explanation. If the answer is not present, reply \
                                exactly: UNKNOWN",
                },
                {
                    "role": "user",
                    "content": format!(
                        "Conversation:\n{context}\n\nQuestion: {}",
                        question.prompt
                    ),
                },
            ],
        });

        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()?
            .error_for_status()?;
        let parsed: Value = response.json()?;
        parsed
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or(ProbeError::NoContent)
    }

    /// Score one scenario by asking every question twice: original, then
    /// compressed.
    ///
    /// A transport failure on either side skips that question rather than
    /// counting it as a loss, since an unanswered question is not evidence the
    /// compressor destroyed anything.
    pub fn score(
        &self,
        questions: &[Question],
        original: &[Message],
        compressed: &[Message],
    ) -> ProbeScore {
        let mut score = ProbeScore::default();
        let mut f1_before = Vec::new();
        let mut f1_after = Vec::new();

        for question in questions {
            let Ok(before) = self.ask(original, question) else {
                continue;
            };
            let Ok(after) = self.ask(compressed, question) else {
                continue;
            };
            score.asked += 1;

            let v_before = evaluate_answer(&before, &question.ground_truth);
            let v_after = evaluate_answer(&after, &question.ground_truth);
            f1_before.push(v_before.f1);
            f1_after.push(v_after.f1);

            if v_before.contains {
                score.correct_uncompressed += 1;
                if v_after.contains {
                    score.retained += 1;
                } else {
                    score
                        .lost
                        .push(format!("{}: {}", question.kind, question.prompt));
                }
            }
        }

        let avg = |v: &[f64]| {
            if v.is_empty() {
                0.0
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        };
        score.f1_uncompressed = avg(&f1_before);
        score.f1_compressed = avg(&f1_after);
        score
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::Suite;
    use crate::l3::asset::AssetSource;

    fn scenario(messages: Vec<Message>, tools: Option<Vec<Value>>) -> Scenario {
        Scenario {
            suite: Suite::Scenario,
            scenario: "t".into(),
            display_name: None,
            content_type: "logs".into(),
            size_label: None,
            source: AssetSource {
                reference: "test".into(),
                headroom_native: true,
                headroom_revision: None,
                headroom_dirty: None,
            },
            headroom_target_ms: None,
            model_limit: 200_000,
            messages,
            tools,
        }
    }

    fn msg(role: &str, content: &str) -> Message {
        let mut m = Message::new();
        m.insert("role".into(), Value::String(role.into()));
        m.insert("content".into(), Value::String(content.into()));
        m
    }

    #[test]
    fn f1_matches_headroom_definition() {
        // Set-based, so a repeated token counts once — identical to the reference's
        // compute_f1, which a score here has to mean the same thing as.
        assert_eq!(compute_f1("disk full", "disk full"), 1.0);
        assert_eq!(compute_f1("", "disk full"), 0.0);
        assert_eq!(compute_f1("nothing here", "disk full"), 0.0);
        let partial = compute_f1("the disk is full", "disk full");
        assert!(partial > 0.0 && partial < 1.0, "got {partial}");
    }

    #[test]
    fn contains_is_the_verdict_not_exact_match() {
        // Models answer in sentences while ground truth is a short fact;
        // exact match would reject a correct answer.
        let v = evaluate_answer("The current date is 2025-01-06.", "2025-01-06");
        assert!(v.contains);
        assert!(!v.exact);
    }

    #[test]
    fn success_is_conditioned_on_the_original_being_right() {
        // A question the model cannot do at all must not be charged to the
        // compressor.
        let score = ProbeScore {
            asked: 4,
            correct_uncompressed: 0,
            ..Default::default()
        };
        assert_eq!(score.success_rate(), None);
        assert_eq!(score.drop(), None);
    }

    #[test]
    fn drop_is_measured_against_the_original() {
        let score = ProbeScore {
            asked: 4,
            correct_uncompressed: 4,
            retained: 3,
            ..Default::default()
        };
        assert_eq!(score.success_rate(), Some(0.75));
        assert_eq!(score.drop(), Some(0.25));
        assert!(score.drop().is_some_and(|d| d > MAX_DROP));
    }

    #[test]
    fn derives_a_count_and_an_error_question_from_a_log_array() {
        let logs = serde_json::json!([
            {"level": "INFO", "message": "started"},
            {"level": "ERROR", "message": "disk full on /var"},
        ]);
        let s = scenario(vec![msg("tool", &logs.to_string())], None);
        let qs = questions(&s);
        assert!(
            qs.iter()
                .any(|q| q.kind == "record_count" && q.ground_truth == "2")
        );
        assert!(
            qs.iter()
                .any(|q| q.kind == "error_message" && q.ground_truth == "disk full on /var"),
            "got {qs:?}"
        );
    }

    #[test]
    fn derives_the_date_question() {
        let s = scenario(vec![msg("system", "Current date: 2025-01-06")], None);
        let qs = questions(&s);
        assert!(qs.iter().any(|q| q.kind == "date"));
    }

    #[test]
    fn derives_a_tool_name_question() {
        let s = scenario(
            vec![msg("user", "go")],
            Some(vec![serde_json::json!({
                "type": "function",
                "function": {"name": "search_code"},
            })]),
        );
        let qs = questions(&s);
        assert!(
            qs.iter()
                .any(|q| q.kind == "tool_name" && q.ground_truth == "search_code")
        );
    }

    #[test]
    fn question_count_is_capped() {
        let big = serde_json::json!(
            (0..50)
                .map(|i| serde_json::json!({"level": "ERROR", "message": format!("e{i}")}))
                .collect::<Vec<_>>()
        );
        let s = scenario(
            vec![
                msg("system", "Current date: 2025-01-06"),
                msg("tool", &big.to_string()),
            ],
            None,
        );
        assert!(questions(&s).len() <= MAX_QUESTIONS);
    }

    #[test]
    fn prose_containing_the_word_error_is_not_an_error_record() {
        // A search result whose snippet mentions "error handling" is not an
        // error. Treating it as one asks the model about an arbitrary record
        // that any truncation drops, reporting damage where none occurred.
        let results = serde_json::json!([
            {"id": "doc_1", "title": "Async Programming",
             "snippet": "Covers error handling and performance optimization."},
        ]);
        let s = scenario(vec![msg("tool", &results.to_string())], None);
        let qs = questions(&s);
        assert!(
            !qs.iter().any(|q| q.kind == "error_message"),
            "prose mention must not become an error question: {qs:?}"
        );
    }

    #[test]
    fn prose_only_scenario_yields_no_questions() {
        // Nothing locatable, so nothing worth asking: a vague question would
        // measure the model rather than the compressor.
        let s = scenario(vec![msg("user", "some free text")], None);
        assert!(questions(&s).is_empty());
    }
}
