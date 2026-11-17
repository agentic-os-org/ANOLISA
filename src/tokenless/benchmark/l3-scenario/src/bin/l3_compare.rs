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

//! L3 scenario comparison driver: runs both sides over every committed scenario
//! and writes `report.json` plus a Markdown summary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokenless_l3_bench::l3::{
    asset::{self, Applicability},
    headroom_side::{self, HeadroomResult, HeadroomWorker, Variant},
    probe::{self, Probe},
    report::{GroupRow, LifecycleSummary, Report, RunSummary, ScenarioRow, keep, pct},
    retention,
    stats::{self, Latency},
    tokenizer::{Tokenizers, compression_rate},
    tokenless_side,
};

/// Command-line options.
///
/// The environment variables `L3_REPORT_DIR` and `L3_NO_PROBE` remain accepted
/// as fallbacks so existing remote scripts keep working; an explicit flag wins
/// over the variable.
struct Options {
    no_probe: bool,
    tokenless_only: bool,
    recovery: bool,
    report_dir: Option<PathBuf>,
}

/// Parses `--no-probe` and `--report-dir <path>`.
///
/// # Errors
///
/// Returns an error for an unknown flag or for `--report-dir` without a value,
/// rather than ignoring it: silently dropping a flag the README documents is how
/// a run ends up probing when the caller asked it not to.
fn parse_args() -> Result<Options> {
    let mut opts = Options {
        no_probe: false,
        tokenless_only: false,
        recovery: false,
        report_dir: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-probe" => opts.no_probe = true,
            "--tokenless-only" => opts.tokenless_only = true,
            "--recovery" => opts.recovery = true,
            "--report-dir" => {
                let value = args.next().context("--report-dir requires a value")?;
                opts.report_dir = Some(PathBuf::from(value));
            }
            other => anyhow::bail!(
                "unknown argument {other:?} (accepts --no-probe, --tokenless-only, --recovery, --report-dir)"
            ),
        }
    }
    Ok(opts)
}

fn main() -> Result<()> {
    let opts = parse_args()?;
    let report_dir = opts.report_dir.clone().unwrap_or_else(report_dir);
    let assets = assets_root();
    let scenarios =
        asset::load_all(&assets).with_context(|| format!("loading scenarios from {assets:?}"))?;
    let tk = Tokenizers::load().context("loading tokenizers")?;

    let mut degradations = Vec::new();

    // A missing worker degrades to a one-sided run: tokenless-only numbers still
    // answer part of the question, and aborting would discard them.
    let python = headroom_side::python_binary();
    let mut worker = if opts.tokenless_only {
        None
    } else {
        match HeadroomWorker::start(&python, &headroom_side::worker_path()) {
            Ok(w) => Some(w),
            Err(e) => {
                degradations.push(format!("the reference side unavailable: {e}"));
                None
            }
        }
    };
    let headroom_provenance = worker.as_ref().map(|w| w.provenance().clone());

    // The semantic probe needs an API key; without one it degrades so the
    // report cannot be read as if the L3 gate had been evaluated.
    let probe_disabled =
        opts.tokenless_only || opts.no_probe || std::env::var("L3_NO_PROBE").is_ok();
    let probe = if probe_disabled {
        None
    } else {
        match Probe::from_env() {
            Ok(p) => Some(p),
            Err(e) => {
                degradations.push(format!(
                    "semantic probe skipped ({e}): the four-layer plan's L3 gate \
                     (probe success-rate drop < 5%) is NOT evaluated in this run"
                ));
                None
            }
        }
    };
    if probe_disabled {
        degradations.push(
            "semantic probe disabled by --tokenless-only/--no-probe/L3_NO_PROBE: the L3 gate is NOT evaluated"
                .to_string(),
        );
    }
    let probe = probe.filter(|_| !probe_disabled);
    let probe_model = probe.as_ref().map(|p| p.model().to_string());

    let mut rows = Vec::with_capacity(scenarios.len());
    let mut tl_times = Vec::new();
    let mut hr_times = Vec::new();
    let mut lifecycle_evidence = Vec::new();
    let mode_id = if opts.recovery {
        "recovery"
    } else {
        "no-recovery"
    };

    for s in &scenarios {
        let tl = tokenless_side::run_with_recovery(s, &tk, opts.recovery)?;
        let tl_summary = tl.lifecycle_summary();
        lifecycle_evidence.push(serde_json::json!({
            "suite": s.suite,
            "scenario": s.scenario,
            "content_type": s.content_type,
            "side": "tokenless",
            "mode": if opts.recovery { "production Runtime/in-memory recovery" } else { "production Runtime/no recovery" },
            "mode_id": mode_id,
            "runtime_final_acceptance_measured": !tl.runtime_outcomes.is_empty() && tl.runtime_outcomes.iter().all(|outcome| outcome["runtime_final_acceptance_measured"] == true),
            "runtime_outcomes": tl.runtime_outcomes,
            "before_model": tl.before_model,
            "recovered_references": tl.recovered_references,
            "actual_agent_retrieval_cost": null,
            "input": {"messages": s.messages, "tools": s.tools},
            "output": {"messages": tl.messages, "tools": tl.tools},
            "lifecycle": tl.lifecycle,
            "lifecycle_summary": tl_summary,
            // key → 原文；带检索工具的语义测试按此返回，不重新调用 Runtime。
            "stash": tl.stash,
        }));
        let before = tl.before.o200k;
        let tl_rate = compression_rate(before, tl.after.o200k);
        tl_times.push(tl.compress_ms);

        // Critical items come from the original payload, so both sides are held
        // to the same list.
        let items = retention::critical_items(s);
        let tl_keep = retention::check(&items, &tl.messages, tl.tools.as_deref());

        let applicability = s.applicability_from_entry_points(tl.entry_points.clone());
        let (entry_points, gap_reason) = match applicability {
            Applicability::Applicable { entry_points } => (entry_points, None),
            Applicability::NoEntryPoint { reason } => (
                Vec::new(),
                Some(if tl.gap_reasons.is_empty() {
                    reason
                } else {
                    tl.gap_reasons.join("; ")
                }),
            ),
        };

        let mut row = ScenarioRow {
            suite: format!("{:?}", s.suite).to_lowercase(),
            content_type: s.content_type.clone(),
            scenario: s.scenario.clone(),
            size_label: s.size_label.clone(),
            headroom_native: s.source.headroom_native,
            capability_overlay: !s.source.headroom_native,
            tokens_before: before,
            headroom_target_ms: s.headroom_target_ms,
            tokenless_rate: tl_rate,
            tokenless_entry_points: entry_points,
            tokenless_gap_reason: gap_reason,
            tokenless_retention: (tl_keep.total > 0).then_some((tl_keep.passed, tl_keep.total)),
            tokenless_missing: tl_keep.missing,
            tokenless_ms: tl.compress_ms,
            tokenless_lifecycle: tl_summary,
            headroom_pure_rate: None,
            headroom_router_rate: None,
            headroom_ccr_entries: None,
            headroom_retention: None,
            headroom_missing: Vec::new(),
            headroom_ms: None,
            headroom_strategies: Vec::new(),
            headroom_self_tokens: None,
            headroom_error: None,
            tokenless_probe: None,
            headroom_probe: None,
        };
        let mut headroom_output: Option<Vec<tokenless_l3_bench::l3::asset::Message>> = None;
        let mut headroom_runs: Vec<HeadroomResult> = Vec::new();

        if let Some(w) = worker.as_mut() {
            for variant in Variant::all() {
                if !w.supports(variant) {
                    continue;
                }
                match w.run(s, variant) {
                    Ok(Ok(r)) => {
                        degradations.extend(r.warnings.iter().map(|warning| {
                            format!("reference {} {}: {warning}", variant.wire(), s.scenario)
                        }));
                        let after = tokenless_side::conversation_tokens(
                            &r.messages,
                            s.tools.as_deref(),
                            &tk,
                        );
                        let rate = compression_rate(before, after.o200k);
                        // A marker whose original the store no longer holds is an
                        // unrecoverable deletion in practice; record it rather than
                        // let a retrieval test serve nothing for it.
                        for entry in r.ccr.iter().filter(|entry| entry.original.is_none()) {
                            degradations.push(format!(
                                "the reference {} {}: CCR marker {} has no stored original",
                                variant.wire(),
                                s.scenario,
                                entry.key
                            ));
                        }
                        match variant {
                            Variant::PureStage => row.headroom_pure_rate = rate,
                            Variant::Router => {
                                row.headroom_router_rate = rate;
                                row.headroom_ccr_entries = Some(r.ccr.len());
                                let k = retention::check(&items, &r.messages, s.tools.as_deref());
                                row.headroom_retention =
                                    (k.total > 0).then_some((k.passed, k.total));
                                row.headroom_missing = k.missing;
                                row.headroom_ms = Some(r.compress_ms);
                                row.headroom_strategies = strategies(&r.transforms_applied);
                                row.headroom_self_tokens =
                                    r.self_tokens_before.zip(r.self_tokens_after);
                                hr_times.push(r.compress_ms);
                                headroom_output = Some(r.messages.clone());
                            }
                        }
                        headroom_runs.push(r);
                    }
                    Ok(Err(reason)) => row.headroom_error = Some(reason),
                    Err(e) => {
                        row.headroom_error = Some(format!("worker lost: {e}"));
                        degradations
                            .push(format!("the reference worker lost at {}: {e}", s.scenario));
                        worker = None;
                        break;
                    }
                }
            }
        }

        // Probe last, so a slow or failing model cannot cost the deterministic
        // measurements that are already in hand.
        if let Some(p) = probe.as_ref() {
            let qs = probe::questions(s);
            if !qs.is_empty() {
                if row.tokenless_rate.is_some_and(|r| r > 0.0) {
                    row.tokenless_probe = Some(p.score(&qs, &s.messages, &tl.messages));
                }
                // Only probe a side that actually changed the conversation:
                // asking about an untouched payload compares it with itself and
                // spends API calls to learn nothing.
                if let Some(output) = &headroom_output
                    && row.headroom_router_rate.is_some_and(|r| r > 0.0)
                {
                    row.headroom_probe = Some(p.score(&qs, &s.messages, output));
                }
            }
        }

        for run in headroom_runs {
            lifecycle_evidence.push(serde_json::json!({
                "suite": s.suite, "scenario": s.scenario, "content_type": s.content_type,
                "side": "headroom", "mode": run.variant.wire(), "mode_id": run.variant.wire(),
                "input": {"messages": s.messages, "tools": s.tools},
                "output": {"messages": run.messages, "tools": s.tools},
                "transforms_applied": run.transforms_applied,
                "actual_agent_retrieval_cost": null,
                // hash → 原文，取自参考实现自己的 CompressionStore。
                "ccr": run.ccr,
            }));
        }
        rows.push(row);
    }

    let (sha, dirty, untracked) = anolisa_provenance();
    let mut report = Report {
        summary: RunSummary {
            date: chrono::Local::now().to_rfc3339(),
            platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            git_sha: sha,
            git_dirty: dirty,
            untracked_build_inputs: untracked,
            headroom: headroom_provenance,
            probe_model,
            tokenless_mode: mode_id.to_owned(),
            comparison_status: if opts.tokenless_only {
                "not_executed"
            } else {
                "requested"
            }
            .to_owned(),
            schema_recovery_tool: opts.recovery.then(|| "fixture_retrieve".to_owned()),
            degradations,
            tokenless_latency: stats::latency(&mut tl_times),
            headroom_latency: stats::latency(&mut hr_times),
        },
        groups: group_rows(&rows),
        scenarios: rows,
        gate: Vec::new(),
    };
    report.compute_gate();

    std::fs::create_dir_all(&report_dir)
        .with_context(|| format!("creating {}", report_dir.display()))?;
    let json_path = report_dir.join("report.json");
    std::fs::write(&json_path, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("writing {}", json_path.display()))?;
    let lifecycle_path = report_dir.join("lifecycle.json");
    std::fs::write(
        &lifecycle_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 2,
            "mode": if opts.tokenless_only { "tokenless-only" } else { "paired-comparison" },
            "tokenless_mode": mode_id,
            "comparison_status": if opts.tokenless_only { "not_executed" } else { "requested" },
            "billing_tokens": null,
            "rows": lifecycle_evidence,
        }))?,
    )
    .with_context(|| format!("writing {}", lifecycle_path.display()))?;
    let md_path = report_dir.join(if opts.tokenless_only {
        "TOKENLESS_L3_REPORT.md"
    } else {
        "L3_SCENARIO_COMPARISON_REPORT.md"
    });
    let rendered = render(&report);
    std::fs::write(&md_path, rendered).with_context(|| format!("writing {}", md_path.display()))?;

    println!(
        "wrote {}, {} and {}\n{} scenarios, {} gate signals, {} degradations",
        json_path.display(),
        md_path.display(),
        lifecycle_path.display(),
        report.scenarios.len(),
        report.gate.len(),
        report.summary.degradations.len()
    );
    Ok(())
}

/// Aggregate rows by the reference's content type.
fn group_rows(rows: &[ScenarioRow]) -> Vec<GroupRow> {
    let mut by_type: BTreeMap<(&str, bool), Vec<&ScenarioRow>> = BTreeMap::new();
    for row in rows {
        by_type
            .entry((row.content_type.as_str(), row.capability_overlay))
            .or_default()
            .push(row);
    }

    by_type
        .into_iter()
        .map(|((content_type, capability_overlay), group)| {
            // Only scenarios where a side actually acted contribute to that
            // side's mean; a scenario with no entry point is a capability gap,
            // not a zero-rate observation, and averaging it in would understate
            // the compressor on payloads it does handle.
            let tl_values: Vec<f64> = group
                .iter()
                .filter(|r| r.tokenless_gap_reason.is_none())
                .filter_map(|r| r.tokenless_rate)
                .collect();
            let hr_values: Vec<f64> = group
                .iter()
                .filter_map(|r| r.headroom_router_rate)
                .collect();
            let paired: Vec<f64> = group
                .iter()
                .filter(|r| r.tokenless_gap_reason.is_none())
                .filter_map(|r| Some(r.tokenless_rate? - r.headroom_router_rate?))
                .collect();

            let sum_retention = |pick: fn(&ScenarioRow) -> Option<(usize, usize)>| {
                group
                    .iter()
                    .filter_map(|r| pick(r))
                    .fold((0, 0), |acc, x| (acc.0 + x.0, acc.1 + x.1))
            };

            GroupRow {
                content_type: content_type.to_string(),
                capability_overlay,
                scenarios: group.len(),
                tokenless_applicable: group
                    .iter()
                    .filter(|r| r.tokenless_gap_reason.is_none())
                    .count(),
                tokenless: stats::summarize(&tl_values),
                headroom_router: stats::summarize(&hr_values),
                paired_gap: stats::summarize(&paired),
                tokenless_retention: sum_retention(|r| r.tokenless_retention),
                headroom_retention: sum_retention(|r| r.headroom_retention),
            }
        })
        .collect()
}

/// Distinct strategies behind the reference's transform names.
fn strategies(transforms: &[String]) -> Vec<String> {
    let mut kinds: Vec<String> = transforms
        .iter()
        .filter_map(|t| t.split(':').nth(1))
        .map(|s| s.split('(').next().unwrap_or(s).to_string())
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

/// anolisa revision, tracked-dirty flag, and untracked files under `src`.
///
/// Untracked files are counted separately because they are compiled into the
/// binary under test while leaving the revision and dirty flag unchanged, so a
/// commit alone does not identify what ran.
///
/// `L3_ANOLISA_SHA` overrides the query outright. That exists because the
/// benchmark is normally run on a machine that received the tree by rsync, which
/// copies no `.git`: git then fails on every query and the report would claim an
/// unknown revision for a checkout the caller knows precisely. Injecting the SHA
/// is the honest fix; guessing one is not.
fn anolisa_provenance() -> (Option<String>, Option<bool>, Option<usize>) {
    if let Ok(sha) = std::env::var("L3_ANOLISA_SHA")
        && !sha.trim().is_empty()
    {
        // Dirty and untracked stay unknown rather than assumed clean: the caller
        // supplied a revision, not a statement about the working tree.
        return (Some(sha.trim().to_string()), None, None);
    }

    // Walk up to the repository root. The crate sits several levels below it,
    // and `git -C` on a subdirectory of a valid checkout works, but a copied
    // tree has no `.git` at any level — detected here rather than reported as a
    // silent unknown.
    let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf);
    let Some(root) = root else {
        return (None, None, None);
    };

    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            // A checkout owned by another user is refused as "dubious
            // ownership"; grant an exception for these read-only queries rather
            // than mutating the user's git config.
            .args(["-c", &format!("safe.directory={}", root.display())])
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let sha = git(&["rev-parse", "HEAD"]);
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty());
    let untracked = git(&["ls-files", "--others", "--exclude-standard", "--", "src"])
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count());
    (sha, dirty, untracked)
}

/// Render the Markdown summary.
fn render(report: &Report) -> String {
    let mut out = String::new();
    let s = &report.summary;

    if s.comparison_status == "not_executed" {
        out.push_str("# Tokenless L3 Report\n\nHeadroom comparison: not_executed; blank comparator columns are not failures.\n\n");
    } else {
        out.push_str("# L3 Scenario Comparison Report\n\n");
    }
    out.push_str(&format!(
        "- Tokenless mode: {}\n- Comparison status: {}\n- Declared schema recovery tool: {}\n",
        s.tokenless_mode,
        s.comparison_status,
        s.schema_recovery_tool.as_deref().unwrap_or("none")
    ));
    out.push_str(&format!("- Date: {}\n- Platform: {}\n", s.date, s.platform));
    out.push_str(&format!(
        "- anolisa: {} (tracked-dirty: {}, untracked under src: {})\n",
        s.git_sha.as_deref().unwrap_or("unknown"),
        opt(s.git_dirty),
        opt(s.untracked_build_inputs)
    ));
    if let Some(h) = &s.headroom {
        out.push_str(&format!(
            "- the reference: {} (tracked-dirty: {}, untracked: {}, its token estimator: {})\n",
            h.revision.as_deref().unwrap_or("unknown"),
            opt(h.dirty),
            opt(h.untracked),
            h.tokenizer.as_deref().unwrap_or("unknown")
        ));
    }
    out.push_str(
        "- Authoritative token counts: tiktoken-rs `o200k_base`. \
                  the reference's own counts are self-reported under a different \
                  estimator and are not comparable with these.\n",
    );

    if !s.degradations.is_empty() {
        out.push_str("\n## Not measured in this run\n\n");
        for d in &s.degradations {
            out.push_str(&format!("- {d}\n"));
        }
    }

    // Gaps first: what a side cannot do is the primary output of this layer.
    out.push_str("\n## Capability gaps\n\n");
    let gaps: Vec<&ScenarioRow> = report
        .scenarios
        .iter()
        .filter(|r| r.tokenless_gap_reason.is_some())
        .collect();
    if gaps.is_empty() {
        out.push_str("None: tokenless has an entry point in every scenario.\n");
    } else {
        out.push_str(&format!(
            "tokenless has no applicable compressor in {} of {} scenarios. This is a \
             capability gap, not a compression failure: the payload never reaches a \
             compressor, so a 0% rate would misdescribe it.\n\n",
            gaps.len(),
            report.scenarios.len()
        ));
        out.push_str("| type | scenario | tokens | the reference (router) | reason |\n");
        out.push_str("|---|---|---|---|---|\n");
        for r in &gaps {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                r.content_type,
                r.scenario,
                r.tokens_before,
                pct(r.headroom_router_rate),
                r.tokenless_gap_reason.as_deref().unwrap_or("")
            ));
        }
    }

    out.push_str("\n## By content type\n\n");
    out.push_str(
        "`N` counts scenarios, not repetitions: the assets are static and both \
         compressors are deterministic, so the only uncertainty present is how \
         much the rate varies across payload shapes of a type.\n\n",
    );
    out.push_str("| type | scenarios | tl applies | tokenless mean (95% CI) | range | the reference mean (95% CI) | range | tl retention | hr retention |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for g in &report.groups {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            if g.capability_overlay {
                format!("{} [capability overlay]", g.content_type)
            } else {
                g.content_type.clone()
            },
            g.scenarios,
            g.tokenless_applicable,
            group_cell(g.tokenless.as_ref()),
            range_cell(g.tokenless.as_ref()),
            group_cell(g.headroom_router.as_ref()),
            range_cell(g.headroom_router.as_ref()),
            ratio(g.tokenless_retention),
            ratio(g.headroom_retention),
        ));
    }

    out.push_str("\n## Every scenario\n\n");
    out.push_str("| type | scenario | tokens | tl | tl keep | tl disposition | hr pure | hr router | hr keep | hr ccr | hr ms | strategies |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for r in &report.scenarios {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.content_type,
            r.scenario,
            r.tokens_before,
            pct(r.tokenless_rate),
            keep(r.tokenless_retention),
            disposition_cell(&r.tokenless_lifecycle),
            pct(r.headroom_pure_rate),
            pct(r.headroom_router_rate),
            keep(r.headroom_retention),
            r.headroom_ccr_entries
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
            r.headroom_ms
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "—".into()),
            r.headroom_strategies.join(",")
        ));
    }

    out.push_str("\n## Gate signals\n\n");
    if report.gate.is_empty() {
        out.push_str("None.\n");
    } else {
        out.push_str("| subject | side | kind | detail |\n|---|---|---|---|\n");
        for g in &report.gate {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                g.subject,
                g.side.as_deref().unwrap_or("—"),
                g.kind,
                g.detail
            ));
        }
    }

    out.push_str("\n## Latency\n\n");
    out.push_str(
        "Bases differ and are not cross-comparable: tokenless is timed in \
         process around production Runtime lifecycle calls (including domain detection and arbitration), the reference inside its worker around \
         `pipeline.apply`.\n\n",
    );
    out.push_str("| side | p50 ms | p95 ms | p99 ms |\n|---|---|---|---|\n");
    for (name, l) in [
        (
            "tokenless (in-process Runtime lifecycle)",
            s.tokenless_latency,
        ),
        ("the reference (worker-internal)", s.headroom_latency),
    ] {
        out.push_str(&format!("| {} | {} |\n", name, latency_cells(l)));
    }

    out
}

/// `disposition [recoverability] operations`, the per-row explanation of a rate.
fn disposition_cell(summary: &LifecycleSummary) -> String {
    if summary.dispositions.is_empty() {
        return "—".to_string();
    }
    let mut cell = summary.dispositions.join(",");
    if !summary.recoverability.is_empty() {
        cell.push_str(&format!(" [{}]", summary.recoverability.join(",")));
    }
    if !summary.operations.is_empty() {
        cell.push_str(&format!(" {}", summary.operations.join("+")));
    }
    cell
}

/// `mean [lo, hi]` for a group, or a dash.
fn group_cell(stats: Option<&stats::GroupStats>) -> String {
    stats
        .map(|s| {
            format!(
                "{:.1}% [{:.1}, {:.1}] (N={})",
                s.mean * 100.0,
                s.ci.0 * 100.0,
                s.ci.1 * 100.0,
                s.n
            )
        })
        .unwrap_or_else(|| "—".to_string())
}

/// Min-to-max of a group, which exposes a bimodal split a mean would hide.
fn range_cell(stats: Option<&stats::GroupStats>) -> String {
    stats
        .map(|s| format!("{:.1}–{:.1}%", s.range.0 * 100.0, s.range.1 * 100.0))
        .unwrap_or_else(|| "—".to_string())
}

/// `passed/total`, or a dash when nothing was checkable.
fn ratio(counts: (usize, usize)) -> String {
    if counts.1 == 0 {
        return "—".to_string();
    }
    format!(
        "{}/{} ({:.0}%)",
        counts.0,
        counts.1,
        100.0 * counts.0 as f64 / counts.1 as f64
    )
}

/// Three latency cells, or dashes.
fn latency_cells(l: Option<Latency>) -> String {
    l.map(|l| format!("{:.3} | {:.3} | {:.3}", l.p50, l.p95, l.p99))
        .unwrap_or_else(|| "— | — | —".to_string())
}

/// Render an optional flag without claiming a value that was not read.
fn opt<T: std::fmt::Display>(value: Option<T>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Assets live next to the crate, so the binary finds them without an env var.
fn assets_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// Report directory, overridable so a caller can keep artifacts elsewhere.
fn report_dir() -> PathBuf {
    std::env::var("L3_REPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("reports"))
}
