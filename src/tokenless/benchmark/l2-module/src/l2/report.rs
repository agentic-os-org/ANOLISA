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

//! Report assembly: `report.json` (machine-readable) and
//! `L2_MODULE_COMPARISON_REPORT.md` (human-readable) from the aggregated
//! comparison data.
//!
//! Every number in the markdown carries its sample size and interval —
//! "value ± CI (N=x)" — and every latency row carries its measurement basis,
//! because the three sides are timed on deliberately different bases and a
//! bare number table would invite invalid cross-side latency comparisons.

use crate::l2::stats::LatencyPercentiles;
use crate::l2::task_sim::TaskSideReplay;
use crate::l2::{Category, L2Error};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;

/// Threshold (percentage points) at which tokenless trailing headroom on
/// compression becomes an L1 candidate signal.
const GAP_SIGNAL_PP: f64 = 15.0;
/// Semantic score below which a side is flagged in the quality gate.
const SEMANTIC_FLOOR: f64 = 0.85;

/// Renders a provenance dirty flag. An unknown state is spelled out rather than
/// shown as clean, so a report never overstates how precisely it is pinned.
fn dirty_suffix(dirty: Option<bool>) -> &'static str {
    match dirty {
        Some(true) => " **(dirty working tree)**",
        Some(false) => "",
        None => " (dirty state unknown)",
    }
}

/// Renders the untracked-build-input count. Reported separately from the dirty
/// flag: untracked sources are synced and compiled just like tracked ones, so
/// they break reproducible attribution even on an otherwise clean checkout.
fn untracked_suffix(count: Option<usize>) -> String {
    match count {
        Some(0) => String::new(),
        Some(n) => format!(" **(+{n} untracked build input(s) — not identified by this SHA)**"),
        None => " (untracked build inputs unknown)".to_string(),
    }
}

/// Aggregated metrics for one side within one category.
#[derive(Debug, Clone, Serialize)]
pub struct SideAggregate {
    /// Independent observations feeding the compression and retention
    /// statistics.
    ///
    /// For deterministic (static) categories this is the number of distinct
    /// samples, not `reps * samples`: repeating a deterministic compression
    /// adds no information, and counting the copies would shrink the intervals
    /// by pseudo-replication.
    pub n: usize,
    /// Repetitions actually executed. Latency percentiles use all of them
    /// (latency varies run-to-run even when the output does not); compression
    /// and retention use `n`.
    pub reps: usize,
    /// Mean o200k "before" token count over the independent observations.
    /// Exposed so an input asymmetry between the two sides is visible in the
    /// numbers rather than only in a footnote.
    pub tokens_before_mean: f64,
    /// Mean compression rate (`1 - after/before`, o200k base).
    pub compression_mean: f64,
    /// Bootstrap 95% CI of the mean compression rate.
    pub compression_ci: (f64, f64),
    /// Side-report mean compression rate under cl100k_base. Reported for
    /// tokenizer-sensitivity analysis only; never feeds the quality gate.
    pub compression_mean_cl100k: f64,
    /// Bootstrap 95% CI of the cl100k_base mean compression rate.
    pub compression_ci_cl100k: (f64, f64),
    /// Ground-truth items retained / checked, pooled over the independent
    /// observations (see [`Self::n`]).
    pub retention_passed: usize,
    pub retention_total: usize,
    /// Wilson 95% interval over the pooled retention counts.
    pub retention_ci: (f64, f64),
    /// Deduplicated descriptions of ground-truth items lost by compression,
    /// collected over the independent observations. Capped at
    /// [`Self::RETENTION_MISSING_CAP`] entries to keep the report compact.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub retention_missing: Vec<String>,
    /// Semantic probe score: the share of questions answerable on the original
    /// text that are still answerable after compression. `None` when unprobed
    /// or when the original answered nothing.
    pub semantic_score: Option<f64>,
    /// Samples that had probe questions and were scored.
    pub probed_samples: usize,
    /// Samples that had no probe question to ask, so they contributed nothing
    /// to [`Self::semantic_score`]. Reported because a sample can lose all its
    /// content and still be absent from the score: without this count a reader
    /// would take the `S` column as covering every sample in the category.
    pub unprobed_samples: usize,
    /// Latency percentiles in milliseconds.
    pub latency_ms: LatencyPercentiles,
    /// Which timing basis produced the latency numbers.
    pub latency_basis: String,
    /// headroom's self-reported (before, after) token counts averaged over
    /// the series — cross-check evidence for the tiktoken headline numbers.
    /// `None` for sides that do not self-report (tokenless/rtk).
    pub hr_tokens_evidence: Option<(u64, u64)>,
}

impl SideAggregate {
    /// Maximum number of missing-item descriptions carried into the report.
    /// Beyond this the list is truncated with a count suffix so the report
    /// stays compact even on categories with many lost items.
    pub const RETENTION_MISSING_CAP: usize = 10;

    /// Builds a deduplicated, capped missing-item list from raw failure
    /// descriptions collected across independent observations.
    pub fn collect_missing(raw: Vec<String>) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut deduped: Vec<String> = Vec::new();
        for s in raw {
            if seen.insert(s.clone()) {
                deduped.push(s);
            }
        }
        if deduped.len() > Self::RETENTION_MISSING_CAP {
            let overflow = deduped.len() - Self::RETENTION_MISSING_CAP;
            deduped.truncate(Self::RETENTION_MISSING_CAP);
            deduped.push(format!("… and {overflow} more"));
        }
        deduped
    }
}

/// Paired per-instance compression-rate difference (tokenless − headroom).
///
/// Positive mean = tokenless compresses harder. Carries its own N because
/// pairing truncates to the repetitions both sides completed.
#[derive(Debug, Clone, Serialize)]
pub struct GapStats {
    /// Mean of the paired differences.
    pub mean: f64,
    /// Bootstrap 95% CI of the paired-difference mean.
    pub ci: (f64, f64),
    /// Number of aligned (sample, repetition) pairs.
    pub n_pairs: usize,
}

/// One category's two-sided comparison; a missing side means degradation.
#[derive(Debug, Clone, Serialize)]
pub struct CategoryComparison {
    pub category: Category,
    /// Paired compression-rate gap; `None` when either side degraded, fewer
    /// than two aligned pairs exist, or the two sides did not receive
    /// byte-identical input (see [`Self::input_asymmetry`]).
    pub compression_gap: Option<GapStats>,
    /// `Some(reason)` when the sides were fed different bytes, so the gap is
    /// deliberately withheld rather than presented as a comparison.
    pub input_asymmetry: Option<String>,
    pub tokenless: Option<SideAggregate>,
    pub headroom: Option<SideAggregate>,
}

/// One task's replay across both sides.
#[derive(Debug, Clone, Serialize)]
pub struct TaskComparison {
    pub name: String,
    pub notes: String,
    /// Categories in this task whose sides were not fed byte-identical input.
    /// Non-empty means the cross-side totals below mix a non-comparable
    /// measurement and must not be read as a head-to-head result.
    pub asymmetric_categories: Vec<String>,
    pub tokenless: Option<TaskSideReplay>,
    pub headroom: Option<TaskSideReplay>,
}

/// Run-level context recorded in the report header.
#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    /// ISO-8601 UTC timestamp of the run.
    pub date: String,
    /// Repository commit the run measured.
    pub git_sha: String,
    /// Whether the measured checkout had uncommitted changes to *tracked*
    /// files. A dirty tree means `git_sha` alone does not identify what was
    /// benchmarked.
    pub git_dirty: Option<bool>,
    /// Untracked files present inside the measured component's paths. These are
    /// synced to the benchmark host and compiled or loaded like any other
    /// source, so a non-zero count also means `git_sha` alone does not identify
    /// the measured code — tracked and untracked drift are reported separately
    /// because they have different causes and remedies.
    pub untracked_build_inputs: Option<usize>,
    /// Commit of the headroom checkout the worker imported, when derivable.
    /// `None` when headroom was unavailable or installed as a wheel.
    pub headroom_revision: Option<String>,
    /// Whether that headroom checkout had uncommitted changes.
    pub headroom_dirty: Option<bool>,
    /// Untracked files in the headroom checkout. The editable install imports
    /// whatever sits in its source dir, so a non-zero count means the headroom
    /// revision alone does not identify the compared code either.
    pub headroom_untracked: Option<usize>,
    /// Commit of the rtk source tree the measured binary was built from, when
    /// derivable. rtk is a pinned clone that lives outside version control
    /// (gitignored), so without this the report could attribute results to an
    /// arbitrary local rtk. `None` when the rtk side was unavailable or its
    /// source dir could not be resolved.
    pub rtk_revision: Option<String>,
    /// Whether that rtk tree had uncommitted changes. Expected `true` in normal
    /// operation because `just setup-rtk` patches the pinned tag.
    pub rtk_dirty: Option<bool>,
    /// `os/arch` of the machine that ran the comparison.
    pub platform: String,
    pub headroom_available: bool,
    pub rtk_available: bool,
    /// Probe model id, or `None` when probing was disabled/keyless.
    pub probe_model: Option<String>,
    /// Every degradation that occurred, verbatim, so a partial run can
    /// never masquerade as a full one.
    pub degradations: Vec<String>,
}

/// The full report bundle.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub summary: RunSummary,
    pub categories: Vec<CategoryComparison>,
    pub tasks: Vec<TaskComparison>,
}

impl Report {
    /// Writes `report.json` and `L2_MODULE_COMPARISON_REPORT.md` into `dir`,
    /// creating it if needed.
    ///
    /// # Errors
    ///
    /// [`L2Error::Io`]/[`L2Error::Json`] on write or serialisation failure.
    pub fn write(&self, dir: &Path) -> Result<(), L2Error> {
        std::fs::create_dir_all(dir)?;
        let json_value = self.to_json();
        std::fs::write(
            dir.join("report.json"),
            serde_json::to_string_pretty(&json_value)?,
        )?;
        std::fs::write(
            dir.join("L2_MODULE_COMPARISON_REPORT.md"),
            self.to_markdown(),
        )?;
        Ok(())
    }

    /// The machine-readable report, quality gate included.
    pub fn to_json(&self) -> Value {
        json!({
            "summary": self.summary,
            "categories": self.categories,
            "tasks": self.tasks,
            "quality_gate": self.quality_gate(),
        })
    }

    /// Evaluates the quality gate over the aggregated data.
    ///
    /// Signals emitted:
    /// * `l1_candidate` — tokenless trails headroom on compression by more
    ///   than 15pp in a category (candidate for L1 deep-dive);
    /// * `semantic_flag` — a side's S dropped below 0.85;
    /// * `latency_flag` — a side's p99 exceeded the category budget.
    pub fn quality_gate(&self) -> Vec<Value> {
        let mut findings = Vec::new();
        for cat in &self.categories {
            // Gate on the paired gap, never on a fresh subtraction of the two
            // per-side means. `compression_gap` is already `None` when the gap
            // is not a legitimate comparison — an input-asymmetric category, a
            // degraded side, or fewer than two aligned pairs — so recomputing
            // one here would let the report call a category non-comparable and
            // still emit an l1_candidate for it.
            if let Some(gap) = &cat.compression_gap {
                // GapStats is tokenless - headroom, so tokenless trailing is a
                // negative mean.
                let gap_pp = -gap.mean * 100.0;
                if gap_pp > GAP_SIGNAL_PP {
                    findings.push(json!({
                        "kind": "l1_candidate",
                        "category": cat.category.name(),
                        "gap_pp": gap_pp,
                        "n_pairs": gap.n_pairs,
                        "detail": format!(
                            "tokenless trails headroom by {gap_pp:.1}pp on compression"
                        ),
                    }));
                }
            }
            for (side, agg) in [("tokenless", &cat.tokenless), ("headroom", &cat.headroom)] {
                let Some(agg) = agg else { continue };
                if let Some(s) = agg.semantic_score.filter(|s| *s < SEMANTIC_FLOOR) {
                    let mut finding = json!({
                        "kind": "semantic_flag",
                        "category": cat.category.name(),
                        "side": side,
                        "semantic_score": s,
                    });
                    if !agg.retention_missing.is_empty() {
                        finding["retention_missing"] =
                            serde_json::to_value(&agg.retention_missing).unwrap();
                    }
                    findings.push(finding);
                }
                let budget = cat.category.p99_budget_ms();
                if agg.latency_ms.p99 > budget {
                    findings.push(json!({
                        "kind": "latency_flag",
                        "category": cat.category.name(),
                        "side": side,
                        "p99_ms": agg.latency_ms.p99,
                        "budget_ms": budget,
                        "basis": agg.latency_basis,
                    }));
                }
            }
        }
        findings
    }

    /// Renders the human-readable markdown report.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("# L2 Module Comparison Report\n\n");
        md.push_str("tokenless vs headroom on identical one-round tool outputs.\n\n");

        md.push_str("## Summary\n\n");
        md.push_str(&format!("- Date: {}\n", self.summary.date));
        md.push_str(&format!(
            "- Git SHA: `{}`{}{}\n",
            self.summary.git_sha,
            dirty_suffix(self.summary.git_dirty),
            untracked_suffix(self.summary.untracked_build_inputs)
        ));
        md.push_str(&format!(
            "- Headroom revision: {}{}{}\n",
            match &self.summary.headroom_revision {
                Some(r) => format!("`{r}`"),
                None => "unknown (not a git checkout, or headroom unavailable)".to_string(),
            },
            dirty_suffix(self.summary.headroom_dirty),
            untracked_suffix(self.summary.headroom_untracked)
        ));
        md.push_str(&format!(
            "- RTK revision: {}{}\n",
            match &self.summary.rtk_revision {
                Some(r) => format!("`{r}`"),
                None => "unknown (rtk unavailable or source dir unresolved)".to_string(),
            },
            dirty_suffix(self.summary.rtk_dirty)
        ));
        md.push_str(&format!("- Platform: {}\n", self.summary.platform));
        md.push_str(&format!(
            "- Headroom side: {}\n",
            if self.summary.headroom_available {
                "available"
            } else {
                "UNAVAILABLE (degraded)"
            }
        ));
        md.push_str(&format!(
            "- RTK side: {}\n",
            if self.summary.rtk_available {
                "available"
            } else {
                "UNAVAILABLE (degraded)"
            }
        ));
        match &self.summary.probe_model {
            Some(m) => md.push_str(&format!("- Semantic probe model: `{m}`\n")),
            None => {
                md.push_str("- Semantic probe: disabled (no key or --no-probe); all S = None\n")
            }
        }
        md.push('\n');

        md.push_str("## Latency bases\n\n");
        md.push_str(
            "| Side | Basis |\n|---|---|\n\
             | tokenless (json/code) | in-process (`Instant` around the compress call) |\n\
             | tokenless (command/grep/diff) | rtk wrapped-minus-raw wall clock (includes process startup) |\n\
             | headroom | worker-internal (`perf_counter` around `router.compress`) |\n\n\
             Latency numbers are **not comparable across bases**; compare within a row's basis only.\n\n",
        );

        md.push_str("## Category matrix\n\n");
        md.push_str(
            "Compression is o200k_base (headline); the cl100k column is a side\n\
             report for tokenizer sensitivity only.\n\n\
             N is the number of *independent* observations behind the compression\n\
             and retention statistics. Deterministic (static) categories\n\
             contribute one observation per sample no matter how many\n\
             repetitions ran, so the intervals are not narrowed by\n\
             pseudo-replication; `reps` records the repetitions, which latency\n\
             still uses in full. The `before tok` column is the mean original\n\
             token count, so any input asymmetry between the two sides is\n\
             visible directly in the numbers.\n\n\
             Each side is de-duplicated on its own output, so the two rows of a\n\
             category can carry different N: a side whose output varies across\n\
             repetitions keeps more observations than one that is byte-stable.\n\
             Read a row against its own N, and take the head-to-head number from\n\
             the paired gap table, which pairs per instance instead of comparing\n\
             two independently pooled means.\n\n",
        );
        md.push_str(
            "| Category | Side | Compression ± 95% CI (N, reps) | before tok | cl100k side-report | Retention [Wilson 95%] | S | p50 ms | p95 ms | p99 ms | Basis |\n",
        );
        md.push_str("|---|---|---|---|---|---|---|---|---|---|---|\n");
        for cat in &self.categories {
            for (side, agg) in [("tokenless", &cat.tokenless), ("headroom", &cat.headroom)] {
                match agg {
                    Some(a) => {
                        let retention = if a.retention_total == 0 {
                            "n/a".to_string()
                        } else {
                            format!(
                                "{}/{} [{:.2}, {:.2}]",
                                a.retention_passed,
                                a.retention_total,
                                a.retention_ci.0,
                                a.retention_ci.1
                            )
                        };
                        // Flag partial probe coverage in the cell itself: an S
                        // computed over a subset while other samples were never
                        // asked would otherwise read as covering the category.
                        let coverage = if a.unprobed_samples > 0 {
                            format!(
                                " ({} of {} probed)",
                                a.probed_samples,
                                a.probed_samples + a.unprobed_samples
                            )
                        } else {
                            String::new()
                        };
                        let s = a
                            .semantic_score
                            .map(|v| {
                                if v < SEMANTIC_FLOOR {
                                    format!("**{v:.2} ⚠**{coverage}")
                                } else {
                                    format!("{v:.2}{coverage}")
                                }
                            })
                            .unwrap_or_else(|| {
                                if a.unprobed_samples > 0 {
                                    // Same denominator as the Some branch: a
                                    // score can be absent while some samples
                                    // were probed (their originals answered
                                    // nothing), so hard-coding 0 would
                                    // misreport the coverage.
                                    format!(
                                        "— ({} of {} probed)",
                                        a.probed_samples,
                                        a.probed_samples + a.unprobed_samples
                                    )
                                } else {
                                    "None".to_string()
                                }
                            });
                        let p99 = if a.latency_ms.p99 > cat.category.p99_budget_ms() {
                            format!("**{:.3} ⚠**", a.latency_ms.p99)
                        } else {
                            format!("{:.3}", a.latency_ms.p99)
                        };
                        md.push_str(&format!(
                            "| {} | {} | {:.1}% ± [{:.1}%, {:.1}%] (N={}, reps={}) | {:.0} | {:.1}% [{:.1}%, {:.1}%] | {} | {} | {:.3} | {:.3} | {} | {} |\n",
                            cat.category.name(),
                            side,
                            a.compression_mean * 100.0,
                            a.compression_ci.0 * 100.0,
                            a.compression_ci.1 * 100.0,
                            a.n,
                            a.reps,
                            a.tokens_before_mean,
                            a.compression_mean_cl100k * 100.0,
                            a.compression_ci_cl100k.0 * 100.0,
                            a.compression_ci_cl100k.1 * 100.0,
                            retention,
                            s,
                            a.latency_ms.p50,
                            a.latency_ms.p95,
                            p99,
                            a.latency_basis,
                        ));
                    }
                    None => {
                        md.push_str(&format!(
                            "| {} | {} | — degraded — | — | — | — | — | — | — | — | — |\n",
                            cat.category.name(),
                            side,
                        ));
                    }
                }
            }
        }
        md.push('\n');

        // Retention missing items: show exactly which ground-truth facts were
        // lost, so reviewers can judge whether the drops are cosmetic or
        // material (error codes, transaction ids, etc.).
        let has_missing = self.categories.iter().any(|c| {
            c.tokenless
                .as_ref()
                .is_some_and(|a| !a.retention_missing.is_empty())
                || c.headroom
                    .as_ref()
                    .is_some_and(|a| !a.retention_missing.is_empty())
        });
        if has_missing {
            md.push_str("## Retention missing items\n\n");
            md.push_str(
                "Ground-truth items lost by compression, per category and side.\n\
                 Empty means all items were retained.\n\n",
            );
            for cat in &self.categories {
                for (side, agg) in [("tokenless", &cat.tokenless), ("headroom", &cat.headroom)] {
                    if let Some(a) = agg
                        && !a.retention_missing.is_empty()
                    {
                        md.push_str(&format!("### {} — {}\n\n", cat.category.name(), side));
                        for item in &a.retention_missing {
                            md.push_str(&format!("- {item}\n"));
                        }
                        md.push('\n');
                    }
                }
            }
        }

        // Non-comparable categories get a named caveat rather than a silently
        // missing gap row.
        let asymmetric: Vec<&CategoryComparison> = self
            .categories
            .iter()
            .filter(|c| c.input_asymmetry.is_some())
            .collect();
        if !asymmetric.is_empty() {
            md.push_str("## Not directly comparable\n\n");
            for cat in asymmetric {
                md.push_str(&format!(
                    "- **{}**: {}\n",
                    cat.category.name(),
                    cat.input_asymmetry.as_deref().unwrap_or(""),
                ));
            }
            md.push('\n');
        }

        md.push_str("## Paired compression gap (tokenless − headroom, o200k)\n\n");
        md.push_str(
            "Per-instance paired differences remove sample-to-sample variance;\n\
             positive = tokenless compresses harder. A CI excluding 0 marks a\n\
             statistically resolvable gap.\n\n",
        );
        md.push_str("| Category | Gap mean ± 95% CI | Pairs |\n|---|---|---|\n");
        for cat in &self.categories {
            match &cat.compression_gap {
                Some(g) => md.push_str(&format!(
                    "| {} | {:+.1}pp ± [{:+.1}pp, {:+.1}pp] | {} |\n",
                    cat.category.name(),
                    g.mean * 100.0,
                    g.ci.0 * 100.0,
                    g.ci.1 * 100.0,
                    g.n_pairs,
                )),
                None => md.push_str(&format!(
                    "| {} | — unavailable (side degraded or < 2 pairs) — | — |\n",
                    cat.category.name(),
                )),
            }
        }
        md.push('\n');

        md.push_str("## Task-level totals\n\n");
        md.push_str(
            "| Task | Side | Interactions (covered) | Tokens before | Tokens after | Saved | Rate | Compress time s |\n",
        );
        md.push_str("|---|---|---|---|---|---|---|---|\n");
        for task in &self.tasks {
            for (side, replay) in [("tokenless", &task.tokenless), ("headroom", &task.headroom)] {
                match replay {
                    Some(r) => md.push_str(&format!(
                        "| {} | {} | {} ({}) | {:.0} | {:.0} | {:.0} | {:.1}% | {:.4} |\n",
                        task.name,
                        side,
                        r.interactions,
                        r.covered_steps,
                        r.tokens_before_total,
                        r.tokens_after_total,
                        r.tokens_saved,
                        r.saving_rate * 100.0,
                        r.compression_time_s,
                    )),
                    None => md.push_str(&format!(
                        "| {} | {} | — degraded — | — | — | — | — | — |\n",
                        task.name, side,
                    )),
                }
            }
            if !task.notes.is_empty() {
                md.push_str(&format!("\n> {}: {}\n\n", task.name, task.notes));
            }
            if !task.asymmetric_categories.is_empty() {
                md.push_str(&format!(
                    "\n> {}: totals include {} sample(s), whose two sides are not fed \
                     byte-identical input — the cross-side totals above are not a \
                     head-to-head result. See \"Not directly comparable\".\n\n",
                    task.name,
                    task.asymmetric_categories.join(", "),
                ));
            }
        }
        md.push('\n');

        md.push_str("## Degradations\n\n");
        if self.summary.degradations.is_empty() {
            md.push_str("None — both sides ran in full.\n\n");
        } else {
            for d in &self.summary.degradations {
                md.push_str(&format!("- {d}\n"));
            }
            md.push('\n');
        }

        md.push_str("## Quality gate\n\n");
        let findings = self.quality_gate();
        if findings.is_empty() {
            md.push_str("All checks passed: no compression gap > 15pp, no S < 0.85, all p99 within budget (json < 2ms, code < 5ms, command/grep/diff < 10ms).\n");
        } else {
            for f in &findings {
                md.push_str(&format!(
                    "- `{}`\n",
                    serde_json::to_string(f).unwrap_or_default()
                ));
            }
        }
        md
    }
}
