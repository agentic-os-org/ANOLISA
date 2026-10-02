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
        let outcome = TabularCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
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
            let outcome =
                TabularCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
            assert_eq!(outcome.operations, [TabularOperation::Compaction]);
            assert_eq!(&cells(&input, delimiter)[0][0], "\u{feff}id");
            assert_eq!(cells(&input, delimiter), cells(&outcome.output, delimiter));
            assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
            assert!(outcome.stash_writes.is_empty());
        }
    }
}

#[test]
fn row_reduction_requires_column_labels() {
    for input in [
        (0..100)
            .map(|i| format!("def function_{i:03}(a, b): return a + b\n"))
            .collect::<String>(),
        (0..60)
            .map(|i| format!("assert_equal(expected_value_number_{i}, actual_value_number_{i})\n"))
            .collect(),
        (0..60)
            .map(|i| format!("- Note {i}, the parser rejected the header row while processing request {i} in the ingestion pipeline\n"))
            .collect(),
    ] {
        let store = InMemoryStore::new();
        let outcome =
            TabularCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        assert!(!outcome.operations.contains(&TabularOperation::RowReduction));
        assert_eq!(cells(&input, b','), cells(&outcome.output, b','));
        assert!(store.is_empty());
    }
    // All-string data, two columns and non-ASCII labels still support reduction.
    for header in ["id,message", "编号,内容", "id,", "id,id"] {
        let input = format!(
            "{header}\n{}",
            "record,ordinary payload that is long enough to benefit from selection\n".repeat(100)
        );
        let store = InMemoryStore::new();
        let outcome =
            TabularCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(outcome.operations, [TabularOperation::RowReduction]);
    }
}

#[test]
fn fragmented_diagnostics_keep_all_rows_without_a_large_range_notice() {
    let input = format!(
        "id,status,message\n{}",
        (0..2000)
            .map(|i| format!(
                "{i},{},{}\n",
                if i % 2 == 0 { "ERROR" } else { "ok" },
                "payload ".repeat(10)
            ))
            .collect::<String>()
    );
    let store = InMemoryStore::new();
    let outcome =
        TabularCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
    assert!(!outcome.operations.contains(&TabularOperation::RowReduction));
    assert_eq!(outcome.metrics.retained_rows, 2000);
    assert_eq!(cells(&input, b','), cells(&outcome.output, b','));
    assert!(!outcome.output.contains("[Table:"));
    assert!(store.is_empty());
}

#[test]
fn full_data_wins_before_sampling_when_it_saves_fifteen_percent() {
    let input = format!(
        "\"a\",\"b\",\"c\"\r\n{}",
        "\"x\",\"y\",\"z\"\r\n".repeat(100)
    );
    let store = InMemoryStore::new();
    let outcome =
        TabularCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
    assert_eq!(outcome.operations, [TabularOperation::Compaction]);
    assert_eq!(outcome.metrics.retained_rows, 100);
    assert_eq!(cells(&input, b','), cells(&outcome.output, b','));
    assert!(store.is_empty());
}

#[test]
fn row_selection_preserves_diagnostics_and_recovers_exact_source() {
    for delimiter in [',', '\t'] {
        for recovery in [
            RecoveryMethod::Shell,
            RecoveryMethod::tool("table_retrieve").unwrap(),
        ] {
            let input = large_table(delimiter);
            let store = InMemoryStore::new();
            let result = TabularCompressor.compress_with_recovery(&input, Some(&store), &recovery);
            assert_eq!(result.operations, [TabularOperation::RowReduction]);
            assert_eq!(result.recoverability, crate::Recoverability::Retrievable);
            assert_eq!(result.metrics.retained_rows, 32);
            let (view, note) = result.output.split_once("\n\n[Table:").unwrap();
            let original = cells(&input, delimiter as u8);
            let kept = cells(view, delimiter as u8);
            assert_eq!(kept.len(), 33);
            assert_eq!(kept[0], original[0]);
            let indices = kept[1..]
                .iter()
                .map(|row| row[0].parse::<usize>().unwrap())
                .collect::<Vec<_>>();
            assert!(indices.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(indices.contains(&61));
            assert_eq!(&indices[..4], &[0, 1, 2, 3]);
            assert_eq!(&indices[28..], &[116, 117, 118, 119]);
            for (row, index) in kept[1..].iter().zip(&indices) {
                assert_eq!(row, &original[index + 1]);
            }
            assert!(note.contains("kept 32 of 120"));
            let ranges = note
                .split_once("excluding header): ")
                .unwrap()
                .1
                .split_once(". Incomplete")
                .unwrap()
                .0;
            let reported: Vec<usize> = ranges
                .split(',')
                .flat_map(|range| {
                    let (start, end) = range.split_once('-').unwrap_or((range, range));
                    start.parse::<usize>().unwrap()..=end.parse::<usize>().unwrap()
                })
                .collect();
            assert_eq!(reported, indices.iter().map(|i| i + 1).collect::<Vec<_>>());
            assert!(note.contains("Incomplete table"));
            assert_eq!(
                store
                    .retrieve(&result.stash_writes[0].key)
                    .unwrap()
                    .as_deref(),
                Some(input.as_str())
            );
            assert!(result.output.contains(&recovery_instruction(
                &result.stash_writes[0].key,
                &recovery
            )));
            assert!(saves_both(&result.output, &input));
            let second = TabularCompressor.compress_with_recovery(&input, Some(&store), &recovery);
            assert_eq!(second.output, result.output);
        }
    }
}

#[test]
fn diagnostic_rows_are_not_capped_and_signals_use_word_boundaries() {
    for value in [
        "ERROR",
        "failed",
        "failure",
        "fatal",
        "panic",
        "exception",
        "warn",
        "warning",
        "发生错误",
        "任务失败",
        "警告",
    ] {
        assert!(has_diagnostic(value), "{value}");
    }
    for value in [
        "error_count",
        "warnings",
        "prefailed",
        "panicology",
        "forward",
    ] {
        assert!(!has_diagnostic(value), "{value}");
    }
    let rows: Vec<_> = (0..100)
        .map(|i| {
            StringRecord::from(vec![
                i.to_string(),
                if (10..60).contains(&i) { "error" } else { "ok" }.to_string(),
            ])
        })
        .collect();
    let selected = select_rows(&rows);
    assert_eq!(selected.len(), 58);
    assert!((10..60).all(|i| selected.contains(&i)));
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
    let outcome = TabularCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
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
fn no_recovery_no_savings_and_small_tables_never_omit_rows() {
    let store = InMemoryStore::new();
    for input in ["a,b\n1,2\n3,4".to_string(), large_table(',')] {
        for (stash, recovery) in [
            (None, RecoveryMethod::Shell),
            (Some(&store as &dyn StashStore), RecoveryMethod::None),
        ] {
            let outcome = TabularCompressor.compress_with_recovery(&input, stash, &recovery);
            assert_eq!(cells(&outcome.output, b','), cells(&input, b','));
            assert!(outcome.stash_writes.is_empty());
        }
    }
    let input = format!("a,b\n{}", "x,y\n".repeat(33));
    let result =
        TabularCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
    assert_eq!(result.metrics.retained_rows, 33);
    assert!(store.is_empty());
}

#[test]
fn reduction_starts_above_thirty_two_data_rows() {
    for count in [32, 33] {
        let input = format!(
            "id,message\n{}",
            (0..count)
                .map(|i| format!("{i},record-{i}-{}", "payload ".repeat(100)))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let store = InMemoryStore::new();
        let result =
            TabularCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(result.metrics.retained_rows, 32);
        if count == 32 {
            assert_eq!(result.output, input);
            assert!(store.is_empty());
        } else {
            assert_eq!(result.operations, [TabularOperation::RowReduction]);
            assert_eq!(store.len(), 1);
        }
    }
}

struct FailingStore;

impl StashStore for FailingStore {
    fn stash(&self, _: &str) -> Result<StashWrite, StashError> {
        Err(StashError::Backend("disk full".into()))
    }
    fn retrieve(&self, _: &str) -> Result<Option<String>, StashError> {
        unreachable!()
    }
    fn len(&self) -> usize {
        0
    }
    fn evict_expired(&self) -> Result<usize, StashError> {
        unreachable!()
    }
    fn delete(&self, _: &str, _: u64) -> Result<bool, StashError> {
        unreachable!()
    }
}

#[test]
fn storage_failure_keeps_all_cells_and_reports_the_failure() {
    let input = large_table(',');
    let result = TabularCompressor.compress_with_recovery(
        &input,
        Some(&FailingStore),
        &RecoveryMethod::Shell,
    );
    assert_eq!(result.metrics.stash_errors, 1);
    assert_eq!(result.metrics.retained_rows, 120);
    assert_eq!(result.recoverability, crate::Recoverability::Lossless);
    assert!(!result.output.contains("[Table:"));
    assert_eq!(cells(&input, b','), cells(&result.output, b','));
}
