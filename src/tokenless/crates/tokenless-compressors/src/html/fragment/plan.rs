//! Subtree action planning for hybrid HTML fragment views.
//!
//! Global hazards (hidden or illegible text, executable scripts, external
//! resources, unreadable or overflowing styles) reject the whole fragment.
//! Otherwise each node is assigned one of four actions bottom-up:
//! - [`PlanKind::Markdown`]: rendered as Markdown by the shared `Renderer`.
//! - [`PlanKind::Removed`]: non-visible `<style>` or comment counted in header.
//! - [`PlanKind::Shell`]: container tag retained with semantic attributes while
//!   its interior is compressed recursively.
//! - [`PlanKind::Verbatim`]: atomic or local-semantic HTML/SVG subtree emitted
//!   in place as compact markup.

use std::collections::HashSet;

use super::css::{self, CssVerdict, InlineStyle, Styled, WsMode};
use super::legibility;
use super::serialize::{self, HTML_NS, SVG_NS};
use super::{
    ANY_ELEMENT_ATTRIBUTES, GROWS_TO_FIT, INLINE, META_HINTS, VOCABULARY, element_attribute,
    has_box_content, has_text, keeps_whitespace, plain_start, shows_all_of_details,
    starts_with_ignore_ascii_case, subtree_has_ascii_whitespace, svg_has_rendered_use,
};
use crate::html::{
    ADMONITION_LABELS, MATHML_NS, MAX_TABLE_COLUMNS, Node, NodeKind, REMOVED_CATEGORIES, Removed,
    brackets_balanced, collapse, destination, find_element, is_block, language_of, span,
    table_children, text_of, url_text,
};

/// Standard HTML5 semantic containers outside [`VOCABULARY`] whose interior can
/// still be rendered as Markdown inside a retained shell tag.
const SHELL_HTML_ELEMENTS: [&str; 6] = ["header", "footer", "nav", "aside", "hgroup", "search"];

/// Standard HTML5 elements outside [`VOCABULARY`] that carry local phrasing,
/// media, or form semantics and are retained whole as compact HTML.
const ATOMIC_HTML_ELEMENTS: [&str; 39] = [
    "area", "audio", "bdi", "bdo", "canvas", "data", "datalist", "del", "dfn", "form", "input",
    "ins", "keygen", "label", "legend", "map", "meter", "optgroup", "option", "output", "picture",
    "progress", "q", "rp", "rt", "ruby", "s", "select", "source", "sub", "sup", "textarea", "time",
    "track", "var", "video", "menu", "param", "slot",
];

/// Standard HTML5 and SVG opening tags accepted by
/// [`super::HtmlExtractor::sniff_fragment`] in addition to [`VOCABULARY`].
pub(super) const EXTRA_SNIFF_TAGS: [&str; 21] = [
    "svg", "script", "header", "footer", "nav", "aside", "hgroup", "search", "del", "ins", "s",
    "sub", "sup", "q", "time", "data", "bdo", "bdi", "ruby", "dfn", "output",
];

/// Retained HTML/SVG attribute name and value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptAttr {
    pub(crate) name: String,
    pub(crate) value: String,
}

/// Planned rendering disposition for one DOM node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanKind {
    Markdown,
    Removed(Removed),
    Shell {
        open: String,
        close: Option<String>,
        structural: bool,
        inline: bool,
    },
    Verbatim {
        html: String,
        block: bool,
        removed: [usize; REMOVED_CATEGORIES],
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Draft {
    Markdown,
    Removed(Removed),
    Shell { structural: bool, inline: bool },
    Verbatim,
}

/// Validated rendering plan for every node in a parsed HTML fragment.
#[derive(Debug)]
pub(crate) struct FragmentPlan {
    kinds: Vec<PlanKind>,
    apart_inline: Vec<bool>,
    pub(super) head: Option<usize>,
    pub(super) body: usize,
}

impl FragmentPlan {
    /// Builds a hybrid fragment plan, returning `None` when global safety fails
    /// or when no content element is meaningfully compressed.
    pub(super) fn build(nodes: &[Node]) -> Option<Self> {
        let html = nodes
            .first()?
            .children
            .iter()
            .copied()
            .find(|&id| nodes[id].is_element("html"))?;
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

        let len = nodes.len();
        let mut inline_styles = vec![InlineStyle::default(); len];
        let mut referenced_ids = HashSet::new();

        for (id, node) in nodes.iter().enumerate() {
            match &node.kind {
                NodeKind::Other => return None,
                NodeKind::Document | NodeKind::Text(_) | NodeKind::Comment => continue,
                NodeKind::Element { name, attrs, .. } => {
                    let is_html = *name.ns == *HTML_NS;
                    let is_svg = *name.ns == *SVG_NS;
                    let is_math = *name.ns == *MATHML_NS;
                    if !(is_html || is_svg || is_math) {
                        return None;
                    }
                    let local = &*name.local;
                    if is_html {
                        if !html_element_allowed(nodes, id, head, local) {
                            return None;
                        }
                    } else if is_svg {
                        if matches!(local, "script" | "foreignObject")
                            || (local == "style" && !style_element_is_cosmetic(nodes, id))
                        {
                            return None;
                        }
                    } else if local == "annotation-xml" {
                        return None;
                    }

                    let is_media_with_zero_guard = (is_html
                        && ((local == "img"
                            && (node.attr("src").is_some()
                                || node.attr("srcset").is_some()
                                || node.attr("alt").is_some_and(|alt| !alt.trim().is_empty())))
                            || matches!(local, "video" | "input" | "source")))
                        || (is_svg
                            && ((local == "svg"
                                && (has_box_content(nodes, id)
                                    || svg_has_rendered_use(nodes, id)))
                                || (local == "use" && node.attr("href").is_some())));

                    for attr in attrs {
                        let attr_local = &*attr.name.local;
                        let attr_val = &*attr.value;
                        if attr_local.eq_ignore_ascii_case("hidden")
                            || starts_with_ignore_ascii_case(attr_local.as_bytes(), b"on")
                        {
                            return None;
                        }
                        if is_media_with_zero_guard
                            && matches!(attr_local, "width" | "height")
                            && is_zero_dimension_attr(attr_val)
                        {
                            return None;
                        }
                        if ((is_html && matches!(attr_local, "action" | "formaction"))
                            || (is_svg && attr_local == "href"))
                            && has_executable_or_data_scheme(attr_val)
                        {
                            return None;
                        }
                        if is_html && attr_local == "face" && css::has_symbol_font(attr_val) {
                            return None;
                        }
                        collect_id_refs(attr_local, attr_val, &mut referenced_ids);
                    }
                }
            }
        }

        // Classify inline styles top-down so a flex/grid container is always
        // recorded before its direct children are evaluated as flex/grid items,
        // and ancestor whitespace preservation is known for descendants.
        let mut ws_mode = vec![None; len];
        let mut style_stack = vec![(0usize, WsMode::Collapse)];
        while let Some((id, anc_ws)) = style_stack.pop() {
            let node = &nodes[id];
            let mut default_ws = anc_ws;
            let mut ws_override = None;
            if let NodeKind::Element { name, .. } = &node.kind {
                let is_html = *name.ns == *HTML_NS;
                let is_svg = *name.ns == *SVG_NS;
                let local = &*name.local;
                if is_html && matches!(local, "pre" | "textarea") {
                    default_ws = WsMode::PreserveAll;
                }
                if let Some(style_val) = node.attr("style") {
                    ws_override = style_whitespace_override(style_val, anc_ws);
                    let flex_or_grid_item = node
                        .parent
                        .is_some_and(|parent| inline_styles[parent].flex_or_grid);
                    let classified = css::classify_inline(
                        style_val,
                        &Styled {
                            inline: is_html && INLINE.contains(&local) && local != "img",
                            grows: is_html && GROWS_TO_FIT.contains(&local),
                            replaced_media: is_html
                                && local == "img"
                                && (node.attr("src").is_some()
                                    || node.attr("srcset").is_some()
                                    || node.attr("alt").is_some_and(|alt| !alt.trim().is_empty())),
                            flex_or_grid_item,
                            svg: is_svg,
                            svg_renders_use: is_svg && svg_has_rendered_use(nodes, id),
                            ancestor_ws: anc_ws,
                            default_ws,
                            has_content: &|| has_box_content(nodes, id),
                            keeps_whitespace: &|| {
                                if default_ws != WsMode::Collapse {
                                    subtree_has_ascii_whitespace(nodes, id)
                                } else {
                                    keeps_whitespace(nodes, id)
                                }
                            },
                        },
                    );
                    if classified.verdict == CssVerdict::Hazard {
                        return None;
                    }
                    inline_styles[id] = classified;
                }
            }
            let child_ws = ws_override.unwrap_or(default_ws);
            ws_mode[id] = Some(child_ws.preserves_newlines());
            for &child in node.children.iter().rev() {
                style_stack.push((child, child_ws));
            }
        }

        if !legibility::text_is_legible(nodes, html) {
            return None;
        }

        // Mark nodes inside atomic subtrees and inherited CSS `dir` top-down so
        // descendants keep or reset inherited directionality accurately.
        let mut inside_atomic = vec![false; len];
        let mut parent_dirs = vec![EffectiveDir::Ltr; len];
        let mut stack = vec![(0usize, false, EffectiveDir::Ltr)];
        while let Some((id, atomic_anc, inherited_dir)) = stack.pop() {
            inside_atomic[id] = atomic_anc;
            parent_dirs[id] = inherited_dir;
            let is_atomic_here = atomic_anc || is_inherently_verbatim(nodes, id, &inline_styles);
            let next_dir = element_effective_dir(&nodes[id], inherited_dir);
            for &child in nodes[id].children.iter().rev() {
                stack.push((child, is_atomic_here, next_dir));
            }
        }

        let mut apart_inline = vec![false; len];
        let mut kept_attrs = vec![Vec::new(); len];
        let mut has_html_only_semantic = vec![false; len];
        let mut stripped_cosmetic = vec![false; len];

        for (id, node) in nodes.iter().enumerate() {
            let NodeKind::Element { name, attrs, .. } = &node.kind else {
                continue;
            };
            let is_html = *name.ns == *HTML_NS;
            let is_svg = *name.ns == *SVG_NS;
            let local = &*name.local;

            apart_inline[id] = inline_styles[id].atomic_inline
                || (is_html
                    && matches!(local, "button" | "label" | "input" | "select" | "textarea"))
                || node
                    .parent
                    .is_some_and(|parent| inline_styles[parent].flex_or_grid);

            if !is_html {
                // Outermost SVG root drops only purely cosmetic `style` and
                // unreferenced `class`; nested `<svg>`, inner SVG geometry, and
                // MathML elements keep every attribute intact.
                let is_svg_root = is_svg
                    && local == "svg"
                    && node
                        .parent
                        .is_none_or(|parent| nodes[parent].name_in(SVG_NS).is_empty());
                let keep_class_for_icon = node
                    .attr("aria-hidden")
                    .is_some_and(|v| !v.trim().eq_ignore_ascii_case("false"));
                for attr in attrs {
                    let qname = serialize::qualified_name(&attr.name);
                    let val = &*attr.value;
                    if is_svg_root
                        && ((qname == "style" && inline_styles[id].verdict == CssVerdict::Cosmetic)
                            || (qname == "class" && !keep_class_for_icon))
                    {
                        stripped_cosmetic[id] = true;
                        continue;
                    }
                    kept_attrs[id].push(KeptAttr {
                        name: qname,
                        value: val.to_owned(),
                    });
                }
                continue;
            }

            let keep_class_for_icon = node
                .attr("aria-hidden")
                .is_some_and(|v| !v.trim().eq_ignore_ascii_case("false") && has_text(nodes, id));

            for attr in attrs {
                let qname = serialize::qualified_name(&attr.name);
                let attr_local = &*attr.name.local;
                let val = &*attr.value;
                match classify_html_attr(
                    nodes,
                    id,
                    local,
                    &qname,
                    attr_local,
                    val,
                    inline_styles[id].verdict,
                    keep_class_for_icon,
                    parent_dirs[id],
                    &referenced_ids,
                ) {
                    AttrRole::Cosmetic => {
                        stripped_cosmetic[id] = true;
                    }
                    AttrRole::MarkdownRepresented => {
                        kept_attrs[id].push(KeptAttr {
                            name: qname,
                            value: val.to_owned(),
                        });
                    }
                    AttrRole::HtmlOnlySemantic => {
                        has_html_only_semantic[id] = true;
                        kept_attrs[id].push(KeptAttr {
                            name: qname,
                            value: val.to_owned(),
                        });
                    }
                }
            }
        }

        let mut draft = vec![Draft::Markdown; len];
        let postorder = postorder_nodes(nodes, 0);

        let mut verbatim_cache: Vec<Option<serialize::SerializedSubtree>> =
            (0..len).map(|_| None).collect();

        for &id in &postorder {
            if inside_atomic[id] {
                continue;
            }
            let node = &nodes[id];
            match &node.kind {
                NodeKind::Document | NodeKind::Text(_) => {
                    draft[id] = Draft::Markdown;
                }
                NodeKind::Comment => {
                    draft[id] = Draft::Removed(Removed::Comment);
                }
                NodeKind::Other => return None,
                NodeKind::Element { name, .. } => {
                    if (*name.ns == *HTML_NS || *name.ns == *SVG_NS) && &*name.local == "style" {
                        draft[id] = Draft::Removed(Removed::Style);
                        continue;
                    }
                    if *name.ns != *HTML_NS {
                        cache_verbatim(
                            nodes,
                            id,
                            !is_block_verbatim_element(node),
                            &kept_attrs,
                            &ws_mode,
                            &mut verbatim_cache,
                        )?;
                        draft[id] = Draft::Verbatim;
                        continue;
                    }
                    let local = &*name.local;
                    if matches!(local, "html" | "head" | "meta" | "title") {
                        if has_html_only_semantic[id] {
                            return None;
                        }
                        draft[id] = Draft::Markdown;
                        continue;
                    }
                    if local == "body" {
                        draft[id] = if has_html_only_semantic[id] {
                            Draft::Shell {
                                structural: false,
                                inline: false,
                            }
                        } else {
                            Draft::Markdown
                        };
                        continue;
                    }
                    if is_inherently_verbatim(nodes, id, &inline_styles) {
                        cache_verbatim(
                            nodes,
                            id,
                            !is_block_verbatim_element(node),
                            &kept_attrs,
                            &ws_mode,
                            &mut verbatim_cache,
                        )?;
                        draft[id] = Draft::Verbatim;
                        continue;
                    }

                    match local {
                        "img" => {
                            let alt = node.attr("alt").map(collapse).unwrap_or_default();
                            let ok = !has_html_only_semantic[id]
                                && brackets_balanced(&alt)
                                && node
                                    .attr("src")
                                    .is_none_or(|src| destination(src).is_some())
                                && (node.attr("src").is_some()
                                    || !alt.is_empty()
                                    || node.attr("alt").is_none_or(|raw| raw.is_empty()));
                            if ok {
                                draft[id] = Draft::Markdown;
                            } else {
                                cache_verbatim(
                                    nodes,
                                    id,
                                    true,
                                    &kept_attrs,
                                    &ws_mode,
                                    &mut verbatim_cache,
                                )?;
                                draft[id] = Draft::Verbatim;
                            }
                        }
                        "br" | "hr" | "wbr" => {
                            if has_html_only_semantic[id] {
                                cache_verbatim(
                                    nodes,
                                    id,
                                    local != "hr",
                                    &kept_attrs,
                                    &ws_mode,
                                    &mut verbatim_cache,
                                )?;
                                draft[id] = Draft::Verbatim;
                            } else {
                                draft[id] = Draft::Markdown;
                            }
                        }
                        "button" => {
                            let plain = !has_html_only_semantic[id]
                                && !subtree_has(&node.children, nodes, |cid| {
                                    matches!(draft[cid], Draft::Shell { .. } | Draft::Verbatim)
                                });
                            if plain {
                                draft[id] = Draft::Markdown;
                            } else {
                                cache_verbatim(
                                    nodes,
                                    id,
                                    true,
                                    &kept_attrs,
                                    &ws_mode,
                                    &mut verbatim_cache,
                                )?;
                                draft[id] = Draft::Verbatim;
                            }
                        }
                        "a" => {
                            let ok = !has_html_only_semantic[id]
                                && link_can_be_markdown(nodes, id, &draft, &verbatim_cache);
                            if ok {
                                draft[id] = Draft::Markdown;
                            } else {
                                cache_verbatim(
                                    nodes,
                                    id,
                                    true,
                                    &kept_attrs,
                                    &ws_mode,
                                    &mut verbatim_cache,
                                )?;
                                draft[id] = Draft::Verbatim;
                            }
                        }
                        "pre" | "code" | "kbd" | "samp" => {
                            let ok = !has_html_only_semantic[id]
                                && code_can_be_markdown(nodes, id, &draft);
                            if ok {
                                draft[id] = Draft::Markdown;
                            } else {
                                cache_verbatim(
                                    nodes,
                                    id,
                                    local != "pre",
                                    &kept_attrs,
                                    &ws_mode,
                                    &mut verbatim_cache,
                                )?;
                                draft[id] = Draft::Verbatim;
                            }
                        }
                        "ul" | "ol" => {
                            if list_is_malformed(nodes, id, local == "ol") {
                                cache_verbatim(
                                    nodes,
                                    id,
                                    false,
                                    &kept_attrs,
                                    &ws_mode,
                                    &mut verbatim_cache,
                                )?;
                                draft[id] = Draft::Verbatim;
                            } else if list_can_be_markdown(
                                nodes,
                                id,
                                local == "ol",
                                &has_html_only_semantic,
                                &draft,
                                &verbatim_cache,
                            ) {
                                draft[id] = Draft::Markdown;
                            } else {
                                draft[id] = Draft::Shell {
                                    structural: true,
                                    inline: false,
                                };
                                for &child in &node.children {
                                    if nodes[child].is_element("li")
                                        && draft[child] != Draft::Verbatim
                                    {
                                        draft[child] = Draft::Shell {
                                            structural: false,
                                            inline: false,
                                        };
                                    }
                                }
                            }
                        }
                        "dl" => {
                            if dl_is_malformed(nodes, id) {
                                cache_verbatim(
                                    nodes,
                                    id,
                                    false,
                                    &kept_attrs,
                                    &ws_mode,
                                    &mut verbatim_cache,
                                )?;
                                draft[id] = Draft::Verbatim;
                            } else if dl_can_be_markdown(
                                nodes,
                                id,
                                &has_html_only_semantic,
                                &draft,
                                &verbatim_cache,
                            ) {
                                draft[id] = Draft::Markdown;
                            } else {
                                draft[id] = Draft::Shell {
                                    structural: true,
                                    inline: false,
                                };
                                for &child in &node.children {
                                    if matches!(nodes[child].local_name(), "dt" | "dd")
                                        && draft[child] != Draft::Verbatim
                                    {
                                        draft[child] = Draft::Shell {
                                            structural: false,
                                            inline: false,
                                        };
                                    }
                                }
                            }
                        }
                        "table" => {
                            if table_is_malformed(nodes, id) {
                                cache_verbatim(
                                    nodes,
                                    id,
                                    false,
                                    &kept_attrs,
                                    &ws_mode,
                                    &mut verbatim_cache,
                                )?;
                                draft[id] = Draft::Verbatim;
                            } else if table_can_be_gfm(
                                nodes,
                                id,
                                &has_html_only_semantic,
                                &kept_attrs,
                                &draft,
                                &verbatim_cache,
                            ) {
                                draft[id] = Draft::Markdown;
                            } else {
                                mark_table_shells(nodes, id, &mut draft);
                            }
                        }
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                            let needs_shell = has_html_only_semantic[id]
                                || subtree_has_shell_or_multiline(
                                    nodes,
                                    &node.children,
                                    &draft,
                                    &verbatim_cache,
                                );
                            draft[id] = if needs_shell {
                                Draft::Shell {
                                    structural: false,
                                    inline: false,
                                }
                            } else {
                                Draft::Markdown
                            };
                        }
                        "blockquote" => {
                            let needs_shell = has_html_only_semantic[id]
                                || subtree_has_multiline_verbatim(
                                    nodes,
                                    &node.children,
                                    &draft,
                                    &verbatim_cache,
                                );
                            draft[id] = if needs_shell {
                                Draft::Shell {
                                    structural: false,
                                    inline: false,
                                }
                            } else {
                                Draft::Markdown
                            };
                        }
                        _ if SHELL_HTML_ELEMENTS.contains(&local) => {
                            draft[id] = Draft::Shell {
                                structural: false,
                                inline: false,
                            };
                        }
                        "strong" | "b" | "em" | "i" => {
                            let needs_shell = has_html_only_semantic[id]
                                || subtree_has_shell_or_multiline(
                                    nodes,
                                    &node.children,
                                    &draft,
                                    &verbatim_cache,
                                );
                            draft[id] = if needs_shell {
                                Draft::Shell {
                                    structural: false,
                                    inline: true,
                                }
                            } else {
                                Draft::Markdown
                            };
                        }
                        _ => {
                            let inline = INLINE.contains(&local);
                            let needs_shell = has_html_only_semantic[id]
                                || (admonition_container(node)
                                    && subtree_has_multiline_verbatim(
                                        nodes,
                                        &node.children,
                                        &draft,
                                        &verbatim_cache,
                                    ));
                            draft[id] = if needs_shell {
                                Draft::Shell {
                                    structural: false,
                                    inline,
                                }
                            } else {
                                Draft::Markdown
                            };
                        }
                    }
                }
            }
        }

        // Bottom-up promotion: a `Shell` subtree that converts no rich content
        // to Markdown, removes no `<style>`, and strips no cosmetic attributes
        // is emitted directly as compact `Verbatim` HTML instead of adding
        // blank-line shell padding.
        let mut compressed_below = vec![false; len];
        for &id in &postorder {
            if inside_atomic[id] {
                continue;
            }
            let node = &nodes[id];
            let child_compressed = node.children.iter().any(|&cid| {
                compressed_below[cid]
                    || matches!(draft[cid], Draft::Removed(Removed::Style))
                    || (matches!(draft[cid], Draft::Markdown)
                        && counts_as_markdown_element(&nodes[cid]))
            });
            if draft[id] == Draft::Verbatim {
                compressed_below[id] = false;
            } else if let Draft::Shell { .. } = draft[id] {
                if !child_compressed && !stripped_cosmetic[id] {
                    if id == body {
                        return None;
                    }
                    cache_verbatim(
                        nodes,
                        id,
                        !is_block_verbatim_element(node),
                        &kept_attrs,
                        &ws_mode,
                        &mut verbatim_cache,
                    )?;
                    draft[id] = Draft::Verbatim;
                    compressed_below[id] = false;
                } else {
                    compressed_below[id] = true;
                }
            } else {
                compressed_below[id] = child_compressed;
            }
        }

        // Reject fragments where nothing outside verbatim subtrees was
        // converted to Markdown or compressed inside a retained shell.
        let mut any_markdown_conversion = false;
        let mut kinds = vec![PlanKind::Markdown; len];
        let mut reach = vec![0usize];
        while let Some(id) = reach.pop() {
            match draft[id] {
                Draft::Markdown => {
                    if counts_as_markdown_element(&nodes[id]) {
                        any_markdown_conversion = true;
                    }
                    kinds[id] = PlanKind::Markdown;
                    reach.extend_from_slice(&nodes[id].children);
                }
                Draft::Removed(cat) => {
                    kinds[id] = PlanKind::Removed(cat);
                }
                Draft::Shell { structural, inline } => {
                    any_markdown_conversion = true;
                    let open = serialize::open_tag(&nodes[id], &kept_attrs[id], inline)?;
                    let close = serialize::close_tag(&nodes[id]);
                    kinds[id] = PlanKind::Shell {
                        open,
                        close,
                        structural,
                        inline,
                    };
                    reach.extend_from_slice(&nodes[id].children);
                }
                Draft::Verbatim => {
                    let inline_ctx = !is_block_verbatim_element(&nodes[id]);
                    let built = match verbatim_cache[id].take() {
                        Some(built) => built,
                        None => serialize::subtree(nodes, id, &kept_attrs, &ws_mode, inline_ctx)?,
                    };
                    kinds[id] = PlanKind::Verbatim {
                        html: built.html,
                        block: !inline_ctx,
                        removed: built.removed,
                    };
                }
            }
        }
        if !any_markdown_conversion {
            return None;
        }

        Some(Self {
            kinds,
            apart_inline,
            head,
            body,
        })
    }

    pub(crate) fn kind(&self, id: usize) -> &PlanKind {
        &self.kinds[id]
    }

    pub(crate) fn is_apart_inline(&self, id: usize) -> bool {
        self.apart_inline[id]
    }
}

fn cache_verbatim(
    nodes: &[Node],
    id: usize,
    inline_ctx: bool,
    kept_attrs: &[Vec<KeptAttr>],
    ws_mode: &[Option<bool>],
    verbatim_cache: &mut [Option<serialize::SerializedSubtree>],
) -> Option<()> {
    if verbatim_cache[id].is_none() {
        let built = serialize::subtree(nodes, id, kept_attrs, ws_mode, inline_ctx)?;
        verbatim_cache[id] = Some(built);
    }
    Some(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EffectiveDir {
    Ltr,
    Rtl,
    Auto,
}

fn element_effective_dir(node: &Node, inherited: EffectiveDir) -> EffectiveDir {
    let NodeKind::Element { name, .. } = &node.kind else {
        return inherited;
    };
    if *name.ns != *HTML_NS {
        return inherited;
    }
    match node.attr("dir").map(str::trim) {
        Some(val) if val.eq_ignore_ascii_case("ltr") => EffectiveDir::Ltr,
        Some(val) if val.eq_ignore_ascii_case("rtl") => EffectiveDir::Rtl,
        Some(val) if val.eq_ignore_ascii_case("auto") => EffectiveDir::Auto,
        _ if &*name.local == "bdi" => EffectiveDir::Auto,
        _ => inherited,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrRole {
    Cosmetic,
    MarkdownRepresented,
    HtmlOnlySemantic,
}

#[allow(clippy::too_many_arguments)]
fn classify_html_attr(
    nodes: &[Node],
    id: usize,
    element: &str,
    qname: &str,
    local: &str,
    value: &str,
    style_verdict: CssVerdict,
    keep_class_for_icon: bool,
    parent_dir: EffectiveDir,
    referenced_ids: &HashSet<String>,
) -> AttrRole {
    let node = &nodes[id];
    if qname.contains(':')
        && qname != "xml:lang"
        && !(element == "html" && qname.starts_with("xmlns:"))
    {
        return AttrRole::HtmlOnlySemantic;
    }
    match (element, local) {
        (_, "style") => match style_verdict {
            CssVerdict::Cosmetic => AttrRole::Cosmetic,
            CssVerdict::LocalSemantic | CssVerdict::Hazard => AttrRole::HtmlOnlySemantic,
        },
        (_, "class") => {
            if keep_class_for_icon {
                AttrRole::HtmlOnlySemantic
            } else if admonition_container(node) || class_carries_code_language(nodes, id, element)
            {
                AttrRole::MarkdownRepresented
            } else {
                AttrRole::Cosmetic
            }
        }
        (_, "id") | ("a", "name") => {
            if !value.is_empty() && referenced_ids.contains(value) {
                AttrRole::HtmlOnlySemantic
            } else {
                AttrRole::Cosmetic
            }
        }
        (_, "dir") => {
            let trimmed = value.trim();
            if (element == "bdo" && !trimmed.is_empty())
                || trimmed.eq_ignore_ascii_case("rtl")
                || trimmed.eq_ignore_ascii_case("auto")
                || (trimmed.eq_ignore_ascii_case("ltr")
                    && (element == "bdi" || parent_dir != EffectiveDir::Ltr))
            {
                AttrRole::HtmlOnlySemantic
            } else {
                AttrRole::Cosmetic
            }
        }
        ("select", "size") | ("canvas", "width" | "height") => AttrRole::HtmlOnlySemantic,
        (_, "role") => {
            if matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "presentation" | "none"
            ) {
                AttrRole::Cosmetic
            } else {
                AttrRole::HtmlOnlySemantic
            }
        }
        (_, "aria-hidden") => {
            if value.trim().eq_ignore_ascii_case("false") || !has_text(nodes, id) {
                AttrRole::Cosmetic
            } else {
                AttrRole::HtmlOnlySemantic
            }
        }
        ("a", "href") => {
            if destination(value).is_some() {
                AttrRole::MarkdownRepresented
            } else {
                AttrRole::HtmlOnlySemantic
            }
        }
        ("a", "title") => {
            if node.attr("href").and_then(destination).is_some() {
                AttrRole::MarkdownRepresented
            } else {
                AttrRole::HtmlOnlySemantic
            }
        }
        ("img", "src") => {
            if destination(value).is_some() {
                AttrRole::MarkdownRepresented
            } else {
                AttrRole::HtmlOnlySemantic
            }
        }
        ("img", "alt") => {
            if brackets_balanced(&collapse(value)) {
                AttrRole::MarkdownRepresented
            } else {
                AttrRole::HtmlOnlySemantic
            }
        }
        ("img", "title") => {
            if node.attr("src").and_then(destination).is_some() {
                AttrRole::MarkdownRepresented
            } else {
                AttrRole::HtmlOnlySemantic
            }
        }
        ("button", "type") => AttrRole::MarkdownRepresented,
        ("ol", "start") => {
            if plain_start(value) {
                AttrRole::MarkdownRepresented
            } else {
                AttrRole::HtmlOnlySemantic
            }
        }
        ("ol", "type") => {
            if value.trim() == "1" {
                AttrRole::MarkdownRepresented
            } else {
                AttrRole::HtmlOnlySemantic
            }
        }
        ("td" | "th", "colspan" | "rowspan" | "scope" | "headers")
        | ("col" | "colgroup", "span")
        | ("details", "open")
        | ("meta", "charset" | "name" | "http-equiv" | "content")
        | ("style", "type" | "media" | "nonce" | "title") => AttrRole::MarkdownRepresented,
        _ if ANY_ELEMENT_ATTRIBUTES.contains(&local) || element_attribute(element, local) => {
            AttrRole::Cosmetic
        }
        _ => AttrRole::HtmlOnlySemantic,
    }
}

fn html_element_allowed(nodes: &[Node], id: usize, head: Option<usize>, local: &str) -> bool {
    let node = &nodes[id];
    if matches!(
        local,
        "base"
            | "link"
            | "template"
            | "noscript"
            | "iframe"
            | "frame"
            | "frameset"
            | "object"
            | "embed"
            | "applet"
            | "dialog"
            | "portal"
            | "fencedframe"
    ) {
        return false;
    }
    if node.parent == head && !matches!(local, "meta" | "title" | "style" | "script") {
        return false;
    }
    match local {
        "script" => is_json_data_script(nodes, id),
        "style" => style_element_is_cosmetic(nodes, id),
        "meta" => meta_element_is_cosmetic(node),
        "title" => {
            node.parent.is_some()
                && node.parent == head
                && find_element(nodes, 0, "title") == Some(id)
        }
        "details" => shows_all_of_details(nodes, id) && node.attr("name").is_none(),
        _ => {
            VOCABULARY.contains(&local)
                || SHELL_HTML_ELEMENTS.contains(&local)
                || ATOMIC_HTML_ELEMENTS.contains(&local)
        }
    }
}

fn is_json_data_script(nodes: &[Node], id: usize) -> bool {
    let node = &nodes[id];
    if node.attr("src").is_some() || node.attr("event").is_some() || node.attr("for").is_some() {
        return false;
    }
    let Some(raw_type) = node.attr("type") else {
        return false;
    };
    let essence = raw_type.split(';').next().unwrap_or("").trim();
    if !essence.eq_ignore_ascii_case("application/json")
        && !essence.eq_ignore_ascii_case("application/ld+json")
    {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(text_of(nodes, id).trim()).is_ok()
}

fn style_element_is_cosmetic(nodes: &[Node], id: usize) -> bool {
    let NodeKind::Element { attrs, .. } = &nodes[id].kind else {
        return false;
    };
    css::sheet_is_cosmetic(&text_of(nodes, id))
        && attrs.iter().all(|attr| {
            let local = &*attr.name.local;
            matches!(local, "type" | "media" | "nonce" | "title")
                || ANY_ELEMENT_ATTRIBUTES.contains(&local)
        })
}

fn meta_element_is_cosmetic(node: &Node) -> bool {
    let NodeKind::Element { attrs, .. } = &node.kind else {
        return false;
    };
    attrs.iter().all(|attr| {
        let local = &*attr.name.local;
        let val = &*attr.value;
        match local {
            "charset" => true,
            "name" => META_HINTS.contains(&val.trim().to_ascii_lowercase().as_str()),
            "http-equiv" => matches!(
                val.trim().to_ascii_lowercase().as_str(),
                "content-type" | "x-ua-compatible"
            ),
            "content" => node.attr("name").is_some() || node.attr("http-equiv").is_some(),
            _ => ANY_ELEMENT_ATTRIBUTES.contains(&local),
        }
    })
}

fn is_zero_dimension_attr(raw: &str) -> bool {
    let trimmed = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    let trimmed = trimmed.strip_prefix('+').unwrap_or(trimmed);
    let mut saw_dot = false;
    let mut saw_digit = false;
    let mut end = 0;
    for b in trimmed.bytes() {
        if b.is_ascii_digit() {
            saw_digit = true;
            end += 1;
        } else if b == b'.' && !saw_dot {
            saw_dot = true;
            end += 1;
        } else {
            break;
        }
    }
    saw_digit
        && trimmed[..end]
            .parse::<f64>()
            .is_ok_and(|value| value == 0.0)
}

fn has_executable_or_data_scheme(raw: &str) -> bool {
    let normalized = url_text(raw);
    normalized.split_once(':').is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("javascript")
            || scheme.eq_ignore_ascii_case("vbscript")
            || scheme.eq_ignore_ascii_case("data")
    })
}

fn collect_id_refs(attr_local: &str, attr_val: &str, out: &mut HashSet<String>) {
    if matches!(attr_local, "href" | "usemap") {
        let trimmed = url_text(attr_val);
        if let Some(frag) = trimmed.strip_prefix('#')
            && !frag.is_empty()
        {
            out.insert(frag.to_owned());
        }
    } else if matches!(
        attr_local,
        "for"
            | "headers"
            | "list"
            | "form"
            | "aria-labelledby"
            | "aria-describedby"
            | "aria-controls"
            | "aria-owns"
            | "aria-activedescendant"
            | "aria-errormessage"
            | "aria-details"
            | "popovertarget"
    ) {
        for token in attr_val.split_ascii_whitespace() {
            out.insert(token.to_owned());
        }
    }
}

fn is_inherently_verbatim(nodes: &[Node], id: usize, inline_styles: &[InlineStyle]) -> bool {
    let node = &nodes[id];
    let NodeKind::Element { name, .. } = &node.kind else {
        return false;
    };
    if *name.ns != *HTML_NS {
        return true;
    }
    let local = &*name.local;
    local == "script"
        || ATOMIC_HTML_ELEMENTS.contains(&local)
        || inline_styles[id].verdict == CssVerdict::LocalSemantic
        || node
            .attr("aria-hidden")
            .is_some_and(|v| !v.trim().eq_ignore_ascii_case("false") && has_text(nodes, id))
}

fn is_block_verbatim_element(node: &Node) -> bool {
    let local = node.local_name();
    if local.is_empty() {
        return false;
    }
    local == "script" || is_block(node) || SHELL_HTML_ELEMENTS.contains(&local)
}

fn counts_as_markdown_element(node: &Node) -> bool {
    let local = node.local_name();
    !local.is_empty()
        && !matches!(
            local,
            "html" | "head" | "body" | "meta" | "title" | "br" | "wbr"
        )
}

fn style_whitespace_override(style_val: &str, anc_ws: WsMode) -> Option<WsMode> {
    let decls = css::declarations(style_val)?;
    decls
        .iter()
        .filter(|d| !d.important && d.property == "white-space")
        .chain(
            decls
                .iter()
                .filter(|d| d.important && d.property == "white-space"),
        )
        .next_back()
        .and_then(|d| match d.value.as_str() {
            "pre" | "pre-wrap" | "break-spaces" => Some(WsMode::PreserveAll),
            "pre-line" => Some(WsMode::PreserveLines),
            "normal" | "nowrap" | "initial" => Some(WsMode::Collapse),
            "inherit" | "unset" => Some(anc_ws),
            _ => None,
        })
}

fn postorder_nodes(nodes: &[Node], root: usize) -> Vec<usize> {
    let mut order = Vec::with_capacity(nodes.len());
    let mut stack = vec![(root, false)];
    while let Some((id, visited)) = stack.pop() {
        if visited {
            order.push(id);
        } else {
            stack.push((id, true));
            for &child in nodes[id].children.iter().rev() {
                stack.push((child, false));
            }
        }
    }
    order
}

fn subtree_has(roots: &[usize], nodes: &[Node], predicate: impl Fn(usize) -> bool) -> bool {
    let mut stack: Vec<usize> = roots.to_vec();
    while let Some(id) = stack.pop() {
        if predicate(id) {
            return true;
        }
        stack.extend_from_slice(&nodes[id].children);
    }
    false
}

fn link_can_be_markdown(
    nodes: &[Node],
    link_id: usize,
    draft: &[Draft],
    verbatim_cache: &[Option<serialize::SerializedSubtree>],
) -> bool {
    let mut probe = String::new();
    let mut stack: Vec<usize> = nodes[link_id].children.iter().copied().rev().collect();
    while let Some(id) = stack.pop() {
        match draft[id] {
            Draft::Removed(_) => continue,
            Draft::Shell { .. } => return false,
            Draft::Verbatim => {
                let Some(built) = verbatim_cache[id].as_ref() else {
                    return false;
                };
                if built.html.contains('\n') || is_block_verbatim_element(&nodes[id]) {
                    return false;
                }
                probe.push_str(&built.html);
                continue;
            }
            Draft::Markdown => {}
        }
        let node = &nodes[id];
        match &node.kind {
            NodeKind::Text(text) => probe.push_str(text),
            NodeKind::Element { .. } => {
                let local = node.local_name();
                if local == "br" || is_block(node) || (local == "a" && node.attr("href").is_some())
                {
                    return false;
                }
                if matches!(local, "code" | "kbd" | "samp") {
                    if subtree_has(&node.children, nodes, |cid| {
                        nodes[cid].is_element("br") || is_block(&nodes[cid])
                    }) {
                        return false;
                    }
                    continue;
                }
                if local == "img" {
                    let alt = node.attr("alt").map(collapse).unwrap_or_default();
                    if !brackets_balanced(&alt) {
                        return false;
                    }
                    continue;
                }
                stack.extend(node.children.iter().copied().rev());
            }
            _ => {}
        }
    }
    if nodes[link_id].attr("href").is_some() {
        brackets_balanced(probe.trim())
    } else {
        true
    }
}

fn code_can_be_markdown(nodes: &[Node], root: usize, draft: &[Draft]) -> bool {
    let mut stack: Vec<usize> = nodes[root].children.to_vec();
    while let Some(id) = stack.pop() {
        match draft[id] {
            Draft::Removed(_) => continue,
            Draft::Shell { .. } | Draft::Verbatim => return false,
            Draft::Markdown => {}
        }
        let node = &nodes[id];
        if let NodeKind::Element { .. } = &node.kind {
            let local = node.local_name();
            if (local == "a" && node.attr("href").is_some())
                || (local == "img" && (node.attr("src").is_some() || node.attr("alt").is_some()))
                || local == "hr"
            {
                return false;
            }
            stack.extend_from_slice(&node.children);
        }
    }
    true
}

fn list_is_malformed(nodes: &[Node], list_id: usize, ordered: bool) -> bool {
    for &child in &nodes[list_id].children {
        match &nodes[child].kind {
            NodeKind::Comment => {}
            NodeKind::Text(text) => {
                if ordered && !text.trim().is_empty() {
                    return true;
                }
            }
            NodeKind::Element { .. } => {
                if !matches!(nodes[child].local_name(), "li" | "style") {
                    return true;
                }
            }
            _ => return true,
        }
    }
    false
}

fn list_can_be_markdown(
    nodes: &[Node],
    list_id: usize,
    ordered: bool,
    has_html_only_semantic: &[bool],
    draft: &[Draft],
    verbatim_cache: &[Option<serialize::SerializedSubtree>],
) -> bool {
    if has_html_only_semantic[list_id] {
        return false;
    }
    for &child in &nodes[list_id].children {
        if matches!(draft[child], Draft::Removed(_)) {
            continue;
        }
        if let NodeKind::Text(text) = &nodes[child].kind
            && text.trim().is_empty()
        {
            continue;
        }
        if !nodes[child].is_element("li")
            || has_html_only_semantic[child]
            || draft[child] != Draft::Markdown
        {
            return false;
        }
        if ordered && !has_rendered_output(nodes, child, draft) {
            return false;
        }
        if subtree_has_shell_or_multiline(nodes, &nodes[child].children, draft, verbatim_cache) {
            return false;
        }
    }
    true
}

fn dl_is_malformed(nodes: &[Node], dl_id: usize) -> bool {
    for &child in &nodes[dl_id].children {
        match &nodes[child].kind {
            NodeKind::Comment => {}
            NodeKind::Text(text) if text.trim().is_empty() => {}
            NodeKind::Element { .. }
                if matches!(nodes[child].local_name(), "dt" | "dd" | "style") => {}
            _ => return true,
        }
    }
    false
}

fn dl_can_be_markdown(
    nodes: &[Node],
    dl_id: usize,
    has_html_only_semantic: &[bool],
    draft: &[Draft],
    verbatim_cache: &[Option<serialize::SerializedSubtree>],
) -> bool {
    if has_html_only_semantic[dl_id] {
        return false;
    }
    for &child in &nodes[dl_id].children {
        if matches!(draft[child], Draft::Removed(_)) {
            continue;
        }
        if let NodeKind::Text(text) = &nodes[child].kind
            && text.trim().is_empty()
        {
            continue;
        }
        let local = nodes[child].local_name();
        if !matches!(local, "dt" | "dd")
            || has_html_only_semantic[child]
            || draft[child] != Draft::Markdown
        {
            return false;
        }
        if local == "dt"
            && subtree_has(&nodes[child].children, nodes, |cid| {
                nodes[cid].is_element("br") || is_block(&nodes[cid])
            })
        {
            return false;
        }
        if subtree_has_shell_or_multiline(nodes, &nodes[child].children, draft, verbatim_cache) {
            return false;
        }
    }
    true
}

fn table_is_malformed(nodes: &[Node], table_id: usize) -> bool {
    let bad_text = |id: usize| matches!(&nodes[id].kind, NodeKind::Text(t) if !t.trim().is_empty());
    for &child in &nodes[table_id].children {
        if bad_text(child) {
            return true;
        }
        if let NodeKind::Element { .. } = &nodes[child].kind {
            match nodes[child].local_name() {
                "caption" | "col" | "style" => {}
                "colgroup" => {
                    if nodes[child].children.iter().any(|&c| {
                        bad_text(c)
                            || matches!(
                                &nodes[c].kind,
                                NodeKind::Element { .. } if !matches!(nodes[c].local_name(), "col" | "style")
                            )
                    }) {
                        return true;
                    }
                }
                "thead" | "tbody" | "tfoot" => {
                    for &row in &nodes[child].children {
                        if bad_text(row) {
                            return true;
                        }
                        if let NodeKind::Element { .. } = &nodes[row].kind {
                            if !matches!(nodes[row].local_name(), "tr" | "style") {
                                return true;
                            }
                            if nodes[row].children.iter().any(|&cell| {
                                bad_text(cell)
                                    || matches!(
                                        &nodes[cell].kind,
                                        NodeKind::Element { .. } if !matches!(nodes[cell].local_name(), "th" | "td" | "style")
                                    )
                            }) {
                                return true;
                            }
                        }
                    }
                }
                "tr" => {
                    if nodes[child].children.iter().any(|&cell| {
                        bad_text(cell)
                            || matches!(
                                &nodes[cell].kind,
                                NodeKind::Element { .. } if !matches!(nodes[cell].local_name(), "th" | "td" | "style")
                            )
                    }) {
                        return true;
                    }
                }
                _ => return true,
            }
        }
    }
    false
}

fn table_can_be_gfm(
    nodes: &[Node],
    table_id: usize,
    has_html_only_semantic: &[bool],
    kept_attrs: &[Vec<KeptAttr>],
    draft: &[Draft],
    verbatim_cache: &[Option<serialize::SerializedSubtree>],
) -> bool {
    if has_html_only_semantic[table_id] {
        return false;
    }
    let mut rows: Vec<bool> = Vec::new();
    let mut pending: Vec<usize> = Vec::new();
    let mut remaining_rows = 0usize;
    let mut stack = table_children(nodes, table_id);
    stack.reverse();

    while let Some(child) = stack.pop() {
        if matches!(draft[child], Draft::Removed(_)) {
            continue;
        }
        let node = &nodes[child];
        if !matches!(node.kind, NodeKind::Element { .. }) {
            continue;
        }
        if has_html_only_semantic[child] || draft[child] != Draft::Markdown {
            return false;
        }
        match node.local_name() {
            "caption" => {
                if !single_line_inline(
                    nodes,
                    &node.children,
                    kept_attrs,
                    draft,
                    verbatim_cache,
                    false,
                ) {
                    return false;
                }
            }
            "colgroup" => {
                for &col in &node.children {
                    if matches!(draft[col], Draft::Removed(_)) {
                        continue;
                    }
                    if let NodeKind::Element { .. } = &nodes[col].kind
                        && (has_html_only_semantic[col] || draft[col] != Draft::Markdown)
                    {
                        return false;
                    }
                }
            }
            "col" => {}
            "thead" | "tbody" | "tfoot" => {
                pending.clear();
                remaining_rows = node
                    .children
                    .iter()
                    .filter(|&&row| nodes[row].is_element("tr"))
                    .count();
                stack.extend(node.children.iter().rev());
            }
            "tr" => {
                remaining_rows = remaining_rows.saturating_sub(1);
                let mut header = true;
                let mut real_cells = 0usize;
                let mut column = 0usize;
                for &cell in &node.children {
                    if matches!(draft[cell], Draft::Removed(_)) {
                        continue;
                    }
                    let cell_node = &nodes[cell];
                    if !matches!(cell_node.local_name(), "th" | "td") {
                        continue;
                    }
                    if has_html_only_semantic[cell]
                        || draft[cell] != Draft::Markdown
                        || !single_line_in_gfm_cell(nodes, cell, kept_attrs, draft, verbatim_cache)
                    {
                        return false;
                    }
                    while column < MAX_TABLE_COLUMNS && pending.get(column).is_some_and(|&r| r > 0)
                    {
                        pending[column] -= 1;
                        column += 1;
                    }
                    header &= cell_node.is_element("th");
                    real_cells += 1;
                    let requested = span(cell_node, "colspan");
                    if column.saturating_add(requested.max(1)) > MAX_TABLE_COLUMNS {
                        return false;
                    }
                    let columns = requested
                        .min(MAX_TABLE_COLUMNS.saturating_sub(column))
                        .max(1);
                    let rowspan = span(cell_node, "rowspan");
                    let rows_below = if rowspan == 0 {
                        remaining_rows
                    } else {
                        rowspan - 1
                    };
                    let end = (column + columns).min(MAX_TABLE_COLUMNS);
                    if pending.len() < end {
                        pending.resize(end, 0);
                    }
                    pending[column.min(end)..end].fill(rows_below);
                    column += columns;
                }
                for slot in pending.iter_mut().skip(column) {
                    *slot = slot.saturating_sub(1);
                }
                rows.push(header && real_cells > 0);
            }
            _ => return false,
        }
    }

    rows.iter().skip(1).all(|&header| !header)
}

fn single_line_in_gfm_cell(
    nodes: &[Node],
    container: usize,
    kept_attrs: &[Vec<KeptAttr>],
    draft: &[Draft],
    verbatim_cache: &[Option<serialize::SerializedSubtree>],
) -> bool {
    let mut blocks = 0usize;
    let mut inline_nonempty = false;

    for &child in &nodes[container].children {
        match draft[child] {
            Draft::Removed(_) => continue,
            Draft::Shell { inline: false, .. } => return false,
            Draft::Shell { inline: true, .. } => {
                if !single_line_inline(nodes, &[child], kept_attrs, draft, verbatim_cache, true) {
                    return false;
                }
                inline_nonempty = true;
                continue;
            }
            Draft::Verbatim => {
                let Some(built) = verbatim_cache[child].as_ref() else {
                    return false;
                };
                if built.html.contains(['\n', '|']) {
                    return false;
                }
                if is_block_verbatim_element(&nodes[child]) {
                    if inline_nonempty {
                        blocks += 1;
                        inline_nonempty = false;
                    }
                    blocks += 1;
                } else if !built.html.trim().is_empty() {
                    inline_nonempty = true;
                }
                continue;
            }
            Draft::Markdown => {}
        }
        let node = &nodes[child];
        if is_block(node) {
            if inline_nonempty {
                blocks += 1;
                inline_nonempty = false;
            }
            if !has_rendered_output(nodes, child, draft) {
                continue;
            }
            blocks += 1;
            if blocks > 1 {
                return false;
            }
            let local = node.local_name();
            if matches!(
                local,
                "pre" | "blockquote" | "ul" | "ol" | "dl" | "table" | "menu"
            ) || admonition_container(node)
            {
                return false;
            }
            if matches!(local, "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "dt") {
                if !single_line_inline(
                    nodes,
                    &node.children,
                    kept_attrs,
                    draft,
                    verbatim_cache,
                    true,
                ) {
                    return false;
                }
            } else if local != "hr"
                && !single_line_in_gfm_cell(nodes, child, kept_attrs, draft, verbatim_cache)
            {
                return false;
            }
        } else {
            if !single_line_inline(nodes, &[child], kept_attrs, draft, verbatim_cache, true) {
                return false;
            }
            if has_rendered_output(nodes, child, draft) {
                inline_nonempty = true;
            }
        }
    }
    if inline_nonempty {
        blocks += 1;
    }
    blocks <= 1
}

fn single_line_inline(
    nodes: &[Node],
    roots: &[usize],
    kept_attrs: &[Vec<KeptAttr>],
    draft: &[Draft],
    verbatim_cache: &[Option<serialize::SerializedSubtree>],
    forbid_pipe_in_markup: bool,
) -> bool {
    let mut stack: Vec<usize> = roots.to_vec();
    while let Some(id) = stack.pop() {
        match draft[id] {
            Draft::Removed(_) => continue,
            Draft::Shell { inline: false, .. } => return false,
            Draft::Shell { inline: true, .. } => {
                if forbid_pipe_in_markup
                    && kept_attrs[id].iter().any(|attr| attr.value.contains('|'))
                {
                    return false;
                }
                stack.extend_from_slice(&nodes[id].children);
                continue;
            }
            Draft::Verbatim => {
                let Some(built) = verbatim_cache[id].as_ref() else {
                    return false;
                };
                if built.html.contains('\n')
                    || is_block_verbatim_element(&nodes[id])
                    || (forbid_pipe_in_markup && built.html.contains('|'))
                {
                    return false;
                }
                continue;
            }
            Draft::Markdown => {}
        }
        let node = &nodes[id];
        if let NodeKind::Element { .. } = &node.kind {
            if node.is_element("br") || is_block(node) {
                return false;
            }
            stack.extend_from_slice(&node.children);
        }
    }
    true
}

fn mark_table_shells(nodes: &[Node], table_id: usize, draft: &mut [Draft]) {
    draft[table_id] = Draft::Shell {
        structural: true,
        inline: false,
    };
    for &child in &nodes[table_id].children {
        if draft[child] == Draft::Verbatim {
            continue;
        }
        match nodes[child].local_name() {
            "caption" | "col" => {
                draft[child] = Draft::Shell {
                    structural: false,
                    inline: false,
                };
            }
            "colgroup" => {
                draft[child] = Draft::Shell {
                    structural: true,
                    inline: false,
                };
                for &col in &nodes[child].children {
                    if nodes[col].is_element("col") && draft[col] != Draft::Verbatim {
                        draft[col] = Draft::Shell {
                            structural: false,
                            inline: false,
                        };
                    }
                }
            }
            "thead" | "tbody" | "tfoot" => {
                draft[child] = Draft::Shell {
                    structural: true,
                    inline: false,
                };
                for &row in &nodes[child].children {
                    if nodes[row].is_element("tr") && draft[row] != Draft::Verbatim {
                        draft[row] = Draft::Shell {
                            structural: true,
                            inline: false,
                        };
                        for &cell in &nodes[row].children {
                            if matches!(nodes[cell].local_name(), "th" | "td")
                                && draft[cell] != Draft::Verbatim
                            {
                                draft[cell] = Draft::Shell {
                                    structural: false,
                                    inline: false,
                                };
                            }
                        }
                    }
                }
            }
            "tr" => {
                draft[child] = Draft::Shell {
                    structural: true,
                    inline: false,
                };
                for &cell in &nodes[child].children {
                    if matches!(nodes[cell].local_name(), "th" | "td")
                        && draft[cell] != Draft::Verbatim
                    {
                        draft[cell] = Draft::Shell {
                            structural: false,
                            inline: false,
                        };
                    }
                }
            }
            _ => {}
        }
    }
}

fn has_rendered_output(nodes: &[Node], root: usize, draft: &[Draft]) -> bool {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        match draft[id] {
            Draft::Removed(_) => continue,
            Draft::Shell { .. } | Draft::Verbatim => return true,
            Draft::Markdown => {}
        }
        let node = &nodes[id];
        match &node.kind {
            NodeKind::Text(text) if !text.trim().is_empty() => return true,
            NodeKind::Element { .. } => {
                let local = node.local_name();
                if matches!(local, "hr" | "pre")
                    || (local == "img"
                        && (node.attr("src").is_some()
                            || node.attr("alt").is_some_and(|a| !a.trim().is_empty())))
                    || (local == "a" && node.attr("href").is_some())
                {
                    return true;
                }
                stack.extend_from_slice(&node.children);
            }
            _ => {}
        }
    }
    false
}

fn subtree_has_shell_or_multiline(
    nodes: &[Node],
    roots: &[usize],
    draft: &[Draft],
    verbatim_cache: &[Option<serialize::SerializedSubtree>],
) -> bool {
    let mut stack: Vec<usize> = roots.to_vec();
    while let Some(id) = stack.pop() {
        match draft[id] {
            Draft::Removed(_) => continue,
            Draft::Shell { inline: false, .. } => return true,
            Draft::Shell { inline: true, .. } => {
                stack.extend_from_slice(&nodes[id].children);
            }
            Draft::Verbatim => {
                if is_block_verbatim_element(&nodes[id])
                    || verbatim_cache[id]
                        .as_ref()
                        .is_none_or(|built| built.html.contains('\n'))
                {
                    return true;
                }
            }
            Draft::Markdown => {
                stack.extend_from_slice(&nodes[id].children);
            }
        }
    }
    false
}

fn subtree_has_multiline_verbatim(
    nodes: &[Node],
    roots: &[usize],
    draft: &[Draft],
    verbatim_cache: &[Option<serialize::SerializedSubtree>],
) -> bool {
    let mut stack: Vec<usize> = roots.to_vec();
    while let Some(id) = stack.pop() {
        match draft[id] {
            Draft::Removed(_) => continue,
            Draft::Verbatim => {
                if verbatim_cache[id]
                    .as_ref()
                    .is_none_or(|built| built.html.contains('\n'))
                {
                    return true;
                }
            }
            Draft::Shell { .. } | Draft::Markdown => {
                stack.extend_from_slice(&nodes[id].children);
            }
        }
    }
    false
}

fn admonition_container(node: &Node) -> bool {
    matches!(
        node.local_name(),
        "div" | "section" | "details" | "fieldset" | "figure"
    ) && node.class_tokens().any(|token| {
        token.split(['-', '_']).any(|piece| {
            ADMONITION_LABELS
                .iter()
                .any(|label| piece.eq_ignore_ascii_case(label))
        })
    })
}

fn class_carries_code_language(nodes: &[Node], id: usize, element: &str) -> bool {
    if language_of(&nodes[id]).is_none() {
        return false;
    }
    matches!(element, "pre" | "code")
        || (element == "div"
            && nodes[id].children.iter().any(|&child| {
                nodes[child].is_element("pre")
                    || (nodes[child].is_element("div")
                        && nodes[child]
                            .children
                            .iter()
                            .any(|&grandchild| nodes[grandchild].is_element("pre")))
            }))
}
