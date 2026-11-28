// Copyright 2026 Alibaba Cloud
// Licensed under the Apache License, Version 2.0.

use std::process::Command;

#[test]
fn tokenless_only_does_not_spawn_comparator_or_claim_semantic_success() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path();
    let result = Command::new(env!("CARGO_BIN_EXE_l2_compare"))
        .args([
            "--tokenless-only",
            "--categories",
            "json,buildlog,tabular,search,html",
            "--model",
            "review-offline-model",
            "--n",
            "5",
            "--report-dir",
        ])
        .arg(output)
        .env("HEADROOM_PYTHON", "/fixture/comparator-must-not-be-spawned")
        .env("DASHSCOPE_API_KEY", "review-offline-invalid-key")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(output.join("report.json")).unwrap())
            .unwrap();
    assert_eq!(report["report_kind"], "tokenless-only");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(
        report["summary"]["resolved_entry_options"]["diff_compression_enabled"],
        true
    );
    assert_eq!(report["comparison_status"], "not_executed");
    assert_eq!(report["coverage"]["semantic"], "not_executed");
    assert_eq!(report["coverage"]["runtime_arbitration"], "measured");
    assert_eq!(report["coverage"]["host_retrieval_tool"], "not_measured");
    assert_eq!(report["recovery_storage"], "in_memory");
    assert_eq!(report["component_modes"]["runtime_mode"], "default");
    assert_eq!(report["summary"]["headroom_available"], false);
    assert!(
        report["summary"]["degradations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| !item.as_str().unwrap().contains("headroom"))
    );
    assert_eq!(report["categories"].as_array().unwrap().len(), 5);
    for category in report["categories"].as_array().unwrap() {
        assert!(category["headroom"].is_null());
        assert!(category["compression_gap"].is_null());
        assert!(category["tokenless"]["semantic_score"].is_null());
        assert_eq!(category["tokenless"]["reps"], 5);
    }
    let evidence: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(output.join("measurements.json")).unwrap())
            .unwrap();
    assert_eq!(evidence["hash_algorithm"], "sha256");
    let rows = evidence["rows"].as_array().unwrap();
    assert!(!rows.is_empty());
    assert_eq!(evidence["schema_version"], 2);
    assert!(evidence["text_store"].as_object().unwrap().len() < rows.len());
    for row in rows {
        assert_eq!(row["side"], "tokenless");
        // The tokenless side is the production entry, so its final acceptance
        // is what every row measures.
        assert_eq!(row["runtime_final_acceptance_measured"], true);
        assert_eq!(row["mode"], "default");
        assert_eq!(row["entry_kind"], "production PostTool");
        assert!(row["row"]["entry"]["post_tool"]["disposition"].is_string());
        assert!(row["row"]["entry"]["post_tool"]["recoverability"].is_string());
        assert!(row["billing_tokens"].is_null());
        assert_eq!(row["row"]["payload_hash"].as_str().unwrap().len(), 64);
        assert_eq!(row["row"]["output_hash"].as_str().unwrap().len(), 64);
        let input_ref = row["row"]["input_ref"].as_str().unwrap();
        let output_ref = row["row"]["output_ref"].as_str().unwrap();
        assert!(evidence["text_store"][input_ref].is_string());
        assert!(evidence["text_store"][output_ref].is_string());
        assert!(row["row"]["ground_truth"].as_array().is_some());
        assert!(row["row"]["tokens_before"].as_u64().unwrap() > 0);
        assert!(row["row"]["retention_total"].as_u64().unwrap() > 0);
        if row["category"] == "html" {
            assert_eq!(row["row"]["entry"]["content_origin"], "api_response");
        }
    }
    assert!(output.join("TOKENLESS_L2_REPORT_zh.md").exists());
    assert!(!output.join("L2_MODULE_COMPARISON_REPORT.md").exists());
    let markdown = std::fs::read_to_string(output.join("TOKENLESS_L2_REPORT_zh.md")).unwrap();
    assert!(markdown.contains("生产分派、最终仲裁与恢复门禁参与测量"));
    assert!(!markdown.contains("Runtime 最终仲裁不在本层计时中"));
    assert!(directory.path().exists());
}

#[test]
fn tokenless_report_storage_tracks_the_actual_runtime_mode() {
    for (mode, storage) in [("no-stash", "disabled"), ("diff-opt-in", "in_memory")] {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path();
        let result = Command::new(env!("CARGO_BIN_EXE_l2_compare"))
            .args([
                "--tokenless-only",
                "--categories",
                "json",
                "--no-probe",
                "--n",
                "5",
                "--mode",
                mode,
                "--report-dir",
            ])
            .arg(output)
            .env("HEADROOM_PYTHON", "/fixture/comparator-must-not-be-spawned")
            .env_remove("DASHSCOPE_API_KEY")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(output.join("report.json")).unwrap())
                .unwrap();
        assert_eq!(report["coverage"]["runtime_arbitration"], "measured");
        assert_eq!(report["component_modes"]["runtime_mode"], mode);
        assert_eq!(report["recovery_storage"], storage);
        if mode == "diff-opt-in" {
            assert!(
                report["summary"]["degradations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|note| note
                        .as_str()
                        .unwrap()
                        .contains("not an independent configuration contrast"))
            );
        }
        assert!(directory.path().exists());
    }
}
