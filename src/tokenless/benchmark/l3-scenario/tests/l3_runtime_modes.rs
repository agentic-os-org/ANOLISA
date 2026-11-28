// Copyright 2026 Alibaba Cloud
// Licensed under the Apache License, Version 2.0.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn independent_runtime_modes_keep_comparison_and_semantic_unmeasured() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("l3-runtime-{}-{stamp}", std::process::id()));
    let _cleanup = TestDirectory(root.clone());
    let assets = tokenless_l3_bench::l3::asset::load_all(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"),
    )
    .unwrap();
    for recovery in [false, true] {
        let output = root.join(if recovery { "recovery" } else { "no-recovery" });
        let mut command = Command::new(env!("CARGO_BIN_EXE_l3_compare"));
        command
            .args(["--tokenless-only", "--no-probe", "--report-dir"])
            .arg(&output)
            .env("HEADROOM_PYTHON", "/fixture/comparator-must-not-be-started")
            .env_remove("DASHSCOPE_API_KEY");
        if recovery {
            command.arg("--recovery");
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(output.join("report.json")).unwrap())
                .unwrap();
        let evidence: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(output.join("lifecycle.json")).unwrap())
                .unwrap();
        assert!(report["summary"]["headroom"].is_null());
        assert!(report["summary"]["probe_model"].is_null());
        assert_eq!(evidence["comparison_status"], "not_executed");
        assert!(evidence["billing_tokens"].is_null());
        let rows = report["scenarios"].as_array().unwrap();
        assert_eq!(rows.len(), assets.len());
        assert_eq!(
            report["summary"]["tokenless_mode"],
            if recovery { "recovery" } else { "no-recovery" }
        );
        assert_eq!(report["summary"]["comparison_status"], "not_executed");
        assert_eq!(
            rows.iter()
                .filter(|row| row["capability_overlay"] == true)
                .count(),
            assets
                .iter()
                .filter(|asset| !asset.source.headroom_native)
                .count()
        );
        for row in rows {
            assert_eq!(row["capability_overlay"], row["headroom_native"] == false);
        }
        for row in rows.iter().filter(|row| row["capability_overlay"] == true) {
            assert_eq!(row["headroom_native"], false);
            let counts = row["tokenless_retention"].as_array().unwrap();
            assert!(counts[1].as_u64().unwrap() > 0);
            assert_eq!(
                counts[0], counts[1],
                "overlay lost facts: {}",
                row["scenario"]
            );
        }
        for row in evidence["rows"].as_array().unwrap() {
            assert_eq!(row["side"], "tokenless");
            assert!(row["input"]["messages"].as_array().is_some());
            assert!(row["output"]["messages"].as_array().is_some());
            assert!(row["input"].get("tools").is_some());
            assert!(row["output"].get("tools").is_some());
        }
        assert_eq!(
            evidence["tokenless_mode"],
            if recovery { "recovery" } else { "no-recovery" }
        );
        // Every exported Stash entry must be a reference the emitted conversation
        // carries, with the bytes a retrieve returns; no-recovery emits none.
        let mut exported = 0;
        for row in evidence["rows"].as_array().unwrap() {
            let emitted = row["output"].to_string();
            let mut sources: Vec<String> = row["input"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|message| message["role"] == "tool")
                .filter_map(|message| message["content"].as_str().map(str::to_owned))
                .collect();
            sources.extend(
                row["input"]["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|tool| tool.to_string()),
            );
            for entry in row["stash"].as_array().unwrap() {
                let key = entry["key"].as_str().unwrap();
                let text = entry["original"].as_str().unwrap();
                assert!(emitted.contains(key), "{} drops {key}", row["scenario"]);
                assert!(!text.is_empty());
                assert!(
                    sources
                        .iter()
                        .any(|source| stash_restores_source(text, source)),
                    "{} stash original is not source content",
                    row["scenario"]
                );
                exported += 1;
            }
        }
        if recovery {
            assert!(exported > 0, "recovery mode exported no Stash entries");
        } else {
            assert_eq!(exported, 0);
        }
        for row in rows {
            let summary = &row["tokenless_lifecycle"];
            assert!(summary["dispositions"].as_array().is_some());
            if !recovery {
                assert!(
                    !summary["recoverability"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == "retrievable"),
                    "{} offloads without a store",
                    row["scenario"]
                );
            }
        }
        let markdown = std::fs::read_to_string(output.join("TOKENLESS_L3_REPORT.md")).unwrap();
        assert!(markdown.contains("Headroom comparison: not_executed"));
        assert!(markdown.contains(if recovery {
            "Tokenless mode: recovery"
        } else {
            "Tokenless mode: no-recovery"
        }));
        assert!(output.join("TOKENLESS_L3_REPORT.md").exists());
        assert!(!output.join("L3_SCENARIO_COMPARISON_REPORT.md").exists());
    }
}

struct TestDirectory(std::path::PathBuf);
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether a Stash original restores source content: a verbatim span of the
/// tool text, or (for JSON, which the Runtime stores compactly) a value equal to
/// the parsed tool payload or to one of its nested values.
fn stash_restores_source(original: &str, source: &str) -> bool {
    if source.contains(original) {
        return true;
    }
    let (Ok(stored), Ok(parsed)) = (
        serde_json::from_str::<serde_json::Value>(original),
        serde_json::from_str::<serde_json::Value>(source),
    ) else {
        return false;
    };
    fn contains(haystack: &serde_json::Value, needle: &serde_json::Value) -> bool {
        haystack == needle
            || match haystack {
                serde_json::Value::Object(map) => map.values().any(|v| contains(v, needle)),
                serde_json::Value::Array(items) => items.iter().any(|v| contains(v, needle)),
                _ => false,
            }
    }
    contains(&parsed, &stored)
}
