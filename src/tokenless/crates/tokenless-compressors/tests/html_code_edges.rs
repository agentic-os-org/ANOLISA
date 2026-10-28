use tokenless_compressors::HtmlExtractor;

fn page(body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><body><p>This command reference preserves literal syntax so copied examples keep their original meaning.</p>{body}</body></html>"
    )
}

#[test]
fn inline_code_edges_keep_literal_backticks_separate_from_fences() {
    for tag in ["code", "kbd", "samp"] {
        for (text, expected) in [
            ("`orders`", "`` `orders` ``"),
            ("`prefix", "`` `prefix ``"),
            ("suffix`", "`` suffix` ``"),
            ("``literal`", "``` ``literal` ```"),
            ("`", "`` ` ``"),
            ("``", "``` `` ```"),
        ] {
            let input = page(&format!("<p>Use <{tag}>{text}</{tag}> here.</p>"));
            let output = HtmlExtractor.render(&input).unwrap().output;
            assert!(
                output.contains(&format!("Use {expected} here.")),
                "{tag}: {text:?}: {output}"
            );
        }
    }
}

#[test]
fn adjacent_code_spans_do_not_merge_their_fences() {
    let input = page("<p><code>alpha</code><code>beta</code></p>");
    let output = HtmlExtractor.render(&input).unwrap().output;
    assert!(output.contains("`alpha`<!-- -->`beta`"), "{output}");
}

#[test]
fn internal_backticks_and_plain_spans_keep_existing_output() {
    let input = page("<p>Use <code>a``b</code> and <code>plain</code>.</p>");
    let output = HtmlExtractor.render(&input).unwrap().output;
    assert!(output.contains("Use ```a``b``` and `plain`."), "{output}");
}
