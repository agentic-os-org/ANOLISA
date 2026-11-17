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

//! Auxiliary canonical byte estimates and RTK discovery for the L1 report.
//!
//! The headline comes from `domains` on o200k_base; canonical bytes/4 estimates
//! remain explicitly under legacy_estimates. No pricing or billing projection
//! is inferred from these component measurements.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use tokenless_schema::SchemaCompressor;

use crate::{compress_json, response_canonical, rtk_command_samples, schema_canonical};

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

/// Locate the `tokenless` whose `rtk` subcommand runs RTK, the form PreTool
/// rewrites commands to: `$TOKENLESS_BIN` when set, else the release build of
/// the enclosing Tokenless workspace, then `tokenless` on `PATH`. Shared by the
/// in-crate reports and the `l1_rtk_format_compat` integration tests so
/// discovery never drifts between the two. Returns `None` when none is
/// runnable.
///
/// `l2-module/src/metrics.rs` carries a byte-identical copy of this function,
/// [`rtk_command`] and `is_runnable` — the two benchmark workspaces are
/// independent, so changing the discovery order here means changing it there
/// too.
pub fn find_tokenless_binary() -> Option<PathBuf> {
    // An explicit choice is final: falling back would measure another build.
    if let Some(program) = std::env::var_os("TOKENLESS_BIN").filter(|p| !p.is_empty()) {
        let program = PathBuf::from(program);
        return is_runnable(&program).then_some(program);
    }
    [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/release/tokenless"),
        PathBuf::from("tokenless"),
    ]
    .into_iter()
    .find(|program| is_runnable(program))
}

/// Starts `<tokenless> rtk`; callers append RTK's own arguments.
pub fn rtk_command(tokenless: &Path) -> Command {
    let mut command = Command::new(tokenless);
    command.arg("rtk");
    command
}

/// A path must be executable; a bare name must answer `rtk --version` from
/// `PATH`, which also rejects a `tokenless` older than `tokenless rtk`.
fn is_runnable(program: &Path) -> bool {
    if program.components().count() > 1 {
        return is_executable(program);
    }
    rtk_command(program)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
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

/// Runs `<tokenless> rtk rewrite <cmd>` with a hard per-invocation timeout.
///
/// Waits on a helper thread and SIGKILLs the child when the deadline
/// elapses, so a hung rtk can never block the caller (report loop or
/// criterion bench) indefinitely. Shared by [`rtk_report`] and
/// `benches/l1_rtk_latency.rs` so both paths get identical hang protection.
pub fn run_rewrite_with_timeout(tokenless: &Path, cmd: &str, timeout: Duration) -> TimedRun {
    let child = match rtk_command(tokenless)
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
/// Shells out to `tokenless rtk rewrite <cmd>` for each sample and counts
/// tokens on the command text before and after the rewrite (same
/// `estimate_tokens` heuristic as the in-process compressors). Exit-code
/// protocol: 0 = rewrite available (allow), 1 = no equivalent, 2 = deny, 3 =
/// rewrite available (ask). Codes 0 AND 3 both carry the rewritten command on
/// stdout, so both count toward the savings figures; every code is also
/// tallied per category. When tokenless is unavailable the report is
/// `{"available": false}` — never an error — so the suite stays runnable on
/// machines without tokenless.
///
/// Note the methodology difference from the in-process compressors: rtk is a
/// cross-process rewrite of shell COMMANDS (not payloads), so its rate is not
/// stackable with the response/schema/TOON numbers. The rewrite typically
/// routes the command through rtk's own output-filtering front-end, so the
/// command TEXT may grow slightly; rtk's token savings materialize in the
/// filtered runtime OUTPUT, which this in-suite metric does not execute.
pub fn rtk_report() -> Value {
    let Some(tokenless) = find_tokenless_binary() else {
        return json!({ "available": false });
    };

    // Best-effort version string for the report header.
    let version = rtk_command(&tokenless)
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
        let out = match run_rewrite_with_timeout(&tokenless, cmd, Duration::from_secs(5)) {
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
        "binary": tokenless.display().to_string(),
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

/// Auxiliary bytes/4 estimates over the shared canonical fixtures.
///
/// Computes isolated stage and stacking estimates without measuring provider
/// usage or projecting price. The cost block explicitly records not_measured.
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
            "status": "not_measured", "models": Value::Null,
            "reason": "requires actual provider usage, deployment-region pricing and cache accounting",
        },
    })
}

/// Full report combining compression metrics and RTK command-rewrite data.
///
/// Used by the `compression_rate` binary for the complete human/machine report.
/// Calls [`compression_metrics`], [`crate::domains::domain_metrics`] and [`rtk_report`].
///
/// # Errors
/// Returns tokenizer or component-fixture failures rather than fabricated zeroes.
pub fn full_report() -> Result<Value, crate::domains::DomainError> {
    let estimates = compression_metrics();
    let domains = crate::domains::domain_metrics()?;
    let mut rtk = rtk_report();
    let mut raw_total = 0usize;
    let mut rewritten_total = 0usize;
    let mut rewritten = 0usize;
    if let Some(samples) = rtk["samples"].as_array_mut() {
        for sample in samples {
            let Some(command) = sample["command"].as_str() else {
                continue;
            };
            let before = crate::domains::count_text(command)?;
            let after = sample["rewritten"]
                .as_str()
                .map(crate::domains::count_text)
                .transpose()?;
            sample["engine_estimate_before"] = sample["raw_tokens"].clone();
            sample["raw_tokens"] = json!(before);
            sample["tokenizer"] = json!("o200k_base");
            if let Some(after) = after {
                sample["engine_estimate_after"] = sample["rewritten_tokens"].clone();
                sample["rewritten_tokens"] = json!(after);
                sample["savings_pct"] = json!(savings_pct(before, after));
                raw_total += before;
                rewritten_total += after;
                rewritten += 1;
            }
        }
        rtk["overall"] = if rewritten > 0 {
            json!({"raw_tokens": raw_total, "rewritten_tokens": rewritten_total,
                "savings_pct": savings_pct(raw_total, rewritten_total), "tokenizer": "o200k_base"})
        } else {
            Value::Null
        };
    }
    Ok(json!({
        "schema_version": 2,
        "headline_tokenizer": "o200k_base",
        "component_domains": domains,
        "legacy_estimates": {"tokenizer": "bytes/4 estimate; not headline or billing",
            "canonical": estimates["canonical"], "stacking": estimates["stacking"]},
        "cost_analysis": {"status": "not_measured", "models": Value::Null,
            "reason": "requires actual provider usage, deployment-region pricing and cache accounting"},
        "rtk": rtk,
    }))
}
