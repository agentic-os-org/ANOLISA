//! Dead-loop detection for agents stuck in logical loops.
//!
//! Unlike `detector.rs` which inspects a single LLMCall for errors,
//! `LoopDetector` performs **cross-call** analysis within a conversation
//! to identify repetitive patterns that indicate an agent is stuck.
//!
//! # Detection Rules (by priority)
//!
//! 1. **Tool Sequence Repetition** — same tool calls (name + argument
//!    fingerprint) emitted N times consecutively
//! 2. **Output Similarity Loop** — similar LLM output text repeated N times
//! 3. **Token Burn Without Progress** — input tokens growing but output stays the same

use std::collections::HashSet;

use super::types::{InterruptionEvent, InterruptionType};

/// Configuration for the loop detector.
#[derive(Debug, Clone)]
pub struct LoopDetectorConfig {
    /// Number of consecutive calls with the same tool sequence to trigger (default: 5)
    pub tool_sequence_repeat_threshold: usize,
    /// Sliding window of recent calls to inspect (default: 10)
    pub window_size: usize,
    /// Jaccard similarity threshold for output text (0.0~1.0, default: 0.85)
    pub output_similarity_threshold: f64,
    /// Number of consecutive similar outputs to trigger (default: 3)
    pub similar_output_repeat_threshold: usize,
}

impl Default for LoopDetectorConfig {
    fn default() -> Self {
        Self {
            tool_sequence_repeat_threshold: 5,
            window_size: 10,
            output_similarity_threshold: 0.85,
            similar_output_repeat_threshold: 3,
        }
    }
}

/// Identity of a single tool call for loop comparison.
///
/// Two calls count as a repetition only when both the tool name and the
/// argument fingerprint match, so the same tool invoked with different
/// arguments (e.g. a terminal tool running different commands) is not a loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallKey {
    pub name: String,
    /// Stable hash of the serialized JSON arguments; `None` when the provider
    /// omitted arguments. `None == None`, which keeps the legacy name-only
    /// comparison for providers that never emit arguments.
    pub args_fingerprint: Option<u64>,
}

impl ToolCallKey {
    /// Hash the serialized arguments into a stable per-process fingerprint.
    ///
    /// serde_json preserves key order (`preserve_order`), so repeated calls
    /// serialized by the same agent yield identical strings. Order-differing
    /// but semantically equal arguments hash differently, which only makes
    /// detection more conservative (never a new false positive).
    pub fn fingerprint_args(args: &serde_json::Value) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        args.to_string().hash(&mut hasher);
        hasher.finish()
    }
}

/// Lightweight summary of a recent LLM call used for loop detection.
#[derive(Debug, Clone)]
pub struct RecentCallSummary {
    pub call_id: String,
    /// Tool calls invoked by this call's output, in emission order
    pub tool_calls: Vec<ToolCallKey>,
    /// Snippet of output text (first ~200 chars) for similarity calculation
    pub output_text_snippet: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// Cross-call loop detector.
pub struct LoopDetector {
    pub config: LoopDetectorConfig,
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new(LoopDetectorConfig::default())
    }
}

impl LoopDetector {
    pub fn new(config: LoopDetectorConfig) -> Self {
        LoopDetector { config }
    }

    /// Detect a dead loop from recent calls in the same conversation.
    ///
    /// `recent_calls` should be ordered oldest-first (ascending by timestamp).
    /// The current call being processed should already be included as the last element.
    ///
    /// Returns `Some(InterruptionEvent)` if a loop is detected.
    pub fn detect(
        &self,
        conversation_id: &str,
        session_id: Option<&str>,
        agent_name: Option<&str>,
        pid: Option<i32>,
        occurred_at_ns: i64,
        recent_calls: &[RecentCallSummary],
    ) -> Option<InterruptionEvent> {
        let min_threshold = self
            .config
            .tool_sequence_repeat_threshold
            .min(self.config.similar_output_repeat_threshold);
        if recent_calls.len() < min_threshold {
            return None;
        }

        // Rule 1: Tool Sequence Repetition
        if let Some(detail) = self.detect_tool_sequence_loop(recent_calls) {
            return Some(self.build_event(
                conversation_id,
                session_id,
                agent_name,
                pid,
                occurred_at_ns,
                "tool_sequence_repetition",
                detail,
            ));
        }

        // Rule 2: Output Similarity Loop
        if let Some(detail) = self.detect_output_similarity_loop(recent_calls) {
            return Some(self.build_event(
                conversation_id,
                session_id,
                agent_name,
                pid,
                occurred_at_ns,
                "output_similarity_loop",
                detail,
            ));
        }

        // Rule 3: Token Burn Without Progress
        if let Some(detail) = self.detect_token_burn(recent_calls) {
            return Some(self.build_event(
                conversation_id,
                session_id,
                agent_name,
                pid,
                occurred_at_ns,
                "token_burn_no_progress",
                detail,
            ));
        }

        None
    }

    /// Rule 1: Check if the last N tool-bearing calls repeat the same tool sequence
    /// with identical arguments (see [`ToolCallKey`]).
    ///
    /// Only considers calls that actually have tool_call outputs (ignores pure-text
    /// responses). This handles architectures like OpenClaw where each tool call
    /// is followed by a text summary call.
    fn detect_tool_sequence_loop(&self, calls: &[RecentCallSummary]) -> Option<serde_json::Value> {
        let threshold = self.config.tool_sequence_repeat_threshold;

        // Filter to only calls that have tool calls (ignore pure-text responses)
        let tool_bearing: Vec<&RecentCallSummary> =
            calls.iter().filter(|c| !c.tool_calls.is_empty()).collect();

        if tool_bearing.len() < threshold {
            return None;
        }

        // Look at the last N tool-bearing calls
        let tail = &tool_bearing[tool_bearing.len().saturating_sub(threshold)..];

        // Check if all tool call sequences in the tail are identical
        let reference = &tail[0].tool_calls;
        let all_same = tail[1..].iter().all(|c| &c.tool_calls == reference);

        if all_same {
            let repeated_tools: Vec<&str> = reference.iter().map(|k| k.name.as_str()).collect();
            Some(serde_json::json!({
                "repeated_tools": repeated_tools,
                "repeat_count": threshold,
            }))
        } else {
            None
        }
    }

    /// Rule 2: Check if the last N text-bearing calls have highly similar output text.
    ///
    /// Only considers calls that have text output (ignores pure tool_call responses).
    /// This handles architectures where tool calls and text responses alternate.
    fn detect_output_similarity_loop(
        &self,
        calls: &[RecentCallSummary],
    ) -> Option<serde_json::Value> {
        let threshold = self.config.similar_output_repeat_threshold;

        // Filter to only calls that have text output
        let text_bearing: Vec<&RecentCallSummary> = calls
            .iter()
            .filter(|c| !c.output_text_snippet.is_empty())
            .collect();

        if text_bearing.len() < threshold {
            return None;
        }

        // Look at the last N text-bearing calls
        let tail = &text_bearing[text_bearing.len().saturating_sub(threshold)..];

        // Compare each pair against the first one
        let reference = &tail[0].output_text_snippet;
        let all_similar = tail[1..].iter().all(|c| {
            jaccard_similarity(reference, &c.output_text_snippet)
                >= self.config.output_similarity_threshold
        });

        if all_similar {
            let similarity = if tail.len() > 1 {
                jaccard_similarity(reference, &tail[tail.len() - 1].output_text_snippet)
            } else {
                1.0
            };
            Some(serde_json::json!({
                "similarity": format!("{:.2}", similarity),
                "repeat_count": threshold,
                "output_snippet": truncate_str(reference, 100),
            }))
        } else {
            None
        }
    }

    /// Rule 3: Check if input tokens are monotonically increasing while output stays similar.
    ///
    /// Only considers calls that have text output (ignores pure tool_call responses).
    /// This handles architectures like OpenClaw where tool calls and text responses
    /// alternate — we check the text responses for repetitive content with growing context.
    fn detect_token_burn(&self, calls: &[RecentCallSummary]) -> Option<serde_json::Value> {
        let threshold = self.config.similar_output_repeat_threshold;

        // Filter to only calls that have text output (ignore pure tool_call responses)
        let text_bearing: Vec<&RecentCallSummary> = calls
            .iter()
            .filter(|c| !c.output_text_snippet.is_empty())
            .collect();

        if text_bearing.len() < threshold {
            return None;
        }

        // Look at the last N text-bearing calls
        let tail = &text_bearing[text_bearing.len().saturating_sub(threshold)..];

        // Check: input_tokens strictly increasing
        let input_increasing = tail
            .windows(2)
            .all(|w| w[1].input_tokens > w[0].input_tokens);
        if !input_increasing {
            return None;
        }

        // Check: output snippets are all similar
        let reference = &tail[0].output_text_snippet;
        let output_similar = tail[1..].iter().all(|c| {
            jaccard_similarity(reference, &c.output_text_snippet)
                >= self.config.output_similarity_threshold
        });

        if output_similar {
            let first_input = tail[0].input_tokens;
            let last_input = tail[tail.len() - 1].input_tokens;
            Some(serde_json::json!({
                "input_tokens_start": first_input,
                "input_tokens_end": last_input,
                "input_growth": last_input - first_input,
                "repeat_count": threshold,
            }))
        } else {
            None
        }
    }

    fn build_event(
        &self,
        conversation_id: &str,
        session_id: Option<&str>,
        agent_name: Option<&str>,
        pid: Option<i32>,
        occurred_at_ns: i64,
        rule: &str,
        detail_extra: serde_json::Value,
    ) -> InterruptionEvent {
        let mut detail = detail_extra;
        if let Some(obj) = detail.as_object_mut() {
            obj.insert(
                "rule".to_string(),
                serde_json::Value::String(rule.to_string()),
            );
        }
        InterruptionEvent::new(
            InterruptionType::DeadLoop,
            session_id.map(|s| s.to_string()),
            None, // trace_id — not meaningful for cross-call detection
            Some(conversation_id.to_string()),
            None, // call_id — not a single call
            pid,
            agent_name.map(|s| s.to_string()),
            occurred_at_ns,
            Some(detail),
        )
    }
}

// ─── Utility functions ───────────────────────────────────────────────────────

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}')
}

fn tokenize(text: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    for word in text.split_whitespace() {
        let cjk_chars: Vec<char> = word.chars().filter(|c| is_cjk(*c)).collect();
        if cjk_chars.len() >= 2 {
            for pair in cjk_chars.windows(2) {
                tokens.insert(format!("{}{}", pair[0], pair[1]));
            }
        } else if cjk_chars.len() == 1 {
            tokens.insert(cjk_chars[0].to_string());
        } else {
            tokens.insert(word.to_string());
        }
    }
    tokens
}

/// Compute Jaccard similarity between two text strings.
///
/// Uses whitespace-split tokens for ASCII/Latin text and character bigrams
/// for CJK text, so deadloop detection works for both English and Chinese.
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let set_a = tokenize(a);
    let set_b = tokenize(b);

    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();

    if union == 0 {
        return 0.0;
    }

    intersection as f64 / union as f64
}

/// Truncate a string to at most `max_len` characters, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut result: String = s.chars().take(max_len).collect();
        result.push_str("...");
        result
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a call whose tool calls carry no arguments (fingerprint `None`).
    fn make_call(tool_names: Vec<&str>, output: &str, input_tokens: i64) -> RecentCallSummary {
        RecentCallSummary {
            call_id: format!("call-{input_tokens}"),
            tool_calls: tool_names
                .into_iter()
                .map(|s| ToolCallKey {
                    name: s.to_string(),
                    args_fingerprint: None,
                })
                .collect(),
            output_text_snippet: output.to_string(),
            input_tokens,
            output_tokens: 100,
        }
    }

    /// Build a call whose tool calls carry JSON arguments.
    fn make_call_with_args(
        tools: Vec<(&str, serde_json::Value)>,
        output: &str,
        input_tokens: i64,
    ) -> RecentCallSummary {
        RecentCallSummary {
            call_id: format!("call-{input_tokens}"),
            tool_calls: tools
                .into_iter()
                .map(|(name, args)| ToolCallKey {
                    name: name.to_string(),
                    args_fingerprint: Some(ToolCallKey::fingerprint_args(&args)),
                })
                .collect(),
            output_text_snippet: output.to_string(),
            input_tokens,
            output_tokens: 100,
        }
    }

    #[test]
    fn test_no_loop_insufficient_calls() {
        let detector = LoopDetector::default();
        let calls = vec![
            make_call(vec!["read_file"], "some output", 100),
            make_call(vec!["read_file"], "some output", 200),
        ];
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        assert!(result.is_none());
    }

    #[test]
    fn test_tool_sequence_loop_detected() {
        let detector = LoopDetector::default();
        let calls = vec![
            make_call(vec!["read_file", "search"], "output a", 100),
            make_call(vec!["read_file", "search"], "output b", 200),
            make_call(vec!["read_file", "search"], "output c", 300),
            make_call(vec!["read_file", "search"], "output d", 400),
            make_call(vec!["read_file", "search"], "output e", 500),
        ];
        let result = detector.detect(
            "conv-1",
            Some("sess-1"),
            Some("agent"),
            Some(123),
            1000,
            &calls,
        );
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.interruption_type, InterruptionType::DeadLoop);
        assert_eq!(event.severity, super::super::types::Severity::Critical);
        assert_eq!(event.conversation_id, Some("conv-1".to_string()));
        let detail: serde_json::Value =
            serde_json::from_str(event.detail.as_ref().unwrap()).unwrap();
        assert_eq!(detail["rule"], "tool_sequence_repetition");
    }

    #[test]
    fn test_tool_sequence_no_loop_different_tools() {
        let detector = LoopDetector::default();
        let calls = vec![
            make_call(vec!["read_file", "search"], "output a", 100),
            make_call(vec!["write_file"], "output b", 200),
            make_call(vec!["read_file", "search"], "output c", 300),
        ];
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        // Rule 1 won't trigger (sequences differ), Rule 2/3 also won't trigger
        assert!(result.is_none());
    }

    #[test]
    fn test_same_tool_different_args_not_a_loop() {
        // Regression for #2691: five consecutive `terminal` calls each running a
        // different command are normal multi-step work, not a dead loop.
        let detector = LoopDetector::default();
        let commands = [
            "pip list",
            "find / -name detect.sh",
            "cat detect.sh",
            "bash detect.sh",
            "bash install.sh",
        ];
        let calls: Vec<RecentCallSummary> = commands
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                make_call_with_args(
                    vec![("terminal", serde_json::json!({ "command": cmd }))],
                    &format!("step {i} done"),
                    (i as i64 + 1) * 100,
                )
            })
            .collect();
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        assert!(
            result.is_none(),
            "different arguments must not count as repetition"
        );
    }

    #[test]
    fn test_same_tool_same_args_loop_detected() {
        // Identical tool name AND identical arguments repeated is a genuine loop.
        let detector = LoopDetector::default();
        let args = serde_json::json!({ "command": "cat /tmp/missing.txt" });
        let calls: Vec<RecentCallSummary> = (1..=5)
            .map(|i| {
                make_call_with_args(
                    vec![("terminal", args.clone())],
                    &format!("attempt {i}"),
                    i * 100,
                )
            })
            .collect();
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        assert!(result.is_some());
        let event = result.unwrap();
        let detail: serde_json::Value =
            serde_json::from_str(event.detail.as_ref().unwrap()).unwrap();
        assert_eq!(detail["rule"], "tool_sequence_repetition");
        assert_eq!(detail["repeated_tools"], serde_json::json!(["terminal"]));
    }

    #[test]
    fn test_args_presence_mismatch_not_a_loop() {
        // A call with arguments and one without must not compare equal even for
        // the same tool name.
        let detector = LoopDetector::default();
        let args = serde_json::json!({ "command": "ls" });
        let calls: Vec<RecentCallSummary> = (1..=5)
            .map(|i| {
                if i % 2 == 0 {
                    make_call_with_args(
                        vec![("terminal", args.clone())],
                        &format!("out {i}"),
                        i * 100,
                    )
                } else {
                    make_call(vec!["terminal"], &format!("out {i}"), i * 100)
                }
            })
            .collect();
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        assert!(result.is_none());
    }

    #[test]
    fn test_fingerprint_args_stability() {
        let a = serde_json::json!({ "command": "ls", "cwd": "/tmp" });
        let b = serde_json::json!({ "command": "ls", "cwd": "/tmp" });
        let c = serde_json::json!({ "command": "ls", "cwd": "/home" });
        assert_eq!(
            ToolCallKey::fingerprint_args(&a),
            ToolCallKey::fingerprint_args(&b)
        );
        assert_ne!(
            ToolCallKey::fingerprint_args(&a),
            ToolCallKey::fingerprint_args(&c)
        );
    }

    #[test]
    fn test_tool_sequence_loop_with_interleaved_text_calls() {
        // Simulates OpenClaw architecture: tool_call → text → tool_call → text → ...
        let detector = LoopDetector::default();
        let calls = vec![
            make_call(vec!["read_file"], "reading file...", 100),
            make_call(vec![], "Here is the content of the file.", 200),
            make_call(vec!["read_file"], "reading file...", 300),
            make_call(vec![], "Here is the content again.", 400),
            make_call(vec!["read_file"], "reading file...", 500),
            make_call(vec![], "Here is the content yet again.", 600),
            make_call(vec!["read_file"], "reading file...", 700),
            make_call(vec![], "Here is the content once more.", 800),
            make_call(vec!["read_file"], "reading file...", 900),
            make_call(vec![], "Here is the content finally.", 1000),
        ];
        let result = detector.detect(
            "conv-1",
            Some("sess-1"),
            Some("agent"),
            Some(123),
            1000,
            &calls,
        );
        // Rule 1 should trigger: 5 tool-bearing calls all have ["read_file"]
        assert!(result.is_some());
        let event = result.unwrap();
        let detail: serde_json::Value =
            serde_json::from_str(event.detail.as_ref().unwrap()).unwrap();
        assert_eq!(detail["rule"], "tool_sequence_repetition");
        assert_eq!(detail["repeated_tools"], serde_json::json!(["read_file"]));
    }

    #[test]
    fn test_output_similarity_loop_detected() {
        let detector = LoopDetector::default();
        let calls = vec![
            make_call(
                vec![],
                "The quick brown fox jumps over the lazy dog repeatedly",
                100,
            ),
            make_call(
                vec![],
                "The quick brown fox jumps over the lazy dog repeatedly",
                200,
            ),
            make_call(
                vec![],
                "The quick brown fox jumps over the lazy dog repeatedly",
                300,
            ),
        ];
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        assert!(result.is_some());
        let event = result.unwrap();
        let detail: serde_json::Value =
            serde_json::from_str(event.detail.as_ref().unwrap()).unwrap();
        assert_eq!(detail["rule"], "output_similarity_loop");
    }

    #[test]
    fn test_output_similarity_no_loop_different_outputs() {
        let detector = LoopDetector::default();
        let calls = vec![
            make_call(vec![], "completely different output alpha", 100),
            make_call(vec![], "totally unrelated text beta gamma", 200),
            make_call(vec![], "yet another unique response delta", 300),
        ];
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        assert!(result.is_none());
    }

    #[test]
    fn test_token_burn_detected() {
        let detector = LoopDetector::new(LoopDetectorConfig {
            tool_sequence_repeat_threshold: 5, // raise so rule 1 doesn't fire
            ..Default::default()
        });
        let output = "I will try to help you with this task using the available tools";
        let calls = vec![
            make_call(vec![], output, 1000),
            make_call(vec![], output, 2000),
            make_call(vec![], output, 3000),
        ];
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        assert!(result.is_some());
        let event = result.unwrap();
        let detail: serde_json::Value =
            serde_json::from_str(event.detail.as_ref().unwrap()).unwrap();
        // Rule 2 fires first (output similarity) since tool_sequence threshold is raised
        assert!(
            detail["rule"] == "output_similarity_loop"
                || detail["rule"] == "token_burn_no_progress"
        );
    }

    #[test]
    fn test_token_burn_no_trigger_decreasing_tokens() {
        let detector = LoopDetector::new(LoopDetectorConfig {
            tool_sequence_repeat_threshold: 5,
            similar_output_repeat_threshold: 5, // raise so rule 2 doesn't fire
            ..Default::default()
        });
        let output = "same output repeated here";
        let calls = vec![
            make_call(vec![], output, 3000),
            make_call(vec![], output, 2000), // decreasing
            make_call(vec![], output, 1000), // decreasing
        ];
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        // Neither rule fires: thresholds raised, tokens not increasing
        assert!(result.is_none());
    }

    #[test]
    fn test_token_burn_with_interleaved_tool_calls() {
        // Simulates OpenClaw: tool_call → text → tool_call → text → tool_call → text
        // Rule 3 should filter to text-bearing calls and detect token burn
        let detector = LoopDetector::new(LoopDetectorConfig {
            tool_sequence_repeat_threshold: 10, // raise so rule 1 doesn't fire
            similar_output_repeat_threshold: 3,
            ..Default::default()
        });
        let similar_text = "The file does not exist, I will try a different approach to find it";
        let calls = vec![
            make_call(vec!["read_file"], "", 18000), // tool_call, no text
            make_call(vec![], similar_text, 18100),  // text response
            make_call(vec!["read_file"], "", 18200), // tool_call, no text
            make_call(vec![], similar_text, 18300),  // text response
            make_call(vec!["read_file"], "", 18400), // tool_call, no text
            make_call(vec![], similar_text, 18500),  // text response
        ];
        let result = detector.detect(
            "conv-1",
            Some("sess-1"),
            Some("agent"),
            Some(123),
            1000,
            &calls,
        );
        assert!(result.is_some());
        let event = result.unwrap();
        let detail: serde_json::Value =
            serde_json::from_str(event.detail.as_ref().unwrap()).unwrap();
        // Should detect either output_similarity_loop (Rule 2) or token_burn (Rule 3)
        // Rule 2 also filters text-bearing calls, so it may fire first
        assert!(
            detail["rule"] == "output_similarity_loop"
                || detail["rule"] == "token_burn_no_progress"
        );
    }

    #[test]
    fn test_token_burn_only_triggers_on_text_bearing() {
        // All calls have tool_calls but NO text output -> Rule 3 should NOT fire
        let detector = LoopDetector::new(LoopDetectorConfig {
            tool_sequence_repeat_threshold: 10, // raise so rule 1 doesn't fire
            similar_output_repeat_threshold: 3,
            ..Default::default()
        });
        let calls = vec![
            make_call(vec!["read_file"], "", 18000),
            make_call(vec!["read_file"], "", 18200),
            make_call(vec!["read_file"], "", 18400),
        ];
        let result = detector.detect("conv-1", None, None, None, 1000, &calls);
        // No text output → Rule 3 can't trigger, Rule 1 threshold raised → nothing fires
        assert!(result.is_none());
    }

    #[test]
    fn test_jaccard_similarity_identical() {
        assert_eq!(jaccard_similarity("hello world", "hello world"), 1.0);
    }

    #[test]
    fn test_jaccard_similarity_disjoint() {
        assert_eq!(jaccard_similarity("hello world", "foo bar"), 0.0);
    }

    #[test]
    fn test_jaccard_similarity_partial() {
        let sim = jaccard_similarity("the quick brown fox", "the quick red fox");
        // intersection: {the, quick, fox} = 3, union: {the, quick, brown, red, fox} = 5
        assert!((sim - 0.6).abs() < 0.01);
    }

    #[test]
    fn test_jaccard_similarity_empty() {
        assert_eq!(jaccard_similarity("", ""), 1.0);
    }

    #[test]
    fn test_jaccard_cjk_near_identical() {
        let a =
            "根据分析，该数据集包含1000条记录，其中异常值占比约3.2%，建议进一步清洗后重新统计。";
        let b =
            "根据分析，该数据集包含1000条记录，其中异常值占比约3.5%，建议进一步清洗后重新统计。";
        let sim = jaccard_similarity(a, b);
        assert!(
            sim > 0.8,
            "CJK near-identical text should score >0.8, got {sim:.4}"
        );
    }

    #[test]
    fn test_jaccard_cjk_different() {
        let a = "今天天气非常好适合出门散步";
        let b = "量子计算机可以解决复杂问题";
        let sim = jaccard_similarity(a, b);
        assert!(
            sim < 0.2,
            "Different CJK text should score <0.2, got {sim:.4}"
        );
    }

    #[test]
    fn test_jaccard_english_still_works() {
        let sim = jaccard_similarity(
            "analyze the key statistics of this dataset",
            "analyze the main statistics of this dataset",
        );
        assert!(
            sim > 0.7,
            "English similarity should still work, got {sim:.4}"
        );
    }
}
