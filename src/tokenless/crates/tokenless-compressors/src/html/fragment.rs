//! Markdown views of HTML fragments embedded in structured data.
//!
//! A page view chooses a main-content root and removes navigation, banners
//! and controls around it. A fragment is already the content a field carries,
//! so its view removes nothing visible: it is rendered whole, and it is
//! rejected unless every element belongs to a vocabulary the renderer
//! represents without loss, its styles only change how text looks, list
//! numbering is reproduced, no structure was flattened, and every text node
//! and link or image target reappears in source order. Rejection keeps the
//! original string; that is the expected outcome for forms, scripts, media,
//! foreign markup and anything else whose meaning a Markdown view could
//! silently change.

mod color;
mod css;
mod legibility;

use std::fmt::Write as _;

use super::{
    HtmlExtractor, Node, NodeKind, REMOVED_LABELS, Removed, Renderer, collapse, destination,
    find_element, is_sectioning, link_tail, parse, table_children, text_of, tooltip,
};

/// Elements a fragment may contain. The parser adds `html`, `head` and
/// `body` around any fragment. A `meta` may only declare how to decode or
/// lay out the document, and the head's `title` is rendered as a `Title:`
/// line. Everything else renders either as explicit Markdown structure or
/// as a container that only styles its text. Left out on purpose because
/// the view would change what a reader sees or lose a fact: form fields and
/// their state, scripts, templates, media, frames, `link` (resources and
/// URLs the view drops), `del`/`ins`/`s` (struck prices), `sub`/`sup`
/// (exponents), `q` (generated quotes), `time`/`data` (machine values
/// behind display text), `bdo` (reordered text), `base` (rewrites every
/// link), and any element outside the HTML namespace. The same facts can be
/// carried by styles and attributes, which [`within_vocabulary`] checks on
/// every element.
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
/// text; every other category is outside the vocabulary.
const REMOVABLE: [Removed; 2] = [Removed::Style, Removed::Comment];

/// First line of every fragment view. Rendered lines that start the same way
/// are escaped, so the header can only come from the view itself.
const HEADER: &str = "[HTML rendered as Markdown";

/// A Markdown view of one HTML fragment, without its recovery instruction.
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
    /// in markup and its first tag is a document opener or a vocabulary
    /// element. Text such as `a < b` or `<T>` and XML records fail here,
    /// before any parsing; [`Self::render_fragment`] is the authority.
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
        ) && VOCABULARY.contains(&name.as_str())
    }

    /// Renders an HTML fragment as Markdown without dropping visible text or
    /// link and image targets. Returns `None` when fidelity cannot be
    /// established: an element outside the vocabulary, styles or attributes
    /// that change meaning or visibility, text its styling makes illegible,
    /// list numbering or table headers the view cannot reproduce, flattened
    /// structure, a target the view cannot print, nothing visible to
    /// render, or markup nested past the parse limit. Callers keep the
    /// original then.
    #[must_use]
    pub(crate) fn render_fragment(&self, input: &str) -> Option<HtmlFragmentView> {
        let nodes = parse(input)?;
        if !(0..nodes.len()).all(|id| within_vocabulary(&nodes, id)) {
            return None;
        }
        let html = nodes[0]
            .children
            .iter()
            .copied()
            .find(|&id| nodes[id].is_element("html"))?;
        if !legibility::text_is_legible(&nodes, html) {
            return None;
        }
        let body = nodes[html]
            .children
            .iter()
            .copied()
            .find(|&id| nodes[id].is_element("body"))?;
        let head = nodes[html]
            .children
            .iter()
            .copied()
            .find(|&id| nodes[id].is_element("head"));
        let title = head
            .and_then(|head| find_element(&nodes, head, "title"))
            .map(|id| collapse(&text_of(&nodes, id)))
            .filter(|title| !title.is_empty());

        let mut renderer = Renderer::new(&nodes, true);
        if let Some(head) = head {
            renderer.count_removals(head, false);
        }
        // The fragment is all content: no root is chosen, and its own
        // `header` or `footer` would be outside the vocabulary anyway.
        let body_markdown = renderer.blocks(body, is_sectioning(&nodes[body]));
        if renderer.lossy || body_markdown.trim().is_empty() {
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
        markdown.push_str(&escape_header_lines(&body_markdown));
        let masked = masked_targets(&nodes, body, &markdown)?;
        if !texts_in_order(&nodes, title.as_deref(), body, &masked) {
            return None;
        }
        Some(HtmlFragmentView { markdown, removed })
    }
}

/// Attributes any element may carry: identifiers and classes, text
/// direction and language, editing and focus hints, ARIA roles, and the
/// presentational attributes of legacy and email markup, which change only
/// how text looks. Any other attribute carries something the view cannot
/// show, such as `data-*` state, an `aria-*` label or state, a tooltip
/// outside links and images, a `th` `abbr` or a quote's `cite`, and rejects
/// the fragment unless [`element_attribute`] lists it for its element.
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

/// Elements that flow inline with adjacent text. Every other element of the
/// vocabulary is laid out apart from its neighbours by default: blocks,
/// table parts, list items, line breaks and buttons.
const INLINE: [&str; 17] = [
    "a", "abbr", "b", "cite", "code", "em", "font", "i", "img", "kbd", "mark", "samp", "small",
    "span", "strong", "u", "wbr",
];

/// Table parts and images: whatever width or height they are given, they
/// grow to fit their content.
const GROWS_TO_FIT: [&str; 10] = [
    "table", "thead", "tbody", "tfoot", "tr", "td", "th", "col", "colgroup", "img",
];

/// `meta` names that only steer layout or formatting in mail clients.
const META_HINTS: [&str; 3] = [
    "viewport",
    "format-detection",
    "x-apple-disable-message-reformatting",
];

/// Whether a node may appear in a fragment, with attributes the view prints
/// or can drop. Hidden text would become visible, semantic styles and
/// attribute values would lose their fact, and list numbering the renderer
/// reads differently from a browser would change step references; each
/// rejects. So does content a browser does not show until the reader acts:
/// the body of a closed `details`, whose collapse the view cannot mark.
fn within_vocabulary(nodes: &[Node], id: usize) -> bool {
    let node = &nodes[id];
    let NodeKind::Element { attrs, .. } = &node.kind else {
        return !matches!(node.kind, NodeKind::Other);
    };
    let name = node.local_name();
    VOCABULARY.contains(&name)
        && attrs
            .iter()
            .all(|attr| attribute_is_kept(nodes, id, name, &attr.name.local, &attr.value))
        && match name {
            "style" => css::sheet_is_cosmetic(&text_of(nodes, id)),
            "details" => shows_all_of_details(nodes, id),
            // A browser shows only the document's first title, in its tab;
            // any other title element is hidden.
            "title" => {
                node.parent
                    .is_some_and(|parent| nodes[parent].is_element("head"))
                    && find_element(nodes, 0, "title") == Some(id)
            }
            _ => true,
        }
}

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

/// Whether one attribute of element `id` is printed by the view or changes
/// nothing a reader could miss in it, with its value checked where that
/// matters.
fn attribute_is_kept(nodes: &[Node], id: usize, element: &str, name: &str, value: &str) -> bool {
    let node = &nodes[id];
    match (element, name) {
        (_, "style") => css::inline_is_cosmetic(
            value,
            &css::Styled {
                inline: INLINE.contains(&element) && element != "img",
                grows: GROWS_TO_FIT.contains(&element),
                has_text: &|| has_text(nodes, id),
                keeps_whitespace: &|| keeps_whitespace(nodes, id),
            },
        ),
        (_, "face") => !css::has_symbol_font(value),
        // Hidden only from assistive technology: text inside may be an icon
        // ligature or a duplicate, which the view cannot mark as decorative.
        (_, "aria-hidden") => value.trim().eq_ignore_ascii_case("false") || !has_text(nodes, id),
        // A tooltip is printed as the Markdown title of a printed target.
        ("a", "title") => node.attr("href").is_some(),
        ("img", "title") => node.attr("src").is_some(),
        ("ol", "start") => plain_start(value),
        ("ol", "type") => value.trim() == "1",
        // Any other `meta` holds a fact no reader sees and the view drops,
        // such as a description, or acts on the page, as a refresh does.
        ("meta", "name") => META_HINTS.contains(&value.trim().to_ascii_lowercase().as_str()),
        ("meta", "http-equiv") => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "content-type" | "x-ua-compatible"
        ),
        ("meta", "content") => node.attr("name").is_some() || node.attr("http-equiv").is_some(),
        _ => ANY_ELEMENT_ATTRIBUTES.contains(&name) || element_attribute(element, name),
    }
}

/// Attributes of one element that the view prints, or that only steer how
/// a browser fetches, targets or lays out the element. A `details` `name`
/// is left out: grouping can close a `details` marked `open`.
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
/// ASCII digits within `i32`, optionally padded with ASCII whitespace. A
/// negative start has no Markdown marker, and a suffixed or overflowing
/// one is malformed, so either leaves the fragment as it is.
fn plain_start(value: &str) -> bool {
    let digits = value.trim_matches(|c: char| c.is_ascii_whitespace());
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && digits.parse::<i32>().is_ok()
}

/// Whether the text under `id` has line breaks, tabs or space runs, which
/// a style preserving whitespace would show and the view collapses. A `br`
/// needs no such style: the view breaks the line there in every context.
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

fn has_text(nodes: &[Node], id: usize) -> bool {
    !text_of(nodes, id).trim().is_empty()
}

/// Every text node of the title and body reappears in the rendered Markdown
/// in the order a browser draws it, which is source order except for table
/// captions, headers and footers, and text a browser shows apart stays
/// apart: where the source separates two text nodes, by whitespace or by an
/// element laid out apart from inline text, the view has whitespace between
/// them too. So no renderer path can fuse `a<br>b` into `ab` and still
/// pass. Both sides are compared with whitespace collapsed and backslashes
/// dropped, since the renderer only adds escapes, wrapping and line breaks.
/// `markdown` has its link and image targets masked, so text cannot match
/// inside them.
fn texts_in_order(nodes: &[Node], title: Option<&str>, body: usize, markdown: &str) -> bool {
    enum Visit {
        Enter(usize),
        ExitApart,
    }
    let rendered = normalized(markdown);
    let mut cursor = 0;
    let mut matched_any = false;
    let mut expect = |needle: &str, apart: bool| {
        let Some(at) = rendered[cursor..].find(needle) else {
            return false;
        };
        if apart && matched_any && !rendered[cursor..cursor + at].contains(' ') {
            return false;
        }
        cursor += at + needle.len();
        matched_any = true;
        true
    };
    if title.is_some_and(|title| !expect(&normalized(title), false)) {
        return false;
    }
    // The title line ends before the body starts.
    let mut apart = true;
    let mut stack = vec![Visit::Enter(body)];
    while let Some(visit) = stack.pop() {
        let Visit::Enter(id) = visit else {
            apart = true;
            continue;
        };
        let node = &nodes[id];
        match &node.kind {
            NodeKind::Text(text) => {
                apart |= text.starts_with(char::is_whitespace);
                let needle = normalized(text);
                if !needle.is_empty() {
                    if !expect(&needle, apart) {
                        return false;
                    }
                    apart = false;
                }
                apart |= text.ends_with(char::is_whitespace);
            }
            // Style text is not visible and is removed with its element.
            NodeKind::Element { .. } if node.is_element("style") => {}
            NodeKind::Element { .. } if !INLINE.contains(&node.local_name()) => {
                apart = true;
                stack.push(Visit::ExitApart);
                stack.extend(drawn_children(nodes, id).rev().map(Visit::Enter));
            }
            _ => stack.extend(drawn_children(nodes, id).rev().map(Visit::Enter)),
        }
    }
    true
}

/// The rendered Markdown with every link and image target masked, or `None`
/// when one does not reappear, with its tooltip as the Markdown title, in
/// the order the renderer prints them: tables in drawing order, an image
/// when it is reached, a link after its text, which may hold images of its
/// own. Masking keeps text nodes from matching inside a URL, a title or alt
/// text, none of which is a text node.
fn masked_targets(nodes: &[Node], body: usize, markdown: &str) -> Option<String> {
    enum Visit {
        Enter(usize),
        Exit(usize),
    }
    let mut masked = markdown.to_owned();
    let mut cursor = 0;
    let mut expect = |printed: &str| {
        let start = cursor + masked[cursor..].find(printed)?;
        let end = start + printed.len();
        // NUL never occurs in parsed text, so masked bytes match no text.
        masked.replace_range(start..end, &"\0".repeat(end - start));
        cursor = end;
        Some(())
    };
    let mut stack = vec![Visit::Enter(body)];
    while let Some(visit) = stack.pop() {
        match visit {
            Visit::Enter(id) => {
                let node = &nodes[id];
                if node.is_element("img")
                    && let Some(src) = node.attr("src")
                {
                    let alt = node.attr("alt").map(collapse).unwrap_or_default();
                    let tail = link_tail(&destination(src)?, tooltip(node).as_deref());
                    expect(&format!("![{alt}]{tail}"))?;
                }
                stack.push(Visit::Exit(id));
                stack.extend(drawn_children(nodes, id).rev().map(Visit::Enter));
            }
            Visit::Exit(id) => {
                let node = &nodes[id];
                if node.is_element("a")
                    && let Some(href) = node.attr("href")
                {
                    let tail = link_tail(&destination(href)?, tooltip(node).as_deref());
                    expect(&format!("]{tail}"))?;
                }
            }
        }
    }
    Some(masked)
}

/// Children of a node in the order the renderer prints them, which for a
/// table is a browser's drawing order rather than source order.
fn drawn_children(nodes: &[Node], id: usize) -> std::vec::IntoIter<usize> {
    if nodes[id].is_element("table") {
        table_children(nodes, id).into_iter()
    } else {
        nodes[id].children.clone().into_iter()
    }
}

/// Whitespace runs collapsed to one space, backslashes dropped.
fn normalized(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !output.is_empty() {
            output.push(' ');
        }
        output.extend(word.chars().filter(|&c| c != '\\'));
    }
    output
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
