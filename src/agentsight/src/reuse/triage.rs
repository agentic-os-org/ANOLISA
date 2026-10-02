//! Automatic trajectory labelling from structure plus deterministic grounding.
//!
//! The point of the label is to keep the retrieval scope honest: a trajectory
//! that answered "1+1" and was never followed up carries nothing a later agent
//! can reuse, and letting it compete for a retrieval slot only crowds out
//! something that does. Everything here is a rule over facts already
//! established elsewhere — no model is consulted, so the same trajectory always
//! gets the same label.

use agentsight_atif::{AtifTrajectory, StepSource};
use serde::{Deserialize, Serialize};

use super::label::TrajectoryLabel;

/// Default answer-length ceiling, in characters, below which a tool-less
/// single-turn trajectory is treated as carrying nothing reusable.
///
/// A starting point, not a calibrated value: the corpus this was meant to be
/// fitted against no longer exists, so `scripts/calibrate-triage.py` must be
/// run against freshly collected data before the rule is relied on. Overriding
/// it is the expected case, which is why it is a config field and not a `const`
/// used directly by [`classify`].
pub const DEFAULT_MAX_AGENT_LEN: usize = 2_000;

/// Identifier of the rule set. **Bump this on any change to what a label
/// means**, including one that only moves a judgement between existing labels.
///
/// Labels are skipped when the content digest and this version both match, so a
/// behaviour change that leaves the version alone silently preserves the old
/// verdict on every already-labelled trajectory. That happened once during
/// development: narrowing which findings may accuse changed nothing until the
/// version moved with it.
///
/// History:
/// - `triage-1` initial rules.
/// - `triage-2` an unplaced claim alone no longer accuses.
/// - `triage-3` tool activity counts as content, so a tool-only transcript is
///   no longer called empty.
/// - `triage-4` no deterministic finding accuses at all; `bad` needs a model
///   review or a person. See `super::summarize`.
const TRIAGE_RULES_VERSION: &str = "triage-4";

/// Thresholds for [`classify`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageConfig {
    /// See [`DEFAULT_MAX_AGENT_LEN`].
    pub max_agent_len: usize,
}

impl Default for TriageConfig {
    fn default() -> Self {
        Self {
            max_agent_len: DEFAULT_MAX_AGENT_LEN,
        }
    }
}

impl TriageConfig {
    /// Version string recorded alongside a label.
    ///
    /// Includes the threshold, not just the rule-set name: a re-calibrated
    /// threshold changes which trajectories come out `useless`, and a label
    /// carrying a version that ignored it would look current while resting on
    /// the old cutoff.
    pub fn version(&self) -> String {
        format!("{TRIAGE_RULES_VERSION}/agent_len={}", self.max_agent_len)
    }
}

/// Structural facts a trajectory's label is derived from.
///
/// Kept on the label row so a threshold can be re-fitted later by querying the
/// database, without re-parsing every stored ATIF document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageMetrics {
    pub n_steps: usize,
    /// User-authored steps. Two or more means the topic was carried forward,
    /// which on its own disqualifies the "nothing reusable here" verdict.
    pub n_user_turns: usize,
    pub n_tool_calls: usize,
    /// Longest agent message, in characters — a proxy for how much substance
    /// the trajectory produced.
    pub max_agent_len: usize,
}

impl TriageMetrics {
    /// Measures a trajectory. Subagent trajectories are not descended into:
    /// they are labelled in their own right.
    pub fn measure(trajectory: &AtifTrajectory) -> Self {
        let mut metrics = Self {
            n_steps: trajectory.steps.len(),
            ..Self::default()
        };
        for step in &trajectory.steps {
            match step.source {
                StepSource::User => metrics.n_user_turns += 1,
                StepSource::Agent => {
                    metrics.max_agent_len = metrics.max_agent_len.max(step.message.chars().count());
                }
                StepSource::System => {}
            }
            metrics.n_tool_calls += step.tool_calls.as_deref().unwrap_or_default().len();
        }
        metrics
    }
}

/// What the deterministic grounding pass concluded, reduced to what labelling
/// needs.
///
/// Supplied by the caller because the grounding engine lives in the
/// `agentsight` crate; depending on it here would be circular. The caller is
/// expected to aggregate across every round of the trajectory — a failure in
/// round 1 still disqualifies the trajectory from being labelled sound.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundingSummary {
    /// Findings the caller has established may drive a verdict.
    ///
    /// Always zero on the deterministic path: every finding grounding produces
    /// rests on a claim string matching could not place, and telling an asserted
    /// fact from a sentence containing a slash is a semantic question. Measured
    /// on 24 real trajectories, counting them anyway produced 7 `bad` labels and
    /// all 7 were wrong.
    ///
    /// A non-zero value therefore means the caller has something stronger: the
    /// model review (`GroundingIndex::apply_review`) cleared the misparses and
    /// these findings survived it.
    pub verdict_driving_findings: usize,
    /// All findings, including ones that merely describe the round. Counted
    /// separately because a descriptive finding must not condemn a trajectory,
    /// but it does prove the trajectory was not trivial.
    pub total_findings: usize,
    /// A failed call was retried unchanged and kept failing
    /// (`Aftermath::Persisted`).
    pub has_persisted_failure: bool,
    /// Every classified call came back `Ok` or `OkProbe`. False when any call
    /// was `Failed`, `Blocked` or `Unknown` — note that `Unknown` is not
    /// success, so it must not clear this flag.
    pub all_calls_ok: bool,
    /// Rule identifiers behind the verdict (`CallVerdict::matched_rule` values
    /// such as `"R3(exit0)"`, or finding kinds), filled in by
    /// [`super::summarize::summarize_trajectory`].
    ///
    /// Carried so that a human overriding the label points at the rules that
    /// produced it. With the original calibration corpus gone, these overrides
    /// are the only signal left for ranking which rule misfires most.
    ///
    /// Only contentious judgements are listed: a plainly successful call
    /// contributes nothing, so an override of a `Good` label cannot be
    /// attributed to any rule. That case is a known blind spot rather than an
    /// oversight — `Good` rests on the *absence* of findings, and an absence has
    /// no rule to blame.
    pub rules: Vec<String>,
}

/// An automatic label with the evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageOutcome {
    pub label: TrajectoryLabel,
    /// Plain-language justification, shown in the dashboard next to the label
    /// so a user deciding whether to override it can see why it was assigned.
    pub reason: String,
    pub metrics: TriageMetrics,
    /// Copied from [`GroundingSummary::total_findings`].
    ///
    /// Stored alongside the structural metrics rather than inside them because
    /// it is grounding-derived, not measurable from the document — but it is
    /// needed to re-fit the threshold later without replaying grounding.
    pub n_findings: usize,
    /// Copied from [`GroundingSummary::rules`]; see the note there.
    pub rules: Vec<String>,
}

/// Assigns a label from structure and grounding.
///
/// Order matters. "Nothing reusable here" is decided before "something went
/// wrong", because a trivial exchange should never reach the more expensive
/// stages — including any later LLM attribution — and because a trajectory with
/// no findings and no tool calls has nothing for those stages to work on.
pub fn classify(
    metrics: &TriageMetrics,
    grounding: &GroundingSummary,
    config: &TriageConfig,
) -> TriageOutcome {
    let outcome = |label: TrajectoryLabel, reason: String| TriageOutcome {
        label,
        reason,
        metrics: *metrics,
        n_findings: grounding.total_findings,
        rules: grounding.rules.clone(),
    };

    // A trajectory that recorded nothing at all. Prose alone is not the test:
    // measured on real Qoder captures, `transcript/*.jsonl` files log only the
    // tool loop — 84 `tool_use` blocks and 84 `tool_result` blocks with no human
    // or assistant text anywhere — so keying on the answer length alone called a
    // 71-step, 84-call trajectory empty. Tool activity is work, and a long chain
    // of real calls and results is the most reusable shape there is.
    if metrics.n_steps <= 1 || (metrics.max_agent_len == 0 && metrics.n_tool_calls == 0) {
        return outcome(
            TrajectoryLabel::Useless,
            "轨迹既无 Agent 回答也无工具调用，没有可复用内容".to_string(),
        );
    }

    if metrics.n_tool_calls == 0
        && metrics.n_user_turns <= 1
        && metrics.max_agent_len < config.max_agent_len
        && grounding.total_findings == 0
    {
        return outcome(
            TrajectoryLabel::Useless,
            format!(
                "一问一答且没有后续追问：没有工具调用，回答 {} 字符（阈值 {}）",
                metrics.max_agent_len, config.max_agent_len
            ),
        );
    }

    if grounding.verdict_driving_findings > 0 {
        return outcome(
            TrajectoryLabel::Bad,
            format!(
                "存在 {} 条可复核的问题证据（失败后编造或同参重复失败）",
                grounding.verdict_driving_findings
            ),
        );
    }

    if grounding.total_findings == 0 && grounding.all_calls_ok && !grounding.has_persisted_failure {
        return outcome(
            TrajectoryLabel::Good,
            "没有发现问题，且所有工具调用都成功或属预期探测".to_string(),
        );
    }

    // Reached when something is off but not provably a defect: an unresolved
    // claim, a call that could not be classified, or a failure whose aftermath
    // the rules cannot judge. Saying "good" here would overstate what was
    // checked, and "bad" would accuse without evidence.
    outcome(
        TrajectoryLabel::Unknown,
        format!(
            "证据不足以判定好坏：{} 条描述性发现，工具调用{}",
            grounding.total_findings,
            if grounding.all_calls_ok {
                "均正常"
            } else {
                "存在失败、被拦截或无法判定的情况"
            }
        ),
    )
}

/// Measures `trajectory` and classifies it in one step.
pub fn triage(
    trajectory: &AtifTrajectory,
    grounding: &GroundingSummary,
    config: &TriageConfig,
) -> TriageOutcome {
    classify(&TriageMetrics::measure(trajectory), grounding, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Grounding output for a trajectory the rules found nothing wrong with.
    fn clean() -> GroundingSummary {
        GroundingSummary {
            all_calls_ok: true,
            ..GroundingSummary::default()
        }
    }

    fn metrics(
        n_steps: usize,
        n_user_turns: usize,
        n_tool_calls: usize,
        agent_len: usize,
    ) -> TriageMetrics {
        TriageMetrics {
            n_steps,
            n_user_turns,
            n_tool_calls,
            max_agent_len: agent_len,
        }
    }

    #[test]
    fn trivial_single_turn_exchange_is_useless() {
        let out = classify(&metrics(2, 1, 0, 1), &clean(), &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Useless);
    }

    #[test]
    fn a_follow_up_user_turn_rescues_a_trivial_exchange() {
        // The whole point of the user-turn clause: the same tiny answer is not
        // useless once the user carried the topic forward.
        let out = classify(&metrics(4, 2, 0, 1), &clean(), &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Good);
    }

    #[test]
    fn tool_use_alone_disqualifies_useless() {
        let out = classify(&metrics(2, 1, 1, 1), &clean(), &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Good);
    }

    #[test]
    fn a_long_answer_disqualifies_useless() {
        let out = classify(&metrics(2, 1, 0, 5_000), &clean(), &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Good);
    }

    #[test]
    fn a_finding_disqualifies_useless_even_on_a_trivial_shape() {
        let grounding = GroundingSummary {
            total_findings: 1,
            all_calls_ok: true,
            ..GroundingSummary::default()
        };
        let out = classify(&metrics(2, 1, 0, 1), &grounding, &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Unknown);
    }

    #[test]
    fn an_agent_that_never_answered_is_useless() {
        let out = classify(&metrics(1, 1, 0, 0), &clean(), &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Useless);
        // Nothing recorded at all: no prose and no tool activity.
        let out = classify(&metrics(9, 3, 0, 0), &clean(), &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Useless);
    }

    #[test]
    fn a_tool_only_transcript_is_not_empty() {
        // Real Qoder `transcript/*.jsonl` captures log only the tool loop, with
        // no human or assistant text at all. Judging those on answer length
        // called 13 of 24 collected trajectories useless, including one with 71
        // steps and 84 tool calls.
        let out = classify(&metrics(71, 0, 84, 0), &clean(), &TriageConfig::default());
        assert_ne!(out.label, TrajectoryLabel::Useless);
    }

    #[test]
    fn verdict_driving_findings_make_it_bad() {
        let grounding = GroundingSummary {
            verdict_driving_findings: 1,
            total_findings: 1,
            ..GroundingSummary::default()
        };
        let out = classify(&metrics(6, 1, 2, 400), &grounding, &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Bad);
    }

    #[test]
    fn an_unclassifiable_call_yields_unknown_not_good() {
        // `all_calls_ok == false` covers Failed, Blocked and Unknown alike:
        // an unclassifiable call is not evidence of soundness.
        let grounding = GroundingSummary {
            all_calls_ok: false,
            ..GroundingSummary::default()
        };
        let out = classify(&metrics(6, 1, 2, 400), &grounding, &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Unknown);
    }

    #[test]
    fn a_persisted_failure_blocks_good_without_accusing() {
        let grounding = GroundingSummary {
            all_calls_ok: true,
            has_persisted_failure: true,
            ..GroundingSummary::default()
        };
        let out = classify(&metrics(6, 1, 3, 400), &grounding, &TriageConfig::default());
        assert_eq!(out.label, TrajectoryLabel::Unknown);
    }

    #[test]
    fn rules_are_carried_into_the_outcome() {
        let grounding = GroundingSummary {
            verdict_driving_findings: 1,
            total_findings: 1,
            rules: vec![
                "failure_then_fabrication".to_string(),
                "R3(exit0)".to_string(),
            ],
            ..GroundingSummary::default()
        };
        let out = classify(&metrics(6, 1, 2, 400), &grounding, &TriageConfig::default());
        assert_eq!(out.rules.len(), 2);
    }

    #[test]
    fn classification_is_deterministic() {
        let m = metrics(6, 1, 2, 400);
        let g = clean();
        let cfg = TriageConfig::default();
        assert_eq!(classify(&m, &g, &cfg), classify(&m, &g, &cfg));
    }

    #[test]
    fn the_version_tracks_the_threshold_so_recalibration_forces_a_recompute() {
        let a = TriageConfig::default();
        let b = TriageConfig { max_agent_len: 900 };
        assert_ne!(a.version(), b.version());
        assert!(a.version().starts_with("triage-4/"));
    }

    #[test]
    fn measure_counts_turns_tools_and_longest_answer() {
        let json = r#"{
            "schema_version": "ATIF-v1.7",
            "agent": {"name": "test"},
            "steps": [
                {"step_id": 1, "source": "user", "message": "查一下"},
                {"step_id": 2, "source": "agent", "message": "好",
                 "tool_calls": [{"tool_call_id": "c1", "function_name": "bash", "arguments": "{}"}]},
                {"step_id": 3, "source": "agent", "message": "结果是这样的"}
            ]
        }"#;
        let trajectory: AtifTrajectory = serde_json::from_str(json).unwrap();
        let m = TriageMetrics::measure(&trajectory);
        assert_eq!(m.n_steps, 3);
        assert_eq!(m.n_user_turns, 1);
        assert_eq!(m.n_tool_calls, 1);
        // Longest agent message, not the last one.
        assert_eq!(m.max_agent_len, "结果是这样的".chars().count());
    }

    #[test]
    fn measure_counts_characters_not_bytes() {
        // A byte count would overstate CJK answers threefold and let them slip
        // past the useless threshold.
        let json = r#"{
            "schema_version": "ATIF-v1.7",
            "agent": {"name": "test"},
            "steps": [
                {"step_id": 1, "source": "user", "message": "问"},
                {"step_id": 2, "source": "agent", "message": "答答答"}
            ]
        }"#;
        let trajectory: AtifTrajectory = serde_json::from_str(json).unwrap();
        assert_eq!(TriageMetrics::measure(&trajectory).max_agent_len, 3);
    }
}
