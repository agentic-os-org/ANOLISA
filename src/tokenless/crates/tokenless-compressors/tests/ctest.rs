//! Independent reconstruction and boundary checks for CTest structural compaction.

use std::path::PathBuf;

use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};
use tokenless_compressors::{BuildLogCompressor, BuildLogOperation, Recoverability};
use tokenless_protocol::{RecoveryToolName, estimate_tokens};

#[path = "common/ctest_oracle.rs"]
mod oracle;
use oracle::decode;

const CTEST_HEADER: &str =
    "CTest templates (kind<TAB>fields; SP/SK/SD=start+result, P/K/D=result, s/S=start, B=blank): ";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs/ctest")
}

fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut padded = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, val) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(val);
        }
    }
    h.iter().map(|w| format!("{w:08x}")).collect()
}

fn check(input: &str, expect_compacted: bool) -> String {
    let store = InMemoryStore::new();
    let outcome =
        BuildLogCompressor.compress_with_recovery(input, Some(&store), &RecoveryMethod::Shell);
    let cleaned = oracle::strip_sgr(input);
    assert_eq!(decode(&outcome.output), cleaned);
    assert_eq!(
        outcome.output.contains(CTEST_HEADER),
        expect_compacted,
        "unexpected compaction state for input starting with: {:?}",
        input.lines().take(3).collect::<Vec<_>>()
    );
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(store.len(), 0);
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
    if expect_compacted {
        assert!(outcome.operations.contains(&BuildLogOperation::Reduction));
        assert!(estimate_tokens(&outcome.output) < estimate_tokens(&cleaned));
        assert!(outcome.output.chars().count() < cleaned.chars().count());
    }
    let again = BuildLogCompressor.compress_with_recovery(
        &outcome.output,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(again.output, outcome.output);
    outcome.output
}

#[test]
fn provenance_matches_all_committed_fixtures() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );

    let dir = fixtures_dir();
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("provenance.json")).unwrap())
            .unwrap();
    assert_eq!(entries.len(), 6);
    for entry in &entries {
        let name = entry["file"].as_str().unwrap();
        let bytes = std::fs::read(dir.join(name)).unwrap();
        assert_eq!(
            sha256_hex(&bytes),
            entry["sha256"].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn committed_fixtures_round_trip_without_stash_and_match_goldens() {
    let dir = fixtures_dir();
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("provenance.json")).unwrap())
            .unwrap();

    for entry in &entries {
        let name = entry["file"].as_str().unwrap();
        let raw = std::fs::read_to_string(dir.join(name)).unwrap();
        let cleaned = oracle::strip_sgr(&raw);
        let exit_code = entry["capture"]["exit_code"].as_i64().unwrap();

        assert!(BuildLogCompressor::detect(&raw), "{name}");
        for recovery in [
            RecoveryMethod::None,
            RecoveryMethod::Shell,
            RecoveryMethod::Tool {
                name: RecoveryToolName::try_from("tokenless_retrieve".to_owned()).unwrap(),
            },
        ] {
            let store = InMemoryStore::new();
            let outcome = BuildLogCompressor.compress_with_recovery(&raw, Some(&store), &recovery);
            assert_eq!(decode(&outcome.output), cleaned, "{name}");
            assert!(outcome.stash_writes.is_empty(), "{name}");
            assert_eq!(store.len(), 0, "{name}");
            assert_eq!(outcome.recoverability, Recoverability::Lossless, "{name}");
            assert!(outcome.output.contains(CTEST_HEADER), "{name}");
            assert!(
                estimate_tokens(&outcome.output) < estimate_tokens(&cleaned),
                "{name}"
            );
            assert!(
                outcome.output.chars().count() < cleaned.chars().count(),
                "{name}"
            );

            let no_stash = BuildLogCompressor.compress_with_recovery(&raw, None, &recovery);
            assert_eq!(no_stash.output, outcome.output, "{name}");

            let again =
                BuildLogCompressor.compress_with_recovery(&outcome.output, Some(&store), &recovery);
            assert_eq!(again.output, outcome.output, "{name}");

            if exit_code != 0 {
                assert!(outcome.output.contains("The following tests FAILED:"));
                assert!(outcome.output.contains("online::customcert::file"));
                assert!(outcome.output.contains("***Failed"));
            }
        }

        if matches!(name, "brotli_serial.txt" | "brotli_parallel.txt") {
            let golden = dir.join(name.replace(".txt", ".expected.txt"));
            let outcome =
                BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
            assert_eq!(
                outcome.output,
                std::fs::read_to_string(golden).unwrap(),
                "{name}"
            );
        }

        let outcome = BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
        println!(
            "FIXTURE {name}: raw={} cleaned={} candidate={}",
            estimate_tokens(&raw),
            estimate_tokens(&cleaned),
            estimate_tokens(&outcome.output)
        );
    }
}

#[test]
fn real_excerpt_boundary_mutations_keep_diagnostics_and_control_sequences_verbatim() {
    let raw = std::fs::read_to_string(fixtures_dir().join("brotli_parallel.txt")).unwrap();
    let sample_lines = raw
        .split_inclusive('\n')
        .skip(2)
        .take(26)
        .collect::<Vec<_>>();

    let injections = [
        " 99/99 Test #99: injected_failure ......................***Failed    0.05 sec\n",
        " 99/99 Test #99: injected_timeout ......................***Timeout   1.00 sec\n",
        " 99/99 Test #99: injected_segfault .....................***Exception: SegFault  0.02 sec\n",
        "Assertion failed: expected 1 == 2\n",
        "progress 50%\rprogress 100%\n",
        "  \x1b[0K\x1b[32m14 passing \x1b[90m(11.6s)\x1b[m\n",
        "\t  99 - injected_failure (Failed)\n",
        "unclassified stdout notice\n",
    ];
    for pos in 0..=sample_lines.len() {
        for injection in injections {
            let mut lines = sample_lines
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>();
            lines.insert(pos, injection.to_owned());
            let mutated = lines.concat();
            let cleaned = oracle::strip_sgr(&mutated);
            let outcome =
                BuildLogCompressor.compress_with_recovery(&mutated, None, &RecoveryMethod::None);
            assert_eq!(
                decode(&outcome.output),
                cleaned,
                "pos={pos}, inj={injection:?}"
            );
            assert!(
                outcome.output.contains(&oracle::strip_sgr(injection)),
                "pos={pos}, inj={injection:?}"
            );
            assert!(outcome.stash_writes.is_empty());
            assert_eq!(outcome.recoverability, Recoverability::Lossless);
        }
    }
}

#[test]
fn routing_fences_crlf_and_anchored_fallback_keep_safety_boundaries() {
    for negative in [
        "      Start  1: solo\n",
        " 1/1 Test  #1: solo .............................   Passed    0.00 sec\n",
        "let text = \"1/2 Test #1: foo ... Passed 0.01 sec\";\n",
        "```text\nTest project /work/build\n 1/2 Test #1: a ...   Passed    0.01 sec\n 2/2 Test #2: b ...   Passed    0.01 sec\n```\n",
        "~~~text\nTest project /work/build\n 1/2 Test #1: a ...   Passed    0.01 sec\n 2/2 Test #2: b ...   Passed    0.01 sec\n~~~\n",
    ] {
        assert!(!BuildLogCompressor::detect(negative), "{negative}");
        check(negative, false);
    }

    let valid = "Test project /work/build\n\
                 1/2 Test #1: alpha ............................   Passed    0.01 sec\n\
                 2/2 Test #2: beta .............................   Passed    0.01 sec\n";
    let crlf = valid.replace('\n', "\r\n");
    assert!(!BuildLogCompressor::detect(&crlf));
    check(&crlf, false);

    let gapped_open_fence = format!("```text\n{}{valid}", "example line\n".repeat(120));
    assert!(!BuildLogCompressor::detect(&gapped_open_fence));

    let anchored_fallback = format!(
        "$ ctest\nTest project /work/build\n      Start  1: runningSuite\n{} 1/1 Test  #1: runningSuite .....................   Passed    0.01 sec\nexit code: 0\n",
        (1..=12)
            .map(|i| format!("processing asset item {i} of 12 in CTest suite\n"))
            .collect::<String>()
    );
    let store = InMemoryStore::new();
    let fallback = BuildLogCompressor.compress_with_recovery(
        &anchored_fallback,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(fallback.recoverability, Recoverability::Retrievable);
    assert_eq!(fallback.stash_writes.len(), 1);
    let marker = fallback
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let recovered = store
        .retrieve(&fallback.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        fallback.output.replacen(marker, &recovered, 1),
        anchored_fallback
    );

    let example: String = (1..=10)
        .map(|i| {
            let tag = format!("#{i}");
            format!(" {i:2}/10 Test {tag:>3}: example/case_{i:02} ....................   Passed    0.01 sec\n")
        })
        .collect();
    let fenced = format!("```text\n{example}```\n");
    let fenced_in_build = format!(
        "Test project /work/build\n 1/2 Test #1: outer_a .........................   Passed    0.01 sec\n 2/2 Test #2: outer_b .........................   Passed    0.01 sec\n{fenced}100% tests passed, 0 tests failed out of 2\n"
    );
    let fenced_out = check(&fenced_in_build, false);
    assert!(fenced_out.contains(&fenced));

    let block_tests: String = (1..=10)
        .map(|i| {
            let tag = format!("#{i}");
            format!(" {i:2}/10 Test {tag:>3}: suite/case_{i:02} .....................   Passed    0.01 sec\n")
        })
        .collect();
    let quoted_template = format!(
        "```text\n{CTEST_HEADER}{{\"rows\":1,\"total\":\"1\",\"start_indent\":6,\"idx_width\":1,\"id_width\":1,\"col\":40,\"prefixes\":[]}}\nSP\t1\t\tex\t0.01\n```\n"
    );
    let with_quoted =
        format!("Test project /work/build\n{block_tests}{quoted_template}{block_tests}");
    let out = check(&with_quoted, true);
    assert!(out.contains(&quoted_template));

    let mut parallel_slow_fail = String::from(concat!(
        "$ ctest -j 2 --output-on-failure || true\n",
        "Test project /work/build\n",
        "      Start  1: suite/slow_fail\n",
    ));
    for id in 2..=13 {
        let ord = id - 1;
        let tag = format!("#{id}");
        let name = format!("suite/fast_{ord:02}");
        parallel_slow_fail.push_str(&format!("      Start {id:2}: {name}\n"));
        parallel_slow_fail.push_str(&format!(
            "{ord:2}/13 Test {tag:>3}: {name} .................................   Passed    0.01 sec\n"
        ));
    }
    parallel_slow_fail.push_str(
        "13/13 Test  #1: suite/slow_fail ................................***Failed    1.25 sec\n\
         assertion failed: expected ok\nexit code: 0\n",
    );
    let parallel_out = check(&parallel_slow_fail, true);
    assert!(parallel_out.contains("      Start  1: suite/slow_fail\n"));
    assert!(parallel_out.contains("***Failed"));
    assert!(!parallel_out.lines().any(|line| {
        line == "s\t1" || (line.starts_with("S\t") && line.contains("suite/slow_fail"))
    }));
}

#[test]
fn captured_ctest_corpus_round_trips_without_stash_and_saves_tokens() {
    let Some(root) = std::env::var_os("CTEST_CAPTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let runs = ["ctest_serial", "ctest_parallel"];
    let streams = ["stdout", "stderr", "observed"];
    let projects = ["brotli", "catch2", "fmt", "libgit2"];
    let mut checked = 0usize;

    for project in projects {
        for run in runs {
            for stream in streams {
                let path = root.join(project).join(run).join(format!("{stream}.txt"));
                let raw = std::fs::read_to_string(&path).unwrap();
                let cleaned = oracle::strip_sgr(&raw);
                let store = InMemoryStore::new();
                let outcome = BuildLogCompressor.compress_with_recovery(
                    &raw,
                    Some(&store),
                    &RecoveryMethod::Shell,
                );
                assert_eq!(decode(&outcome.output), cleaned, "{}", path.display());
                assert!(outcome.stash_writes.is_empty(), "{}", path.display());
                assert_eq!(store.len(), 0, "{}", path.display());
                assert_eq!(outcome.recoverability, Recoverability::Lossless);
                let again = BuildLogCompressor.compress_with_recovery(
                    &outcome.output,
                    Some(&store),
                    &RecoveryMethod::Shell,
                );
                assert_eq!(again.output, outcome.output, "{}", path.display());
                if stream != "stderr" {
                    assert!(
                        outcome.output.contains(CTEST_HEADER),
                        "expected CTest compaction on {}",
                        path.display()
                    );
                    assert!(estimate_tokens(&outcome.output) < estimate_tokens(&cleaned));
                    assert!(outcome.output.chars().count() < cleaned.chars().count());
                } else {
                    assert_eq!(outcome.output, cleaned);
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 24);
}

#[test]
#[ignore = "rewrites reviewed goldens; run explicitly after an intentional format change"]
fn regenerate_ctest_goldens() {
    let dir = fixtures_dir();
    for name in ["brotli_serial.txt", "brotli_parallel.txt"] {
        let path = dir.join(name);
        let raw = std::fs::read_to_string(&path).unwrap();
        let outcome = BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
        assert_eq!(decode(&outcome.output), oracle::strip_sgr(&raw));
        let expected = dir.join(name.replace(".txt", ".expected.txt"));
        std::fs::write(expected, outcome.output).unwrap();
    }
}
