// Detector contract tests: one positive case per taxonomy class, plus the
// adversarial orderings the detector documents (a log containing a traceback
// stays a build log; a diff full of error lines stays a diff).

use super::*;

#[test]
fn isolated_maven_download_does_not_override_other_content_routes() {
    let download = "Downloaded from central: https://repo.maven.apache.org/maven2/org/example/sample/1.0/sample-1.0.pom (22 kB at 34 kB/s)\n";
    for (before, after, expected) in [
        ("Transfer example:\n".to_owned(), "", ContentType::PlainText),
        (
            "# Transfer example\n```text\n".to_owned(),
            "```\n",
            ContentType::PlainText,
        ),
        (
            format!("name,count\n{}", "sample,42\n".repeat(400)),
            "",
            ContentType::Tabular,
        ),
        (
            "use std::io;\nuse std::fmt;\nuse std::fs;\nuse std::env;\nfn main() {}\n".to_owned(),
            "",
            ContentType::SourceCode,
        ),
    ] {
        assert_eq!(detect(&format!("{before}{after}")), expected);
        assert_eq!(detect(&format!("{before}{download}{after}")), expected);
    }
}

#[test]
fn maven_examples_do_not_override_the_enclosing_document() {
    let download = "Downloaded from central: https://repo.maven.apache.org/maven2/org/example/sample/1.0/sample-1.0.pom (22 kB at 34 kB/s)\n";
    let phase = "[INFO] --- compiler:3.13.0:compile (default-compile) @ sample ---\n";
    for prefix in ["", "[INFO] "] {
        for count in [2, 30, 400] {
            let transfers = format!("{prefix}{download}").repeat(count);
            for body in [
                transfers.clone(),
                format!("{phase}{transfers}"),
                format!("$ mvn verify\n{phase}{transfers}exit code: 0\n"),
            ] {
                for (before, after, expected) in [
                    ("Transfer example:\n".to_owned(), "", ContentType::PlainText),
                    (
                        "+ /usr/bin/cat README.md\n".to_owned(),
                        "",
                        ContentType::PlainText,
                    ),
                    (
                        "+ env cat README.md\n".to_owned(),
                        "",
                        ContentType::PlainText,
                    ),
                    (
                        "WARNING: Quoted Maven log below:\n".to_owned(),
                        "",
                        ContentType::PlainText,
                    ),
                    (
                        "[INFO] Scanning for projects...\n```text\n".to_owned(),
                        "```\n",
                        ContentType::PlainText,
                    ),
                    (
                        "# Maven example\n```text\n".to_owned(),
                        "```\n",
                        ContentType::PlainText,
                    ),
                    ("~~~text\n".to_owned(), "~~~\n", ContentType::PlainText),
                    (
                        format!("name,count\n{}", "sample,42\n".repeat(400)),
                        "",
                        ContentType::Tabular,
                    ),
                    (
                        "use std::io;\nuse std::fmt;\nuse std::fs;\nuse std::env;\nfn main() {}\n"
                            .to_owned(),
                        "",
                        ContentType::SourceCode,
                    ),
                ] {
                    let input = format!("{before}{body}{after}");
                    assert_eq!(
                        detect(&input),
                        expected,
                        "prefix={prefix:?}, count={count}, before={before:?}"
                    );
                }
            }
            assert!(!tokenless_compressors::BuildLogCompressor::detect(
                &transfers
            ));
        }
    }
}

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
fn a_leading_mark_does_not_hide_the_other_domains() {
    // The mark is format, not content, for every domain's reader — the
    // HTML tokenizer just already had it fixed. Each of these is the
    // twin of the same document without the mark.
    assert_eq!(
        detect("\u{feff}diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-x\n+y\n"),
        ContentType::Diff
    );
    assert_eq!(
        detect(
            "\u{feff}Traceback (most recent call last):\n  File \"t.py\", line 1\nValueError: boom\n"
        ),
        ContentType::StackTrace
    );
    assert_eq!(
        detect("\u{feff}{\"items\": [{\"name\": \"a\", \"description\": \"ddd\"}]}\n"),
        ContentType::Json
    );
    assert_eq!(
        detect("\u{feff}| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n"),
        ContentType::Tabular
    );
    // A second mark is content: still nothing detectable.
    assert_eq!(
        detect("\u{feff}\u{feff}plain words"),
        ContentType::PlainText
    );
}

#[test]
fn detects_a_page_opened_by_a_byte_order_mark() {
    // The tokenizer ignores one leading BOM, so the page behind it is the same
    // document the renderer reads. U+FEFF is not whitespace, so trimming alone
    // leaves the mark in front of the doctype and the page is never routed to
    // the HTML domain.
    for page in [
        "\u{feff}<!DOCTYPE html>\n<html><body>hi</body></html>",
        "\u{feff}<!doctype HTML>\n<html><body>hi</body></html>",
        "\u{feff}<html><body>hi</body></html>",
        "\u{feff}\n<!DOCTYPE html>\n<html><body>hi</body></html>",
    ] {
        assert_eq!(detect(page), ContentType::Html, "{page:?}");
    }
    // Only the first mark is a BOM: behind a second one, or behind any other
    // content, the doctype no longer opens the document.
    assert_ne!(
        detect("\u{feff}\u{feff}<!DOCTYPE html>\n<html><body>hi</body></html>"),
        ContentType::Html
    );
    assert_ne!(
        detect("\n\u{feff}<!DOCTYPE html>\n<html><body>hi</body></html>"),
        ContentType::Html
    );
    // The mark does not turn a fragment or prose into a page.
    assert_ne!(detect("\u{feff}<div>partial</div>"), ContentType::Html);
    assert_ne!(detect("\u{feff}plain words"), ContentType::Html);
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
        detect(&format!(
            "{{\"status\": 200, \"body\": {}}}",
            serde_json::to_string(page).unwrap()
        )),
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
    assert_eq!(
        detect("| col | n |  \r\n|---|---:|\r\n| a\\|b | 1 |  \r\n"),
        ContentType::Tabular
    );
    // Horizontal rule below a single pipe row or mixed prose/fences must not become Tabular.
    assert_eq!(detect("| a | b |\n---\nSome prose"), ContentType::PlainText);
    assert_eq!(
        detect("| a | b |\n|---|---|\n| 1 | 2 |\n\nFollow-up prose."),
        ContentType::PlainText
    );
    let table_then_json = format!(
        "| a | b |\n|---|---|\n| 1 | 2 |\n\n```json\n{}\n```",
        r#"{"items": [1, 2, 3]}"#
    );
    assert_ne!(detect(&table_then_json), ContentType::Tabular);
    let table_then_tilde = "| a | b |\n|---|---|\n| 1 | 2 |\n\n   ~~~rust\nfn f() {}\n   ~~~";
    assert_ne!(detect(table_then_tilde), ContentType::Tabular);
    // Standalone GFM table with multi-byte cells and escaped pipes stays Tabular.
    assert_eq!(
        detect("| label | value |\n|---|---|\n| café \\| 中 | 42 |\n"),
        ContentType::Tabular
    );
    // Regression: exhausting MAX_SCAN_LINES must not pop a trailing prose line and
    // misclassify a large table + prose document as a standalone table.
    let mut overflow_then_prose = String::from("| a | b |\n|---|---|\n");
    for i in 0..197 {
        overflow_then_prose.push_str(&format!("| {i} | {i} |\n"));
    }
    overflow_then_prose.push_str("Notes:\ntrailing prose that must break the table");
    assert_eq!(detect(&overflow_then_prose), ContentType::PlainText);
    // Over-window table followed by fenced JSON must fall through cleanly rather
    // than blocking mixed-content routing.
    let mut big_table_then_json = String::from("| a | b |\n|---|---|\n");
    for i in 0..250 {
        big_table_then_json.push_str(&format!("| {i} | {i} |\n"));
    }
    big_table_then_json.push_str("\n```json\n{\"items\": [1, 2, 3]}\n```\n");
    assert_ne!(detect(&big_table_then_json), ContentType::Tabular);

    // GFM Example 199: optional edge pipes on data row.
    assert_eq!(
        detect("| baz | bar |\n| --- | --- |\nhello | world"),
        ContentType::Tabular
    );
    // GFM Example 204: data rows with fewer or more cells than header.
    assert_eq!(
        detect("| foo | bar |\n| --- | --- |\n| baz |\n| boo | foo | goo |"),
        ContentType::Tabular
    );

    // Block boundary checks: interior blank lines break table blocks.
    assert_eq!(
        detect("| a | b |\n\n|---|---|\n| 1 | 2 |"),
        ContentType::PlainText
    );
    assert_eq!(
        detect("| a | b |\n|---|---|\n| 1 | 2 |\n\n| c | d |\n|---|---|\n| 3 | 4 |"),
        ContentType::PlainText
    );
    assert_eq!(
        detect("| a | b |\n|---|---|\n| 1 | 2 |\n\npipe | in | prose"),
        ContentType::PlainText
    );
    // GFM tab stop rule: space + tab evaluates to column 4, starting an indented code block.
    assert_eq!(
        detect(" \t| a | b |\n \t|---|---|\n \t| 1 | \"2\" |"),
        ContentType::PlainText
    );
    // Block boundaries (GFM §4.6 & §4.12): blockquotes, headings, HTML blocks (types 1–6),
    // fences, or lists after a table break Tabular; inline Type-7 HTML stays part of the row.
    assert_eq!(
        detect("| a | b |\n|---|---|\n| 1 | 2 |\n> follow-up | quoted"),
        ContentType::PlainText
    );
    assert_eq!(
        detect("| a | b |\n|---|---|\n| 1 | 2 |\n# result | notes"),
        ContentType::PlainText
    );
    assert_eq!(
        detect("| a | b |\n|---|---|\n| 1 | 2 |\n<div>follow-up | notes</div>"),
        ContentType::PlainText
    );
    assert_eq!(
        detect("| a | b |\n|---|---|\n| 1 | 2 |\n<!-- follow-up | notes -->"),
        ContentType::PlainText
    );
    assert_eq!(
        detect("| a | b |\n|---|---|\n| 1 | 2 |\n<span>inline | ok</span>"),
        ContentType::Tabular
    );
    // Standalone TSV with pipe-bearing records (including a `---|---` row) stays Tabular.
    let mut tsv_with_pipe_record = String::from("col1\tcol2\n---\t|---\n");
    for i in 0..8 {
        tsv_with_pipe_record.push_str(&format!("k{i}\tv{i}\n"));
    }
    assert_eq!(detect(&tsv_with_pipe_record), ContentType::Tabular);
    assert_eq!(
        detect("name\tnotes\r\nalice\t|short\r\n---\t|---\r\nbob\tshort\r\n"),
        ContentType::Tabular
    );

    // 64 KiB byte budget: separator after a >64 KiB quoted CSV line must not alter classification.
    let csv_byte_prefix = format!("name,notes\na,short\nb,short\nc,\"{}\n", "x".repeat(65_536));
    let csv_with_sep_past_64k = format!("{csv_byte_prefix}|---|---|\nend\"\n");
    let csv_with_word_past_64k = format!("{csv_byte_prefix}divider\nend\"\n");
    assert_eq!(
        detect(&csv_with_sep_past_64k),
        detect(&csv_with_word_past_64k)
    );
    assert_eq!(detect(&csv_with_sep_past_64k), ContentType::Tabular);
}

#[test]
fn detects_markdown_table_with_inline_backticks_in_first_cell() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    assert_eq!(
        detect(&format!("{table}```label``` | details\n")),
        ContentType::Tabular
    );
}

#[test]
fn preserves_pipe_free_gfm_data_row_as_standalone_table() {
    // GFM §4.12: a row without pipes renders as a data row with trailing empty
    // cells. Without a blank line it stays part of the table.
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let array_row = format!("[{}1]\n", "                1,".repeat(20));
    assert_eq!(detect(&format!("{table}{array_row}")), ContentType::Tabular);
}

#[test]
fn blank_line_separated_array_after_table_is_not_standalone() {
    // A blank line ends the table; the array becomes a separate paragraph that
    // may be compressed, so the document is not a standalone table.
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let array_row = format!("[{}1]\n", "                1,".repeat(20));
    assert_eq!(
        detect(&format!("{table}\n{array_row}")),
        ContentType::PlainText
    );
}

#[test]
fn zero_pipe_and_setext_heading_documents_are_not_standalone_tables() {
    let padded = format!("[{}1]\n", "                1,".repeat(20));
    for (name, doc) in [
        ("setext_h2_padded", format!("Summary\n-------\n{padded}")),
        (
            "piped_header_setext_h2_padded",
            format!("| a |\n---\n{padded}"),
        ),
        ("alpha_beta", "alpha\n---\nbeta\ngamma".to_owned()),
        ("triple_dashes", "---\n---\n---".to_owned()),
        ("summary_h2_alone", "Summary\n-------\n".to_owned()),
        ("piped_header_alone", "| a |\n---\n".to_owned()),
    ] {
        assert_eq!(
            detect(&doc),
            ContentType::PlainText,
            "failed zero-pipe/setext case {name}: {doc:?}"
        );
    }
}

#[test]
fn thematic_breaks_and_block_interrupts_after_valid_table_break_tabular() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let padded = format!("[{}1]\n", "                1,".repeat(20));
    for sep in ["---", "***", "___", "_ _ _"] {
        assert_eq!(
            detect(&format!("{table}{sep}\n{padded}")),
            ContentType::PlainText,
            "thematic break {sep} followed by array must route as plain text"
        );
        assert_eq!(
            detect(&format!("{table}{sep}\n")),
            ContentType::PlainText,
            "thematic break {sep} after table must route as plain text"
        );
    }
    // Lone pipe after table or alone
    assert_eq!(
        detect(&format!("{table}|\n{padded}")),
        ContentType::PlainText
    );
    assert_eq!(detect(&format!("{table}|\n")), ContentType::PlainText);
    assert_eq!(detect("|\n"), ContentType::PlainText);

    // CommonMark ordered list item interrupts table
    assert_eq!(
        detect(&format!("{table}10. item\n")),
        ContentType::PlainText
    );
}

#[test]
fn hyphen_controls_in_gfm_tables() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    // Two-hyphen row is not a thematic break (CommonMark §4.9 requires 3+ markers)
    // and stays a valid pipe-free data row inside the table.
    assert_eq!(detect(&format!("{table}--\n")), ContentType::Tabular);
    // Delimiter rows with pipes stay valid GFM tables.
    assert_eq!(
        detect("col1 | col2\n---|---\n1 | 2\n"),
        ContentType::Tabular
    );
    assert_eq!(
        detect("| col1 | col2 |\n|---|---|\n| 1 | 2 |\n"),
        ContentType::Tabular
    );
}

#[test]
fn escaped_pipe_body_rows_and_single_col_tables_stay_tabular() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let padded = format!("[{}1]\n", "                1,".repeat(20));

    // GFM example 200: body row containing only escaped pipes (`left \| right`).
    assert_eq!(
        detect(&format!("{table}left \\| right\n{padded}")),
        ContentType::Tabular
    );
    assert_eq!(
        detect(&format!("{table}| left \\| right |\n{padded}")),
        ContentType::Tabular
    );

    // One-column tables (including pipe-free headers paired with a
    // colon-containing alignment delimiter).
    for one_col in [
        format!("| a |\n:---:\n{padded}"),
        format!("| a |\n:---\n{padded}"),
        format!("| a |\n---:\n{padded}"),
        format!("| a |\n|:---:|\n{padded}"),
        format!("a\n|:---:|\n{padded}"),
        format!("a\n|---|\n{padded}"),
        format!("Summary\n:---:\n{padded}"),
        format!("Summary\n:---\n{padded}"),
        format!("Summary\n---:\n{padded}"),
        format!("Summary\n:-:\n{padded}"),
        format!("Summary\n:--\n{padded}"),
        format!("Summary\n:-\n{padded}"),
        format!("Total revenue\n:---:\n{padded}"),
        format!("left \\| right\n:---:\n{padded}"),
    ] {
        assert_eq!(detect(&one_col), ContentType::Tabular, "{one_col:?}");
    }

    // Setext headings (no alignment colon) and mismatched column counts remain PlainText.
    for non_table in [
        format!("Summary\n---\n{padded}"),
        format!("Summary\n-------\n{padded}"),
        format!("Summary\n--\n{padded}"),
        format!("Summary\n-\n{padded}"),
        format!("| a |\n---\n{padded}"),
        format!("| a |\n--\n{padded}"),
        format!("| a | b |\n:---:\n{padded}"),
    ] {
        assert_eq!(detect(&non_table), ContentType::PlainText, "{non_table:?}");
    }
}

#[test]
fn fenced_code_with_pipes_interrupts_markdown_table() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    for opener in ["```text | details", "~~~text | details"] {
        // An unclosed fence extends to EOF, even when every line has pipes.
        assert_eq!(
            detect(&format!("{table}{opener}\n| code | body |\n")),
            ContentType::PlainText
        );
    }
}

#[test]
fn tab_padded_one_column_gfm_table_before_fence_exits_delimited_fallback() {
    let fence = "\n```json\n{\"items\": [1, 2, 3]}\n```\n";
    for (header, delim, row) in [
        ("| a |\t\n", ":---:\t\n", "| x |\t\n"),
        ("| a |\t\n", ":---\t\n", "| x |\t\n"),
        ("| a |\t\n", "---:\t\n", "| x |\t\n"),
        ("| a |\t\n", ":-\t\n", "| x |\t\n"),
        ("a\t\n", "|:---:|\t\n", "| x |\t\n"),
        ("a\t\n", "|---|\t\n", "| x |\t\n"),
        ("Summary\t\n", ":---:\t\n", "x\t\n"),
    ] {
        // 10 leading tab-terminated records pass TabularCompressor::detect,
        // but the trailing fence breaks rectangular TSV and the one-column
        // GFM table evidence must route the document to PlainText.
        let mut table = String::from(header);
        table.push_str(delim);
        for _ in 0..8 {
            table.push_str(row);
        }
        assert_eq!(
            detect(&format!("{table}{fence}")),
            ContentType::PlainText,
            "mixed one-column GFM + fence should route to PlainText: {header:?} / {delim:?}"
        );
        // Without the trailing fence, the standalone table stays Tabular.
        assert_eq!(
            detect(&table),
            ContentType::Tabular,
            "standalone tab-padded one-column table must stay Tabular: {header:?} / {delim:?}"
        );
    }

    // Valid rectangular TSV containing one-column-like markers stays Tabular.
    let mut rect_tsv = String::from("col\tmeta\n:---:\taligned\n");
    for i in 0..8 {
        rect_tsv.push_str(&format!("row{i}\tval{i}\n"));
    }
    assert_eq!(detect(&rect_tsv), ContentType::Tabular);

    // Quoted multiline CSV containing a one-column GFM table stays Tabular.
    let quoted_csv = "id,note\r\n1,\"| a |\r\n:---:\r\n| x |\"\r\n2,short\r\n3,short\r\n";
    assert_eq!(detect(quoted_csv), ContentType::Tabular);
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
fn detects_plain_path_lists_without_misclassifying_numbers_or_mixed_output() {
    for prefix in [
        "/workspace/project with spaces/",
        "./project/",
        "../project/",
        "src/",
    ] {
        let paths: String = (0..10).map(|i| format!("{prefix}file {i}.rs\n")).collect();
        assert_eq!(detect(&paths), ContentType::SearchResults);
        assert_ne!(
            detect(&format!("{paths}---\n755 uploads/\n")),
            ContentType::SearchResults
        );
    }
    assert_ne!(detect("60\n"), ContentType::SearchResults);
    assert_ne!(
        detect("https://example.com/a\nhttps://example.com/b\nhttps://example.com/c\n"),
        ContentType::SearchResults
    );
    assert_ne!(
        detect("// comment /a\n// comment /b\n// comment /c\n"),
        ContentType::SearchResults
    );
    assert_eq!(
        detect("src/a.rs,1\nsrc/b.rs,2\nsrc/c.rs,3\n"),
        ContentType::Tabular
    );
}

#[test]
fn path_list_detection_only_probes_complete_records_at_the_byte_limit() {
    for (ending, boundary, remaining) in [
        ("\n", " ", "file.json\n"),
        ("\r\n", "\r", "\n"),
        ("\n", "", "中文/file.json\n"),
    ] {
        let head = format!("/workspace/project with spaces/中文/file.json{ending}").repeat(60);
        let prefix = "/workspace/";
        // Put a space, the CR of CRLF, or the first byte of a Unicode scalar
        // exactly at the byte limit without changing any full path record.
        let padding = MAX_SCAN_BYTES - head.len() - prefix.len() - 1;
        let input = format!(
            "{head}{prefix}{}{boundary}{remaining}{head}",
            "a".repeat(padding)
        );
        assert!(input.len() > MAX_SCAN_BYTES);
        assert_eq!(
            detect(&input),
            ContentType::SearchResults,
            "{ending:?} {boundary:?}"
        );
    }
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

#[test]
fn detects_maven_with_and_without_downloads() {
    for content in [
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/maven/jsoup_batch.txt"
        ),
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/maven/commons_warm.txt"
        ),
    ] {
        assert_eq!(detect(content), ContentType::BuildLog);
    }
}

#[test]
fn detects_gradle_cold_and_incremental_logs() {
    for content in [
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/gradle/caffeine_cold.txt"
        ),
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/gradle/resilience4j_incremental.txt"
        ),
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/gradle/mockito_cold_excerpt.txt"
        ),
    ] {
        assert_eq!(detect(content), ContentType::BuildLog);
    }
}

#[test]
fn isolated_gradle_markers_and_quoted_examples_do_not_override_enclosing_documents() {
    let tasks = (0..12)
        .map(|i| format!("> Task :mod-{i}:compileJava UP-TO-DATE\n"))
        .collect::<String>();
    let valid_log = format!("{tasks}BUILD SUCCESSFUL in 2s\n12 actionable tasks: 12 up-to-date\n");
    for (before, after, expected) in [
        ("Build excerpt:\n".to_owned(), "", ContentType::PlainText),
        (
            "+ /usr/bin/cat README.md\n".to_owned(),
            "",
            ContentType::PlainText,
        ),
        (
            "WARNING: Quoted Gradle log below:\n".to_owned(),
            "",
            ContentType::PlainText,
        ),
        (
            "# Gradle example\n```text\n".to_owned(),
            "```\n",
            ContentType::PlainText,
        ),
        ("~~~text\n".to_owned(), "~~~\n", ContentType::PlainText),
        (
            format!("name,count\n{}", "sample,42\n".repeat(400)),
            "",
            ContentType::Tabular,
        ),
        (
            "use std::io;\nuse std::fmt;\nuse std::fs;\nuse std::env;\nfn main() {}\n".to_owned(),
            "",
            ContentType::SourceCode,
        ),
    ] {
        assert_eq!(detect(&format!("{before}{valid_log}{after}")), expected);
    }
    for isolated in [
        "> Task :app:compileJava\n",
        "> Configure project :\nBuilding version '1.0'\n",
        "BUILD SUCCESSFUL in 1s\n",
    ] {
        assert_eq!(detect(isolated), ContentType::PlainText, "{isolated:?}");
    }
}

#[test]
fn detects_serial_parallel_and_failure_ctest_logs() {
    for content in [
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/ctest/brotli_serial.txt"
        ),
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/ctest/brotli_parallel.txt"
        ),
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/ctest/libgit2_offline_failure.txt"
        ),
    ] {
        assert_eq!(detect(content), ContentType::BuildLog);
    }
}

#[test]
fn isolated_ctest_markers_and_quoted_examples_do_not_override_enclosing_documents() {
    let valid_log = concat!(
        "Test project /work/build\n",
        "      Start  1: suite/a\n",
        " 1/2 Test  #1: suite/a ..........................   Passed    0.01 sec\n",
        "      Start  2: suite/b\n",
        " 2/2 Test  #2: suite/b ..........................   Passed    0.01 sec\n",
        "\n",
        "100% tests passed, 0 tests failed out of 2\n",
    );
    for (before, after, expected) in [
        (
            "CTest output example:\n".to_owned(),
            "",
            ContentType::PlainText,
        ),
        (
            "+ /usr/bin/cat ctest.log\n".to_owned(),
            "",
            ContentType::PlainText,
        ),
        (
            "# CTest example\n```text\n".to_owned(),
            "```\n",
            ContentType::PlainText,
        ),
        ("~~~text\n".to_owned(), "~~~\n", ContentType::PlainText),
        (
            format!("name,count\n{}", "sample,42\n".repeat(400)),
            "",
            ContentType::Tabular,
        ),
        (
            "use std::io;\nuse std::fmt;\nuse std::fs;\nuse std::env;\nfn main() {}\n".to_owned(),
            "",
            ContentType::SourceCode,
        ),
    ] {
        assert_eq!(detect(&format!("{before}{valid_log}{after}")), expected);
    }
    for isolated in [
        "Test project /work/build\n",
        "      Start  1: suite/a\n",
        "100% tests passed, 0 tests failed out of 1\n",
    ] {
        assert_eq!(detect(isolated), ContentType::PlainText, "{isolated:?}");
    }
}

#[test]
fn detects_ninja_and_cmake_make_build_logs_and_isolates_quotes() {
    for content in [
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/cmake_build/brotli_ninja_cold.txt"
        ),
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/cmake_build/brotli_make_cold.txt"
        ),
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/cmake_build/fmt_ninja_cold.txt"
        ),
        include_str!(
            "../../../../tokenless-compressors/tests/fixtures/build_logs/cmake_build/fmt_make_incremental.txt"
        ),
    ] {
        assert_eq!(detect(content), ContentType::BuildLog);
    }

    let valid_log = concat!(
        "[1/3] Building CXX object CMakeFiles/app.dir/a.cpp.o\n",
        "[2/3] Building CXX object CMakeFiles/app.dir/b.cpp.o\n",
        "[3/3] Linking CXX executable app\n",
    );
    for (before, after, expected) in [
        (
            "Ninja build example:\n".to_owned(),
            "",
            ContentType::PlainText,
        ),
        (
            "+ /usr/bin/cat ninja.log\n".to_owned(),
            "",
            ContentType::PlainText,
        ),
        (
            "# Build example\n```text\n".to_owned(),
            "```\n",
            ContentType::PlainText,
        ),
        ("~~~text\n".to_owned(), "~~~\n", ContentType::PlainText),
        (
            format!("name,count\n{}", "sample,42\n".repeat(400)),
            "",
            ContentType::Tabular,
        ),
        (
            "use std::io;\nuse std::fmt;\nuse std::fs;\nuse std::env;\nfn main() {}\n".to_owned(),
            "",
            ContentType::SourceCode,
        ),
    ] {
        assert_eq!(detect(&format!("{before}{valid_log}{after}")), expected);
    }
    for isolated in [
        "[1/1] Building CXX object CMakeFiles/app.dir/a.cpp.o\n",
        "[100%] Built target app\n",
        "[1/2] Check configuration\n[2/2] Done\n",
    ] {
        assert_eq!(detect(isolated), ContentType::PlainText, "{isolated:?}");
    }
}

// ---- Browser accessibility snapshots ----

const PW_SNAPSHOT: &str = "### Page\n- Page URL: https://example.com/\n- Page Title: Example\n### Snapshot\n```yaml\n- generic [ref=e1]:\n  - link \"Home\" [ref=e2] [cursor=pointer]:\n    - /url: https://example.com/\n  - button \"Go\" [ref=e3]\n```";

const CDT_SNAPSHOT: &str = "## Latest page snapshot\nuid=1_0 RootWebArea \"Example\" url=\"https://example.com/\"\n  uid=1_1 link \"Home\" url=\"https://example.com/\"\n  uid=1_2 button \"Go\" focusable\n  uid=1_6 \" | \"";

#[test]
fn detects_browser_snapshots_in_both_subformats() {
    assert_eq!(detect(PW_SNAPSHOT), ContentType::BrowserSnapshot);
    assert_eq!(detect(CDT_SNAPSHOT), ContentType::BrowserSnapshot);
    let bare_pw = "- generic [ref=e1]:\n  - link \"Home\" [ref=e2] [cursor=pointer]:\n    - /url: https://example.com/\n  - button \"Go\" [ref=e3]\n";
    assert_eq!(detect(bare_pw), ContentType::BrowserSnapshot);
    let bare_cdt =
        "uid=1_0 RootWebArea \"x\"\nuid=1_1 link \"a\"\nuid=1_2 link \"b\"\nuid=1_3 \" | \"";
    assert_eq!(detect(bare_cdt), ContentType::BrowserSnapshot);
}

#[test]
fn detects_cdt_action_responses_with_snapshot_attached() {
    let input = format!(
        "Successfully clicked on the element\nPage navigated to https://example.com/next.\n{CDT_SNAPSHOT}"
    );
    assert_eq!(detect(&input), ContentType::BrowserSnapshot);
}

#[test]
fn detects_pw_snapshots_after_action_preambles() {
    // Playwright's response builder can emit `### Open tabs` and
    // `### Ran Playwright code` sections before `### Page`/`### Snapshot`, so
    // the first line is not always `### Page`; anchor on the snapshot section.
    let open_tabs =
        format!("### Open tabs\n- tab 0: (current) [Example](https://example.com/)\n{PW_SNAPSHOT}");
    assert_eq!(detect(&open_tabs), ContentType::BrowserSnapshot);
    let ran_code = format!(
        "### Ran Playwright code\n```js\nawait page.getByRole('button').click();\n```\n{PW_SNAPSHOT}"
    );
    assert_eq!(detect(&ran_code), ContentType::BrowserSnapshot);
}

#[test]
fn snapshot_quoting_diff_text_stays_a_snapshot() {
    let input = PW_SNAPSHOT.replace(
        "- button \"Go\" [ref=e3]",
        "- paragraph [ref=e3]: diff --git a/x b/x",
    );
    assert_eq!(detect(&input), ContentType::BrowserSnapshot);
}

#[test]
fn echoed_git_diff_with_snapshot_context_stays_diff() {
    // A command preamble bypasses the head-anchored diff guard, and the hunk
    // context carries `### Snapshot`, a YAML fence, and `[ref=]`. The heading
    // is a diff context row (leading space), not a column-0 section heading,
    // so the snapshot detector must decline and leave it on the Diff route.
    let diff = "diff --git a/snapshot.md b/snapshot.md\n--- a/snapshot.md\n+++ b/snapshot.md\n@@ -1,6 +1,6 @@\n ### Page\n ### Snapshot\n ```yaml\n - link \"x\" [ref=e1]\n+- link \"y\" [ref=e2]\n - link \"z\" [ref=e3]\n ```\n";
    let input = format!("$ git diff snapshot.md\n{diff}");
    assert_eq!(detect(&input), ContentType::Diff);
    assert_eq!(detect(diff), ContentType::Diff);
}

#[test]
fn diffs_of_snapshot_files_stay_diffs() {
    let diff = "diff --git a/snap.yml b/snap.yml\n--- a/snap.yml\n+++ b/snap.yml\n@@ -1,3 +1,3 @@\n - link \"x\" [ref=e1]\n+- link \"y\" [ref=e2]\n - link \"z\" [ref=e3]\n";
    assert_eq!(detect(diff), ContentType::Diff);
    let preamble = format!("$ git diff snap.yml\n{diff}");
    assert_eq!(detect(&preamble), ContentType::Diff);
    let cdt_diff = "diff --git a/snap.txt b/snap.txt\n--- a/snap.txt\n+++ b/snap.txt\n@@ -1,3 +1,3 @@\n uid=1_1 link \"x\"\n+uid=1_2 link \"y\"\n uid=1_3 link \"z\"\n";
    assert_eq!(detect(cdt_diff), ContentType::Diff);
}

#[test]
fn ci_logs_and_prose_mentioning_refs_do_not_become_snapshots() {
    let ci =
        "running 3 tests\ntest tests::a PASSED\ntest tests::b PASSED\ntest result: ok. 3 passed\n";
    assert_eq!(detect(ci), ContentType::BuildLog);
    let prose =
        "See [ref=e1] and uid=1_2 in the documentation for details.\nThey are not snapshots.\n";
    assert_eq!(detect(prose), ContentType::PlainText);
}
