//! Hybrid Markdown + HTML views of HTML fragments embedded in structured data.
//!
//! A page view chooses a main-content root and removes navigation, banners
//! and controls around it. A fragment is already the content a field carries,
//! so its view removes nothing visible: global hazards (hidden or illegible
//! text, executable scripts, external stylesheets, overflowing layout) still
//! reject the whole fragment, while local HTML semantics (`<svg>`, JSON data
//! `<script>` payloads, `data-*`/`aria-*` attributes, form controls, struck
//! prices, superscripts, non-GFM tables and non-1-step lists) stay intact in
//! place as compact HTML around Markdown-compressed rich-text subtrees.

mod color;
mod css;
mod legibility;
mod plan;
mod serialize;

use std::fmt::Write as _;

pub(super) use plan::{FragmentPlan, PlanKind};

use super::{
    HtmlExtractor, Node, NodeKind, REMOVED_CATEGORIES, REMOVED_LABELS, Removed, Renderer,
    brackets_balanced, collapse, destination, find_element, is_sectioning, link_tail, parse,
    table_children, text_of, tooltip,
};

/// Elements the shared Markdown renderer represents directly when their
/// attributes and styles carry no HTML-only semantics.
const VOCABULARY: [&str; 61] = [
    "html",
    "head",
    "body",
    "meta",
    "style",
    "title",
    "p",
    "div",
    "section",
    "article",
    "main",
    "address",
    "blockquote",
    "figure",
    "figcaption",
    "details",
    "summary",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "pre",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "table",
    "caption",
    "colgroup",
    "col",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "th",
    "td",
    "a",
    "abbr",
    "b",
    "br",
    "button",
    "center",
    "cite",
    "code",
    "em",
    "font",
    "i",
    "img",
    "kbd",
    "mark",
    "samp",
    "small",
    "span",
    "strong",
    "u",
    "wbr",
];

/// Removal categories a fragment view may apply. Neither carries visible
/// text; every other category is either rejected globally or preserved.
const REMOVABLE: [Removed; 2] = [Removed::Style, Removed::Comment];

/// First line of every fragment view. Rendered lines that start the same way
/// are escaped, so the header can only come from the view itself.
const HEADER: &str = "[HTML rendered as Markdown";

/// A hybrid Markdown view of one HTML fragment, without its recovery instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HtmlFragmentView {
    /// `Title:` line when the fragment has a title, then the rendered body.
    markdown: String,
    /// Non-visible elements removed, as `"1 style, 2 comment"`.
    removed: String,
}

impl HtmlFragmentView {
    /// The view as it replaces the fragment: a header naming any removals
    /// and ending with `instruction`, then the Markdown on the next line.
    pub(crate) fn output(&self, instruction: &str) -> String {
        let mut output = String::from(HEADER);
        if !self.removed.is_empty() {
            let _ = write!(output, "; removed {}", self.removed);
        }
        let _ = write!(output, ". {instruction}]\n{}", self.markdown);
        output
    }
}

impl HtmlExtractor {
    /// Whether a string value is worth parsing as a fragment: it is wrapped
    /// in markup and its first tag is a document opener or a supported HTML5
    /// or SVG element. Text such as `a < b` or `<T>` and arbitrary XML
    /// records fail here, before any parsing; [`Self::render_fragment`] is
    /// the authority.
    #[must_use]
    pub(crate) fn sniff_fragment(&self, value: &str) -> bool {
        let value = value.trim();
        if !value.starts_with('<') || !value.ends_with('>') {
            return false;
        }
        let head = &value.as_bytes()[1..];
        if head.starts_with(b"!--") || starts_with_ignore_ascii_case(head, b"!doctype html") {
            return true;
        }
        let name_len = head
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric())
            .count();
        // A tag name is ASCII, so this byte range is a whole `str` slice.
        let name = value[1..1 + name_len].to_ascii_lowercase();
        matches!(
            head.get(name_len),
            Some(b'>' | b'/' | b' ' | b'\t' | b'\n' | b'\r')
        ) && (VOCABULARY.contains(&name.as_str())
            || plan::EXTRA_SNIFF_TAGS.contains(&name.as_str()))
    }

    /// Renders an HTML fragment as a hybrid Markdown view without dropping
    /// visible text, link or image targets, or local card payloads (`<svg>`,
    /// JSON data `<script>` blocks, `data-*`/`aria-*` attributes, form state).
    /// Returns `None` when global safety fails (hidden/illegible text,
    /// executable scripts, external resources, custom/XML elements, deep
    /// nesting) or when no subtree can be compressed to Markdown.
    #[must_use]
    pub(crate) fn render_fragment(&self, input: &str) -> Option<HtmlFragmentView> {
        let nodes = parse(input)?;
        let plan = FragmentPlan::build(&nodes)?;
        let title = plan
            .head
            .and_then(|head| find_element(&nodes, head, "title"))
            .map(|id| collapse(&text_of(&nodes, id)))
            .filter(|title| !title.is_empty());

        let mut renderer = Renderer::fragment(&nodes, &plan);
        let mut parts = Vec::new();
        if let Some(head) = plan.head {
            renderer.count_removals(head, false);
            for &child in &nodes[head].children {
                if let PlanKind::Verbatim { html, removed, .. } = plan.kind(child) {
                    for (dst, &count) in renderer.removed.iter_mut().zip(removed) {
                        *dst += count;
                    }
                    if !html.trim().is_empty() {
                        parts.push(html.clone());
                    }
                }
            }
        }
        let body_rendered = if matches!(plan.kind(plan.body), PlanKind::Shell { .. }) {
            renderer.block(plan.body, is_sectioning(&nodes[plan.body]))
        } else {
            renderer.blocks(plan.body, is_sectioning(&nodes[plan.body]))
        };
        if renderer.lossy {
            return None;
        }
        if !body_rendered.trim().is_empty() {
            parts.push(escape_header_lines(&body_rendered));
        }
        let combined_body = parts.join("\n\n");
        if combined_body.trim().is_empty() {
            return None;
        }

        let removed = REMOVED_LABELS
            .iter()
            .zip(renderer.removed)
            .enumerate()
            .filter(|(_, (_, count))| *count > 0)
            .map(|(index, (label, count))| {
                REMOVABLE
                    .iter()
                    .any(|category| *category as usize == index)
                    .then(|| format!("{count} {label}"))
            })
            .collect::<Option<Vec<_>>>()?
            .join(", ");

        let mut markdown = String::new();
        if let Some(title) = &title {
            let _ = writeln!(markdown, "Title: {title}");
        }
        let body_start = markdown.len();
        markdown.push_str(&combined_body);
        let masked = masked_targets(&nodes, &plan, body_start, &markdown)?;
        if !texts_in_order(&nodes, &plan, title.as_deref(), &masked) {
            return None;
        }
        Some(HtmlFragmentView { markdown, removed })
    }
}

/// Presentational and global layout attributes that carry no hidden machine
/// state on converted Markdown elements (subject to value checks in `plan` for
/// `id`, `class`, `style`, `role` and `face`).
const ANY_ELEMENT_ATTRIBUTES: &[&str] = &[
    "class",
    "id",
    "style",
    "dir",
    "lang",
    "xml:lang",
    "translate",
    "spellcheck",
    "tabindex",
    "contenteditable",
    "draggable",
    "role",
    "align",
    "valign",
    "bgcolor",
    "background",
    "width",
    "height",
    "border",
    "cellpadding",
    "cellspacing",
    "color",
    "face",
    "size",
    "nowrap",
    "hspace",
    "vspace",
    "frame",
    "rules",
    "clear",
    "noshade",
    "text",
    "link",
    "vlink",
    "alink",
    "leftmargin",
    "topmargin",
    "marginwidth",
    "marginheight",
];

/// Vocabulary elements that flow inline with adjacent text unless CSS gives
/// them a separate layout box.
const INLINE: [&str; 17] = [
    "a", "abbr", "b", "cite", "code", "em", "font", "i", "img", "kbd", "mark", "samp", "small",
    "span", "strong", "u", "wbr",
];

/// Table parts: whatever width or height they are given, they grow to fit
/// their content.
const GROWS_TO_FIT: [&str; 9] = [
    "table", "thead", "tbody", "tfoot", "tr", "td", "th", "col", "colgroup",
];

/// `meta` names that only steer layout or formatting in mail clients.
const META_HINTS: [&str; 3] = [
    "viewport",
    "format-detection",
    "x-apple-disable-message-reformatting",
];

/// Whether a `details` element shows a reader all it holds without a click:
/// it has a `summary`, which labels it where a browser would print its own
/// word, and is open or holds nothing else.
fn shows_all_of_details(nodes: &[Node], id: usize) -> bool {
    let node = &nodes[id];
    let summary = node
        .children
        .iter()
        .copied()
        .find(|&child| nodes[child].is_element("summary"));
    summary.is_some()
        && (node.attr("open").is_some()
            || node.children.iter().all(|&child| {
                Some(child) == summary
                    || match &nodes[child].kind {
                        NodeKind::Text(text) => text.trim().is_empty(),
                        NodeKind::Comment => true,
                        _ => false,
                    }
            }))
}

/// Element-specific attributes that only steer browser fetching, targeting or
/// layout without adding an unprinted fact.
fn element_attribute(element: &str, name: &str) -> bool {
    matches!(
        (element, name),
        (
            "a",
            "href" | "target" | "rel" | "name" | "hreflang" | "type" | "referrerpolicy"
        ) | (
            "img",
            "src"
                | "alt"
                | "srcset"
                | "sizes"
                | "loading"
                | "decoding"
                | "referrerpolicy"
                | "crossorigin"
                | "fetchpriority"
        ) | ("td" | "th", "colspan" | "rowspan" | "scope" | "headers")
            | ("col" | "colgroup", "span")
            | ("details", "open")
            | ("button", "type")
            | ("meta", "charset")
            | ("style", "type" | "media" | "nonce" | "title")
    ) || (element == "html" && (name == "xmlns" || name.starts_with("xmlns:")))
}

/// Whether an `ol` start is a number a Markdown list can begin with:
/// ASCII digits within `i32`, optionally padded with ASCII whitespace.
fn plain_start(value: &str) -> bool {
    let digits = value.trim_matches(|c: char| c.is_ascii_whitespace());
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && digits.parse::<i32>().is_ok()
}

/// Whether the text under `id` has line breaks, tabs or space runs, which
/// a style preserving whitespace would show and Markdown collapses.
fn keeps_whitespace(nodes: &[Node], id: usize) -> bool {
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if let NodeKind::Text(text) = &nodes[id].kind
            && (text.contains(['\n', '\r', '\t', '\u{c}']) || text.contains("  "))
        {
            return true;
        }
        stack.extend(&nodes[id].children);
    }
    false
}

/// Whether the subtree under `id` contains any ASCII whitespace byte.
pub(super) fn subtree_has_ascii_whitespace(nodes: &[Node], id: usize) -> bool {
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if let NodeKind::Text(text) = &nodes[id].kind
            && text.bytes().any(|b| b.is_ascii_whitespace())
        {
            return true;
        }
        stack.extend(&nodes[id].children);
    }
    false
}

fn has_text(nodes: &[Node], id: usize) -> bool {
    !text_of(nodes, id).trim().is_empty()
}

/// Whether `node` is an HTML or SVG element whose subtree is never
/// directly rendered on the canvas (metadata, scripts, stylesheets, or
/// reusable SVG definitions such as `<defs>` and `<symbol>`).
fn is_non_directly_rendered_element(node: &Node) -> bool {
    matches!(
        node.local_name(),
        "head" | "style" | "script" | "noscript" | "template"
    ) || matches!(
        node.name_in(serialize::SVG_NS),
        "defs"
            | "symbol"
            | "title"
            | "desc"
            | "metadata"
            | "clipPath"
            | "mask"
            | "marker"
            | "pattern"
            | "linearGradient"
            | "radialGradient"
            | "filter"
            | "style"
            | "script"
    )
}

/// Whether an SVG element's directly rendered subtree instantiates a `<use>`
/// reference with an `href` (or `xlink:href`) attribute.
pub(super) fn svg_has_rendered_use(nodes: &[Node], root: usize) -> bool {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let node = &nodes[id];
        if let NodeKind::Element { name, .. } = &node.kind {
            if id != root && is_non_directly_rendered_element(node) {
                continue;
            }
            if *name.ns == *serialize::SVG_NS
                && &*name.local == "use"
                && node.attr("href").is_some()
            {
                return true;
            }
            stack.extend_from_slice(&node.children);
        }
    }
    false
}

/// Whether `root` or any descendant draws visible text, replaced media, or
/// interactive controls that a narrow or zero-sized container would clip or
/// overflow.
fn has_box_content(nodes: &[Node], root: usize) -> bool {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let node = &nodes[id];
        match &node.kind {
            NodeKind::Text(text) if !text.trim().is_empty() => return true,
            NodeKind::Element { name, .. } => {
                if id != root && is_non_directly_rendered_element(node) {
                    continue;
                }
                if *name.ns == *serialize::SVG_NS {
                    if (id != root && &*name.local == "svg")
                        || (&*name.local == "image" && node.attr("href").is_some())
                    {
                        return true;
                    }
                } else if *name.ns == *serialize::HTML_NS {
                    let local = &*name.local;
                    if (local == "img"
                        && (node.attr("src").is_some()
                            || node.attr("srcset").is_some()
                            || node.attr("alt").is_some_and(|alt| !alt.trim().is_empty())))
                        || matches!(
                            local,
                            "audio"
                                | "button"
                                | "canvas"
                                | "hr"
                                | "input"
                                | "meter"
                                | "picture"
                                | "progress"
                                | "select"
                                | "textarea"
                                | "video"
                        )
                    {
                        return true;
                    }
                }
                stack.extend_from_slice(&node.children);
            }
            _ => {}
        }
    }
    false
}

/// Verifies that every converted text node and every retained HTML tag or
/// verbatim subtree reappears in rendered order, and that boxes laid out apart
/// stay separated by whitespace.
fn texts_in_order(
    nodes: &[Node],
    plan: &FragmentPlan,
    title: Option<&str>,
    masked_markdown: &str,
) -> bool {
    enum Visit {
        Enter(usize),
        ExitApart,
        ExitLink { cursor_at_enter: usize },
        ExitShell { has_close: bool, apart_after: bool },
    }
    struct OrderCheck<'a> {
        rendered: &'a str,
        masks: &'a [usize],
        mask_idx: usize,
        cursor: usize,
        matched_any: bool,
        apart: bool,
    }

    impl OrderCheck<'_> {
        fn expect_text(&mut self, needle: &str) -> bool {
            let end = self
                .masks
                .get(self.mask_idx)
                .copied()
                .unwrap_or(self.rendered.len());
            if end < self.cursor {
                return false;
            }
            let Some(at) = self.rendered[self.cursor..end].find(needle) else {
                return false;
            };
            if self.apart
                && self.matched_any
                && !self.rendered[self.cursor..self.cursor + at].contains(' ')
            {
                return false;
            }
            self.cursor += at + needle.len();
            self.matched_any = true;
            self.apart = false;
            true
        }

        fn advance_mask(&mut self, require_space_before: bool) -> bool {
            let Some(&pos) = self.masks.get(self.mask_idx) else {
                return false;
            };
            if pos < self.cursor {
                return false;
            }
            let saw_space = self.rendered[self.cursor..pos].contains(' ');
            if require_space_before && self.matched_any && !saw_space {
                return false;
            }
            if saw_space {
                self.apart = false;
            }
            self.cursor = pos + 1;
            self.mask_idx += 1;
            true
        }
    }

    let (rendered, masks) = normalized_with_masks(masked_markdown);
    let mut check = OrderCheck {
        rendered: &rendered,
        masks: &masks,
        mask_idx: 0,
        cursor: 0,
        matched_any: false,
        apart: false,
    };
    if title.is_some_and(|title| !check.expect_text(&normalized(title))) {
        return false;
    }
    check.apart = true;
    if let Some(head) = plan.head {
        for &child in &nodes[head].children {
            if let PlanKind::Verbatim { html, .. } = plan.kind(child)
                && !html.trim().is_empty()
            {
                if !check.advance_mask(true) {
                    return false;
                }
                check.matched_any = true;
                check.apart = true;
            }
        }
    }
    let mut stack = vec![Visit::Enter(plan.body)];
    while let Some(visit) = stack.pop() {
        match visit {
            Visit::ExitApart => {
                check.apart = true;
            }
            Visit::ExitLink { cursor_at_enter } => {
                let empty_link = check.cursor == cursor_at_enter;
                if !check.advance_mask(empty_link && check.apart) {
                    return false;
                }
                if empty_link {
                    check.matched_any = true;
                    check.apart = false;
                }
            }
            Visit::ExitShell {
                has_close,
                apart_after,
            } => {
                if has_close && !check.advance_mask(false) {
                    return false;
                }
                if apart_after {
                    check.apart = true;
                }
            }
            Visit::Enter(id) => match plan.kind(id) {
                PlanKind::Removed(_) => {}
                PlanKind::Verbatim { html, block, .. } => {
                    let box_apart = *block || plan.is_apart_inline(id);
                    if !html.trim().is_empty() {
                        if !check.advance_mask(check.apart || box_apart) {
                            return false;
                        }
                        check.matched_any = true;
                        check.apart = box_apart;
                    }
                }
                PlanKind::Shell { close, inline, .. } => {
                    let box_apart = !*inline || plan.is_apart_inline(id);
                    if !check.advance_mask(check.apart || box_apart) {
                        return false;
                    }
                    check.matched_any = true;
                    check.apart = !*inline;
                    stack.push(Visit::ExitShell {
                        has_close: close.is_some(),
                        apart_after: box_apart,
                    });
                    stack.extend(drawn_children(nodes, plan, id).rev().map(Visit::Enter));
                }
                PlanKind::Markdown => {
                    let node = &nodes[id];
                    match &node.kind {
                        NodeKind::Text(text) => {
                            check.apart |= text.starts_with(char::is_whitespace);
                            let needle = normalized(text);
                            if !needle.is_empty() && !check.expect_text(&needle) {
                                return false;
                            }
                            check.apart |= text.ends_with(char::is_whitespace);
                        }
                        NodeKind::Element { .. } if node.is_element("img") => {
                            if matches!(markdown_img_token(node), Some(Some(_))) {
                                let box_apart = plan.is_apart_inline(id);
                                if !check.advance_mask(check.apart || box_apart) {
                                    return false;
                                }
                                check.matched_any = true;
                                check.apart = box_apart;
                            }
                        }
                        NodeKind::Element { .. } => {
                            let box_apart =
                                !INLINE.contains(&node.local_name()) || plan.is_apart_inline(id);
                            if box_apart {
                                check.apart = true;
                                stack.push(Visit::ExitApart);
                            }
                            if node.is_element("a") && node.attr("href").is_some() {
                                stack.push(Visit::ExitLink {
                                    cursor_at_enter: check.cursor,
                                });
                            }
                            stack.extend(drawn_children(nodes, plan, id).rev().map(Visit::Enter));
                        }
                        _ => {
                            stack.extend(drawn_children(nodes, plan, id).rev().map(Visit::Enter));
                        }
                    }
                }
            },
        }
    }
    check.mask_idx == check.masks.len()
}

/// Replaces every emitted verbatim HTML subtree, shell tag, and Markdown link
/// or image target with a single NUL sentinel in render order, or returns
/// `None` when one does not reappear as expected.
fn masked_targets(
    nodes: &[Node],
    plan: &FragmentPlan,
    body_start: usize,
    markdown: &str,
) -> Option<String> {
    enum Visit {
        Enter(usize),
        Exit(usize),
    }
    if markdown.contains('\0') {
        return None;
    }
    let mut masked = markdown.to_owned();
    let mut cursor = body_start;
    let mut expect = |printed: &str| {
        if printed.is_empty() {
            return None;
        }
        let start = cursor + masked[cursor..].find(printed)?;
        let end = start + printed.len();
        // One NUL per masked span avoids length collisions while matching no text.
        masked.replace_range(start..end, "\0");
        cursor = start + 1;
        Some(())
    };
    if let Some(head) = plan.head {
        for &child in &nodes[head].children {
            if let PlanKind::Verbatim { html, .. } = plan.kind(child)
                && !html.trim().is_empty()
            {
                expect(html)?;
            }
        }
    }
    let mut stack = vec![Visit::Enter(plan.body)];
    while let Some(visit) = stack.pop() {
        match visit {
            Visit::Enter(id) => match plan.kind(id) {
                PlanKind::Removed(_) => {}
                PlanKind::Verbatim { html, .. } => {
                    if !html.trim().is_empty() {
                        expect(html)?;
                    }
                }
                PlanKind::Shell { open, .. } => {
                    expect(open)?;
                    stack.push(Visit::Exit(id));
                    stack.extend(drawn_children(nodes, plan, id).rev().map(Visit::Enter));
                }
                PlanKind::Markdown => {
                    let node = &nodes[id];
                    if let Some(token) = markdown_img_token(node)? {
                        expect(&token)?;
                    }
                    stack.push(Visit::Exit(id));
                    stack.extend(drawn_children(nodes, plan, id).rev().map(Visit::Enter));
                }
            },
            Visit::Exit(id) => match plan.kind(id) {
                PlanKind::Shell {
                    close: Some(close), ..
                } => {
                    expect(close)?;
                }
                PlanKind::Markdown => {
                    let node = &nodes[id];
                    if node.is_element("a")
                        && let Some(href) = node.attr("href")
                    {
                        let tail = link_tail(&destination(href)?, tooltip(node).as_deref());
                        expect(&format!("]{tail}"))?;
                    }
                }
                _ => {}
            },
        }
    }
    Some(masked)
}

fn markdown_img_token(node: &Node) -> Option<Option<String>> {
    if !node.is_element("img") {
        return Some(None);
    }
    let alt = node.attr("alt").map(collapse).unwrap_or_default();
    if let Some(src) = node.attr("src") {
        let tail = link_tail(&destination(src)?, tooltip(node).as_deref());
        Some(Some(format!("![{alt}]{tail}")))
    } else if !alt.is_empty() && brackets_balanced(&alt) {
        Some(Some(format!("![{alt}]")))
    } else {
        Some(None)
    }
}

/// Children of a node in the order the renderer prints them: browser drawing
/// order for GFM-converted tables, source order everywhere else.
fn drawn_children(nodes: &[Node], plan: &FragmentPlan, id: usize) -> std::vec::IntoIter<usize> {
    if nodes[id].is_element("table") && matches!(plan.kind(id), PlanKind::Markdown) {
        table_children(nodes, id).into_iter()
    } else {
        nodes[id].children.clone().into_iter()
    }
}

/// Whitespace runs collapsed to one space, backslashes dropped, and NUL mask
/// byte positions recorded in order.
fn normalized_with_masks(text: &str) -> (String, Vec<usize>) {
    let mut output = String::with_capacity(text.len());
    let mut masks = Vec::new();
    for word in text.split_whitespace() {
        if !output.is_empty() {
            output.push(' ');
        }
        for ch in word.chars() {
            if ch == '\0' {
                masks.push(output.len());
                output.push('\0');
            } else if ch != '\\' {
                output.push(ch);
            }
        }
    }
    (output, masks)
}

/// Whitespace runs collapsed to one space, backslashes dropped.
fn normalized(text: &str) -> String {
    normalized_with_masks(text).0
}

fn escape_header_lines(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.starts_with(HEADER) {
                format!("\\{line}")
            } else {
                line.to_owned()
            }
        })
        .collect()
}

fn starts_with_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/html_fragment_tests.rs");
}
