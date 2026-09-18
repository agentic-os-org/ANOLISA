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

//! Prints the in-process compression-rate and cost-analysis report over the
//! canonical fixtures.
//!
//! Token counts are taken around the real compressor calls inside this
//! process — no CLI subprocess — so the rates are attributable to the library
//! code and can be regenerated from any commit for tracking. `--json` emits
//! the machine-readable form; the default mode prints a human-readable summary
//! covering compression rates, stacking configs, and projected dollar savings.

use tokenless_bench::metrics::full_report;

fn main() {
    let report = full_report();

    if std::env::args().any(|a| a == "--json") {
        // Pretty JSON is fine here: parsers ignore whitespace and humans can
        // diff it directly across runs.
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("report is valid JSON")
        );
        return;
    }

    println!("=== Tokenless Compression Rate (in-process, canonical fixtures) ===\n");

    let resp = &report["canonical"]["response"];
    let schema = &report["canonical"]["schema"];
    println!("[canonical response]");
    println!(
        "  raw {} -> compressed {} tok ({}% saved), +TOON {} tok ({}% saved)",
        resp["raw_tokens"],
        resp["compressed_tokens"],
        resp["savings_pct"],
        resp["compressed_toon_tokens"],
        resp["savings_with_toon_pct"],
    );
    println!("[canonical schema]");
    println!(
        "  raw {} -> compressed {} tok ({}% saved), +TOON {} tok ({}% saved)\n",
        schema["raw_tokens"],
        schema["compressed_tokens"],
        schema["savings_pct"],
        schema["compressed_toon_tokens"],
        schema["savings_with_toon_pct"],
    );

    println!(
        "[stacking] baseline {} tokens",
        report["stacking"]["baseline_tokens"]
    );
    if let Some(configs) = report["stacking"]["configs"].as_array() {
        for row in configs {
            println!(
                "  {:<18} {:>7} tok {:>6}% saved",
                row["config"].as_str().unwrap_or("?"),
                row["tokens"],
                row["savings_pct"],
            );
        }
    }

    // RTK command-rewrite section: cross-process, commands not payloads, so
    // shown separately from the stacking configs.
    let rtk = &report["rtk"];
    if rtk["available"].as_bool() == Some(true) {
        println!("\n[rtk rewrite] {}", rtk["version"].as_str().unwrap_or("?"));
        if let Some(samples) = rtk["samples"].as_array() {
            for s in samples {
                let label = s["label"].as_str().unwrap_or("?");
                match s["exit_code"].as_i64() {
                    // 0 (allow) and 3 (ask) both carry a rewrite.
                    Some(0) | Some(3) => println!(
                        "  {:<15} {:>4} -> {:>4} tok {:>6}% saved",
                        label, s["raw_tokens"], s["rewritten_tokens"], s["savings_pct"],
                    ),
                    Some(code) => println!("  {label:<15} exit {code} (no rewrite counted)"),
                    None => println!("  {label:<15} failed to run"),
                }
            }
        }
        if rtk["overall"].is_object() {
            println!(
                "  overall (rewrite-available samples): {} -> {} tok ({}% saved)",
                rtk["overall"]["raw_tokens"],
                rtk["overall"]["rewritten_tokens"],
                rtk["overall"]["savings_pct"],
            );
        }
    } else {
        println!("\nRTK: not available in this environment");
    }

    // Cost analysis section.
    // Caveat: estimates use bytes/4 heuristic token counting, a single canonical
    // fixture, and linear session-count projection. Real-world savings depend on
    // actual payload distributions, tokenizer specifics, and session patterns.
    let cost = &report["cost_analysis"];
    let assumptions = &cost["assumptions"];
    println!(
        "\n[cost analysis] (heuristic estimate: bytes/4 tokens, single canonical fixture, linear projection)"
    );
    println!("  note: order-of-magnitude guidance only; not a billing-grade prediction.");
    println!(
        "  assumptions: {}-round session, {} sessions/day, {} days/month",
        assumptions["rounds_per_session"],
        assumptions["sessions_per_day"],
        assumptions["days_per_month"],
    );
    println!(
        "  baseline: {} tok/session | tokenless: {} tok/session ({}% saved)",
        cost["baseline_tokens"], cost["tokenless_tokens"], cost["token_savings_pct"],
    );
    println!(
        "\n  {:<18} {:>14} {:>14} {:>12}",
        "Model", "Baseline/mo", "Tokenless/mo", "Saved/mo"
    );
    if let Some(models) = cost["models"].as_array() {
        for m in models {
            println!(
                "  {:<18} ${:>13.2} ${:>13.2} ${:>10.2}",
                m["model"].as_str().unwrap_or("?"),
                m["baseline_monthly_usd"].as_f64().unwrap_or(0.0),
                m["tokenless_monthly_usd"].as_f64().unwrap_or(0.0),
                m["monthly_savings_usd"].as_f64().unwrap_or(0.0),
            );
        }
    }
}
