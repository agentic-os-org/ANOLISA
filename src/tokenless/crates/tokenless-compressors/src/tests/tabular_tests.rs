use tokenless_ccr::{InMemoryStore, StashError};

fn cells(input: &str, delimiter: u8) -> Vec<StringRecord> {
    csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .from_reader(input.as_bytes())
        .records()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn large_table(delimiter: char) -> String {
    let mut input = format!("id{delimiter}status{delimiter}message\r\n");
    for index in 0..120 {
        let status = if index == 61 { "FAILED" } else { "ok" };
        input.push_str(&format!(
            "{index:04}{delimiter}{status}{delimiter}record-{index}-{}\r\n",
            "ordinary payload ".repeat(6)
        ));
    }
    input
}

#[test]
fn full_compaction_preserves_cells_headers_and_order() {
    for delimiter in *b",\t" {
        // An independent CSV writer supplies redundant quoting and CRLF.
        let rows = [
            vec!["id", "id", "", "value", "message"],
            vec![
                "001",
                "001",
                "",
                "184467440737095516160",
                "line 1\r\nline 2",
            ],
            vec!["null", "false", "", "  padded  ", "comma,tab\tquote\"中文"],
            vec!["", "", "", "", ""],
            vec![
                "001",
                "001",
                "",
                "184467440737095516160",
                "line 1\r\nline 2",
            ],
        ];
        let mut writer = csv::WriterBuilder::new()
            .delimiter(delimiter)
            .quote_style(csv::QuoteStyle::Always)
            .terminator(csv::Terminator::CRLF)
            .from_writer(Vec::new());
        for row in rows {
            writer.write_record(row).unwrap();
        }
        let input = String::from_utf8(writer.into_inner().unwrap()).unwrap();
        let outcome = TabularCompressor.compress(&input);
        assert_eq!(outcome.operations, [TabularOperation::Compaction]);
        assert_eq!(cells(&input, delimiter), cells(&outcome.output, delimiter));
        assert_eq!(outcome.metrics.retained_rows, 4);
        assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
        assert!(outcome.stash_writes.is_empty());
    }
}

#[test]
fn full_compaction_keeps_a_leading_bom_inside_the_first_cell() {
    for delimiter in *b",\t" {
        for prefix in ["", "\u{feff}", "\n\r\n"] {
            let separator = char::from(delimiter);
            let input = format!(
                "{prefix}\"\u{feff}id\"{separator}\"name\"\r\n{}",
                format!("\"001\"{separator}\"alpha\"\r\n").repeat(50)
            );
            let outcome = TabularCompressor.compress(&input);
            assert_eq!(outcome.operations, [TabularOperation::Compaction]);
            assert_eq!(&cells(&input, delimiter)[0][0], "\u{feff}id");
            assert_eq!(cells(&input, delimiter), cells(&outcome.output, delimiter));
            assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
            assert!(outcome.stash_writes.is_empty());
        }
    }
}

#[test]
fn unsupported_or_malformed_tables_pass_through_without_storage() {
    for input in [
        "a\nb\nc",
        "a,b\n1,2",
        "a,b\n1,2,3\n4,5,6",
        "a,b\n1,\"unterminated\n2,value",
        "a,b\n1,un\"quoted\n2,value",
        "a,b\n1,\"closed\"tail\n2,value",
        "a,b\tc\n1,2\t3\n4,5\t6",
        "| a | b |\n|---|---|\n| 1 | 2 |",
        "a;b\n1;2\n3;4",
    ] {
        let store = InMemoryStore::new();
        let result =
            TabularCompressor.compress_with_recovery(input, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(result.output, input);
        assert!(result.operations.is_empty(), "{input}");
        assert!(store.is_empty());
    }
    let input = format!("{}ragged,row,with,extra,fields\n", large_table(','));
    assert!(TabularCompressor::detect(&input));
    let outcome = TabularCompressor.compress(&input);
    assert_eq!(outcome.output, input);
}

#[test]
fn detection_is_bounded_and_ignores_a_partial_trailing_record() {
    assert!(TabularCompressor::detect("a,b\n1,\"two\nlines\"\n3,x"));
    assert!(TabularCompressor::detect("\u{feff}a,b\r\n1,2\r\n3,4"));
    let input = format!("a,b\n1,2\n3,4\n5,\"{}\"", "x".repeat(DETECTION_BYTES));
    assert!(TabularCompressor::detect(&input));
    assert!(!TabularCompressor::detect(&format!(
        "a,b\n1,\"{}\"\n3,4",
        "x".repeat(DETECTION_BYTES)
    )));
}

#[test]
fn compaction_applies_when_savings_are_small() {
    for delimiter in [',', '\t'] {
        let input = large_table(delimiter);
        let outcome = TabularCompressor.compress(&input);
        assert_eq!(outcome.operations, [TabularOperation::Compaction]);
        assert_eq!(outcome.metrics.original_rows, 120);
        assert_eq!(outcome.metrics.retained_rows, 120);
        assert_eq!(
            cells(&input, delimiter as u8),
            cells(&outcome.output, delimiter as u8)
        );
        assert!(saves_both(&outcome.output, &input));
        let before = estimate_tokens(&input);
        let after = estimate_tokens(&outcome.output);
        assert!((before - after) * 100 < before * 15);
        assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
        assert!(outcome.stash_writes.is_empty());
        assert_eq!(outcome.metrics.stash_errors, 0);
        assert!(!outcome.output.contains("[Table:"));
    }
}

#[test]
fn compact_tables_keep_all_rows_at_and_above_the_old_selection_boundary() {
    for delimiter in [',', '\t'] {
        for count in [32, 33, 120, 2000] {
            let input = format!(
                "id{delimiter}status{delimiter}message\n{}",
                (0..count)
                    .map(|i| format!(
                        "{i:04}{delimiter}{}{delimiter}record-{i}-{}",
                        if i == 17 { "FAILED" } else { "ok" },
                        "payload ".repeat(10)
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let outcome = TabularCompressor.compress(&input);
            assert_eq!(outcome.output, input);
            assert!(outcome.operations.is_empty());
            assert_eq!(outcome.metrics.original_rows, count);
            assert_eq!(outcome.metrics.retained_rows, count);
            assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
        }
    }
}

#[test]
fn recovery_capabilities_never_change_full_table_compaction_or_access_storage() {
    let store = InMemoryStore::new();
    let unavailable = FailingStore;
    for delimiter in [',', '\t'] {
        for input in [
            large_table(delimiter),
            format!("a{delimiter}b\n1{delimiter}2\n3{delimiter}4"),
        ] {
            let expected = TabularCompressor.compress(&input);
            for recovery in [
                RecoveryMethod::None,
                RecoveryMethod::Shell,
                RecoveryMethod::tool("table_retrieve").unwrap(),
            ] {
                for stash in [
                    None,
                    Some(&store as &dyn StashStore),
                    Some(&unavailable as &dyn StashStore),
                ] {
                    let result = TabularCompressor.compress_with_recovery(&input, stash, &recovery);
                    assert_eq!(result.output, expected.output);
                    assert_eq!(result.operations, expected.operations);
                    assert_eq!(result.metrics, expected.metrics);
                    assert!(result.stash_writes.is_empty());
                    assert_eq!(result.recoverability, crate::Recoverability::Lossless);
                }
            }
        }
    }
    assert!(store.is_empty());
}

struct FailingStore;

impl StashStore for FailingStore {
    fn stash(&self, _: &str) -> Result<StashWrite, StashError> {
        panic!("tabular compaction must not write storage")
    }
    fn retrieve(&self, _: &str) -> Result<Option<String>, StashError> {
        panic!("tabular compaction must not read storage")
    }
    fn len(&self) -> usize {
        panic!("tabular compaction must not inspect storage")
    }
    fn evict_expired(&self) -> Result<usize, StashError> {
        panic!("tabular compaction must not evict storage")
    }
    fn delete(&self, _: &str, _: u64) -> Result<bool, StashError> {
        panic!("tabular compaction must not delete storage")
    }
}

#[test]
fn trailing_record_separator_is_removed_only_when_estimated_tokens_decrease() {
    for delimiter in [',', '\t'] {
        for (last, compacted) in [("45", true), ("4", false)] {
            let input = format!("a{delimiter}b\n1{delimiter}2\n3{delimiter}{last}\n");
            let result = TabularCompressor.compress(&input);
            if compacted {
                assert_eq!(result.output, input.trim_end_matches('\n'));
                assert_eq!(result.operations, [TabularOperation::Compaction]);
                assert_eq!(estimate_tokens(&input) - estimate_tokens(&result.output), 1);
            } else {
                assert_eq!(result.output, input);
                assert!(result.operations.is_empty());
            }
            assert_eq!(
                cells(&input, delimiter as u8),
                cells(&result.output, delimiter as u8)
            );
            assert_eq!(result.metrics.original_rows, 2);
            assert_eq!(result.metrics.retained_rows, 2);
            assert!(result.stash_writes.is_empty());
        }
    }
}
