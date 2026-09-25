// Detector contract tests: one positive case per taxonomy class, plus the
// adversarial orderings the detector documents (a log containing a traceback
// stays a build log; a diff full of error lines stays a diff).

#[test]
fn detects_json() {
    assert_eq!(
        detect(r#"[{"id": 1, "state": "open"}, {"id": 2, "state": "closed"}]"#),
        ContentType::Json
    );
    assert_eq!(
        detect("{\n  \"items\": [1, 2, 3],\n  \"total\": 3\n}"),
        ContentType::Json
    );
}

#[test]
fn json_exclusion_switches_off_only_the_bracket_sniff() {
    // A real document keeps its class under `detect` and loses it here.
    let json = r#"[{"id": 1, "state": "open"}, {"id": 2, "state": "closed"}]"#;
    assert_eq!(detect(json), ContentType::Json);
    assert_eq!(detect_excluding_json(json), ContentType::PlainText);

    // Shapes the sniff claims but the JSON domain's parse rejects.
    let ndjson = "{\"id\": 1, \"state\": \"open\"}\n{\"id\": 2, \"state\": \"closed\"}\n";
    assert_eq!(detect(ndjson), ContentType::Json);
    assert_eq!(detect_excluding_json(ndjson), ContentType::PlainText);
    let concatenated = r#"{"id": 1, "state": "open"}{"id": 2, "state": "closed"}"#;
    assert_eq!(detect(concatenated), ContentType::Json);
    assert_eq!(detect_excluding_json(concatenated), ContentType::PlainText);
    let bracketed_log = "[2026-09-25 10:00:00] INFO worker 1 handled req-1 status=200 [ok]\n\
                         [2026-09-25 10:00:01] INFO worker 2 handled req-2 status=500 [failed]";
    assert_eq!(detect(bracketed_log), ContentType::Json);
    assert_eq!(detect_excluding_json(bracketed_log), ContentType::PlainText);

    // Classes that never consulted the sniff are unchanged.
    let cargo = "   \u{1b}[1m\u{1b}[32mCompiling\u{1b}[0m serde v1.0.229\n\
                 warning: unused variable: `seam`\n\
                     Finished `release` profile [optimized] target(s) in 42.18s";
    assert_eq!(detect_excluding_json(cargo), ContentType::BuildLog);
    let grep = "src/main.rs:10:fn main() {\n\
                src/lib.rs:42:    let value = compute();\n\
                tests/it.rs:7:fn it_works() {\n\
                src/main.rs:11:    run();";
    assert_eq!(detect_excluding_json(grep), ContentType::SearchResults);
    assert_eq!(detect_excluding_json("   \n\t  "), ContentType::Unknown);
}

#[test]
fn detects_search_results() {
    let grep = "src/main.rs:10:fn main() {\n\
                src/lib.rs:42:    let value = compute();\n\
                tests/it.rs:7:fn it_works() {\n\
                src/main.rs:11:    run();";
    assert_eq!(detect(grep), ContentType::SearchResults);
}

#[test]
fn detects_build_log() {
    let cargo = "   \u{1b}[1m\u{1b}[32mCompiling\u{1b}[0m serde v1.0.229\n\
                 warning: unused variable: `seam`\n\
                     Finished `release` profile [optimized] target(s) in 42.18s";
    assert_eq!(detect(cargo), ContentType::BuildLog);
}

#[test]
fn detects_make_style_build_log() {
    let make = "make: Entering directory '/work/project'\n\
                cc -O2 -c src/main.c -o build/main.o\n\
                ar rcs build/libapp.a build/main.o\n\
                make: Leaving directory '/work/project'";
    assert_eq!(detect(make), ContentType::BuildLog);
}

#[test]
fn go_test_rows_are_build_logs_before_generic_tabular_data() {
    let go = (0..30)
        .map(|index| format!("ok  \tgithub.com/acme/pkg{index:02}\t0.{index:03}s\n"))
        .collect::<String>();
    assert_eq!(detect(&go), ContentType::BuildLog);
}

#[test]
fn log_containing_a_traceback_stays_build_log() {
    let pytest = "$ pytest -q\n\
                  ...F\n\
                  =================================== FAILURES ===================================\n\
                  Traceback (most recent call last):\n\
                    File \"test_hooks.py\", line 118, in test_threshold\n\
                  AssertionError: assert 2048 == 4096\n\
                  1 failed, 74 passed in 6.41s";
    assert_eq!(detect(pytest), ContentType::BuildLog);
}

#[test]
fn detects_stack_traces_that_start_as_one() {
    let python = "Traceback (most recent call last):\n\
                  \x20 File \"app.py\", line 3, in <module>\n\
                  ValueError: bad input";
    assert_eq!(detect(python), ContentType::StackTrace);

    let rust = "thread 'main' panicked at src/main.rs:4:5:\nindex out of bounds";
    assert_eq!(detect(rust), ContentType::StackTrace);

    let java = "Exception in thread \"main\" java.lang.NullPointerException\n\
                \tat com.example.Main.main(Main.java:14)";
    assert_eq!(detect(java), ContentType::StackTrace);

    let go = "panic: runtime error: invalid memory address\n\ngoroutine 1 [running]:";
    assert_eq!(detect(go), ContentType::StackTrace);
}

#[test]
fn detects_diffs_over_their_own_error_lines() {
    let git = "commit 585fbdb9\nAuthor: dev <d@example.com>\n\n    fix\n\n\
               diff --git a/src/lib.rs b/src/lib.rs\n\
               --- a/src/lib.rs\n\
               +++ b/src/lib.rs\n\
               @@ -1,3 +1,3 @@\n\
               -    error: old\n\
               +    error: new";
    assert_eq!(detect(git), ContentType::Diff);

    let bare = "--- before.txt\n+++ after.txt\n@@ -1 +1 @@\n-old\n+new";
    assert_eq!(detect(bare), ContentType::Diff);
}

#[test]
fn detects_html_documents_but_not_fragments() {
    assert_eq!(
        detect("<!DOCTYPE html>\n<html><body>hi</body></html>"),
        ContentType::Html
    );
    // Ambiguous fragments are not classified as HTML (roadmap M4 policy).
    assert_ne!(detect("<div>partial</div>"), ContentType::Html);
}

#[test]
fn html_fragments_inside_other_domains_do_not_make_them_html() {
    let page = "<!DOCTYPE html>\n<html lang=\"en\">\n<body><main><p>Widget reference</p></main></body>\n</html>";
    let prefixed_grep = page
        .lines()
        .enumerate()
        .map(|(index, line)| format!("site/index.html:{}:{line}\n", index + 1))
        .collect::<String>();
    assert_eq!(detect(&prefixed_grep), ContentType::SearchResults);
    let numbered_grep = page
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{}:{line}\n", index + 1))
        .collect::<String>();
    assert_ne!(detect(&numbered_grep), ContentType::Html);
    assert_eq!(
        detect(&format!("{{\"status\": 200, \"body\": {}}}", serde_json::to_string(page).unwrap())),
        ContentType::Json
    );
    let jest = format!(
        "$ npm test\n\n> widget@1.4.0 test\n> jest --ci\n\nFAIL src/render.test.js\n  ● render › markup\n\n    - Snapshot  - 1\n    + Received  + 1\n\n    - {page}\n    + <div class=\"widget\"></div>\n\nTests:       1 failed, 7 passed, 8 total\nTime:        2.41 s\n"
    );
    assert_eq!(detect(&jest), ContentType::BuildLog);
    let diff = format!(
        "diff --git a/site/index.html b/site/index.html\nindex 1111111..2222222 100644\n--- a/site/index.html\n+++ b/site/index.html\n@@ -1,3 +1,3 @@\n {}\n",
        page.replace('\n', "\n ")
    );
    assert_eq!(detect(&diff), ContentType::Diff);
    let traceback = format!(
        "Traceback (most recent call last):\n  File \"fetch.py\", line 8, in main\n    raise RuntimeError(body)\nRuntimeError: {}\n",
        page.replace('\n', "")
    );
    assert_eq!(detect(&traceback), ContentType::StackTrace);
    let csv = format!(
        "id,title,snippet\n{}",
        (0..40)
            .map(|index| format!("{index},Row {index},<p>Widget <b>{index}</b> summary</p>\n"))
            .collect::<String>()
    );
    assert_eq!(detect(&csv), ContentType::Tabular);
    let sfc = "<template>\n  <div class=\"widget\">{{ title }}</div>\n</template>\n<script setup>\nconst title = 'Widget'\n</script>\n";
    assert_ne!(detect(sfc), ContentType::Html);
    let svg = "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect width=\"1\" height=\"1\"/></svg>\n";
    assert_ne!(detect(svg), ContentType::Html);
}

#[test]
fn detects_tabular_content() {
    for input in [
        "name,value\nalice,1\nbob,2",
        "name,value\ralice,1\rbob,2",
        "name,value\n\"alice, a\",1\n\"bob\nb\",2",
        "name\tvalue\nalice\t001\nbob\t002",
    ] {
        assert_eq!(detect(input), ContentType::Tabular);
    }
    assert_eq!(
        detect("name,age,city\nalice,30,berlin\nbob,25,tokyo"),
        ContentType::Tabular
    );
    assert_eq!(detect("a\tb\nc\td\ne\tf"), ContentType::Tabular);
    assert_eq!(
        detect("| col | n |\n|---|---:|\n| x | 1 |"),
        ContentType::Tabular
    );
}

#[test]
fn detects_source_code_on_strong_signals_only() {
    assert_eq!(
        detect("#!/usr/bin/env bash\necho hi"),
        ContentType::SourceCode
    );
    let rust = "use std::fs;\n\
                pub struct Config;\n\
                impl Config {\n\
                fn load() {}\n\
                pub fn save() {}\n\
                use std::io;";
    assert_eq!(detect(rust), ContentType::SourceCode);
    // One keyword in prose is not code.
    assert_eq!(
        detect("please use the new API for this import step"),
        ContentType::PlainText
    );
}

#[test]
fn source_declarations_take_precedence_over_rectangular_commas() {
    let input = (0..100)
        .map(|i| format!("def function_{i:03}(a, b): return a + b\n"))
        .collect::<String>();
    assert_eq!(detect(&input), ContentType::SourceCode);
}

#[test]
fn readable_prose_is_plain_text() {
    assert_eq!(
        detect("压缩按无损、可取回有损、截断三级阶梯递进。检测器必须廉价且确定。"),
        ContentType::PlainText
    );
    assert_eq!(detect("just a short sentence"), ContentType::PlainText);
}

#[test]
fn prose_carrying_build_words_is_not_a_build_log() {
    let mut doc = String::new();
    doc.push_str("Toolchain notes for new contributors.\n\n");
    doc.push_str("We build with gcc on every supported platform. The make: prefix in\n");
    doc.push_str("the transcript below is the recursive build announcing itself.\n\n");
    for i in 0..36 {
        doc.push_str(&format!(
            "Paragraph {i} explains one configuration knob in prose.\n"
        ));
    }
    assert_eq!(detect(&doc), ContentType::PlainText);

    let single = doc.replace("make: ", "recursive ");
    assert_eq!(detect(&single), ContentType::PlainText);
}

#[test]
fn empty_and_binary_are_unknown() {
    assert_eq!(detect(""), ContentType::Unknown);
    assert_eq!(detect("   \n\t  "), ContentType::Unknown);
    let binary = "\u{0}\u{1}\u{2}abc".repeat(50);
    assert_eq!(detect(&binary), ContentType::Unknown);
}

#[test]
fn large_json_beyond_the_scan_window_stays_json() {
    let mut big = String::from("[\n");
    while big.len() <= MAX_SCAN_BYTES {
        big.push_str("  {\"id\": 1, \"state\": \"open\"},\n");
    }
    big.push_str("  {\"id\": 2}\n]");
    assert_eq!(detect(&big), ContentType::Json);
}

#[test]
fn whitespace_padding_beyond_the_tail_window_is_not_json() {
    let padded = format!("{{\"k\": 1}}{}", " ".repeat(MAX_SCAN_BYTES + 1));
    assert_ne!(detect(&padded), ContentType::Json);
}

#[test]
fn timestamped_logs_are_not_search_results() {
    let log = "12:30:00 starting worker\n12:30:01 ready\n12:30:05 done";
    assert_ne!(detect(log), ContentType::SearchResults);
}

#[test]
fn detection_is_deterministic_and_bounded() {
    let large = "some plain prose line about nothing in particular\n".repeat(100_000);
    let first = detect(&large);
    assert_eq!(first, detect(&large));
    assert_eq!(first, ContentType::PlainText);
}

#[test]
fn wire_values_are_stable_and_unique() {
    let all = [
        (ContentType::Json, "json"),
        (ContentType::SearchResults, "search_results"),
        (ContentType::BuildLog, "build_log"),
        (ContentType::StackTrace, "stack_trace"),
        (ContentType::Diff, "diff"),
        (ContentType::Html, "html"),
        (ContentType::Tabular, "tabular"),
        (ContentType::SourceCode, "source_code"),
        (ContentType::PlainText, "plain_text"),
        (ContentType::Unknown, "unknown"),
    ];
    for (ty, wire) in all {
        assert_eq!(ty.wire_str(), wire);
    }
    let mut wires: Vec<&str> = all.iter().map(|(ty, _)| ty.wire_str()).collect();
    wires.sort_unstable();
    wires.dedup();
    assert_eq!(wires.len(), all.len());
}
