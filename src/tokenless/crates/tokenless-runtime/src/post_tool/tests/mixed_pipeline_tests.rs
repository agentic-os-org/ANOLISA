// Included into `post_tool::pipeline::tests`, which this file shares its
// imports with, like the other included test suites in this directory.

fn mixed_json_region(records: usize) -> String {
    // Pretty-printed: the mixed route must show the same compaction
    // behavior the standalone JSON route has on this shape.
    serde_json::to_string_pretty(
        &(0..records)
            .map(|index| {
                serde_json::json!({
                    "id": index,
                    "message": format!("record-{index}-{}", "x".repeat(60)),
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

fn mixed_tabular_region(rows: usize) -> String {
    (0..rows)
        .map(|i| format!("{i},record-{i}-{}", "payload ".repeat(8)))
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[test]
fn regions_are_recognized_inside_prose_and_reconstructed_in_order() {
    let table = mixed_tabular_region(40);
    // A bare table stays a whole-content route (or passes through with no
    // verifiable boundary), so the mixed fixture uses an explicit fence.
    let input = format!(
        "Request complete, status 200 OK.\n\n```csv\n{table}\r\n```\r\n\r\nFollow-up diagnostics pending.\n"
    );
    assert_eq!(detect(&input), ContentType::PlainText);
    let found = mixed::regions(&input);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0], (&table[..], mixed::RegionKind::Tabular));

    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.response.content_type, Some(ContentType::PlainText));
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::TabularCompaction]
    );
    // Surrounding prose, fence lines and separators are byte-for-byte.
    assert!(run.response.output.starts_with("Request complete, status 200 OK.\n\n```csv\n"));
    assert!(run.response.output.ends_with("\r\n\r\nFollow-up diagnostics pending.\n"));
    // Cells survive; only formatting compacted.
    assert!(run.response.output.contains(&table.replace("\r\n", "\n")));
}

#[test]
fn fenced_json_region_compresses_and_keeps_its_fence() {
    let body = mixed_json_region(60);
    let input = format!("```json\n{body}\n```\n");
    assert_eq!(detect(&input), ContentType::PlainText);
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    let output = &run.response.output;
    assert!(output.starts_with("```json\n"), "{output}");
    assert!(output.ends_with("\n```\n"), "{output}");
    // The body is still valid JSON with every record present.
    let inner = output
        .trim_start_matches("```json\n")
        .trim_end_matches("\n```\n");
    let value: serde_json::Value = serde_json::from_str(inner).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 60);
}

#[test]
fn fenced_json_relabeled_toon_together_with_its_body() {
    // A large uniform array is TOON's best case; standalone JSON with the
    // same body must take the same route under the same policy.
    let body = serde_json::to_string(
        &(0..200)
            .map(|i| serde_json::json!({"id": i, "score": i * 3, "flag": true}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let standalone = body.clone();
    let mut config = mixed_config();
    config.allow_toon = true;
    config.min_toon_chars = 0;
    let solo = PostToolPipeline::run(&request(&standalone), &config, None);
    assert_eq!(
        solo.response.applied_operations,
        [AppliedOperation::Toon],
        "precondition: the standalone body routes to TOON"
    );
    let input = format!("```json\n{body}\n```\n");
    let run = PostToolPipeline::run(&request(&input), &config, None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(run.response.applied_operations, [AppliedOperation::Toon]);
    let output = &run.response.output;
    assert!(output.starts_with("```toon\n"), "{output}");
    assert!(output.ends_with("\n```\n"), "{output}");
    // The original fence label is gone from the transformed block.
    assert!(!output.contains("```json"));
}

#[test]
fn bare_json_document_inside_prose_is_a_region() {
    let body = mixed_json_region(60);
    let input = format!("HEAD /api/items HTTP/1.1\n\n{body}\n\n-- end of response --\n");
    assert_eq!(detect(&input), ContentType::PlainText);
    let found = mixed::regions(&input);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0], (&body[..], mixed::RegionKind::Json));
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    let output = &run.response.output;
    assert!(output.starts_with("HEAD /api/items HTTP/1.1\n\n"));
    assert!(output.ends_with("\n\n-- end of response --\n"));
}

#[test]
fn json_and_tabular_regions_coexist_in_one_result() {
    let body = mixed_json_region(60);
    let table = mixed_tabular_region(40);
    let input = format!("part 1\n\n{body}\n\npart 2\n\n```csv\n{table}\n```\n\nend\n");
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    let operations = &run.response.applied_operations;
    assert!(operations.contains(&AppliedOperation::JsonCleanup));
    assert!(operations.contains(&AppliedOperation::TabularCompaction));
    let output = &run.response.output;
    assert!(output.starts_with("part 1\n\n"));
    assert!(output.contains("part 2\n\n"));
    assert!(output.ends_with("\n\nend\n"));
}

#[test]
fn malformed_and_unsupported_regions_stay_byte_for_byte() {
    let broken = "{\n  \"a\": 1,\n  \"b\": \n"; // unbalanced
    let python = "```python\nprint({\"not\": \"json\"})\n```\n";
    let table = mixed_tabular_region(40);
    let input = format!(
        "start\n\n{broken}\n\n{python}\n\n```csv\n{table}\n```\n\nend\n"
    );
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    let output = &run.response.output;
    assert!(output.contains(broken), "malformed JSON preserved");
    assert!(output.contains(python), "unsupported fence preserved");
}

#[test]
fn protected_code_blocks_are_never_extracted_from() {
    // A ```json fence that contains a fenced csv block: the outer block is
    // one region whose body is not valid JSON — nothing inside it may be
    // recognized, and the block must survive byte-for-byte.
    let nested = "```json\n{\n  \"snippet\": \"```csv\\na,b\\n1,2\\n```\",\n  \"n\": 1\n}\n```";
    let input = format!("before\n\n{nested}\n\nafter\n");
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    // The body is valid JSON; whether it compresses depends on savings, but
    // no csv region may be carved out of it.
    if run.response.disposition == Disposition::Applied {
        assert_eq!(run.response.applied_operations, [AppliedOperation::JsonCleanup]);
    }
    assert!(run.response.output.contains("before\n\n"));
    assert!(run.response.output.contains("\n\nafter\n"));
}

#[test]
fn unclosed_fence_protects_the_rest_of_the_output() {
    let body = mixed_json_region(60);
    let input = format!("```json\n{{\"open\": 1\n\n{body}\n");
    assert_eq!(detect(&input), ContentType::PlainText);
    let found = mixed::regions(&input);
    assert!(found.is_empty(), "unclosed fence hides the document");
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Passthrough);
    assert_eq!(run.response.output, input);
}

#[test]
fn unknown_fence_labels_protect_their_block() {
    let table = mixed_tabular_region(40);
    let input = format!("```rust\nlet x = 1;\n{table}\n```\n\n```csv\n{table}\n```\n");
    let found = mixed::regions(&input);
    assert_eq!(found.len(), 1, "only the fenced csv after the rust block");
    assert_eq!(found[0].1, mixed::RegionKind::Tabular);
    assert_eq!(found[0].0, &table[..]);
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    // The rust block is preserved verbatim.
    assert!(run.response.output.contains("```rust\nlet x = 1;\n"));
}

#[test]
fn whole_content_routes_take_precedence_over_mixed() {
    // Standalone JSON must keep its existing route even with mixed enabled.
    let input = mixed_json_region(60);
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.content_type, Some(ContentType::Json));
    // Standalone tabular likewise.
    let table = mixed_tabular_region(40);
    let run = PostToolPipeline::run(&request(&table), &mixed_config(), None);
    assert_eq!(run.response.content_type, Some(ContentType::Tabular));
}

#[test]
fn mixed_route_respects_the_switch() {
    let table = mixed_tabular_region(40);
    let input = format!("prose\n\n```csv\n{table}\n```\n\nprose\n");
    let mut config = mixed_config();
    config.mixed_content_enabled = false;
    let run = PostToolPipeline::run(&request(&input), &config, None);
    assert_eq!(run.response.disposition, Disposition::Passthrough);
    assert_eq!(run.response.output, input);
}

#[test]
fn search_outputs_never_enter_the_mixed_route() {
    let table = mixed_tabular_region(40);
    let input = format!("prose\n\n```csv\n{table}\n```\n\nprose\n");
    let mut config = mixed_config();
    config.search_path_sharing_enabled = false;
    for (tool, command) in [
        ("Grep", None),
        ("Bash", Some("grep -n match file && cargo test")),
    ] {
        let mut req = request(&input);
        req.tool_name = tool.into();
        req.command = command.map(str::to_owned);
        let run = PostToolPipeline::run(&req, &config, None);
        assert_eq!(run.response.disposition, Disposition::Passthrough);
        assert_eq!(run.response.output, input);
    }
}

#[test]
fn no_savings_reconstructs_the_original() {
    // An already-compact table offers compaction nothing: every region is
    // preserved, no candidate exists, and the original output is kept.
    let rows = (0..40)
        .map(|i| format!("{i},v{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let input = format!("prose before\n\n```csv\n{rows}\n```\n\nprose after\n");
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Passthrough);
    assert_eq!(run.response.output, input);
}

#[test]
fn a_bare_table_without_boundaries_never_enters_the_route() {
    let table = mixed_tabular_region(40);
    let input = format!("prose before\n\n{table}\r\n\r\nprose after\n");
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Passthrough);
    assert_eq!(run.response.output, input);
}

#[test]
fn timeout_keeps_the_original_output() {
    let table = mixed_tabular_region(40);
    let input = format!("prose\n\n```csv\n{table}\n```\n\nprose\n");
    let mut config = mixed_config();
    config.timeout = Duration::ZERO;
    let run = PostToolPipeline::run(&request(&input), &config, None);
    assert_eq!(run.response.output, input);
    assert_ne!(run.response.disposition, Disposition::Applied);
}

#[test]
fn truncated_json_region_reports_recoverability_and_stash() {
    // Mirror of the standalone truncation fixture: an object wrapping a
    // string array far past the head budget, so the bounded flow — not the
    // record-sampling flow, which only applies to arrays of objects —
    // produces the truncation.
    let body = serde_json::to_string(&serde_json::json!({
        "report": "diagnostic context",
        "items": (0..100)
            .map(|index| format!("item-{index}-{}", "x".repeat(80)))
            .collect::<Vec<_>>(),
    }))
    .unwrap();
    let input = format!("report\n\n{body}\n\nend\n");
    let mut config = mixed_config();
    config.json.truncate_arrays_at = 4;
    config.json.array_tail_preserve = 0;
    config.require_reversibility = false;
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert!(run
        .response
        .applied_operations
        .contains(&AppliedOperation::JsonTruncation));
    assert_eq!(run.response.stash_keys.len(), 1);
    assert!(!run.response.output.is_empty());
}
