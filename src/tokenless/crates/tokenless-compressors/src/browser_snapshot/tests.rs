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
            depth: 0,
            role: "link".into(),
            name: "a".into(),
            reference: "e1".into(),
            attrs: "cursor=pointer".into(),
            url: String::new(),
            text: String::new(),
        },
        Record {
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
