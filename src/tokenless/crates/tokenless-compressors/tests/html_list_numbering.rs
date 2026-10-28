use tokenless_compressors::HtmlExtractor;

fn page(body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><body><p>These numbered migration steps are referenced by number elsewhere in this guide. Keep each step's original identifier.</p>{body}</body></html>"
    )
}

#[test]
fn nonconsecutive_or_styled_numbering_keeps_the_original_page() {
    for list in [
        "<ol reversed><li>third</li><li>second</li><li>first</li></ol>",
        "<ol start=\"5\" reversed><li>fifth</li><li>fourth</li></ol>",
        "<ol><li>first</li><li value=\"5\">fifth</li><li>sixth</li></ol>",
        "<ol start=\"-2\"><li>minus two</li><li>minus one</li></ol>",
        "<ol type=\"A\"><li>Appendix A</li><li>Appendix B</li></ol>",
        "<ul><li>outer<ol reversed><li>two</li><li>one</li></ol></li></ul>",
        "<table><tr><td><ol reversed><li>two</li><li>one</li></ol></td></tr></table>",
    ] {
        assert!(HtmlExtractor.render(&page(list)).is_none(), "{list}");
    }
}

#[test]
fn omitted_ordered_items_do_not_renumber_later_steps() {
    for list in [
        "<ol><li></li><li>second</li></ol>",
        "<ol><li><script>unused()</script></li><li>second</li></ol>",
        "<ol><li role=\"navigation\">first</li><li>second</li></ol>",
        "<ol>intro<li>first</li><li>second</li></ol>",
    ] {
        assert!(HtmlExtractor.render(&page(list)).is_none(), "{list}");
    }
}

#[test]
fn out_of_range_markers_pass_through_without_overflow() {
    for start in ["999999999", "1000000000", "18446744073709551615"] {
        let list = format!("<ol start=\"{start}\"><li>a</li><li>b</li></ol>");
        assert!(HtmlExtractor.render(&page(&list)).is_none(), "{list}");
    }
}

#[test]
fn ordinary_lists_and_lists_outside_the_view_still_render() {
    for (start, expected) in [("0", "0. zero\n1. next"), ("3", "3. zero\n4. next")] {
        let list = format!("<ol start=\"{start}\"><li>zero</li><li>next</li></ol>");
        let output = HtmlExtractor.render(&page(&list)).unwrap().output;
        assert!(output.contains(expected), "{output}");
    }
    let input = page(
        "<nav><ol reversed><li>outside</li></ol></nav>\
         <main><p>This retained guide contains enough text to remain a page. \
         The navigation is deliberately outside its selected root.</p>\
         <ul><li value=\"5\">plain</li><li>next</li></ul></main>",
    );
    let output = HtmlExtractor.render(&input).unwrap().output;
    assert!(output.contains("- plain\n- next"), "{output}");
    assert!(
        HtmlExtractor
            .render(&page("<ol start=\"999999999\"><li>last</li></ol>"))
            .unwrap()
            .output
            .contains("999999999. last")
    );
    assert!(
        HtmlExtractor
            .render(&page("<nav><ol reversed><li>removed</li></ol></nav>"))
            .is_some()
    );
}
