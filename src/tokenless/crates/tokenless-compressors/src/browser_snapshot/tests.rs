use super::encode::{self, Col};
use super::{BrowserSnapshotCompressor, Record};
use tokenless_protocol::estimate_tokens;

const PW_WRAP: &str =
    "### Page\n- Page URL: https://example.com/\n- Page Title: Example\n### Snapshot\n";

fn pw_input(tree: &str) -> String {
    format!("{PW_WRAP}```yaml\n{tree}\n```")
}

fn cdt_input(body: &str) -> String {
    format!("## Latest page snapshot\n{body}")
}

fn decode_table(table: &str, cols: &[Col]) -> Vec<Record> {
    let mut lines = table.split('\n');
    lines.next().unwrap();
    lines
        .map(|line| {
            let cells: Vec<String> = line.split('\t').map(encode::unescape_cell).collect();
            let mut record = Record {
                block: 0,
                depth: 0,
                role: String::new(),
                name: String::new(),
                reference: String::new(),
                attrs: String::new(),
                url: String::new(),
                text: String::new(),
            };
            for (col, cell) in cols.iter().zip(&cells) {
                match col {
                    Col::Block => record.block = cell.parse().unwrap(),
                    Col::Depth => record.depth = cell.parse().unwrap(),
                    Col::Role => record.role = cell.clone(),
                    Col::Name => record.name = cell.clone(),
                    Col::Ref => record.reference = cell.clone(),
                    Col::Attrs => record.attrs = cell.clone(),
                    Col::Url => record.url = cell.clone(),
                    Col::Text => record.text = cell.clone(),
                }
            }
            record
        })
        .collect()
}

// ---- Playwright parser edges ----

#[test]
fn pw_parses_tree_with_url_children_and_text_nodes() {
    let input = pw_input(
        "- generic [ref=e1]:\n  - link \"Home\" [ref=e2] [cursor=pointer]:\n    - /url: https://example.com/\n  - paragraph [ref=e3]: Hello\n  - text: world",
    );
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.prefix, PW_WRAP);
    assert_eq!(parsed.suffix, "");
    assert_eq!(parsed.records.len(), 4);
    assert_eq!(parsed.records[1].reference, "e2");
    assert_eq!(parsed.records[1].url, "https://example.com/");
    assert_eq!(parsed.records[1].attrs, "cursor=pointer");
    assert_eq!(parsed.records[2].text, "Hello");
    assert_eq!(parsed.records[3].role, "text");
    assert_eq!(parsed.records[3].text, "world");
}

#[test]
fn pw_single_quoted_scalar_with_colon_in_name() {
    let input = pw_input(
        "- 'link \"Breaking: Example Story\" [ref=e40] [cursor=pointer]':\n  - /url: https://example.com/a",
    );
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.records[0].name, "Breaking: Example Story");
    assert_eq!(parsed.records[0].reference, "e40");
    assert_eq!(parsed.records[0].url, "https://example.com/a");
}

#[test]
fn pw_multiline_quoted_name_folds_to_space() {
    let input = pw_input("- paragraph \"first\n  second\" [ref=e7]:\n- link \"Next\" [ref=e8]");
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.records.len(), 2);
    assert!(parsed.records[0].name.contains("first"));
    assert!(parsed.records[0].name.contains("second"));
}

#[test]
fn pw_inline_text_quote_keeps_subsequent_elements() {
    // A literal quote in the inline YAML value (an inch mark) must not fold the
    // following actionable elements into the heading's text cell: only the
    // quoted name field's own closure drives multiline folding.
    let mut tree = String::from(
        "- generic [active] [ref=e1]:\n  - heading \"Measurements\" [level=2] [ref=e2]: Width 10\"\n  - button \"Continue\" [ref=e3]\n  - button \"Cancel\" [ref=e4]\n",
    );
    for i in 0..120 {
        tree.push_str(&format!(
            "  - link \"Product details {i}\" [ref=e{}] [cursor=pointer]:\n    - /url: https://example.com/products/{i}\n",
            i + 5
        ));
    }
    let input = pw_input(&tree);
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.records.len(), 124);
    assert_eq!(parsed.records[1].text, "Width 10\"");
    assert_eq!(parsed.records[2].role, "button");
    assert_eq!(parsed.records[2].reference, "e3");
    assert_eq!(parsed.records[3].reference, "e4");
    assert_eq!(parsed.records[4].reference, "e5");
    let output = BrowserSnapshotCompressor.compress(&input).unwrap();
    for cell in ["\te3\t", "\te4\t", "\te124\t"] {
        assert!(output.contains(cell), "reference lost: {cell}");
    }
    assert!(output.contains("Width 10\""));
}

#[test]
fn pw_frame_refs_and_variable_attribute_order() {
    let input = pw_input(
        "- generic [active] [ref=f2e2] [box=0,0,1280,720]:\n  - textbox \"q\" [active] [ref=f2e38]",
    );
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.records[0].reference, "f2e2");
    assert_eq!(parsed.records[0].attrs, "active box=0,0,1280,720");
    assert_eq!(parsed.records[1].reference, "f2e38");
    assert_eq!(parsed.records[1].attrs, "active");
}

#[test]
fn pw_orphan_url_and_bad_indent_reject_whole_snapshot() {
    assert!(super::pw::parse(&pw_input("- /url: https://example.com/")).is_none());
    assert!(super::pw::parse(&pw_input("- generic [ref=e1]:\n   - link \"x\" [ref=e2]")).is_none());
    assert!(super::pw::parse(&pw_input("- generic [ref=e1]:\n\t- link \"x\" [ref=e2]")).is_none());
    assert!(super::pw::parse(&pw_input("- generic [ref=e1]\n  - /url: https://x/")).is_some());
    assert!(
        super::pw::parse(&pw_input(
            "- generic [ref=e1]\n  - /url: https://x/\n  - /url: https://y/"
        ))
        .is_none()
    );
}

#[test]
fn pw_trailer_after_fence_is_preserved() {
    let input = format!(
        "{PW_WRAP}```yaml\n- link \"Home\" [ref=e2]\n```\n### Events\n- New console entries: file.log#L1-L2"
    );
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(
        parsed.suffix,
        "\n### Events\n- New console entries: file.log#L1-L2"
    );
}

#[test]
fn pw_parses_snapshot_section_fence_not_an_earlier_yaml_block() {
    // An earlier fenced YAML result (e.g. under `### Ran Playwright code`)
    // must stay verbatim in the prefix; only the `### Snapshot` section's
    // fence is the aria tree. Detector and parser agree on the section.
    let input = format!(
        "### Ran Playwright code\n```yaml\n- earlier: block\n  not: a snapshot\n```\n{PW_WRAP}```yaml\n- generic [ref=e1]:\n  - link \"Home\" [ref=e2] [cursor=pointer]:\n    - /url: https://example.com/\n```"
    );
    let parsed = super::pw::parse(&input).unwrap();
    assert!(parsed.records.iter().any(|r| r.reference == "e2"));
    assert!(!parsed.records.iter().any(|r| r.role == "earlier"));
    assert!(parsed.prefix.contains("### Ran Playwright code"));
    assert!(parsed.prefix.contains("- earlier: block"));
}

// ---- chrome-devtools passthrough policy ----

#[test]
fn cdt_snapshots_pass_through_unchanged() {
    // Upstream wraps names and attribute values in raw quotes without escaping
    // embedded quotes or newlines, so record boundaries are byte-ambiguous and
    // cannot be decomposed losslessly. Even a clean name-only listing that an
    // earlier heuristic would have compressed now passes through whole, as do
    // the multiline-name continuations that defeated successive guard rounds.
    let mut body = String::from("uid=1_0 RootWebArea \"News\" busy\n");
    for i in 0..40 {
        body.push_str(&format!(
            "  uid=1_{} link \"Story number {i} with a reasonably long title\"\n",
            i + 1
        ));
    }
    for input in [
        cdt_input(&body),
        cdt_input("uid=1_1 link \"Home\" focusable"),
        cdt_input("uid=1_1 link \"x\" url=\"https://example.com/\""),
        cdt_input("  uid=1_1 \"Case A Draft \" \n    status \"Ready\""),
        cdt_input("uid=2_0 \"Case B First line\"\n  uid=999_777 \"Second line\""),
    ] {
        assert!(
            BrowserSnapshotCompressor.compress(&input).is_none(),
            "{input:?}"
        );
    }
}

#[test]
fn cdt_snapshot_with_embedded_pw_fence_passes_through() {
    // A CDT name may embed PW-shaped YAML rows. The compressor must reject the
    // listing by shape, not let pw::parse search it for a fence: compressing
    // the fragment would alter the CDT bytes and present literal name text
    // (`[ref=e2]`) as a real element reference.
    let input = "## Latest page snapshot\nuid=1_0 RootWebArea \"Demo\"\n  uid=1_1 StaticText \"intro\n### Snapshot\n```yaml\n- link \"Home\" [ref=e2] [cursor=pointer]:\n  - /url: https://example.com/\n```\noutro\"";
    assert!(super::is_cdt_listing(input));
    assert!(BrowserSnapshotCompressor.compress(input).is_none());
}

// ---- encoding transforms ----

#[test]
fn smart_cursor_prunes_interactive_roles_only() {
    let mut records = vec![
        Record {
            block: 0,
            depth: 0,
            role: "link".into(),
            name: "a".into(),
            reference: "e1".into(),
            attrs: "cursor=pointer".into(),
            url: String::new(),
            text: String::new(),
        },
        Record {
            block: 0,
            depth: 0,
            role: "generic".into(),
            name: String::new(),
            reference: "e2".into(),
            attrs: "cursor=pointer".into(),
            url: String::new(),
            text: String::new(),
        },
    ];
    encode::strip_redundant_cursor(&mut records);
    assert_eq!(records[0].attrs, "");
    assert_eq!(records[1].attrs, "cursor=pointer");
}

#[test]
fn escape_roundtrip_handles_backslash_tab_newline_and_quotes() {
    for raw in ["a\\b", "a\tb", "a\nb", "a\\nb", "\"q\"", "中文\\n"] {
        assert_eq!(encode::unescape_cell(&encode::escape_cell(raw)), raw);
    }
}

#[test]
fn url_share_legend_encodes_and_decodes_exactly() {
    let records: Vec<Record> = (0..6)
        .map(|i| Record {
            block: 0,
            depth: 1,
            role: "link".into(),
            name: format!("item {i}"),
            reference: format!("e{i}"),
            attrs: String::new(),
            url: format!("https://example.com/items/item-{i}"),
            text: String::new(),
        })
        .collect();
    let shared = encode::share_urls(&records).unwrap();
    assert_eq!(shared.legend_text, "U1=https://example.com/items/");
    assert_eq!(shared.records[0].url, "U1/item-0");
    for (orig, enc) in records.iter().zip(&shared.records) {
        assert_eq!(encode::decode_url(&enc.url, &shared.legend), orig.url);
    }
}

#[test]
fn url_share_collision_guard_disables_legend() {
    let mut records: Vec<Record> = (0..4)
        .map(|i| Record {
            block: 0,
            depth: 1,
            role: "link".into(),
            name: format!("n{i}"),
            reference: format!("e{i}"),
            attrs: String::new(),
            url: format!("https://example.com/items/{i}"),
            text: String::new(),
        })
        .collect();
    records[0].url = "U1/evil".to_owned();
    assert!(encode::share_urls(&records).is_none());
}

// ---- end-to-end compress ----

fn pw_links(count: usize) -> String {
    let mut tree = String::from("- navigation [ref=e1]:\n");
    for i in 0..count {
        tree.push_str(&format!(
            "  - link \"Story number {i} with a reasonably long title\" [ref=e{}] [cursor=pointer]:\n    - /url: https://news.example.com/item?id={i}\n",
            i + 2
        ));
    }
    pw_input(&tree)
}

#[test]
fn compress_pw_yields_lossless_table_with_wrapper_and_legend() {
    let input = pw_links(40);
    let output = BrowserSnapshotCompressor.compress(&input).unwrap();
    assert!(output.starts_with(PW_WRAP));
    assert!(output.contains("U1=https://news.example.com/"));
    assert!(output.len() < input.len());
    assert!(estimate_tokens(&output) < estimate_tokens(&input));
    let body = output.strip_prefix(PW_WRAP).unwrap();
    let (legend_text, table) = body.split_once('\n').unwrap();
    let legend: Vec<(String, String)> = legend_text
        .split('\n')
        .filter(|l| l.starts_with("U1="))
        .map(|l| {
            let (id, p) = l.split_once('=').unwrap();
            (id.to_owned(), p.to_owned())
        })
        .collect();
    let decoded = decode_table(table, encode::PW_COLS);
    assert_eq!(decoded.len(), 41);
    assert_eq!(decoded[1].reference, "e2");
    assert_eq!(decoded[1].attrs, "");
    assert_eq!(
        encode::decode_url(&decoded[1].url, &legend),
        "https://news.example.com/item?id=0"
    );
    assert_eq!(
        decoded[1].name,
        "Story number 0 with a reasonably long title"
    );
}

#[test]
fn compress_fails_open_on_unrelated_or_tiny_input() {
    for input in [
        "",
        "just some prose about the web",
        "{\"json\": true}",
        &pw_input("- link \"x\" [ref=e1]"),
        &cdt_input("uid=1_1 link \"x\""),
    ] {
        assert!(
            BrowserSnapshotCompressor.compress(input).is_none(),
            "{input:?}"
        );
    }
}

#[test]
fn compress_synthetic_large_tree_roundtrips_every_record() {
    let mut tree = String::from("- generic [ref=e1]:\n");
    for i in 0..500 {
        let name = format!("node {i} 中文 \\\\ back\\ttab");
        tree.push_str(&format!(
            "  - link \"{name}\" [ref=e{}] [cursor=pointer]:\n    - /url: https://example.com/deep/path/item-{i}\n",
            i + 2
        ));
    }
    let input = pw_input(&tree);
    let output = BrowserSnapshotCompressor.compress(&input).unwrap();
    let parsed = super::pw::parse(&input).unwrap();
    let body = output.strip_prefix(PW_WRAP).unwrap();
    let table = match body.split_once('\n') {
        Some((first, rest)) if first.starts_with("U1=") => rest,
        _ => body,
    };
    let decoded = decode_table(table, encode::PW_COLS);
    assert_eq!(decoded.len(), parsed.records.len());
    for (orig, dec) in parsed.records.iter().zip(&decoded) {
        assert_eq!(orig.depth, dec.depth);
        assert_eq!(orig.role, dec.role);
        assert_eq!(orig.name, dec.name);
        assert_eq!(orig.reference, dec.reference);
        assert_eq!(orig.text, dec.text);
    }
}

// ---- Playwright browser_find results ----

fn pw_find_matches(count: usize) -> String {
    let mut blocks = Vec::with_capacity(count);
    for i in 0..count {
        blocks.push(format!(
            "- link \"Show HN: Match {i} with detailed subtitle\" [ref=e{}] [cursor=pointer]:\n  - /url: https://news.example.com/item?id={i}",
            i + 10
        ));
    }
    format!("### Result\n{}", blocks.join("\n----\n"))
}

#[test]
fn compress_browser_find_multi_block_preserves_blocks_urls_and_refs() {
    let input = pw_find_matches(30);
    let output = BrowserSnapshotCompressor.compress(&input).unwrap();
    assert!(output.starts_with("### Result\n"));
    assert!(output.contains("U1=https://news.example.com/\n"));
    assert!(output.contains("block\tdepth\trole\tname\tref\tattrs\turl\ttext\n"));
    assert!(output.len() < input.len());
    assert!(estimate_tokens(&output) < estimate_tokens(&input));

    let parsed = super::find::parse(&input).unwrap();
    let body = output.strip_prefix("### Result\n").unwrap();
    let (legend_line, table) = body.split_once('\n').unwrap();
    let (id, prefix) = legend_line.split_once('=').unwrap();
    let legend = vec![(id.to_owned(), prefix.to_owned())];
    let decoded = decode_table(table, encode::FIND_COLS);
    assert_eq!(decoded.len(), 30);
    for (i, (orig, dec)) in parsed.records.iter().zip(&decoded).enumerate() {
        assert_eq!(dec.block, i);
        assert_eq!(dec.depth, orig.depth);
        assert_eq!(dec.role, orig.role);
        assert_eq!(dec.name, orig.name);
        assert_eq!(dec.reference, orig.reference);
        assert_eq!(dec.attrs, "");
        assert_eq!(encode::decode_url(&dec.url, &legend), orig.url);
    }
}

#[test]
fn compress_browser_find_with_upstream_summary_header() {
    let raw = pw_find_matches(30);
    let input = raw.replace(
        "### Result\n",
        "### Result\nFound 30 matches for \"Story\":\n\n",
    );
    let output = BrowserSnapshotCompressor.compress(&input).unwrap();
    assert!(output.starts_with("### Result\nFound 30 matches for \"Story\":\n\n"));
    assert!(output.contains("block\tdepth\trole\tname\tref\tattrs\turl\ttext\n"));
    assert!(output.len() < input.len());
    assert!(estimate_tokens(&output) < estimate_tokens(&input));
}

#[test]
fn compress_browser_find_preserves_context_gap_markers_without_affecting_pw_parse() {
    let mut blocks = Vec::with_capacity(30);
    for i in 0..30 {
        blocks.push(format!(
            "- toolbar:\n  ...\n  - link \"Story number {i} with a reasonably long title\" [ref=e{}] [cursor=pointer]:\n    - /url: https://news.example.com/item?id={i}",
            i + 10
        ));
    }
    let input = format!(
        "### Result\nFound 30 matches for \"Story\":\n\n{}",
        blocks.join("\n----\n")
    );
    let output = BrowserSnapshotCompressor.compress(&input).unwrap();
    assert!(output.contains("\t1\t...\t\t\t\t\t\n"));
    assert!(output.contains("\te10\t"));
    assert!(output.len() < input.len());
    assert!(estimate_tokens(&output) < estimate_tokens(&input));

    // Standard fenced `pw::parse` stays strict on `...` outside `browser_find`.
    let fenced_with_gap = "### Page\n### Snapshot\n```yaml\n- toolbar:\n  ...\n  - button \"Go\" [ref=e1]\n  - button \"Stop\" [ref=e2]\n  - button \"Next\" [ref=e3]\n```";
    assert!(
        BrowserSnapshotCompressor
            .compress(fenced_with_gap)
            .is_none()
    );
}

#[test]
fn find_isolates_url_parent_lookup_across_block_boundaries() {
    // An orphan /url child at the top of block 1 must not attach to the tail of block 0.
    let orphan_cross_block = "### Result\n- generic [ref=e1]:\n  - link \"A\" [ref=e2]\n----\n  - /url: https://example.com/evil\n- link \"B\" [ref=e3]";
    assert!(super::find::parse(orphan_cross_block).is_none());
    assert!(
        BrowserSnapshotCompressor
            .compress(orphan_cross_block)
            .is_none()
    );
}

#[test]
fn find_keeps_separator_inside_multiline_quoted_name_and_preserves_trailer() {
    let input = "### Page\n- Page URL: https://example.com/\n### Result\n- link \"Line one\n----\n  line two\" [ref=e1] [cursor=pointer]:\n  - /url: https://example.com/a\n----\n- link \"Second match item with long title\" [ref=e2] [cursor=pointer]:\n  - /url: https://example.com/b\n----\n- link \"Third match item with long title\" [ref=e3] [cursor=pointer]:\n  - /url: https://example.com/c\n\n### Events\n- done";
    let parsed = super::find::parse(input).unwrap();
    assert_eq!(parsed.records.len(), 3);
    assert_eq!(parsed.records[0].block, 0);
    assert_eq!(parsed.records[1].block, 1);
    assert_eq!(parsed.records[2].block, 2);
    assert!(parsed.records[0].name.contains("----"));
    assert!(parsed.prefix.contains("### Page"));
    assert_eq!(parsed.suffix, "\n\n### Events\n- done");
    let output = BrowserSnapshotCompressor.compress(input).unwrap();
    assert!(output.ends_with("\n\n### Events\n- done"));
}

#[test]
fn find_handles_single_block_without_block_column_and_fails_open_on_non_pw() {
    let single = "### Result\n- list [ref=e1]:\n  - link \"Alpha documentation page link\" [ref=e2] [cursor=pointer]:\n    - /url: https://docs.example.com/alpha\n  - link \"Beta documentation page link\" [ref=e3] [cursor=pointer]:\n    - /url: https://docs.example.com/beta\n  - link \"Gamma documentation page link\" [ref=e4] [cursor=pointer]:\n    - /url: https://docs.example.com/gamma";
    let output = BrowserSnapshotCompressor.compress(single).unwrap();
    assert!(output.contains("\ndepth\trole\tname\tref\tattrs\turl\ttext\n"));
    assert!(!output.contains("block\tdepth"));

    for bad in [
        "### Result\n42",
        "### Result\n[\"a\", \"b\"]",
        "### Result\n- item only prose without refs\n----\n- another prose item",
        // Trailing ---- yields an empty block (even with >=2 unique refs before it).
        "### Result\n- link \"Alpha\" [ref=e1]\n----\n- link \"Beta\" [ref=e2]\n----\n",
        // Leading or malformed separator variants fail open cleanly.
        "### Result\n----\n- link \"A\" [ref=e1]\n----\n- link \"B\" [ref=e2]",
        "### Result\n- link \"A\" [ref=e1]\n---- \n- link \"B\" [ref=e2]",
        "### Result\n- link \"A\" [ref=e1]\n  ----\n- link \"B\" [ref=e2]",
        // Duplicate refs across blocks do not satisfy the >=2 unique refs gate.
        "### Result\n- link \"Same element\" [ref=e1]\n----\n- link \"Same element\" [ref=e1]",
        // Mid-block CR leaks and CRLF wrappers fail open without polluting cells.
        "### Result\n- link \"A\" [ref=e1]\n  - /url: https://example.com/a\r\n----\n- link \"B\" [ref=e2]",
        "### Result\n- text: hi\r\n- link \"A\" [ref=e1]\n- link \"B\" [ref=e2]",
        "### Result\n- link \"A\" [ref=e1]\r\n----\r\n- link \"B\" [ref=e2]",
        // Trailing EOF without newline after header or unterminated quote fails open.
        "### Result",
        "### Result\n- link \"unterminated [ref=e1]\n### Events\n- done",
        "diff --git a/r.md b/r.md\n### Result\n- link \"A\" [ref=e1]\n----\n- link \"B\" [ref=e2]",
    ] {
        assert!(BrowserSnapshotCompressor.compress(bad).is_none(), "{bad:?}");
    }
}

#[test]
fn find_covers_single_quoted_elements_blank_lines_two_results_snapshot_order_and_u1_guard() {
    // Blank line inside block + single-quoted YAML element + second ### Result preserved verbatim.
    let two_results = "### Result\n- 'link \"Colon: One\" [ref=e1] [cursor=pointer]':\n  - /url: https://example.com/path/one\n\n  - text: body\n----\n- link \"Target Two with a long accessible name\" [ref=e2] [cursor=pointer]:\n  - /url: https://example.com/path/two\n\n### Result\n- literal second section [ref=e9]";
    let parsed = super::find::parse(two_results).unwrap();
    assert_eq!(parsed.records[0].name, "Colon: One");
    assert!(
        parsed
            .suffix
            .contains("### Result\n- literal second section")
    );
    let compressed = BrowserSnapshotCompressor.compress(two_results).unwrap();
    assert!(compressed.ends_with("### Result\n- literal second section [ref=e9]"));

    // Quoted heading inside element name must not cut the section prematurely.
    let quoted_heading = "### Result\n- link \"First line\n### Events\n  continuation\" [ref=e1] [cursor=pointer]:\n  - /url: https://example.com/items/one\n----\n- link \"Second match item with long title\" [ref=e2] [cursor=pointer]:\n  - /url: https://example.com/items/two\n### Events\n- real event";
    let q_parsed = super::find::parse(quoted_heading).unwrap();
    assert!(q_parsed.records[0].name.contains("### Events"));
    assert_eq!(q_parsed.suffix, "\n### Events\n- real event");

    // ### Result followed by fenced ### Snapshot prefers fenced snapshot compression.
    let mut snap_tree = String::from("- generic [ref=e1]:\n");
    for i in 0..20 {
        snap_tree.push_str(&format!(
            "  - link \"Snapshot Story {i} with long label\" [ref=e{}]:\n    - /url: https://example.com/story/{i}\n",
            i + 2
        ));
    }
    let mixed = format!(
        "### Result\n- link \"Pref\" [ref=e99]\n### Page\n### Snapshot\n```yaml\n{snap_tree}\n```"
    );
    let snap_out = BrowserSnapshotCompressor.compress(&mixed).unwrap();
    assert!(
        snap_out.starts_with("### Result\n- link \"Pref\" [ref=e99]\n### Page\n### Snapshot\n")
    );

    // U1/ collision in a find URL disables URL legend generation.
    let mut u1_blocks = vec![
        "- link \"Legend trap item with long label\" [ref=e1] [cursor=pointer]:\n  - /url: U1/evil"
            .to_owned(),
    ];
    for i in 2..12 {
        u1_blocks.push(format!(
            "- link \"Story item {i} with long label\" [ref=e{i}] [cursor=pointer]:\n  - /url: https://example.com/long/prefix/{i}"
        ));
    }
    let u1_input = format!("### Result\n{}", u1_blocks.join("\n----\n"));
    let u1_out = BrowserSnapshotCompressor.compress(&u1_input).unwrap();
    assert!(!u1_out.contains("U1=https://"));

    // HTML documents or test logs quoting `### Result` must fail open cleanly.
    let html_wrapped =
        format!("<!DOCTYPE html><html><body><pre>\n{u1_input}\n</pre></body></html>");
    assert!(super::find::parse(&html_wrapped).is_none());
    assert!(BrowserSnapshotCompressor.compress(&html_wrapped).is_none());
}

#[test]
fn browser_find_rejects_orphan_url_under_context_gap() {
    let input = concat!(
        "### Result\n",
        "Found 2 matches for \"Story\":\n\n",
        "- toolbar:\n",
        "  ...\n",
        "    - /url: https://example.test/orphan\n",
        "  - link \"Story one with long title\" [ref=e10] [cursor=pointer]:\n",
        "    - /url: https://news.example.com/item?id=10\n",
        "----\n",
        "- link \"Story two with long title\" [ref=e11] [cursor=pointer]:\n",
        "  - /url: https://news.example.com/item?id=11\n",
    );
    assert!(super::find::parse(input).is_none());
    assert!(BrowserSnapshotCompressor.compress(input).is_none());
}
