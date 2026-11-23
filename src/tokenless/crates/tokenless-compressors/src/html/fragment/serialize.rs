//! Compact HTML5 and SVG serialization for preserved fragment subtrees and
//! container shells.

use std::fmt::Write as _;

use html5ever::QualName;

use super::HEADER;
use super::plan::KeptAttr;
use super::{Node, NodeKind, REMOVED_CATEGORIES, Removed};

pub(super) const HTML_NS: &str = "http://www.w3.org/1999/xhtml";
pub(super) const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// Formats a qualified tag or attribute name, keeping any namespace prefix
/// such as `xlink:href` or `xmlns:xlink`.
pub(super) fn qualified_name(name: &QualName) -> String {
    match &name.prefix {
        Some(prefix) if !prefix.is_empty() => format!("{prefix}:{}", name.local),
        _ => name.local.to_string(),
    }
}

/// Whether `node` is an HTML5 void element that takes no closing tag.
pub(super) fn is_html_void(node: &Node) -> bool {
    matches!(
        node.local_name(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Serializes an opening tag with `attrs`. Returns `None` if the tag text
/// would imitate the fragment view header.
pub(super) fn open_tag(node: &Node, attrs: &[KeptAttr], inline_context: bool) -> Option<String> {
    let NodeKind::Element { name, .. } = &node.kind else {
        return None;
    };
    let is_html = *name.ns == *HTML_NS;
    let mut out = String::new();
    let _ = write!(out, "<{}", qualified_name(name));
    for attr in attrs {
        write_attr(&mut out, &attr.name, &attr.value, is_html, inline_context);
    }
    out.push('>');
    (!imitates_header(&out)).then_some(out)
}

/// Serializes the closing tag for `node`, or `None` for HTML5 void elements.
pub(super) fn close_tag(node: &Node) -> Option<String> {
    if is_html_void(node) {
        return None;
    }
    let NodeKind::Element { name, .. } = &node.kind else {
        return None;
    };
    Some(format!("</{}>", qualified_name(name)))
}

/// Serialized `KeepVerbatim` subtree together with any non-visible nodes
/// (`<style>` or comments) omitted inside it.
pub(super) struct SerializedSubtree {
    pub(super) html: String,
    pub(super) removed: [usize; REMOVED_CATEGORIES],
}

/// Serializes the subtree rooted at `root` as compact HTML5/SVG.
///
/// Raw-text elements (`<script type="application/json">`) emit their payload
/// unescaped unless it contains a closing `</script` sequence. Returns `None`
/// if any emitted line would imitate the fragment view header.
pub(super) fn subtree(
    nodes: &[Node],
    root: usize,
    kept_attrs: &[Vec<KeptAttr>],
    ws_mode: &[Option<bool>],
    inline_context: bool,
) -> Option<SerializedSubtree> {
    enum Visit {
        Enter { id: usize, preserve_ws: bool },
        Exit(usize),
    }

    let mut html = String::new();
    let mut removed = [0usize; REMOVED_CATEGORIES];
    let mut stack = vec![Visit::Enter {
        id: root,
        preserve_ws: ws_mode[root].unwrap_or(false),
    }];

    while let Some(visit) = stack.pop() {
        match visit {
            Visit::Enter { id, preserve_ws } => {
                let node = &nodes[id];
                match &node.kind {
                    NodeKind::Comment => {
                        removed[Removed::Comment as usize] += 1;
                    }
                    NodeKind::Element { .. }
                        if node.is_element("style") || node.name_in(SVG_NS) == "style" =>
                    {
                        removed[Removed::Style as usize] += 1;
                    }
                    NodeKind::Element { .. } => {
                        let local_preserve = ws_mode[id].unwrap_or_else(|| {
                            preserve_ws
                                || matches!(node.local_name(), "pre" | "textarea" | "script")
                        });
                        let tag_inline = inline_context && !local_preserve;
                        html.push_str(&open_tag(node, &kept_attrs[id], tag_inline)?);
                        if !is_html_void(node) {
                            stack.push(Visit::Exit(id));
                            for &child in node.children.iter().rev() {
                                stack.push(Visit::Enter {
                                    id: child,
                                    preserve_ws: local_preserve,
                                });
                            }
                        }
                    }
                    NodeKind::Text(text) => {
                        let parent = node.parent.map(|p| &nodes[p]);
                        if parent.is_some_and(|p| p.is_element("script")) {
                            if contains_raw_close(text, b"</script") {
                                return None;
                            }
                            html.push_str(text);
                        } else if parent
                            .is_some_and(|p| p.is_element("style") || p.name_in(SVG_NS) == "style")
                        {
                            if contains_raw_close(text, b"</style") {
                                return None;
                            }
                            html.push_str(text);
                        } else if parent.is_some_and(is_svg_container_without_text)
                            && text.trim().is_empty()
                        {
                            // Drop pure XML indentation between SVG elements.
                        } else {
                            write_escaped_text(&mut html, text, inline_context && !preserve_ws);
                        }
                    }
                    NodeKind::Document | NodeKind::Other => return None,
                }
            }
            Visit::Exit(id) => {
                if let Some(close) = close_tag(&nodes[id]) {
                    html.push_str(&close);
                }
            }
        }
    }

    (!imitates_header(&html)).then_some(SerializedSubtree { html, removed })
}

fn write_attr(out: &mut String, name: &str, value: &str, is_html_ns: bool, escape_newlines: bool) {
    out.push(' ');
    out.push_str(name);
    if value.is_empty() && is_html_ns && is_html_boolean_attr(name) {
        return;
    }
    out.push('=');
    // Single-quoting JSON-valued attributes (`data-actions='{"id":1}'`) avoids
    // `&quot;` bloat and reduces outer JSON string escaping.
    if value.contains('"') && !value.contains('\'') {
        out.push('\'');
        for ch in value.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '\n' if escape_newlines => out.push_str("&#10;"),
                '\r' if escape_newlines => out.push_str("&#13;"),
                _ => out.push(ch),
            }
        }
        out.push('\'');
    } else {
        out.push('"');
        for ch in value.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '"' => out.push_str("&quot;"),
                '\n' if escape_newlines => out.push_str("&#10;"),
                '\r' if escape_newlines => out.push_str("&#13;"),
                _ => out.push(ch),
            }
        }
        out.push('"');
    }
}

fn write_escaped_text(out: &mut String, text: &str, collapse_newlines: bool) {
    let mut prev_space = false;
    for ch in text.chars() {
        if collapse_newlines && ch.is_ascii_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        prev_space = false;
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn is_svg_container_without_text(node: &Node) -> bool {
    let svg_name = node.name_in(SVG_NS);
    !svg_name.is_empty() && !matches!(svg_name, "text" | "tspan" | "textPath")
}

fn is_html_boolean_attr(name: &str) -> bool {
    matches!(
        name,
        "allowfullscreen"
            | "async"
            | "autofocus"
            | "autoplay"
            | "checked"
            | "controls"
            | "default"
            | "defer"
            | "disabled"
            | "formnovalidate"
            | "inert"
            | "ismap"
            | "itemscope"
            | "loop"
            | "multiple"
            | "muted"
            | "nomodule"
            | "novalidate"
            | "open"
            | "playsinline"
            | "readonly"
            | "required"
            | "reversed"
            | "selected"
    )
}

fn contains_raw_close(text: &str, close_prefix: &[u8]) -> bool {
    text.as_bytes()
        .windows(close_prefix.len())
        .any(|window| window.eq_ignore_ascii_case(close_prefix))
}

fn imitates_header(text: &str) -> bool {
    text.lines().any(|line| line.starts_with(HEADER))
}
