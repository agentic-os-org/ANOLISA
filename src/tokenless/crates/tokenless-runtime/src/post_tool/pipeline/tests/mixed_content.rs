use crate::post_tool::content::{MAX_SCAN_BYTES, MAX_SCAN_LINES};

use super::embedded_html::{embedded_document, rich_text};
use super::*;

fn mixed_config() -> PostToolPipelineConfig {
    let mut config = build_log_config();
    config.force_json = false;
    config.mixed_content_enabled = true;
    config
}

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
    assert!(
        run.response
            .output
            .starts_with("Request complete, status 200 OK.\n\n```csv\n")
    );
    assert!(
        run.response
            .output
            .ends_with("\r\n\r\nFollow-up diagnostics pending.\n")
    );
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
    let body = mixed_json_region(30);
    let table = mixed_tabular_region(40);
    let input = format!("part 1\n\n{body}\n\npart 2\n\n```csv\n{table}\n```\n\nend\n");
    assert!(
        input.lines().count() <= MAX_SCAN_LINES,
        "both regions must be inside the recognition window",
    );
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
fn mixed_json_regions_follow_embedded_html_policy() {
    let body = embedded_document();
    let input = format!("before\n{body}\nafter\n");
    for (origin, enabled) in [
        (ContentOrigin::CommandOutput, true),
        (ContentOrigin::ApiResponse, true),
        (ContentOrigin::FileRead, true),
        (ContentOrigin::CommandOutput, false),
    ] {
        let mut config = mixed_config();
        config.html_extraction_enabled = enabled;
        let mut req = request(&input);
        req.content_origin = origin;
        let concrete = Arc::new(CountingStore::default());
        let store: Arc<dyn StashStore> = concrete.clone();
        let run = PostToolPipeline::run(&req, &config, Some(&store));
        if enabled && origin != ContentOrigin::FileRead {
            assert_eq!(run.response.disposition, Disposition::Applied);
            assert_eq!(
                run.response.applied_operations,
                [AppliedOperation::HtmlExtraction]
            );
            assert!(run.response.output.starts_with("before\n"));
            assert!(run.response.output.ends_with("\nafter\n"));
            assert_eq!(run.response.stash_keys.len(), 2);
            for key in &run.response.stash_keys {
                let payload = concrete.retrieve(key).unwrap().unwrap();
                assert!([rich_text("record"), rich_text("item")].contains(&payload));
            }
        } else {
            assert_eq!(run.response.output, input);
            assert!(run.response.applied_operations.is_empty());
            assert_eq!(concrete.len(), 0);
        }
    }
}

#[test]
fn malformed_and_unsupported_regions_stay_byte_for_byte() {
    let broken = "{\n  \"a\": 1,\n  \"b\": \n"; // unbalanced
    let python = "```python\nprint({\"not\": \"json\"})\n```\n";
    let table = mixed_tabular_region(40);
    // The table sits before the unbalanced opener: like an unclosed fence,
    // a bare document that never balances protects its remainder rather
    // than letting recognition carve regions out of an unterminated one.
    let input = format!("start\n\n```csv\n{table}\n```\n\n{broken}\n\n{python}\n");
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::TabularCompaction]
    );
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
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::JsonCleanup]
        );
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
fn grep_outputs_never_enter_the_mixed_route() {
    let table = mixed_tabular_region(40);
    let input = format!("prose\n\n```csv\n{table}\n```\n\nprose\n");
    let mut req = request(&input);
    req.tool_name = "Grep".into();
    // The search-output-protection gate claims Grep results for the search
    // route before mixed could ever see them; for csv prose the search
    // compressor offers nothing, so the run ends in the same no-savings
    // verdict the search route itself issues for untouched output.
    let run = PostToolPipeline::run(&req, &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(run.response.output, input);
    // With search path sharing off, the gate drops and Grep output falls
    // through every route untouched.
    let mut config = mixed_config();
    config.search_path_sharing_enabled = false;
    let run = PostToolPipeline::run(&req, &config, None);
    assert_eq!(run.response.disposition, Disposition::Passthrough);
    assert_eq!(run.response.output, input);
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
fn an_unchanged_json_region_rolls_back_its_tentative_stash_writes() {
    // A compact numeric array the record-sampling flow cannot shrink: the
    // bounded session still stashes its dropped-tail candidate before the
    // savings gate rejects it — the recovery marker is far longer than the
    // dropped tail, so the selected candidate equals the region — while the
    // tentative Stash write has already reached the store. The run must
    // route those writes through the ledger so the reject path rolls them
    // back; dropping them with the rejected outcome would leak an
    // unreferenced Stash row into the store.
    let body = serde_json::to_string(&(0..40).map(|_i| 0).collect::<Vec<_>>()).unwrap();
    let input = format!("before\n{body}\nafter\n");
    let config = mixed_config();
    // Precondition: the standalone route also reports no savings for this
    // body, so the mixed region's outcome really is an unchanged one.
    let solo = PostToolPipeline::run(&request(&body), &config, None);
    assert_eq!(solo.response.disposition, Disposition::NoSavings);
    let concrete = Arc::new(CountingStore::default());
    let store: Arc<dyn StashStore> = concrete.clone();
    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));
    // A recognized region that produced only tentative writes is a tried
    // and rejected candidate, not untouched content: the same no-savings
    // verdict the standalone JSON route issues for an unchanged body.
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(run.response.output, input);
    assert_eq!(run.response.applied_operations, []);
    assert_eq!(run.response.stash_keys, Vec::<String>::new());
    // Nothing persists: the ledger owned the tentative write and rolled it
    // back when the run rejected the operation-free candidate.
    assert_eq!(concrete.len(), 0);
}

#[test]
fn a_failing_region_write_surfaces_as_a_stash_error_not_passthrough() {
    // An unchanged region whose only Stash write fails must not degrade to
    // an ordinary passthrough: the standalone array route reports
    // `{count} stash write operation(s) failed` for the same body and the
    // same fault, and the wrapped mixed run reports the same error count.
    // The original text is preserved either way; the defect would be the
    // silent success.
    let body = serde_json::to_string(&(0..40).map(|_i| 0).collect::<Vec<_>>()).unwrap();
    let input = format!("before\n{body}\nafter\n");
    let config = mixed_config();
    // Precondition: the standalone route surfaces the storage failure.
    let solo_store = Arc::new(CountingStore {
        // Pre-advance the counter so the very first `stash` call fails, the
        // same trick the diff route's fault-injection test uses.
        stash_calls: AtomicUsize::new(1),
        fail_stashes_after: 1,
        ..CountingStore::default()
    });
    let store: Arc<dyn StashStore> = solo_store.clone();
    let solo = PostToolPipeline::run(&request(&body), &config, Some(&store));
    assert!(
        solo.stash_errors.is_some_and(|count| count > 0),
        "precondition: standalone route reports the stash failure",
    );
    // The wrapped run must too.
    let mixed_store = Arc::new(CountingStore {
        stash_calls: AtomicUsize::new(1),
        fail_stashes_after: 1,
        ..CountingStore::default()
    });
    let store: Arc<dyn StashStore> = mixed_store.clone();
    let run = PostToolPipeline::run(&request(&input), &config, Some(&store));
    // The failed write leaves a zero-operation candidate, which the arbiter
    // reads as no-savings — the same disposition the diff and build-log
    // routes issue under an injected stash fault. The output is unchanged
    // either way; the defect this pins is the error count, not the verdict.
    assert_eq!(run.response.disposition, Disposition::NoSavings);
    assert_eq!(run.response.output, input);
    assert_eq!(run.stash_errors, Some(1));
    assert_eq!(run.stash_writes, Some(0));
    assert_eq!(mixed_store.len(), 0);
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
    assert!(
        run.response
            .applied_operations
            .contains(&AppliedOperation::JsonTruncation)
    );
    assert_eq!(run.response.stash_keys.len(), 1);
    assert!(!run.response.output.is_empty());
    // Same metric attribution as the standalone JSON route: whatever the
    // region's truncation produced, the mixed run reports exactly what the
    // standalone run reports for the same body under the same config —
    // not a hardcoded None.
    let solo = PostToolPipeline::run(&request(&body), &config, Some(&store));
    assert_eq!(
        solo.unrecoverable_truncations,
        run.unrecoverable_truncations
    );
    assert!(run.unrecoverable_truncations.is_some());
}

#[test]
fn out_of_window_tail_survives_byte_for_byte_through_the_pipeline() {
    // The recognition tests at the byte/line bounds only pin recognition.
    // This pins the applied end-to-end contract: a region inside the scan
    // window compresses while prose before it, prose between the fence and
    // the window cut, and the tail beyond the cut all survive verbatim.
    let body = mixed_json_region(60);
    let head = "prose head\n\n";
    let tail = "y".repeat(MAX_SCAN_BYTES - head.len() - 100) + "\n\nout-of-window tail\n";
    let input = format!("{head}```json\n{body}\n```\n{tail}");
    assert_eq!(detect(&input), ContentType::PlainText);
    assert!(
        input.len() > MAX_SCAN_BYTES,
        "precondition: tail crosses the byte bound"
    );
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::JsonCleanup]
    );
    let output = &run.response.output;
    assert!(output.starts_with(&format!("{head}```json\n")), "{output}");
    assert!(output.ends_with(&tail), "{output}");
}

#[test]
fn markdown_table_with_inline_backticks_keeps_json_looking_cell_unchanged() {
    let padded_ones = format!("[{}                1]", "                1,".repeat(19));
    let input =
        format!("| a | b |\n|---|---|\n| 1 | 2 |\n```label``` | details\n{padded_ones} | x | y\n");

    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.output, input);
    assert_eq!(run.response.content_type, Some(ContentType::Tabular));
    assert!(run.response.applied_operations.is_empty());
}

#[test]
fn markdown_table_followed_by_fenced_json_keeps_mixed_compression() {
    let body = mixed_json_region(40);
    let input = format!("| col | val |\n|---|---|\n| a | 1 |\n\n```json\n{body}\n```\n");
    assert_ne!(detect(&input), ContentType::Tabular);
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::JsonCleanup]
    );
    assert!(run.response.output.starts_with("| col | val |\n"));
    assert!(run.response.after_tokens < run.response.before_tokens);
}

#[test]
fn tab_padded_markdown_table_followed_by_fenced_json_keeps_mixed_compression() {
    let body = mixed_json_region(40);
    let mut table = String::from("| a\t| b |\n|---\t|---|\n");
    for _ in 0..8 {
        table.push_str("| 1\t| 2 |\n");
    }
    let input = format!("{table}\n```json\n{body}\n```\n");
    assert_ne!(detect(&input), ContentType::Tabular);
    let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
    assert_eq!(run.response.disposition, Disposition::Applied);
    assert_eq!(
        run.response.applied_operations,
        [AppliedOperation::JsonCleanup]
    );
    assert!(run.response.output.starts_with(&table));
    assert!(run.response.after_tokens < run.response.before_tokens);
}

#[test]
fn tab_padded_one_column_gfm_table_followed_by_fenced_json_keeps_mixed_compression() {
    let body = mixed_json_region(40);
    for (header, delim, row) in [
        ("| a |\t\n", ":---:\t\n", "| x |\t\n"),
        ("a\t\n", "|:---:|\t\n", "| x |\t\n"),
        ("Summary\t\n", ":---:\t\n", "x\t\n"),
    ] {
        let mut table = String::from(header);
        table.push_str(delim);
        for _ in 0..8 {
            table.push_str(row);
        }
        let input = format!("{table}\n```json\n{body}\n```\n");
        assert_eq!(
            detect(&input),
            ContentType::PlainText,
            "one-column tab-padded GFM + fence must not route to Tabular: {header:?} / {delim:?}"
        );
        let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
        assert_eq!(run.response.content_type, Some(ContentType::PlainText));
        assert_eq!(run.response.disposition, Disposition::Applied);
        assert_eq!(
            run.response.applied_operations,
            [AppliedOperation::JsonCleanup]
        );
        assert!(run.response.output.starts_with(&table));
        assert!(run.response.after_tokens < run.response.before_tokens);
    }

    // Rectangular TSV control with colon/pipe-like cells stays on the Tabular route.
    let mut rect_tsv = String::from("name\tmarker\r\nalice\t:---:\r\nbob\t|:---:|\r\n");
    for i in 0..8 {
        rect_tsv.push_str(&format!("u{i}\tv{i}\r\n"));
    }
    assert_eq!(detect(&rect_tsv), ContentType::Tabular);
    let run = PostToolPipeline::run(&request(&rect_tsv), &mixed_config(), None);
    assert_eq!(run.response.content_type, Some(ContentType::Tabular));
    assert!(
        !run.response
            .applied_operations
            .contains(&AppliedOperation::JsonCleanup)
    );
    assert_eq!(
        parse_delimited_cells(&run.response.output, '\t'),
        parse_delimited_cells(&rect_tsv, '\t')
    );
}

fn parse_delimited_cells(input: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = input.chars().peekable();
    let mut in_quotes = false;
    let mut started = false;

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => {
                in_quotes = true;
                started = true;
            }
            c if c == delimiter => {
                row.push(std::mem::take(&mut field));
                started = true;
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if started || !row.is_empty() {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    started = false;
                }
            }
            '\n' => {
                if started || !row.is_empty() {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    started = false;
                }
            }
            c => {
                field.push(c);
                started = true;
            }
        }
    }
    if started || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[test]
fn quoted_multiline_csv_with_markdown_separator_and_json_stays_tabular() {
    let padded_ones = format!("[\n{}\n  1\n]", "                1,\n".repeat(20));
    for record_count in [3usize, 12usize] {
        let mut rows = String::from("id,note\r\n");
        rows.push_str(&format!(
            "1,\"intro | note\r\n|---|---|\r\n```json\r\n{padded_ones}\r\n```\r\nend\"\r\n"
        ));
        for i in 2..=record_count {
            rows.push_str(&format!("{i},short\r\n"));
        }
        assert_eq!(detect(&rows), ContentType::Tabular);
        let run = PostToolPipeline::run(&request(&rows), &mixed_config(), None);
        assert_eq!(run.response.content_type, Some(ContentType::Tabular));
        assert!(
            !run.response
                .applied_operations
                .contains(&AppliedOperation::JsonCleanup)
        );
        assert_eq!(
            parse_delimited_cells(&run.response.output, ','),
            parse_delimited_cells(&rows, ','),
            "CSV cells must remain equivalent after tabular routing (records={record_count})"
        );
    }
}

#[test]
fn valid_tsv_with_pipe_and_separator_cells_stays_tabular_with_equal_cells() {
    let input = "name\tnotes\r\nalice\t|short\r\n---\t|---\r\nbob\tshort\r\n";
    assert_eq!(detect(input), ContentType::Tabular);
    let run = PostToolPipeline::run(&request(input), &mixed_config(), None);
    assert_eq!(run.response.content_type, Some(ContentType::Tabular));
    assert!(
        !run.response
            .applied_operations
            .contains(&AppliedOperation::JsonCleanup)
    );
    assert_eq!(
        parse_delimited_cells(&run.response.output, '\t'),
        parse_delimited_cells(input, '\t')
    );
}

#[test]
fn markdown_table_followed_by_html_block_routes_as_plain_text() {
    let input = "| a | b |\n|---|---|\n| 1 | 2 |\n<div>follow-up | notes</div>";
    assert_eq!(detect(input), ContentType::PlainText);
    let run = PostToolPipeline::run(&request(input), &mixed_config(), None);
    assert_eq!(run.response.content_type, Some(ContentType::PlainText));
}

#[test]
fn setext_headings_and_thematic_breaks_after_table_preserve_mixed_compression() {
    let padded_array = format!("[{}1]\n", "                1,".repeat(20));
    // Setext heading variants (colon-free `-` underlines).
    for header in [
        "Summary\n-------\n",
        "Summary\n---\n",
        "Summary\n--\n",
        "Summary\n-\n",
        "| a |\n---\n",
        "| a |\n--\n",
    ] {
        let input = format!("{header}{padded_array}");
        assert_eq!(detect(&input), ContentType::PlainText);
        let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
        assert_eq!(run.response.content_type, Some(ContentType::PlainText));
        assert!(
            run.response
                .applied_operations
                .contains(&AppliedOperation::JsonCleanup)
        );
    }

    // Thematic breaks after valid table: ---, ***, ___, _ _ _
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    for sep in ["---", "***", "___", "_ _ _"] {
        let input = format!("{table}{sep}\n{padded_array}");
        assert_eq!(detect(&input), ContentType::PlainText);
        let run = PostToolPipeline::run(&request(&input), &mixed_config(), None);
        assert_eq!(run.response.content_type, Some(ContentType::PlainText));
        assert!(
            run.response
                .applied_operations
                .contains(&AppliedOperation::JsonCleanup)
        );
    }

    // Lone pipe after valid table leaves following array eligible for compression
    let lone_pipe_input = format!("{table}|\n{padded_array}");
    assert_eq!(detect(&lone_pipe_input), ContentType::PlainText);
    let run = PostToolPipeline::run(&request(&lone_pipe_input), &mixed_config(), None);
    assert_eq!(run.response.content_type, Some(ContentType::PlainText));
    assert!(
        run.response
            .applied_operations
            .contains(&AppliedOperation::JsonCleanup)
    );

    // Two-hyphen control stays tabular (does NOT route to mixed json cleanup)
    let two_hyphen_table = format!("{table}--\n");
    assert_eq!(detect(&two_hyphen_table), ContentType::Tabular);
    let run = PostToolPipeline::run(&request(&two_hyphen_table), &mixed_config(), None);
    assert_eq!(run.response.content_type, Some(ContentType::Tabular));
    assert!(
        !run.response
            .applied_operations
            .contains(&AppliedOperation::JsonCleanup)
    );
    assert_eq!(run.response.output, two_hyphen_table);
}

#[test]
fn standalone_gfm_table_shapes_stay_byte_identical() {
    let padded_array = format!("[{}1]\n", "                1,".repeat(20));
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";

    // Standalone GFM tables with escaped-pipe body rows (example 200) or
    // one-column header/alignment delimiters (including pipe-free colon
    // delimiters) stay byte-for-byte on the Tabular route.
    for kept in [
        format!("{table}left \\| right\n{padded_array}"),
        format!("{table}| left \\| right |\n{padded_array}"),
        format!("| a |\n:---:\n{padded_array}"),
        format!("| a |\n|:---:|\n{padded_array}"),
        format!("a\n|:---:|\n{padded_array}"),
        format!("Summary\n:---:\n{padded_array}"),
        format!("Summary\n:---\n{padded_array}"),
        format!("Summary\n---:\n{padded_array}"),
        format!("Summary\n:-\n{padded_array}"),
        format!("left \\| right\n:---:\n{padded_array}"),
    ] {
        assert_eq!(detect(&kept), ContentType::Tabular, "{kept:?}");
        let run = PostToolPipeline::run(&request(&kept), &mixed_config(), None);
        assert_eq!(run.response.content_type, Some(ContentType::Tabular));
        assert!(
            !run.response
                .applied_operations
                .contains(&AppliedOperation::JsonCleanup)
        );
        assert_eq!(run.response.output, kept);
    }
}
