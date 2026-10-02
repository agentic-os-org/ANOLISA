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

//! In-process compression-rate measurement and cost analysis.
//!
//! Token counts are taken immediately before and after each compressor call —
//! the same bytes/4 heuristic the engine's own estimator uses — so the savings
//! numbers are attributable to the library code itself, with no CLI subprocess
//! in the loop. The rates regress alongside `cargo test` (see
//! `tests/l1_compression_rate.rs`), which makes any compression-behaviour change
//! traceable to the exact commit that introduced it.
//!
//! Cost analysis (formerly a Python post-processing script) is computed
//! in-process here so the full report — compression rates, stacking configs,
//! and dollar savings — is emitted by a single `compression_rate` binary.
//!
//! **Cost-analysis limitations**: token counts use a bytes/4 heuristic (not a
//! real tokenizer), projections extrapolate linearly from a single canonical
//! fixture, and model pricing is point-in-time. Treat the figures as
//! order-of-magnitude guidance, not billing-grade predictions.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use tokenless_schema::SchemaCompressor;

use crate::{compress_json, response_canonical, rtk_command_samples, schema_canonical};

// Cost-analysis limitations:
// 1. Token counting: bytes/4 heuristic (div_ceil), not a real tokenizer
// 2. Payload: single canonical fixture × linear scaling (no workload diversity)
// 3. Pricing: point-in-time snapshot, subject to provider changes
// 4. Sessions: uniform 50-round model; real sessions vary significantly
// Treat output as order-of-magnitude guidance only.

/// Sessions assumed for the cost-analysis projection.
const ROUNDS_PER_SESSION: usize = 50;
const SESSIONS_PER_DAY: usize = 1000;
const DAYS_PER_MONTH: usize = 30;

/// Model pricing table: (name, USD per 1M input tokens).
const MODELS: &[(&str, f64)] = &[
    ("Claude Sonnet 4", 3.00),
    ("GPT-4o", 2.50),
    ("Gemini 2.5 Pro", 1.25),
];

/// Rough token estimate: div_ceil(len, 4) — ceiling division by 4.
///
/// Mirrors tokenless's own `estimate_tokens_from_bytes` heuristic so the
/// Rust-side numbers line up with what the engine records.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Checks that a path points to a regular file with at least one execute bit set.
fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Locate the rtk binary: `$RTK_BIN`, the vendored release build next to this
/// crate, or `rtk` on `PATH`. Shared by the in-crate reports and the
/// `l1_rtk_format_compat` integration tests so discovery never drifts between
/// the two. Returns `None` when none is runnable.
///
/// `l2-module/src/metrics.rs` carries a byte-identical copy — the two benchmark
/// workspaces are independent, so changing the discovery order here means
/// changing it there too.
pub fn find_rtk_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RTK_BIN") {
        let pb = PathBuf::from(p);
        if is_executable(&pb) {
            return Some(pb);
        }
    }
    let vendored =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/rtk/target/release/rtk");
    if is_executable(&vendored) {
        return Some(vendored);
    }
    // Fall back to PATH: only accept it if `--version` actually runs.
    if Command::new("rtk").arg("--version").output().is_ok() {
        return Some(PathBuf::from("rtk"));
    }
    None
}

/// Outcome of running an rtk rewrite under a hard wall-clock timeout.
pub enum TimedRun {
    /// The child exited within the deadline.
    Completed(std::process::Output),
    /// The child could not be spawned at all.
    SpawnFailed,
    /// The child was reaped but collecting its output failed — an I/O error
    /// while waiting, not a hang; there is nothing left to kill.
    WaitError,
    /// Deadline elapsed; the child was SIGKILLed and confirmed reaped.
    TimedOutKilled,
    /// Deadline elapsed and the child could not be confirmed dead within a
    /// grace second after SIGKILL.
    TimedOutKillFailed,
}

/// Runs `<bin> rewrite <cmd>` with a hard per-invocation timeout.
///
/// Waits on a helper thread and SIGKILLs the child when the deadline
/// elapses, so a hung rtk can never block the caller (report loop or
/// criterion bench) indefinitely. Shared by [`rtk_report`] and
/// `benches/l1_rtk_latency.rs` so both paths get identical hang protection.
pub fn run_rewrite_with_timeout(bin: &Path, cmd: &str, timeout: Duration) -> TimedRun {
    let child = match Command::new(bin)
        .arg("rewrite")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return TimedRun::SpawnFailed,
    };
    let child_id = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => TimedRun::Completed(output),
        // Distinct from a timeout: the process already terminated, only the
        // reap failed, so killing would target a dead (or recycled) pid.
        Ok(Err(_)) => TimedRun::WaitError,
        Err(_) => {
            // Kill the orphaned process to avoid leaking children.
            #[cfg(unix)]
            unsafe {
                libc::kill(child_id as i32, libc::SIGKILL);
            }
            #[cfg(not(unix))]
            {
                let _ = child_id; // best-effort on non-unix
            }
            if rx.recv_timeout(Duration::from_secs(1)).is_ok() {
                TimedRun::TimedOutKilled
            } else {
                TimedRun::TimedOutKillFailed
            }
        }
    }
}

/// RTK command-rewrite compression report over [`rtk_command_samples`].
///
/// Shells out to `rtk rewrite <cmd>` for each sample and counts tokens on the
/// command text before and after the rewrite (same `estimate_tokens`
/// heuristic as the in-process compressors). Exit-code protocol: 0 = rewrite
/// available (allow), 1 = no equivalent, 2 = deny, 3 = rewrite available
/// (ask). Codes 0 AND 3 both carry the rewritten command on stdout, so both
/// count toward the savings figures; every code is also tallied per category.
/// When the rtk binary is unavailable the report is `{"available": false}` —
/// never an error — so the suite stays runnable on machines without rtk.
///
/// Note the methodology difference from the in-process compressors: rtk is a
/// cross-process rewrite of shell COMMANDS (not payloads), so its rate is not
/// stackable with the response/schema/TOON numbers. The rewrite typically
/// routes the command through rtk's own output-filtering front-end, so the
/// command TEXT may grow slightly; rtk's token savings materialize in the
/// filtered runtime OUTPUT, which this in-suite metric does not execute.
pub fn rtk_report() -> Value {
    let Some(bin) = find_rtk_binary() else {
        return json!({ "available": false });
    };

    // Best-effort version string for the report header.
    let version = Command::new(&bin)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let mut samples = Vec::new();
    let mut rewritten = 0usize;
    let mut allow = 0usize;
    let mut passthrough = 0usize;
    let mut denied = 0usize;
    let mut ask = 0usize;
    let mut other = 0usize;
    let mut raw_total = 0usize;
    let mut rewritten_total = 0usize;

    for (label, cmd) in rtk_command_samples() {
        let out = match run_rewrite_with_timeout(&bin, cmd, Duration::from_secs(5)) {
            TimedRun::Completed(output) => output,
            TimedRun::SpawnFailed => {
                other += 1;
                samples.push(
                    json!({ "label": label, "exit_code": Value::Null, "status": "spawn_failure" }),
                );
                continue;
            }
            TimedRun::WaitError => {
                other += 1;
                samples.push(
                    json!({ "label": label, "exit_code": Value::Null, "status": "wait_error" }),
                );
                continue;
            }
            TimedRun::TimedOutKilled => {
                other += 1;
                samples.push(
                    json!({ "label": label, "exit_code": Value::Null, "status": "timeout_killed" }),
                );
                continue;
            }
            TimedRun::TimedOutKillFailed => {
                other += 1;
                samples.push(json!({
                    "label": label, "exit_code": Value::Null, "status": "timeout_kill_failed"
                }));
                continue;
            }
        };
        let code = out.status.code().unwrap_or(-1);
        let raw_tok = estimate_tokens(cmd);
        let mut row = json!({
            "label": label,
            "command": cmd,
            "exit_code": code,
            "raw_tokens": raw_tok,
        });
        match code {
            0 | 3 => {
                // Rewrite available (0 = allow, 3 = ask): stdout carries the
                // rewritten command either way.
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let new_tok = estimate_tokens(&stdout);
                rewritten += 1;
                raw_total += raw_tok;
                rewritten_total += new_tok;
                row["rewritten"] = json!(stdout);
                row["rewritten_tokens"] = json!(new_tok);
                row["savings_pct"] = json!(savings_pct(raw_tok, new_tok));
                if code == 3 {
                    ask += 1;
                } else {
                    allow += 1;
                }
            }
            1 => passthrough += 1,
            2 => denied += 1,
            _ => other += 1,
        }
        samples.push(row);
    }

    let overall = if rewritten > 0 {
        json!({
            "raw_tokens": raw_total,
            "rewritten_tokens": rewritten_total,
            "savings_pct": savings_pct(raw_total, rewritten_total),
        })
    } else {
        Value::Null
    };

    json!({
        "available": true,
        "binary": bin.display().to_string(),
        "version": version,
        "samples": samples,
        "exit_codes": {
            "rewrite_available": rewritten,
            "allow": allow,
            "ask": ask,
            "passthrough": passthrough,
            "deny": denied,
            "other": other,
        },
        "overall": overall,
    })
}

/// Percentage saved vs `baseline`, rounded to one decimal place.
fn savings_pct(baseline: usize, tokens: usize) -> f64 {
    ((1.0 - tokens as f64 / baseline as f64) * 1000.0).round() / 10.0
}

/// Wire form of a value (compact JSON) — what token counts are taken on.
fn wire(value: &Value) -> String {
    // A `Value` always has string keys, so serialization cannot fail.
    serde_json::to_string(value).expect("serde_json::Value serializes infallibly")
}

/// TOON-encode a value from the canonical fixtures.
fn toon(value: &Value) -> String {
    // Canonical fixtures are known-encodable; a failure here means fixture
    // corruption and should surface loudly.
    toon_format::encode_default(value).expect("canonical value is TOON-encodable")
}

/// Pure compression-rate and cost-analysis metrics over the canonical fixtures.
///
/// Runs `JsonCompressor`, `SchemaCompressor`, and TOON encoding in-process
/// on the SAME canonical payloads the latency benches measure, counting tokens
/// immediately before and after each stage. Also derives the 7 stacking
/// configurations from the V5 report and projects dollar savings over a
/// multi-round agent session.
///
/// Does NOT invoke RTK (no subprocess), making it safe for `cargo test`
/// environments without rtk installed.
pub fn compression_metrics() -> Value {
    let response = response_canonical();
    let schema = schema_canonical();

    let schema_compressor = SchemaCompressor::new();

    // Wire-form strings for byte counting.
    let resp_raw_wire = wire(&response);
    let schema_raw_wire = wire(&schema);

    // Tokens before the compressors.
    let resp_raw_tok = estimate_tokens(&resp_raw_wire);
    let schema_raw_tok = estimate_tokens(&schema_raw_wire);

    // Tokens after each stage.
    let resp_compressed = compress_json(&response);
    let schema_compressed = schema_compressor.compress(&schema);
    let resp_comp_wire = wire(&resp_compressed);
    let schema_comp_wire = wire(&schema_compressed);
    let resp_comp_tok = estimate_tokens(&resp_comp_wire);
    let schema_comp_tok = estimate_tokens(&schema_comp_wire);
    let resp_comp_toon = toon(&resp_compressed);
    let schema_comp_toon = toon(&schema_compressed);
    let resp_comp_toon_tok = estimate_tokens(&resp_comp_toon);
    let schema_comp_toon_tok = estimate_tokens(&schema_comp_toon);

    // TOON-only tokens (no compressor, just TOON encode of the raw payload).
    let resp_toon_only = toon(&response);
    let schema_toon_only = toon(&schema);
    let resp_toon_only_tok = estimate_tokens(&resp_toon_only);
    let schema_toon_only_tok = estimate_tokens(&schema_toon_only);

    // Stacking configurations — 7 combinations measuring cumulative token savings.
    // Each config name has a fixed definition:
    //   baseline:        raw JSON, no compression (reference point)
    //   response_only:   JsonCompressor only (no TOON, no schema compression)
    //   toon_only:       TOON-encode raw payloads directly (no compressor stage)
    //   schema_only:     SchemaCompressor only (no TOON, no response compression)
    //   response_toon:   JsonCompressor + TOON on response; schema raw
    //   schema_response: JsonCompressor + SchemaCompressor (no TOON)
    //   full_stack:      JsonCompressor + SchemaCompressor + TOON on both
    //
    // NOTE: "toon_only" here encodes the RAW input. In benches/l1_pipeline_latency.rs,
    // "toon_encode_on_compressed" encodes the COMPRESSED output. These are different
    // inputs producing different results — do not conflate.
    let baseline = resp_raw_tok + schema_raw_tok;
    let configs = [
        ("baseline", baseline),
        ("response_only", resp_comp_tok + schema_raw_tok),
        ("toon_only", resp_toon_only_tok + schema_toon_only_tok),
        ("schema_only", resp_raw_tok + schema_comp_tok),
        ("response_toon", resp_comp_toon_tok + schema_raw_tok),
        ("schema_response", resp_comp_tok + schema_comp_tok),
        ("full_stack", resp_comp_toon_tok + schema_comp_toon_tok),
    ];

    // Cost analysis: project session-level token savings to monthly USD.
    // NOTE: This is a rough heuristic estimate based on:
    //   - bytes/4 token counting (not a real tokenizer),
    //   - a single canonical fixture linearly scaled to sessions/month,
    //   - model pricing as of the last update (subject to change).
    // These numbers illustrate order-of-magnitude savings, not precise billing.
    let cost_baseline = (resp_raw_tok + schema_raw_tok) * ROUNDS_PER_SESSION;
    // Uses schema_response strategy (best non-inflating combination).
    // CLI token-gate rejects TOON when it inflates, so the actual deployed
    // cost equals compressed-only for this canonical payload shape.
    let cost_tokenless = (resp_comp_tok + schema_comp_tok) * ROUNDS_PER_SESSION;
    // Savings percentage: schema_response vs baseline (no TOON inflation risk).
    let cost_savings = savings_pct(cost_baseline, cost_tokenless);
    let models = MODELS
        .iter()
        .map(|(name, price)| {
            let monthly_base =
                cost_baseline as f64 * SESSIONS_PER_DAY as f64 * DAYS_PER_MONTH as f64;
            let monthly_tl =
                cost_tokenless as f64 * SESSIONS_PER_DAY as f64 * DAYS_PER_MONTH as f64;
            let base_usd = monthly_base / 1_000_000.0 * price;
            let tl_usd = monthly_tl / 1_000_000.0 * price;
            json!({
                "model": name,
                "input_per_mtok": price,
                "baseline_monthly_usd": (base_usd * 100.0).round() / 100.0,
                "tokenless_monthly_usd": (tl_usd * 100.0).round() / 100.0,
                "monthly_savings_usd": ((base_usd - tl_usd) * 100.0).round() / 100.0,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "canonical": {
            "response": {
                "raw_tokens": resp_raw_tok,
                "raw_bytes": resp_raw_wire.len(),
                "compressed_tokens": resp_comp_tok,
                "compressed_bytes": resp_comp_wire.len(),
                "compressed_toon_tokens": resp_comp_toon_tok,
                "toon_bytes": resp_comp_toon.len(),
                "toon_only_tokens": resp_toon_only_tok,
                "savings_pct": savings_pct(resp_raw_tok, resp_comp_tok),
                "savings_with_toon_pct": savings_pct(resp_raw_tok, resp_comp_toon_tok),
                "toon_only_savings_pct": savings_pct(resp_raw_tok, resp_toon_only_tok),
            },
            "schema": {
                "raw_tokens": schema_raw_tok,
                "raw_bytes": schema_raw_wire.len(),
                "compressed_tokens": schema_comp_tok,
                "compressed_bytes": schema_comp_wire.len(),
                "compressed_toon_tokens": schema_comp_toon_tok,
                "toon_bytes": schema_comp_toon.len(),
                "toon_only_tokens": schema_toon_only_tok,
                "savings_pct": savings_pct(schema_raw_tok, schema_comp_tok),
                "savings_with_toon_pct": savings_pct(schema_raw_tok, schema_comp_toon_tok),
                "toon_only_savings_pct": savings_pct(schema_raw_tok, schema_toon_only_tok),
            }
        },
        "stacking": {
            "baseline_tokens": baseline,
            "configs": configs
                .iter()
                .map(|(name, tokens)| json!({
                    "config": name,
                    "tokens": tokens,
                    "savings_pct": savings_pct(baseline, *tokens),
                }))
                .collect::<Vec<_>>(),
        },
        "cost_analysis": {
            "assumptions": {
                "rounds_per_session": ROUNDS_PER_SESSION,
                "sessions_per_day": SESSIONS_PER_DAY,
                "days_per_month": DAYS_PER_MONTH,
            },
            "baseline_tokens": cost_baseline,
            "tokenless_tokens": cost_tokenless,
            "token_savings_pct": cost_savings,
            "models": models,
        },
    })
}

/// Full report combining compression metrics and RTK command-rewrite data.
///
/// Used by the `compression_rate` binary for the complete human/machine report.
/// Calls [`compression_metrics`] + [`rtk_report`].
pub fn full_report() -> Value {
    let mut report = compression_metrics();
    report["rtk"] = rtk_report();
    report
}
