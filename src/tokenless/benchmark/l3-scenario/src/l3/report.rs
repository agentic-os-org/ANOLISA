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

//! Report structures and rendering.
//!
//! Two artifacts per run: `report.json` for machine consumption and a Markdown
//! summary for review. Capability gaps are rendered directly after the summary,
//! before any comparison table — what each side *cannot* do is the primary
//! output of this layer, since it feeds the decision about which compressor to
//! add next, and burying it under a rate table would invert that.

use serde::Serialize;

use super::headroom_side::HeadroomProvenance;
use super::stats::{GroupStats, Latency};

/// One scenario measured on both sides.
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioRow {
    /// Suite the scenario came from.
    pub suite: String,
    /// The reference's content-type grouping.
    pub content_type: String,
    /// Scenario identifier.
    pub scenario: String,
    /// the reference's own size label.
    pub size_label: Option<String>,
    /// False for assets this repo adds on top of the reference's fixtures.
    pub headroom_native: bool,
    /// Authoritative token count before compression.
    pub tokens_before: usize,
    /// Latency target the reference sets for this scenario, when it sets one.
    pub headroom_target_ms: Option<f64>,

    /// tokenless compression rate, or `None` where it has no entry point.
    pub tokenless_rate: Option<f64>,
    /// Which tokenless entry points matched.
    pub tokenless_entry_points: Vec<String>,
    /// Why tokenless could not act, when it could not.
    pub tokenless_gap_reason: Option<String>,
    /// tokenless retention as passed/total.
    pub tokenless_retention: Option<(usize, usize)>,
    /// Critical items tokenless dropped.
    pub tokenless_missing: Vec<String>,
    /// tokenless compression time, in-process.
    pub tokenless_ms: f64,

    /// The reference rate under its own benchmark's pipeline assembly.
    pub headroom_pure_rate: Option<f64>,
    /// The reference rate with ContentRouter, its recommended entry point.
    pub headroom_router_rate: Option<f64>,
    /// The reference retention under the router variant.
    pub headroom_retention: Option<(usize, usize)>,
    /// Critical items the reference dropped.
    pub headroom_missing: Vec<String>,
    /// The reference compression time under the router variant, worker-internal.
    pub headroom_ms: Option<f64>,
    /// Distinct strategies the reference applied, e.g. `lossless`, `object`.
    ///
    /// The most direct evidence of *how* it compressed: a lossless re-encoding
    /// and an item-dropping truncation can land on similar rates while differing
    /// entirely in what survives.
    pub headroom_strategies: Vec<String>,
    /// The reference's own token self-report, for corroboration only.
    pub headroom_self_tokens: Option<(i64, i64)>,
    /// Why the reference produced no result, when it produced none.
    pub headroom_error: Option<String>,

    /// Semantic probe on the tokenless output, when the probe ran.
    pub tokenless_probe: Option<super::probe::ProbeScore>,
    /// Semantic probe on the reference router output, when the probe ran.
    pub headroom_probe: Option<super::probe::ProbeScore>,
}

/// Aggregate of one content-type group.
#[derive(Debug, Clone, Serialize)]
pub struct GroupRow {
    /// The reference's content type.
    pub content_type: String,
    /// Scenarios in the group.
    pub scenarios: usize,
    /// Scenarios where tokenless has an entry point.
    pub tokenless_applicable: usize,
    /// Compression across scenarios, tokenless side.
    pub tokenless: Option<GroupStats>,
    /// Compression across scenarios, the reference router variant.
    pub headroom_router: Option<GroupStats>,
    /// Paired difference where both sides acted.
    pub paired_gap: Option<GroupStats>,
    /// Retention totals, tokenless side.
    pub tokenless_retention: (usize, usize),
    /// Retention totals, the reference side.
    pub headroom_retention: (usize, usize),
}

/// A gate signal worth a reviewer's attention.
#[derive(Debug, Clone, Serialize)]
pub struct GateSignal {
    /// Scenario or group the signal is about.
    pub subject: String,
    /// Which side, when the signal is one-sided.
    pub side: Option<String>,
    /// Signal category.
    pub kind: String,
    /// Human-readable detail.
    pub detail: String,
}

/// Everything about the run itself.
#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    /// When the run finished, RFC 3339.
    pub date: String,
    /// Platform the run happened on.
    pub platform: String,
    /// anolisa revision under test.
    pub git_sha: Option<String>,
    /// Whether anolisa had uncommitted tracked changes.
    pub git_dirty: Option<bool>,
    /// Untracked files under `src`, which are compiled but not identified by the
    /// revision alone.
    pub untracked_build_inputs: Option<usize>,
    /// What the worker reported about the reference it imported.
    pub headroom: Option<HeadroomProvenance>,
    /// Probe model, when the semantic probe ran.
    pub probe_model: Option<String>,
    /// Anything that degraded rather than failed the run.
    pub degradations: Vec<String>,
    /// Latency of each side, for the summary table.
    pub tokenless_latency: Option<Latency>,
    /// Latency of the reference's router variant.
    pub headroom_latency: Option<Latency>,
}

/// A full L3 report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Run metadata and provenance.
    pub summary: RunSummary,
    /// Per-content-type aggregates.
    pub groups: Vec<GroupRow>,
    /// Every scenario measured.
    pub scenarios: Vec<ScenarioRow>,
    /// Gate signals.
    pub gate: Vec<GateSignal>,
}

/// Retention below this share is called out, since a compressor that keeps most
/// of the payload but drops the error line has failed at the job.
pub const RETENTION_FLOOR: f64 = 0.85;

impl Report {
    /// Collect gate signals from the measured rows.
    ///
    /// Deliberately narrow: only conditions a reviewer must look at. Anything
    /// derivable from the tables belongs in the tables.
    pub fn compute_gate(&mut self) {
        let mut gate = Vec::new();

        for row in &self.scenarios {
            for (side, retention, rate, missing) in [
                (
                    "tokenless",
                    row.tokenless_retention,
                    row.tokenless_rate,
                    &row.tokenless_missing,
                ),
                (
                    "the reference",
                    row.headroom_retention,
                    row.headroom_router_rate,
                    &row.headroom_missing,
                ),
            ] {
                // Retention only carries information where something was
                // actually compressed: with a 0% rate the output equals the
                // input, so a perfect score is tautological.
                let Some((passed, total)) = retention else {
                    continue;
                };
                if total == 0 || rate.is_none_or(|r| r <= 0.0) {
                    continue;
                }
                let share = passed as f64 / total as f64;
                if share < RETENTION_FLOOR {
                    gate.push(GateSignal {
                        subject: row.scenario.clone(),
                        side: Some(side.to_string()),
                        kind: "retention".to_string(),
                        detail: format!(
                            "kept {passed}/{total} critical items at {:.1}% compression; dropped: {}",
                            rate.unwrap_or(0.0) * 100.0,
                            if missing.is_empty() {
                                "(not recorded)".to_string()
                            } else {
                                missing.join("; ")
                            }
                        ),
                    });
                }
            }

            // The reference publishes latency targets for the pipeline scenarios;
            // exceeding one is its own signal, on its own basis.
            if let (Some(target), Some(actual)) = (row.headroom_target_ms, row.headroom_ms)
                && actual > target
            {
                gate.push(GateSignal {
                    subject: row.scenario.clone(),
                    side: Some("the reference".to_string()),
                    kind: "latency_target".to_string(),
                    detail: format!("{actual:.1} ms against its own {target:.0} ms target"),
                });
            }

            if let Some(error) = &row.headroom_error {
                gate.push(GateSignal {
                    subject: row.scenario.clone(),
                    side: Some("the reference".to_string()),
                    kind: "failed".to_string(),
                    detail: error.clone(),
                });
            }

            // The gate this layer is defined on: how much answerability the
            // compression cost, measured against the uncompressed conversation.
            for (side, probe) in [
                ("tokenless", row.tokenless_probe.as_ref()),
                ("the reference", row.headroom_probe.as_ref()),
            ] {
                let Some(probe) = probe else { continue };
                let Some(drop) = probe.drop() else { continue };
                if drop > super::probe::MAX_DROP {
                    gate.push(GateSignal {
                        subject: row.scenario.clone(),
                        side: Some(side.to_string()),
                        kind: "probe_drop".to_string(),
                        detail: format!(
                            "answerability fell {:.0}% ({}/{} of originally-answerable \
                             questions survived); lost: {}",
                            drop * 100.0,
                            probe.retained,
                            probe.correct_uncompressed,
                            if probe.lost.is_empty() {
                                "(not recorded)".to_string()
                            } else {
                                probe.lost.join("; ")
                            }
                        ),
                    });
                }
            }
        }

        // A content type where tokenless has no entry point anywhere is the
        // capability-gap signal this layer exists to produce.
        for group in &self.groups {
            if group.tokenless_applicable == 0 {
                gate.push(GateSignal {
                    subject: group.content_type.clone(),
                    side: Some("tokenless".to_string()),
                    kind: "capability_gap".to_string(),
                    detail: format!(
                        "no entry point in any of {} scenarios of this content type",
                        group.scenarios
                    ),
                });
            }
        }

        self.gate = gate;
    }
}

/// Format a rate as a percentage, or a dash when there is nothing to report.
pub fn pct(rate: Option<f64>) -> String {
    rate.map(|v| format!("{:.1}%", v * 100.0))
        .unwrap_or_else(|| "—".to_string())
}

/// Format retention as passed/total, or a dash when nothing was checkable.
///
/// An unchecked scenario must not render as a perfect score.
pub fn keep(retention: Option<(usize, usize)>) -> String {
    match retention {
        Some((_, 0)) | None => "—".to_string(),
        Some((passed, total)) => format!("{passed}/{total}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rate: Option<f64>, retention: Option<(usize, usize)>) -> ScenarioRow {
        ScenarioRow {
            suite: "scenario".into(),
            content_type: "logs".into(),
            scenario: "structured_5000".into(),
            size_label: None,
            headroom_native: true,
            tokens_before: 1000,
            headroom_target_ms: None,
            tokenless_rate: rate,
            tokenless_entry_points: vec![],
            tokenless_gap_reason: None,
            tokenless_retention: retention,
            tokenless_missing: vec!["error_entry: disk full".into()],
            tokenless_ms: 0.0,
            headroom_pure_rate: None,
            headroom_router_rate: None,
            headroom_retention: None,
            headroom_missing: vec![],
            headroom_ms: None,
            headroom_strategies: vec![],
            headroom_self_tokens: None,
            headroom_error: None,
            tokenless_probe: None,
            headroom_probe: None,
        }
    }

    fn report(scenarios: Vec<ScenarioRow>, groups: Vec<GroupRow>) -> Report {
        Report {
            summary: RunSummary {
                date: "now".into(),
                platform: "test".into(),
                git_sha: None,
                git_dirty: None,
                untracked_build_inputs: None,
                headroom: None,
                probe_model: None,
                degradations: vec![],
                tokenless_latency: None,
                headroom_latency: None,
            },
            groups,
            scenarios,
            gate: vec![],
        }
    }

    #[test]
    fn low_retention_at_high_compression_is_flagged() {
        let mut r = report(vec![row(Some(0.994), Some((1, 5)))], vec![]);
        r.compute_gate();
        assert_eq!(r.gate.len(), 1, "{:?}", r.gate);
        assert_eq!(r.gate[0].kind, "retention");
        assert!(r.gate[0].detail.contains("disk full"));
    }

    #[test]
    fn perfect_retention_at_zero_compression_is_not_a_signal() {
        // With nothing compressed the output equals the input, so the score is
        // tautological and must not be presented as evidence of quality.
        let mut r = report(vec![row(Some(0.0), Some((5, 5)))], vec![]);
        r.compute_gate();
        assert!(r.gate.is_empty(), "{:?}", r.gate);
    }

    #[test]
    fn unchecked_retention_is_not_a_signal() {
        let mut r = report(vec![row(Some(0.9), Some((0, 0)))], vec![]);
        r.compute_gate();
        assert!(r.gate.is_empty(), "{:?}", r.gate);
    }

    #[test]
    fn a_content_type_with_no_entry_point_is_a_capability_gap() {
        let group = GroupRow {
            content_type: "text".into(),
            scenarios: 4,
            tokenless_applicable: 0,
            tokenless: None,
            headroom_router: None,
            paired_gap: None,
            tokenless_retention: (0, 0),
            headroom_retention: (0, 0),
        };
        let mut r = report(vec![], vec![group]);
        r.compute_gate();
        assert_eq!(r.gate.len(), 1);
        assert_eq!(r.gate[0].kind, "capability_gap");
    }

    #[test]
    fn missing_headroom_target_is_not_a_latency_signal() {
        let mut base = row(Some(0.5), Some((5, 5)));
        base.headroom_ms = Some(900.0);
        base.headroom_target_ms = None;
        let mut r = report(vec![base], vec![]);
        r.compute_gate();
        assert!(r.gate.iter().all(|g| g.kind != "latency_target"));
    }

    #[test]
    fn exceeding_a_published_target_is_flagged() {
        let mut base = row(Some(0.5), Some((5, 5)));
        base.headroom_ms = Some(120.0);
        base.headroom_target_ms = Some(30.0);
        let mut r = report(vec![base], vec![]);
        r.compute_gate();
        assert!(r.gate.iter().any(|g| g.kind == "latency_target"));
    }

    #[test]
    fn formatting_distinguishes_absent_from_zero() {
        assert_eq!(pct(None), "—");
        assert_eq!(pct(Some(0.0)), "0.0%");
        assert_eq!(keep(None), "—");
        assert_eq!(keep(Some((0, 0))), "—");
        assert_eq!(keep(Some((1, 5))), "1/5");
    }
}
