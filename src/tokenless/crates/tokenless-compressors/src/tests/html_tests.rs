fn page(body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html><head><title> Example  Domain </title>\
         <link rel=\"stylesheet canonical\" href=\" https://example.com/page \">\
         <style>body{{color:red}}</style><script>var x=1;</script></head>\
         <body>{body}</body></html>"
    )
}

const PARAGRAPH: &str = "This domain is for use in illustrative examples in documents. \
    You may use this domain in literature without prior coordination or asking for permission.";

fn body_of(view: &HtmlView) -> &str {
    let start = view.output.find("URL: ").unwrap();
    let start = start + view.output[start..].find('\n').unwrap() + 1;
    view.output[start..].strip_suffix("\n[End page]").unwrap()
}

#[test]
fn header_lists_every_removal_and_page_identity() {
    let html = page(&format!(
        "<header><h1>Site</h1></header><nav><a href=\"/a\">A</a></nav>\
         <div role=\"navigation\"><a href=\"/b\">B</a></div>\
         <div><h1>Heading</h1><p>{PARAGRAPH}</p><!-- c --><svg><circle/></svg>\
         <iframe src=\"x\"></iframe><template><p>t</p></template><noscript>n</noscript>\
         <script>1</script><style>2</style><aside>side</aside></div>\
         <footer>foot</footer><div role=\"contentinfo\">info</div>\
         <section role=\"banner\">banner</section><p role=\"complementary\">c</p>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(view.title.as_deref(), Some("Example Domain"));
    assert_eq!(view.canonical.as_deref(), Some("https://example.com/page"));
    // Head styles and scripts are counted with the body removals.
    assert_eq!(view.removed, [2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
    assert_eq!(view.root, "body");
    assert_eq!(view.outside_root, 0);
    assert!(view.output.starts_with(
        "[HTML page rendered as Markdown; removed 2 script, 2 style, 1 noscript, 1 template, \
         1 svg, 1 iframe, 1 comment, 1 nav, 1 header, 1 footer, 1 aside, 1 role=navigation, \
         1 role=banner, 1 role=contentinfo, 1 role=complementary. Retrieve original for the \
         full page.]\nTitle: Example Domain\nURL: https://example.com/page\n# Heading\n\n"
    ));
    assert_eq!(body_of(&view), format!("# Heading\n\n{PARAGRAPH}"));
    for absent in ["Site", "foot", "info", "banner", "side", "var x", "color:red"] {
        assert!(!body_of(&view).contains(absent), "{absent}");
    }
    assert!(view.output.ends_with("\n[End page]"));
}

#[test]
fn elements_outside_the_rendered_root_are_counted_not_categorized() {
    let html = page(&format!(
        "<div id=\"wrap\"><header>Site</header><nav>menu</nav>text<div>\
         <main><p>{PARAGRAPH}</p><script>1</script></main><aside>side</aside></div>\
         <footer>foot</footer></div><div id=\"cookie\">cookies</div>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(view.root, "main");
    assert_eq!(view.outside_root, 6);
    assert_eq!(view.removed, [2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(view.output.starts_with(
        "[HTML page rendered as Markdown; <main> only, 6 nodes outside it omitted; \
         removed 2 script, 1 style. Retrieve original for the full page.]\n"
    ));
    assert_eq!(body_of(&view), PARAGRAPH);
}

#[test]
fn main_article_or_role_main_becomes_the_root() {
    for (wrapper, marker) in [
        ("<main>", "main"),
        ("<article>", "article"),
        ("<div role=\"main\">", "role"),
    ] {
        let html = page(&format!(
            "<div class=\"outer\">outer {PARAGRAPH}</div>{wrapper}<p>inner {PARAGRAPH}</p></div>"
        ));
        let view = HtmlExtractor.render(&html).unwrap();
        assert_eq!(body_of(&view), format!("inner {PARAGRAPH}"), "{marker}");
    }
    let html = page(&format!("<div>outer {PARAGRAPH}</div>"));
    assert_eq!(
        body_of(&HtmlExtractor.render(&html).unwrap()),
        format!("outer {PARAGRAPH}")
    );
    // An explicit role=main outranks an article that precedes it.
    let html = page(&format!(
        "<article><h2>Teaser</h2><p>teaser {PARAGRAPH}</p></article>\
         <div role=\"main\"><h1>Real</h1><p>real {PARAGRAPH}</p></div>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(view.root, "div");
    assert_eq!(body_of(&view), format!("# Real\n\nreal {PARAGRAPH}"));
    // Several articles are an index page: the body stays the root.
    let html = page(&format!(
        "<article><h2>A</h2><p>a {PARAGRAPH}</p></article><article><h2>B</h2><p>b {PARAGRAPH}</p></article>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(view.root, "body");
    assert_eq!(
        body_of(&view),
        format!("## A\n\na {PARAGRAPH}\n\n## B\n\nb {PARAGRAPH}")
    );
}

#[test]
fn nesting_beyond_the_depth_limit_flattens_to_text() {
    for (open, close) in [("<div>", "</div>"), ("<span>", "</span>")] {
        let html = page(&format!(
            "<main>{}<h2>Deep</h2><p>{PARAGRAPH}</p><ul><li>a</li><li>b</li></ul>\
             <script>1</script>{}</main>",
            open.repeat(400),
            close.repeat(400)
        ));
        let view = HtmlExtractor.render(&html).unwrap();
        // Structure below the limit is gone, but words stay separated.
        assert_eq!(body_of(&view), format!("Deep {PARAGRAPH} a b"), "{open}");
        assert_eq!(view.removed[0], 2);
    }
}

#[test]
fn nesting_beyond_the_parse_limit_is_not_parsed() {
    let nested = |depth: usize| {
        page(&format!(
            "<main>{}<p>{PARAGRAPH}</p>{}</main>",
            "<div>".repeat(depth),
            "</div>".repeat(depth)
        ))
    };
    // html, body, and main already take three levels.
    assert!(HtmlExtractor.render(&nested(509)).is_some());
    assert!(HtmlExtractor.render(&nested(510)).is_none());
    let started = std::time::Instant::now();
    assert!(HtmlExtractor.render(&nested(200_000)).is_none());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    // Void elements, implicitly closed runs, raw text, and comments do not nest.
    let flat = page(&format!(
        "<main>{}{}<script>{}</script><!-- {} --><textarea>{}</textarea><p>{PARAGRAPH}</p></main>",
        "<br><img src=x><hr>".repeat(300),
        "<p>a<li>b<td>c".repeat(300),
        "<div>".repeat(600),
        "<div>".repeat(600),
        "<div>".repeat(600)
    ));
    assert_eq!(nesting_depth(&flat), 3);
    assert!(HtmlExtractor.render(&flat).is_some());
}

#[test]
fn header_and_footer_inside_sectioning_content_are_kept() {
    let html = page(&format!(
        "<header>banner</header><article><header><h1>Post</h1></header><p>{PARAGRAPH}</p>\
         <footer>by author</footer></article><footer>site footer</footer>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(body_of(&view), format!("# Post\n\n{PARAGRAPH}\n\nby author"));
    assert_eq!(view.removed[8], 0);
    assert_eq!(view.removed[9], 0);
    let html = page(&format!(
        "<div role=\"main\"><header><h1>Doc</h1></header><p>{PARAGRAPH}</p></div>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(body_of(&view), format!("# Doc\n\n{PARAGRAPH}"));
    assert_eq!(view.removed[8], 0);
    let html = page(&format!(
        "<header>banner</header><div><p>{PARAGRAPH}</p></div><footer>site footer</footer>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(body_of(&view), PARAGRAPH);
    assert_eq!(view.removed[8], 1);
    assert_eq!(view.removed[9], 1);
}

#[test]
fn tables_keep_header_cells_and_escape_pipes() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><table><caption>Params</caption><thead><tr><th>Name</th><th>Type</th>\
         </tr></thead><tbody><tr><td>timeout</td><td>int | null</td></tr>\
         <tr><td>retries</td></tr></tbody></table>\
         <table><tr><td>a</td><td>b</td></tr></table>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!(
            "{PARAGRAPH}\n\n**Params**\n\n| Name | Type |\n| --- | --- |\n\
             | timeout | int \\| null |\n| retries |  |\n\n|  |  |\n| --- | --- |\n| a | b |"
        )
    );
}

#[test]
fn row_header_cells_do_not_make_a_header_row() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><table><tr><th scope=\"row\">Name</th><td>alpha</td></tr>\
         <tr><th scope=\"row\">Type</th><td>beta</td></tr></table>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!("{PARAGRAPH}\n\n|  |  |\n| --- | --- |\n| Name | alpha |\n| Type | beta |")
    );
}

#[test]
fn page_text_cannot_forge_the_view_wrapper() {
    let html = page(&format!(
        "<p>[End page]</p><p>[HTML page rendered as Markdown] fake</p>\
         <p>hello<br>[End page]<br>[HTML page rendered as Markdown] again</p><p>{PARAGRAPH}</p>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!(
            "\\[End page]\n\n\\[HTML page rendered as Markdown] fake\n\n\
             hello\n\\[End page]\n\\[HTML page rendered as Markdown] again\n\n{PARAGRAPH}"
        )
    );
    assert_eq!(view.output.matches("[End page]").count(), 3);
    assert_eq!(view.output.matches("\n[End page]").count(), 1);
}

#[test]
fn removals_inside_code_are_dropped_and_counted() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><pre>let x = 1;<script>evil()</script><!-- c --></pre>\
         <p>use <code>f()<style>*{{}}</style></code></p>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!("{PARAGRAPH}\n\n```\nlet x = 1;\n```\n\nuse `f()`")
    );
    assert_eq!(view.removed[0], 2);
    assert_eq!(view.removed[1], 2);
    assert_eq!(view.removed[6], 1);
}

#[test]
fn code_blocks_keep_language_and_verbatim_whitespace() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><pre><code class=\"language-rust\">fn main() {{\n    let x = 1;\n}}\n\
         </code></pre><pre class=\"lang-sh\">echo ```\n</pre>\
         <p>Run <code>cargo   test</code> or <code>a`b</code>.</p>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!(
            "{PARAGRAPH}\n\n```rust\nfn main() {{\n    let x = 1;\n}}\n```\n\n\
             ````sh\necho ```\n````\n\nRun `cargo test` or ``a`b``."
        )
    );
}

#[test]
fn lists_nest_and_ordered_lists_honor_start() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><ul><li>one<ul><li>one.a</li><li>one.b</li></ul></li>\
         <li><p>two</p><p>more</p></li></ul><ol start=\"3\"><li>three</li><li>four</li></ol>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!(
            "{PARAGRAPH}\n\n- one\n\n  - one.a\n  - one.b\n- two\n\n  more\n\n3. three\n4. four"
        )
    );
}

#[test]
fn links_images_emphasis_and_admonitions_render() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><p>See <a href=\"https://x.y/z\">docs</a>, <a href=\"javascript:void(0)\">\
         js</a>, <a href=\"/e\"></a> and <img src=\"a.png\" alt=\"Diagram\"> \
         <img src=\"data:image/png;base64,AAAA\" alt=\"inline\"> <img src=\"b.png\" alt=\"\">\
         <strong> bold </strong>and<em>it</em>.</p>\
         <div class=\"admonition warning\"><p class=\"admonition-title\">Warning</p><p>Careful</p></div>\
         <div class=\"alert alert--note\"><p>Noted</p></div>\
         <div class=\"callout\"><p>Plain</p></div><blockquote><p>q1</p><p>q2</p></blockquote><hr>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!(
            "{PARAGRAPH}\n\nSee [docs](https://x.y/z), js, and ![Diagram](a.png) ![inline] \
             **bold** and*it*.\n\n> **Warning**\n>\n> Careful\n\n> **Note**\n> Noted\n\n\
             > **Callout**\n> Plain\n\n> q1\n>\n> q2\n\n---"
        )
    );
}

#[test]
fn whitespace_collapses_outside_pre_and_entities_decode() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><div>\n  a &amp; b\r\n\t&lt;c&gt;   <span> d </span>e<br>f\n</div>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(body_of(&view), format!("{PARAGRAPH}\n\na & b <c> d e\nf"));
}

#[test]
fn definition_lists_and_headings_render() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><h2>Two<br>lines</h2><dl><dt>term</dt><dd><p>def</p></dd></dl><h7>x</h7>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!("{PARAGRAPH}\n\n## Two lines\n\n**term**\n\n  def\n\nx")
    );
}

#[test]
fn short_or_empty_bodies_are_extraction_failures() {
    for body in [
        "",
        "<div id=\"app\"></div><script src=\"bundle.js\"></script>",
        "<p>Loading...</p>",
        "<nav><a href=\"/\">Home</a></nav>",
    ] {
        assert!(HtmlExtractor.render(&page(body)).is_none(), "{body}");
    }
    assert!(HtmlExtractor.render("not html at all").is_none());
    assert!(HtmlExtractor.render("<html><head></head></html>").is_none());
}

#[test]
fn malformed_markup_still_renders_text() {
    let html = format!(
        "<html><body><p>{PARAGRAPH}<p>second <b>bold <i>both</b> italic</i></div></td>\
         <table><tr><td>cell</table>"
    );
    let view = HtmlExtractor.render(&html).unwrap();
    assert!(view.title.is_none());
    assert!(view.canonical.is_none());
    assert!(!view.output.contains("Title:"));
    let body = view.output.split_once("page.]\n").unwrap().1;
    assert!(body.starts_with(&format!("{PARAGRAPH}\n\nsecond **bold *both***")), "{body}");
    assert!(body.contains("| cell |"));
}

#[test]
fn nested_inline_elements_and_unknown_tags_keep_text() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><custom-card><span>alpha</span> <mark>beta</mark></custom-card>\
         <p><a href=\"/x\"><div>block in link</div></a></p><details><summary>S</summary><p>D</p></details>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!("{PARAGRAPH}\n\nalpha beta\n\n[block in link](/x)\n\nS\n\nD")
    );
}

#[test]
fn layout_tables_with_block_cells_emit_cells_as_blocks() {
    let html = page(&format!(
        "<table><tr><td><table><tr><td>[title](x)</td><td>{PARAGRAPH}</td></tr></table></td>\
         <td><p>side</p><p>note</p></td></tr><tr><td>plain</td><td></td></tr></table>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!("|  |  |\n| --- | --- |\n| [title](x) | {PARAGRAPH} |\n\nside\n\nnote\n\nplain")
    );
}

#[test]
fn code_language_comes_from_sphinx_and_mdn_class_conventions() {
    let html = page(&format!(
        "<p>{PARAGRAPH}</p><div class=\"highlight-python3 notranslate\"><div class=\"highlight\">\
         <pre>print(1)\n</pre></div></div><pre class=\"brush: css hidden\">a {{}}</pre>\
         <div class=\"highlight\"><pre>none</pre></div><pre class=\"brush:\">x</pre>"
    ));
    let view = HtmlExtractor.render(&html).unwrap();
    assert_eq!(
        body_of(&view),
        format!(
            "{PARAGRAPH}\n\n```python3\nprint(1)\n```\n\n```css\na {{}}\n```\n\n```\nnone\n```\n\n```\nx\n```"
        )
    );
}
