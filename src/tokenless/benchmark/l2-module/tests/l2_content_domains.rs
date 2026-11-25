// Copyright 2026 Alibaba Cloud
// Licensed under the Apache License, Version 2.0.

use std::path::PathBuf;
use tokenless_l2_bench::l2::{Category, retention, samples, tokenless_side};

#[test]
fn current_content_domain_assets_keep_raw_input_and_ground_truth() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    for category in [
        Category::BuildLog,
        Category::Tabular,
        Category::Search,
        Category::Html,
    ] {
        assert_eq!(Category::parse(category.name()), Some(category));
        assert!(!category.is_dynamic());
        let records = samples::load_domain_samples(&root, category).unwrap();
        assert!(records.len() >= 2);
        assert!(
            records.iter().any(|record| record.id.contains("control")),
            "{category}: negative control disappeared"
        );
        for record in records {
            let baseline = retention::check(&record.ground_truth, &record.content).unwrap();
            assert!(baseline.total > 0, "empty denominator {}", record.id);
            assert_eq!(baseline.passed, baseline.total, "bad asset {}", record.id);
            let frame_only = match category {
                Category::BuildLog => "error[E0000]: unrelated diagnostic\n --> other.rs:1:1\n",
                Category::Tabular => "\"other_id\",\"other_message\"\n\"nothing\",\"unrelated\"\n",
                Category::Search => "other/directory/source.rs:1:unrelated-row\n",
                Category::Html => {
                    "<html><head></head><body><main><h1>Empty view</h1></main></body></html>"
                }
                _ => unreachable!("this test enumerates only content domains"),
            };
            let reverse = retention::check(&record.ground_truth, frame_only).unwrap();
            assert!(
                reverse.passed < reverse.total,
                "{} rewards a frame without its source facts",
                record.id
            );
            let wire = tokenless_side::wire_before(category, &record.content).unwrap();
            assert_eq!(
                wire, record.content,
                "{} unexpectedly acquired an envelope",
                record.id
            );
            let output = tokenless_side::compress(category, &record.content).unwrap();
            let retained = retention::check(&record.ground_truth, &output.retention_text).unwrap();
            assert_eq!(
                retained.passed, retained.total,
                "{} lost {:?}",
                record.id, retained.failures
            );
            if record.id.contains("control") {
                assert_eq!(
                    output.compressed, record.content,
                    "negative control {} changed",
                    record.id
                );
            }
        }
    }
}

#[test]
fn no_recovery_mode_rejects_unrecoverable_bounded_json_candidates() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    for record in samples::load_json_samples(&root).unwrap() {
        let original = retention::check(&record.ground_truth, &record.content).unwrap();
        assert!(original.total > 0);
        assert_eq!(original.passed, original.total);
        let result = tokenless_side::compress(Category::Json, &record.content).unwrap();
        let retained = retention::check(&record.ground_truth, &result.retention_text).unwrap();
        assert_eq!(
            retained.passed, retained.total,
            "{} published an unrecoverable candidate as model output",
            record.id
        );
    }
}

#[test]
fn json_numeric_literals_are_not_normalized_before_measurement() {
    let input = "{\n\"integer\":9007199254740993,\"decimal\":0.12345678901234567890123456789,\"zero\":-0.0\n}";
    assert_eq!(
        tokenless_side::wire_before(Category::Json, input).unwrap(),
        input
    );
    let output = tokenless_side::compress(Category::Json, input).unwrap();
    for literal in [
        "9007199254740993",
        "0.12345678901234567890123456789",
        "-0.0",
    ] {
        assert!(
            output.compressed.contains(literal),
            "numeric literal changed: {literal}"
        );
    }
}
