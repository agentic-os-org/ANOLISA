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

fn main() -> Result<(), tokenless_bench::domains::DomainError> {
    let report = full_report()?;

    if std::env::args().any(|a| a == "--json") {
        // Pretty JSON is fine here: parsers ignore whitespace and humans can
        // diff it directly across runs.
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("report is valid JSON")
        );
        return Ok(());
    }

    println!("=== Tokenless component domains (o200k_base; offline, not billing) ===\n");
    if let Some(rows) = report["component_domains"]["rows"].as_array() {
        for row in rows {
            println!(
                "  {} [{}]: {} -> {} tokens; {} ms; checks={}",
                row["sample_id"],
                row["mode"],
                row["tokens_before"],
                row["tokens_after"],
                row["latency_ms"],
                row["checks"]
            );
        }
    }
    println!("\n=== Legacy byte estimates (auxiliary only) ===\n");

    let resp = &report["legacy_estimates"]["canonical"]["response"];
    let schema = &report["legacy_estimates"]["canonical"]["schema"];
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
        report["legacy_estimates"]["stacking"]["baseline_tokens"]
    );
    if let Some(configs) = report["legacy_estimates"]["stacking"]["configs"].as_array() {
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

    println!(
        "\n[cost] not measured; actual provider usage and deployment-region prices are required."
    );
    Ok(())
}
