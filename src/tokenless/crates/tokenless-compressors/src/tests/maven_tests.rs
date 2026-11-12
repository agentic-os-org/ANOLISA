/// Maven download facts: (action, repository, url) triples in original order.
#[derive(Debug, PartialEq, Eq)]
struct MavenDownloadFact {
    action: String,
    repository: String,
    url: String,
}

fn maven_download_facts(text: &str) -> Vec<MavenDownloadFact> {
    let mut facts = Vec::new();
    for line in text.lines() {
        let record = line.strip_prefix("[INFO] ").unwrap_or(line);
        let (action, rest) = if let Some(rest) = record.strip_prefix("Downloaded from ") {
            ("Downloaded", rest)
        } else if let Some(rest) = record.strip_prefix("Downloading from ") {
            ("Downloading", rest)
        } else {
            continue;
        };
        if let Some((repository, url)) = rest.split_once(": ") {
            facts.push(MavenDownloadFact {
                action: action.to_owned(),
                repository: repository.to_owned(),
                url: url.to_owned(),
            });
        }
    }
    facts
}

/// Compact form decodes rows `get <tail>` / `ok <tail>` under the block
/// header, in both the `[INFO]`-prefixed and bare record shapes.
fn maven_compact_download_facts(text: &str) -> Vec<MavenDownloadFact> {
    let mut facts = Vec::new();
    let mut repository = None;
    let mut prefix = None;
    for line in text.lines() {
        let record = line.strip_prefix("[INFO] ").unwrap_or(line);
        let header = record
            .strip_prefix("download records from ")
            .or_else(|| record.strip_prefix("downloading records from "))
            .or_else(|| record.strip_prefix("downloaded records from "));
        if let Some(rest) = header {
            // header: <repo>: <prefix>
            if let Some((repo, shared)) = rest.split_once(": ") {
                repository = Some(repo.to_owned());
                prefix = Some(shared.to_owned());
            }
            continue;
        }
        let (Some(repository), Some(prefix)) = (&repository, &prefix) else {
            continue;
        };
        if let Some(tail) = record.strip_prefix("get ") {
            facts.push(MavenDownloadFact {
                action: "Downloading".to_owned(),
                repository: repository.clone(),
                url: format!("{prefix}{tail}"),
            });
        } else if let Some(tail) = record.strip_prefix("ok ") {
            facts.push(MavenDownloadFact {
                action: "Downloaded".to_owned(),
                repository: repository.clone(),
                url: format!("{prefix}{tail}"),
            });
        }
    }
    facts
}

#[test]
fn maven_layout_preserves_download_facts_through_an_independent_decoder() {
    let input = include_str!("../../tests/fixtures/build_logs/maven_success.txt");
    let outcome = BuildLogCompressor.compress_with_recovery(input, None, &RecoveryMethod::None);
    assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
    assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
    assert!(outcome.stash_writes.is_empty());
    let original = maven_download_facts(input);
    assert_eq!(original.len(), 14);
    assert_eq!(maven_compact_download_facts(&outcome.output), original);
    // Idempotence: a second pass changes nothing.
    let second =
        BuildLogCompressor.compress_with_recovery(&outcome.output, None, &RecoveryMethod::None);
    assert_eq!(second.output, outcome.output);
    assert!(second.operations.is_empty());
}

#[test]
fn maven_batch_fixture_stays_lossless_and_keeps_order() {
    let input = include_str!("../../tests/fixtures/build_logs/maven_batch_downloads.txt");
    let outcome = BuildLogCompressor.compress_with_recovery(input, None, &RecoveryMethod::None);
    // 40 rows under every bound: structural compaction applies without a stash.
    assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
    let original = maven_download_facts(input);
    assert_eq!(maven_compact_download_facts(&outcome.output), original);
    let second =
        BuildLogCompressor.compress_with_recovery(&outcome.output, None, &RecoveryMethod::None);
    assert_eq!(second.output, outcome.output);
    assert!(second.operations.is_empty());
}

#[test]
fn maven_failure_fixture_is_never_partially_compacted() {
    let input = include_str!("../../tests/fixtures/build_logs/maven_failure.txt");
    let store = tokenless_ccr::InMemoryStore::new();
    let outcome = BuildLogCompressor.compress_with_recovery(input, Some(&store), &RecoveryMethod::Shell);
    assert_eq!(outcome.output, input.trim_end_matches('\n').to_owned() + "\n");
    assert!(outcome.operations.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn maven_oversize_block_is_atomic_at_256_rows() {
    for count in [256usize, 257] {
        let mut input = String::new();
        for i in 0..count - 1 {
            input.push_str(&format!(
                "Downloading from central: https://repo.maven.apache.org/maven2/org/slf4j/slf4j-api/2.0.17/row-{i:03}.jar\n"
            ));
        }
        // Detection requires one completed record; real streams always have one.
        input.push_str("Downloaded from central: https://repo.maven.apache.org/maven2/org/slf4j/slf4j-api/2.0.17/row-999.jar\n");
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
        if count == 256 {
            assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
            assert!(outcome.output.chars().count() < input.chars().count());
            let facts = maven_download_facts(&input);
            assert_eq!(maven_compact_download_facts(&outcome.output), facts);
        } else {
            assert_eq!(outcome.output, input);
            assert!(outcome.operations.is_empty());
        }
    }
}

#[test]
fn maven_warning_or_error_never_reopens_a_closed_scope() {
    let block = |start: usize| -> String {
        (start..start + 6)
            .map(|i| {
                format!("[INFO] Downloading from central: https://repo.maven.apache.org/maven2/org/slf4j/slf4j-api/2.0.17/row-{i:03}.jar\n[INFO] Downloaded from central: https://repo.maven.apache.org/maven2/org/slf4j/slf4j-api/2.0.17/row-{i:03}.jar\n")
            })
            .collect()
    };
    for boundary in [
        "[WARNING] ambiguous plugin descriptor\n",
        "[ERROR] failed mid-resolution\n",
        "an unexplained but important line\n",
        "\n",
    ] {
        let input = format!("{}{boundary}{}", block(0), block(20));
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
        assert!(outcome.output.contains(boundary.trim_end_matches('\n')), "boundary dropped: {boundary}");
        let facts = maven_download_facts(&input);
        assert_eq!(maven_compact_download_facts(&outcome.output), facts);
    }
}

#[test]
fn maven_carriage_returns_keep_the_whole_input_verbatim() {
    let mut input = String::new();
    for i in 0..9 {
        input.push_str(&format!(
            "Downloading from central: https://repo.maven.apache.org/maven2/org/slf4j/slf4j-api/2.0.17/row-{i:03}.jar\r"
        ));
    }
    let outcome = BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
}

#[test]
fn maven_solo_and_unknown_downloads_stay_verbatim() {
    // One pair cannot establish a stream; malformed rows end eligibility.
    for fragment in [
        "[INFO] Downloading from central: https://repo.maven.apache.org/maven2/a/b/1.0/b-1.0.jar\n[INFO] Downloaded from central: https://repo.maven.apache.org/maven2/a/b/1.0/b-1.0.jar\n",
        "Downloading from central: https://repo.maven.apache.org/maven2/a/b/1.0/b-1.0.jar\n",
        "[INFO] Downloading from central: bad repo url\n",
        "[INFO] Downloading from central: https://repo.maven.apache.org/maven2/has space.jar\n",
        "[INFO] Downloading from custom repo with spaces: https://repo.maven.apache.org/maven2/a/b/1.0/b-1.0.jar\n",
    ] {
        let mut input = String::new();
        for i in 0..12 {
            input.push_str(&format!("[INFO] some other build record {i}\n"));
        }
        input.push_str(fragment);
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
        assert_eq!(outcome.output, input, "fragment altered: {fragment}");
    }
}

#[test]
fn maven_duplicate_rows_keep_every_receipt_in_order() {
    let mut input = String::new();
    for _ in 0..3 {
        input.push_str("[INFO] Downloading from central: https://repo.maven.apache.org/maven2/org/slf4j/slf4j-api/2.0.13/slf4j-api-2.0.13.jar\n");
        input.push_str("[INFO] Downloaded from central: https://repo.maven.apache.org/maven2/org/slf4j/slf4j-api/2.0.13/slf4j-api-2.0.13.jar (41 kB at 1.5 MB/s)\n");
    }
    // duplicates share a block only if the URL shares a prefix — they do, and
    // the compact rows must preserve all six actions in order.
    let store = tokenless_ccr::InMemoryStore::new();
    let outcome =
        BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
    let facts = maven_download_facts(&input);
    assert_eq!(facts.len(), 6);
    assert_eq!(maven_compact_download_facts(&outcome.output), facts);
    // receipt suffixes (sizes/speeds) are NOT part of bare records here; they
    // make the row unparseable as a bare URL record only when the URL slot
    // contains trailing bytes — this capture uses the "(41 kB...)" receipt on
    // Downloaded, so those rows stay verbatim unless the whole block is bare.
    println!("ops={:?} out_len={} in_len={}", outcome.operations, outcome.output.len(), input.len());
}

