//! Markdown views of complete HTML documents. Only enumerated non-content
//! elements are removed; every removal is counted in the view header and the
//! original page stays retrievable. No text-density or link-density scoring.

use std::cell::RefCell;
use std::fmt::Write as _;

use html5ever::interface::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{Attribute, LocalName, Namespace, ParseOpts, QualName, parse_document};

/// Rendered bodies shorter than this are extraction failures, not pages.
/// Provisional until the sample set fixes the threshold.
const MIN_BODY_CHARS: usize = 64;

/// Element nesting beyond this depth is flattened to its collapsed text
/// instead of rendered recursively, so a deeply nested page cannot exhaust
/// the stack. Flattening loses block structure, including the line breaks
/// and indentation of code blocks; the words stay.
const MAX_DEPTH: usize = 128;

/// Pages whose markup nests deeper than this are not parsed at all: the
/// HTML5 tree builder scans the open-element stack per start tag, so parse
/// time grows quadratically with depth (about 1 s at 10 000 levels, 4 min at
/// 160 000). Browsers cap the tree at 512; real pages stay under 50.
const MAX_PARSE_DEPTH: usize = 512;

/// Elements that never take content, so a start tag does not open a level.
const VOID_ELEMENTS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements the tree builder closes implicitly before a sibling, so they
/// cannot stack on themselves and an unclosed run does not count as nesting.
const SELF_CLOSING_RUN: [&str; 14] = [
    "p", "li", "dt", "dd", "tr", "td", "th", "option", "optgroup", "thead", "tbody", "tfoot",
    "colgroup", "caption",
];

/// Elements whose content is raw text: tags inside them are not markup.
/// `noscript` is left out on purpose: the parser treats it as raw text
/// too, so counting its tags overestimates depth, which is the safe side.
const RAW_TEXT: [&str; 6] = ["script", "style", "textarea", "title", "xmp", "plaintext"];

/// Removed element categories, in header order. `form control` covers
/// button, input, select, textarea, datalist, progress and meter; `media`
/// covers audio, video, canvas, object, embed and map. Labels, legends and
/// fieldsets stay: MkDocs content tabs carry their titles in labels.
const REMOVED_LABELS: [&str; 18] = [
    "script",
    "style",
    "noscript",
    "template",
    "svg",
    "iframe",
    "comment",
    "nav",
    "header",
    "footer",
    "aside",
    "role=navigation",
    "role=banner",
    "role=contentinfo",
    "role=complementary",
    "form control",
    "media",
    "dialog",
];

const REMOVED_CATEGORIES: usize = REMOVED_LABELS.len();

/// Index into `REMOVED_LABELS` and `HtmlView::removed`, in label order.
#[derive(Clone, Copy)]
enum Removed {
    Script,
    Style,
    Noscript,
    Template,
    Svg,
    Iframe,
    Comment,
    Nav,
    Header,
    Footer,
    Aside,
    RoleNavigation,
    RoleBanner,
    RoleContentinfo,
    RoleComplementary,
    FormControl,
    Media,
    Dialog,
}

const MATHML_NS: &str = "http://www.w3.org/1998/Math/MathML";

/// Placeholder cells for `colspan`/`rowspan` are only inserted up to this
/// column, so one row of wide spans cannot multiply the rows below it.
/// Real cells beyond it are still emitted, only never aligned.
const MAX_TABLE_COLUMNS: usize = 64;

/// One Markdown view of a complete HTML document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlView {
    /// Header line, title, URL, rendered body and end marker.
    pub output: String,
    /// `<title>` text, collapsed.
    pub title: Option<String>,
    /// `<link rel="canonical">` target, normalized as the URL parser reads
    /// it: controls and whitespace trimmed, tabs and newlines dropped.
    pub canonical: Option<String>,
    /// Removed element counts by category, in the order of the header labels:
    /// script, style, noscript, template, svg, iframe, comment, nav, header,
    /// footer, aside, role=navigation, role=banner, role=contentinfo,
    /// role=complementary, form control, media, dialog.
    pub removed: [usize; REMOVED_CATEGORIES],
    /// Local name of the rendered root: `main`, the element carrying
    /// `role=main`, the sole outermost `article`, or `body`.
    pub root: String,
    /// Elements and non-blank text nodes outside the rendered root that were
    /// omitted uncounted by category.
    pub outside_root: usize,
}

/// Stateless renderer for complete HTML documents.
#[derive(Debug, Clone, Copy, Default)]
pub struct HtmlExtractor;

impl HtmlExtractor {
    /// Renders the page as Markdown. Returns `None` when the rendered body is
    /// too short to be a page or the markup nests deeper than the parse
    /// limit; callers keep the original in that case.
    #[must_use]
    pub fn render(&self, input: &str) -> Option<HtmlView> {
        if nesting_depth(input) > MAX_PARSE_DEPTH {
            return None;
        }
        let dom =
            parse_document(Dom::default(), ParseOpts::default()).one(StrTendril::from_slice(input));
        let nodes = dom.nodes.into_inner();
        let html = nodes[0]
            .children
            .iter()
            .copied()
            .find(|&id| nodes[id].is_element("html"))?;
        let head = nodes[html]
            .children
            .iter()
            .copied()
            .find(|&id| nodes[id].is_element("head"));
        let body = nodes[html]
            .children
            .iter()
            .copied()
            .find(|&id| nodes[id].is_element("body"))?;

        let title = head
            .and_then(|head| find_element(&nodes, head, "title"))
            .map(|id| collapse(&text_of(&nodes, id)))
            .filter(|title| !title.is_empty());
        let canonical = head.and_then(|head| {
            nodes[head].children.iter().copied().find_map(|id| {
                let node = &nodes[id];
                (node.is_element("link")
                    && node
                        .attr("rel")
                        .is_some_and(|rel| rel.split_ascii_whitespace().any(|r| r == "canonical")))
                .then(|| node.attr("href").map(|href| url_text(href).into_owned()))
                .flatten()
                .filter(|href| !href.is_empty())
            })
        });

        // An explicit `role=main` outranks a bare `article`, and an `article`
        // is the root only when it is the sole outermost one: index pages
        // list many, while a post's comment articles nest inside it.
        let root = find_element(&nodes, body, "main")
            .or_else(|| find_element_by_role(&nodes, body, "main"))
            .or_else(|| sole_element(&nodes, body, "article"))
            .unwrap_or(body);
        let mut outside_root = 0;
        let mut chain = root;
        while let Some(parent) = nodes[chain].parent.filter(|_| chain != body) {
            outside_root += nodes[parent]
                .children
                .iter()
                .filter(|&&child| {
                    child != chain
                        && match &nodes[child].kind {
                            NodeKind::Element { .. } => true,
                            NodeKind::Text(text) => !text.trim().is_empty(),
                            _ => false,
                        }
                })
                .count();
            chain = parent;
        }
        let mut renderer = Renderer {
            nodes: &nodes,
            removed: [0; REMOVED_CATEGORIES],
            depth: 0,
        };
        if let Some(head) = head {
            renderer.count_removals(head, false);
        }
        // Escaped once here, where every rendering path ends up, so the
        // view boundary can only come from the renderer.
        let body_markdown =
            escape_wrapper_lines(&renderer.blocks(root, is_sectioning(&nodes[root])));
        if body_markdown.chars().count() < MIN_BODY_CHARS {
            return None;
        }

        let root_name = nodes[root].local_name().to_owned();
        let mut output = String::from("[HTML page rendered as Markdown");
        if root != body {
            let _ = write!(
                output,
                "; <{root_name}> only, {outside_root} nodes outside it omitted"
            );
        }
        let mut listed = false;
        for (label, count) in REMOVED_LABELS.iter().zip(renderer.removed) {
            if count == 0 {
                continue;
            }
            output.push_str(if listed { ", " } else { "; removed " });
            let _ = write!(output, "{count} {label}");
            listed = true;
        }
        output.push_str(". Retrieve original for the full page.]\n");
        if let Some(title) = &title {
            let _ = writeln!(output, "Title: {title}");
        }
        if let Some(canonical) = &canonical {
            let _ = writeln!(output, "URL: {canonical}");
        }
        output.push_str(&body_markdown);
        output.push_str("\n[End page]");
        Some(HtmlView {
            output,
            title,
            canonical,
            removed: renderer.removed,
            root: root_name,
            outside_root,
        })
    }
}

// ---- DOM ----------------------------------------------------------------

enum NodeKind {
    Document,
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
        template: Option<usize>,
    },
    Text(String),
    Comment,
    Other,
}

struct Node {
    kind: NodeKind,
    parent: Option<usize>,
    children: Vec<usize>,
}

impl Node {
    fn local_name(&self) -> &str {
        self.name_in("http://www.w3.org/1999/xhtml")
    }

    /// Local name when the element is in namespace `ns`, else empty.
    fn name_in(&self, ns: &str) -> &str {
        match &self.kind {
            NodeKind::Element { name, .. } if *name.ns == *ns => &name.local,
            _ => "",
        }
    }

    fn is_element(&self, local: &str) -> bool {
        self.local_name() == local
    }

    fn attr(&self, key: &str) -> Option<&str> {
        match &self.kind {
            NodeKind::Element { attrs, .. } => attrs
                .iter()
                .find(|attr| &*attr.name.local == key)
                .map(|attr| &*attr.value),
            _ => None,
        }
    }

    fn class_tokens(&self) -> impl Iterator<Item = &str> {
        self.attr("class")
            .unwrap_or("")
            .split_ascii_whitespace()
            .map(|token| token.trim_matches(|c: char| c == '-' || c == '_'))
    }
}

#[derive(Default)]
struct Dom {
    nodes: RefCell<Vec<Node>>,
}

impl Dom {
    fn push(&self, kind: NodeKind) -> usize {
        let mut nodes = self.nodes.borrow_mut();
        nodes.push(Node {
            kind,
            parent: None,
            children: Vec::new(),
        });
        nodes.len() - 1
    }

    /// Inserts a node or text at `index`; text merges into a preceding text node.
    fn insert_child(&self, parent: usize, index: usize, child: NodeOrText<usize>) {
        match child {
            NodeOrText::AppendNode(id) => {
                Self::insert(&mut self.nodes.borrow_mut(), parent, index, id)
            }
            NodeOrText::AppendText(text) => {
                {
                    let mut nodes = self.nodes.borrow_mut();
                    if index > 0
                        && let Some(&previous) = nodes[parent].children.get(index - 1)
                        && let NodeKind::Text(existing) = &mut nodes[previous].kind
                    {
                        existing.push_str(&text);
                        return;
                    }
                }
                let id = self.push(NodeKind::Text(text.to_string()));
                let mut nodes = self.nodes.borrow_mut();
                nodes[id].parent = Some(parent);
                nodes[parent].children.insert(index, id);
            }
        }
    }

    fn detach(nodes: &mut [Node], id: usize) {
        if let Some(parent) = nodes[id].parent.take() {
            nodes[parent].children.retain(|&child| child != id);
        }
    }

    fn insert(nodes: &mut [Node], parent: usize, index: usize, id: usize) {
        Self::detach(nodes, id);
        nodes[id].parent = Some(parent);
        nodes[parent].children.insert(index, id);
    }
}

#[derive(Debug)]
struct OwnedName(QualName);

impl ElemName for OwnedName {
    fn ns(&self) -> &Namespace {
        &self.0.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

impl TreeSink for Dom {
    type Handle = usize;
    type Output = Self;
    type ElemName<'a> = OwnedName;

    fn finish(self) -> Self {
        self
    }

    fn parse_error(&self, _msg: std::borrow::Cow<'static, str>) {}

    fn get_document(&self) -> usize {
        if self.nodes.borrow().is_empty() {
            self.push(NodeKind::Document);
        }
        0
    }

    fn elem_name<'a>(&'a self, target: &'a usize) -> OwnedName {
        match &self.nodes.borrow()[*target].kind {
            NodeKind::Element { name, .. } => OwnedName(name.clone()),
            _ => unreachable!("elem_name on a non-element node"),
        }
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> usize {
        let template = flags.template.then(|| self.push(NodeKind::Other));
        self.push(NodeKind::Element {
            name,
            attrs,
            template,
        })
    }

    fn create_comment(&self, _text: StrTendril) -> usize {
        self.push(NodeKind::Comment)
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> usize {
        self.push(NodeKind::Other)
    }

    fn append(&self, parent: &usize, child: NodeOrText<usize>) {
        let index = self.nodes.borrow()[*parent].children.len();
        self.insert_child(*parent, index, child);
    }

    fn append_based_on_parent_node(
        &self,
        element: &usize,
        prev_element: &usize,
        child: NodeOrText<usize>,
    ) {
        if self.nodes.borrow()[*element].parent.is_some() {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        _name: StrTendril,
        _public_id: StrTendril,
        _system_id: StrTendril,
    ) {
    }

    fn get_template_contents(&self, target: &usize) -> usize {
        match self.nodes.borrow()[*target].kind {
            NodeKind::Element {
                template: Some(contents),
                ..
            } => contents,
            _ => unreachable!("template contents requested for a non-template element"),
        }
    }

    fn same_node(&self, x: &usize, y: &usize) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &usize, new_node: NodeOrText<usize>) {
        let nodes = self.nodes.borrow();
        let parent = nodes[*sibling]
            .parent
            .expect("tree builder inserts before an attached sibling");
        let index = nodes[parent]
            .children
            .iter()
            .position(|&child| child == *sibling)
            .expect("sibling is a child of its parent");
        drop(nodes);
        self.insert_child(parent, index, new_node);
    }

    fn add_attrs_if_missing(&self, target: &usize, new_attrs: Vec<Attribute>) {
        let mut nodes = self.nodes.borrow_mut();
        let NodeKind::Element { attrs, .. } = &mut nodes[*target].kind else {
            unreachable!("attributes added to a non-element node")
        };
        for attr in new_attrs {
            if !attrs.iter().any(|existing| existing.name == attr.name) {
                attrs.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &usize) {
        Dom::detach(&mut self.nodes.borrow_mut(), *target);
    }

    fn reparent_children(&self, node: &usize, new_parent: &usize) {
        let mut nodes = self.nodes.borrow_mut();
        let children = std::mem::take(&mut nodes[*node].children);
        for &child in &children {
            nodes[child].parent = Some(*new_parent);
        }
        nodes[*new_parent].children.extend(children);
    }
}

/// Maximum element nesting of raw markup, from a single byte scan without
/// building a tree. Errs high: an unclosed tag counts as an open level, and
/// a `>` inside a quoted attribute value ends the tag early, so whatever
/// follows it is scanned as content. Neither can hide real nesting.
fn nesting_depth(input: &str) -> usize {
    let bytes = input.as_bytes();
    let (mut depth, mut max, mut i) = (0usize, 0usize, 0usize);
    let find = |from: usize, needle: &[u8]| {
        bytes[from..]
            .windows(needle.len())
            .position(|w| w.eq_ignore_ascii_case(needle))
            .map(|at| from + at + needle.len())
    };
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"<!--") {
            i = find(i + 4, b"-->").unwrap_or(bytes.len());
            continue;
        }
        let closing = bytes.get(i + 1) == Some(&b'/');
        let start = i + 1 + usize::from(closing);
        let end = start
            + bytes[start..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric())
                .count();
        if end == start || !bytes[start].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let name = input[start..end].to_ascii_lowercase();
        i = find(end, b">").unwrap_or(bytes.len());
        if closing {
            depth = depth.saturating_sub(1);
        } else if RAW_TEXT.contains(&name.as_str()) {
            i = find(i, format!("</{name}").as_bytes()).unwrap_or(bytes.len());
        } else if !VOID_ELEMENTS.contains(&name.as_str())
            && !SELF_CLOSING_RUN.contains(&name.as_str())
        {
            depth += 1;
            max = max.max(depth);
        }
    }
    max
}

fn find_element(nodes: &[Node], from: usize, local: &str) -> Option<usize> {
    let mut stack = vec![from];
    while let Some(id) = stack.pop() {
        if id != from && nodes[id].is_element(local) {
            return Some(id);
        }
        stack.extend(nodes[id].children.iter().rev());
    }
    None
}

fn find_element_by_role(nodes: &[Node], from: usize, role: &str) -> Option<usize> {
    let mut stack = vec![from];
    while let Some(id) = stack.pop() {
        if id != from && has_role(&nodes[id], role) {
            return Some(id);
        }
        stack.extend(nodes[id].children.iter().rev());
    }
    None
}

/// The only outermost element with this name under `from`, or `None` when
/// there are several or none. Matches are not searched inside, so a post
/// whose comments are nested articles still yields the post.
fn sole_element(nodes: &[Node], from: usize, local: &str) -> Option<usize> {
    let mut found = None;
    let mut stack = vec![from];
    while let Some(id) = stack.pop() {
        if id != from && nodes[id].is_element(local) {
            if found.is_some() {
                return None;
            }
            found = Some(id);
            continue;
        }
        stack.extend(nodes[id].children.iter().rev());
    }
    found
}

fn has_role(node: &Node, role: &str) -> bool {
    node.attr("role").is_some_and(|value| {
        value
            .split_ascii_whitespace()
            .any(|r| r.eq_ignore_ascii_case(role))
    })
}

fn text_of(nodes: &[Node], id: usize) -> String {
    let mut text = String::new();
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if let NodeKind::Text(value) = &nodes[id].kind {
            text.push_str(value);
        }
        stack.extend(nodes[id].children.iter().rev());
    }
    text
}

/// Collapses HTML whitespace runs into single spaces and trims both ends.
fn collapse(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

// ---- Rendering -----------------------------------------------------------

/// Elements whose `header`/`footer` descendants are section content rather
/// than the page banner or contentinfo (HTML-AAM implicit roles).
const SECTIONING: [&str; 5] = ["article", "aside", "main", "nav", "section"];

fn is_sectioning(node: &Node) -> bool {
    SECTIONING.contains(&node.local_name()) || has_role(node, "main")
}

const ADMONITION_LABELS: [&str; 10] = [
    "warning",
    "caution",
    "danger",
    "important",
    "attention",
    "note",
    "tip",
    "hint",
    "admonition",
    "callout",
];

struct Renderer<'a> {
    nodes: &'a [Node],
    removed: [usize; REMOVED_CATEGORIES],
    depth: usize,
}

impl Renderer<'_> {
    /// Returns the removal category for a node, or `None` when it is content.
    fn removal(&self, node: &Node, in_sectioning: bool) -> Option<Removed> {
        if matches!(node.kind, NodeKind::Comment) {
            return Some(Removed::Comment);
        }
        let category = match node.local_name() {
            "script" => Removed::Script,
            "style" => Removed::Style,
            "noscript" => Removed::Noscript,
            "template" => Removed::Template,
            "iframe" => Removed::Iframe,
            "nav" => Removed::Nav,
            "header" if !in_sectioning => Removed::Header,
            "footer" if !in_sectioning => Removed::Footer,
            "aside" => Removed::Aside,
            "button" | "input" | "select" | "textarea" | "datalist" | "progress" | "meter" => {
                Removed::FormControl
            }
            "audio" | "video" | "canvas" | "object" | "embed" | "map" => Removed::Media,
            "dialog" => Removed::Dialog,
            _ if !node.name_in("http://www.w3.org/2000/svg").is_empty() => Removed::Svg,
            _ if has_role(node, "navigation") => Removed::RoleNavigation,
            _ if has_role(node, "banner") => Removed::RoleBanner,
            _ if has_role(node, "contentinfo") => Removed::RoleContentinfo,
            _ if has_role(node, "complementary") => Removed::RoleComplementary,
            _ => return None,
        };
        Some(category)
    }

    /// Counts removable nodes in a subtree that is never rendered: the head,
    /// or a formula replaced by its TeX. Content nodes in such a subtree are
    /// dropped with it and never counted: they were not removed, just not
    /// rendered.
    fn count_removals(&mut self, id: usize, in_sectioning: bool) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            if let Some(category) = self.removal(&self.nodes[id], in_sectioning) {
                self.removed[category as usize] += 1;
                continue;
            }
            stack.extend(self.nodes[id].children.iter().rev());
        }
    }

    /// Text of a subtree with removable nodes dropped and counted. `sep` goes
    /// between text nodes: empty for verbatim code, a space when flattening
    /// markup so words from adjacent elements do not run together.
    fn text(&mut self, id: usize, in_sectioning: bool, sep: &str) -> String {
        let mut text = String::new();
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            if let Some(category) = self.removal(node, in_sectioning) {
                self.removed[category as usize] += 1;
                continue;
            }
            if let NodeKind::Text(value) = &node.kind {
                if !text.is_empty() {
                    text.push_str(sep);
                }
                text.push_str(value);
            }
            stack.extend(node.children.iter().rev());
        }
        text
    }

    /// Renders the children of a block container as blank-line separated blocks.
    fn blocks(&mut self, id: usize, in_sectioning: bool) -> String {
        let mut blocks: Vec<String> = Vec::new();
        let mut inline = String::new();
        let mut inline_open = false;
        for &child in &self.nodes[id].children {
            let node = &self.nodes[child];
            if let Some(category) = self.removal(node, in_sectioning) {
                self.removed[category as usize] += 1;
                continue;
            }
            if is_block(node) {
                flush_inline(&mut blocks, &mut inline, &mut inline_open);
                let child_sectioning = in_sectioning || is_sectioning(node);
                let rendered = self.block(child, child_sectioning);
                if !rendered.trim().is_empty() {
                    blocks.push(rendered);
                }
            } else {
                self.inline(child, &mut inline, in_sectioning);
                inline_open = true;
            }
        }
        flush_inline(&mut blocks, &mut inline, &mut inline_open);
        blocks.join("\n\n")
    }

    fn block(&mut self, id: usize, in_sectioning: bool) -> String {
        if self.depth == MAX_DEPTH {
            return collapse(&self.text(id, in_sectioning, " "));
        }
        self.depth += 1;
        let rendered = self.block_at_depth(id, in_sectioning);
        self.depth -= 1;
        rendered
    }

    fn block_at_depth(&mut self, id: usize, in_sectioning: bool) -> String {
        let node = &self.nodes[id];
        match node.local_name() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = node.local_name().as_bytes()[1] - b'0';
                let mut text = String::new();
                self.inline_children(id, &mut text, in_sectioning);
                let text = collapse(&text);
                if text.is_empty() {
                    return String::new();
                }
                format!("{} {text}", "#".repeat(usize::from(level)))
            }
            "pre" => {
                let language = language_of(node)
                    .or_else(|| {
                        node.children
                            .iter()
                            .map(|&child| &self.nodes[child])
                            .find(|child| child.is_element("code"))
                            .and_then(language_of)
                    })
                    .or_else(|| {
                        // Sphinx: div.highlight-<lang> > div.highlight > pre.
                        let mut ancestor = node.parent;
                        (0..2).find_map(|_| {
                            let id = ancestor?;
                            ancestor = self.nodes[id].parent;
                            language_of(&self.nodes[id])
                        })
                    })
                    .unwrap_or("");
                let code = self.text(id, in_sectioning, "");
                let code = code.strip_prefix('\n').unwrap_or(&code);
                let code = code.trim_end_matches('\n');
                let mut fence = "```".to_owned();
                while code.contains(&fence) {
                    fence.push('`');
                }
                format!("{fence}{language}\n{code}\n{fence}")
            }
            "ul" | "ol" | "menu" => self.list(id, in_sectioning),
            "table" => self.table(id, in_sectioning),
            "blockquote" => quote(&self.blocks(id, in_sectioning), None),
            "hr" => "---".to_owned(),
            "dt" => {
                let mut text = String::new();
                self.inline_children(id, &mut text, in_sectioning);
                format!("**{}**", collapse(&text))
            }
            "dd" => indent(&self.blocks(id, in_sectioning), "  "),
            local => {
                let inner = self.blocks(id, in_sectioning);
                let container =
                    matches!(local, "div" | "section" | "details" | "fieldset" | "figure");
                match admonition_label(node).filter(|_| container) {
                    Some(label) => quote(&inner, Some(label)),
                    None => inner,
                }
            }
        }
    }

    fn list(&mut self, id: usize, in_sectioning: bool) -> String {
        let ordered = self.nodes[id].is_element("ol");
        let start = self.nodes[id]
            .attr("start")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(1);
        let mut items = Vec::new();
        let mut number = start;
        for &child in &self.nodes[id].children {
            let node = &self.nodes[child];
            if let Some(category) = self.removal(node, in_sectioning) {
                self.removed[category as usize] += 1;
                continue;
            }
            // Stray inline content between items renders as its own item.
            let body = if node.is_element("li") || is_block(node) {
                self.blocks(child, in_sectioning)
            } else {
                let mut text = String::new();
                self.inline(child, &mut text, in_sectioning);
                text.trim().to_owned()
            };
            if body.is_empty() {
                continue;
            }
            let marker = if ordered {
                let marker = format!("{number}. ");
                number += 1;
                marker
            } else {
                "- ".to_owned()
            };
            let pad = " ".repeat(marker.len());
            let mut item = marker;
            for (index, line) in body.lines().enumerate() {
                if index > 0 {
                    item.push('\n');
                    if !line.is_empty() {
                        item.push_str(&pad);
                    }
                }
                item.push_str(line);
            }
            items.push(item);
        }
        items.join("\n")
    }

    fn table(&mut self, id: usize, in_sectioning: bool) -> String {
        let mut rows: Vec<(bool, Vec<String>)> = Vec::new();
        let mut caption = None;
        // Rows still covered by a `rowspan` above, per column.
        let mut pending: Vec<usize> = Vec::new();
        let mut stack: Vec<usize> = self.nodes[id].children.iter().rev().copied().collect();
        while let Some(child) = stack.pop() {
            let node = &self.nodes[child];
            if let Some(category) = self.removal(node, in_sectioning) {
                self.removed[category as usize] += 1;
                continue;
            }
            match node.local_name() {
                "caption" => {
                    let mut text = String::new();
                    self.inline_children(child, &mut text, in_sectioning);
                    caption = Some(collapse(&text));
                }
                "thead" | "tbody" | "tfoot" => {
                    stack.extend(node.children.iter().rev());
                }
                "tr" => {
                    // Only an all-`th` row is a header: `th scope=row` labels data rows.
                    // Spanned cells insert empty cells so later cells stay in their
                    // columns; trailing empties are dropped, since GFM pads short rows
                    // itself and padding every row to the widest would be quadratic.
                    let mut header = true;
                    let mut real_cells = 0;
                    let mut cells = Vec::new();
                    let mut column = 0;
                    for &cell in &node.children {
                        let cell_node = &self.nodes[cell];
                        if let Some(category) = self.removal(cell_node, in_sectioning) {
                            self.removed[category as usize] += 1;
                            continue;
                        }
                        if !matches!(cell_node.local_name(), "th" | "td") {
                            continue;
                        }
                        while column < MAX_TABLE_COLUMNS
                            && pending.get(column).is_some_and(|&rows| rows > 0)
                        {
                            pending[column] -= 1;
                            cells.push(String::new());
                            column += 1;
                        }
                        header &= cell_node.is_element("th");
                        real_cells += 1;
                        cells.push(self.blocks(cell, in_sectioning));
                        let columns = span(cell_node, "colspan")
                            .min(MAX_TABLE_COLUMNS.saturating_sub(column))
                            .max(1);
                        let rows_below = span(cell_node, "rowspan") - 1;
                        let end = (column + columns).min(MAX_TABLE_COLUMNS);
                        if pending.len() < end {
                            pending.resize(end, 0);
                        }
                        pending[column.min(end)..end].fill(rows_below);
                        cells.extend(std::iter::repeat_n(String::new(), columns - 1));
                        column += columns;
                    }
                    // Columns still covered below this row's last cell consume a row too.
                    for slot in pending.iter_mut().skip(column) {
                        *slot = slot.saturating_sub(1);
                    }
                    while cells.last().is_some_and(String::is_empty) {
                        cells.pop();
                    }
                    rows.push((header && real_cells > 0, cells));
                }
                _ => {}
            }
        }
        let width = rows.iter().map(|(_, cells)| cells.len()).max().unwrap_or(0);
        let mut out = String::new();
        if let Some(caption) = caption.filter(|c| !c.is_empty()) {
            let _ = writeln!(out, "**{caption}**\n");
        }
        if width == 0 {
            return out.trim_end().to_owned();
        }
        // A cell holding block content (a nested table, a list, several
        // paragraphs) cannot sit on one GFM line: the table is a layout grid,
        // so its cells are emitted as consecutive blocks in source order.
        if rows
            .iter()
            .flat_map(|(_, cells)| cells)
            .any(|cell| cell.contains('\n'))
        {
            let blocks: Vec<&str> = rows
                .iter()
                .flat_map(|(_, cells)| cells)
                .map(String::as_str)
                .filter(|cell| !cell.is_empty())
                .collect();
            out.push_str(&blocks.join("\n\n"));
            return out.trim_end().to_owned();
        }
        let line = |cells: &[String]| {
            let cells: Vec<String> = cells.iter().map(|cell| cell.replace('|', "\\|")).collect();
            format!("| {} |", cells.join(" | "))
        };
        let mut body = rows.iter();
        let header: Vec<String> = if rows[0].0 {
            let mut cells = body
                .next()
                .map(|(_, cells)| cells.clone())
                .unwrap_or_default();
            cells.resize(width, String::new());
            cells
        } else {
            vec![String::new(); width]
        };
        let _ = writeln!(out, "{}", line(&header));
        let _ = writeln!(out, "|{}", " --- |".repeat(width));
        for (_, cells) in body {
            let _ = writeln!(out, "{}", line(cells));
        }
        out.trim_end().to_owned()
    }

    fn inline_children(&mut self, id: usize, out: &mut String, in_sectioning: bool) {
        for &child in &self.nodes[id].children {
            let node = &self.nodes[child];
            if let Some(category) = self.removal(node, in_sectioning) {
                self.removed[category as usize] += 1;
                continue;
            }
            self.inline(child, out, in_sectioning);
        }
    }

    fn inline(&mut self, id: usize, out: &mut String, in_sectioning: bool) {
        if self.depth == MAX_DEPTH {
            push_collapsed(out, &self.text(id, in_sectioning, " "));
            return;
        }
        self.depth += 1;
        self.inline_at_depth(id, out, in_sectioning);
        self.depth -= 1;
    }

    fn inline_at_depth(&mut self, id: usize, out: &mut String, in_sectioning: bool) {
        let node = &self.nodes[id];
        match &node.kind {
            NodeKind::Text(text) => push_collapsed(out, text),
            NodeKind::Element { .. } if node.name_in(MATHML_NS) == "math" => {
                match self.tex_of(id) {
                    Some(tex) => {
                        self.count_removals(id, in_sectioning);
                        let _ = write!(out, "${tex}$");
                    }
                    None => self.inline_children(id, out, in_sectioning),
                }
            }
            NodeKind::Element { .. } => match node.local_name() {
                "br" => out.push('\n'),
                "img" => {
                    let alt = node.attr("alt").map(collapse).unwrap_or_default();
                    if alt.is_empty() {
                        return;
                    }
                    if !brackets_balanced(&alt) {
                        out.push_str(&alt);
                        return;
                    }
                    match node.attr("src").and_then(destination) {
                        Some(target) => {
                            let _ = write!(out, "![{alt}]({target})");
                        }
                        None => {
                            let _ = write!(out, "![{alt}]");
                        }
                    }
                }
                "a" => {
                    let mut text = String::new();
                    self.inline_children(id, &mut text, in_sectioning);
                    let text = text.trim();
                    match node.attr("href").and_then(destination) {
                        Some(target) if !text.is_empty() && brackets_balanced(text) => {
                            let _ = write!(out, "[{text}]({target})");
                        }
                        _ => out.push_str(text),
                    }
                }
                "code" | "kbd" | "samp" => {
                    let text = collapse(&self.text(id, in_sectioning, ""));
                    if text.is_empty() {
                        return;
                    }
                    let ticks = if text.contains('`') { "``" } else { "`" };
                    let _ = write!(out, "{ticks}{text}{ticks}");
                }
                "label" => {
                    // Adjacent labels (content-tab titles) must not run together.
                    if !out.is_empty() && !out.ends_with([' ', '\n']) {
                        out.push(' ');
                    }
                    self.inline_children(id, out, in_sectioning);
                    if !out.is_empty() && !out.ends_with([' ', '\n']) {
                        out.push(' ');
                    }
                }
                "strong" | "b" => self.wrapped(id, out, "**", in_sectioning),
                "em" | "i" => self.wrapped(id, out, "*", in_sectioning),
                _ if is_block(node) => {
                    // Block inside inline context: keep its text on its own line.
                    let rendered = self.block(id, in_sectioning);
                    if !rendered.is_empty() {
                        out.push('\n');
                        out.push_str(&rendered);
                        out.push('\n');
                    }
                }
                _ => self.inline_children(id, out, in_sectioning),
            },
            _ => {}
        }
    }

    /// TeX source of a MathML formula: its `application/x-tex` annotation,
    /// else the `alttext` attribute. Source containing `$` (even as `\$`)
    /// is rejected and the formula falls back to its rendered children: a
    /// missing formula is safer than a mispaired delimiter.
    fn tex_of(&self, id: usize) -> Option<String> {
        let usable = |tex: String| (!tex.is_empty() && !tex.contains('$')).then_some(tex);
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            let node = &self.nodes[current];
            if node.name_in(MATHML_NS) == "annotation"
                && node.attr("encoding") == Some("application/x-tex")
                && let Some(tex) = usable(collapse(&text_of(self.nodes, current)))
            {
                return Some(tex);
            }
            stack.extend(node.children.iter().rev());
        }
        self.nodes[id]
            .attr("alttext")
            .map(collapse)
            .and_then(usable)
    }

    fn wrapped(&mut self, id: usize, out: &mut String, marker: &str, in_sectioning: bool) {
        let mut text = String::new();
        self.inline_children(id, &mut text, in_sectioning);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if text.starts_with(char::is_whitespace) && !out.ends_with([' ', '\n']) {
            out.push(' ');
        }
        let _ = write!(out, "{marker}{trimmed}{marker}");
        if text.ends_with(char::is_whitespace) {
            out.push(' ');
        }
    }
}

fn is_block(node: &Node) -> bool {
    matches!(
        node.local_name(),
        "address"
            | "article"
            | "blockquote"
            | "body"
            | "dd"
            | "details"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "menu"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "ul"
    )
}

fn flush_inline(blocks: &mut Vec<String>, inline: &mut String, open: &mut bool) {
    if *open {
        let text = inline.trim();
        if !text.is_empty() {
            blocks.push(text.to_owned());
        }
        inline.clear();
        *open = false;
    }
}

/// Escapes body lines that imitate the view's own wrapper lines. Applied
/// once to the whole rendered body, so paragraphs, code, table cells and
/// flattened deep nesting are all covered; an escaped line stays escaped.
fn escape_wrapper_lines(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.starts_with("[End page]") || line.starts_with("[HTML page rendered as Markdown")
            {
                format!("\\{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where to insert a backslash so a line of page text is not read as a
/// Markdown block: an ATX heading, block quote, list item, thematic break,
/// setext underline or code fence. Only the forms CommonMark recognises
/// count, so `#hashtag`, `-1` and `1.5 s` stay as written. An ordered-list
/// marker is escaped at its delimiter, since a digit cannot be escaped.
fn block_marker_escape(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let &first = bytes.first()?;
    let run = bytes.iter().take_while(|&&b| b == first).count();
    // Blank means any ASCII whitespace: that is what `push_collapsed`
    // folds into the space CommonMark then reads after the marker.
    let blank_after = |index: usize| bytes.get(index).is_none_or(u8::is_ascii_whitespace);
    let only_marker = || bytes.iter().all(|&b| b == first || b.is_ascii_whitespace());
    let escaped = match first {
        b'>' => true,
        b'#' => run <= 6 && blank_after(run),
        b'-' | b'+' | b'*' => blank_after(1) || only_marker(),
        b'=' | b'_' => only_marker(),
        b'`' | b'~' => run >= 3,
        b'0'..=b'9' => {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            return (digits <= 9
                && matches!(bytes.get(digits), Some(b'.' | b')'))
                && blank_after(digits + 1))
            .then_some(digits);
        }
        _ => false,
    };
    escaped.then_some(0)
}

/// A URL from page markup as the URL parser reads it: control characters
/// and whitespace stripped from both ends, tabs and newlines dropped
/// throughout. Every URL the view prints goes through here, so none of them
/// can start a new line or hide its scheme behind a control character.
fn url_text(url: &str) -> std::borrow::Cow<'_, str> {
    let url = url.trim_matches(|c: char| c.is_ascii_control() || c.is_whitespace());
    if url.contains(['\t', '\n', '\r']) {
        url.replace(['\t', '\n', '\r'], "").into()
    } else {
        url.into()
    }
}

/// The Markdown destination for a page URL, or `None` when the view does not
/// link it: nothing left after normalization, or a `javascript:` or `data:`
/// scheme, checked case-insensitively on the normalized URL so nothing can
/// hide it. Scripts must not run from the view and data blobs would bloat
/// it; the link text or alt text stays. Backslashes are escaped in either
/// form; a target with whitespace, parentheses or angle brackets takes the
/// `<…>` form, inside which angle brackets are escaped too.
fn destination(url: &str) -> Option<String> {
    let url = url_text(url);
    let scheme = url.split_once(':').map(|(scheme, _)| scheme);
    if url.is_empty()
        || scheme.is_some_and(|scheme| {
            scheme.eq_ignore_ascii_case("javascript") || scheme.eq_ignore_ascii_case("data")
        })
    {
        return None;
    }
    let escaped = url.replace('\\', "\\\\");
    Some(
        if url.contains(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>')) {
            format!("<{}>", escaped.replace('<', "\\<").replace('>', "\\>"))
        } else {
            escaped
        },
    )
}

/// Whether text can sit between `[` and `]`: brackets outside code spans
/// must nest and close, or Markdown drops the whole link. Text failing this
/// is written without its link instead.
fn brackets_balanced(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'`' => {
                // Skip a code span; an unclosed backtick run is literal text.
                let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                let ticks = &text[i..i + run];
                i += run;
                if let Some(end) = text[i..].find(ticks) {
                    i += end + run;
                }
            }
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    depth == 0
}

/// A `colspan`/`rowspan` value; missing, unparsable and zero mean 1.
fn span(node: &Node, attr: &str) -> usize {
    node.attr(attr)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

/// Appends text with HTML whitespace collapsing; a run of whitespace becomes
/// one space unless the output already ends with whitespace. Text that
/// opens a line and starts like a Markdown block is escaped, so block
/// structure can only come from the renderer's own markup. An inline run
/// rendered into its own buffer (emphasis, link text, a heading) counts as
/// a line start too; the extra backslash there is harmless.
fn push_collapsed(out: &mut String, text: &str) {
    let mut escape_at = None;
    let mut seen_visible = false;
    for (index, c) in text.char_indices() {
        if c.is_ascii_whitespace() {
            if !out.ends_with([' ', '\n']) {
                out.push(' ');
            }
            continue;
        }
        if !seen_visible {
            seen_visible = true;
            if out.ends_with('\n') || out.trim_start_matches(' ').is_empty() {
                escape_at = block_marker_escape(&text[index..]).map(|at| index + at);
            }
        }
        if escape_at == Some(index) {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Code language from `language-x`, `lang-x`, Sphinx `highlight-x`, or MDN `brush: x`.
fn language_of(node: &Node) -> Option<&str> {
    let mut tokens = node.class_tokens().peekable();
    while let Some(token) = tokens.next() {
        let language = token
            .strip_prefix("language-")
            .or_else(|| token.strip_prefix("lang-"))
            .or_else(|| token.strip_prefix("highlight-"))
            .or_else(|| {
                (token == "brush:")
                    .then(|| tokens.peek().copied())
                    .flatten()
            });
        if let Some(language) = language.filter(|l| {
            !l.is_empty()
                && l.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '+' || c == '#')
        }) {
            return Some(language);
        }
    }
    None
}

fn admonition_label(node: &Node) -> Option<&'static str> {
    let mut generic = None;
    for token in node.class_tokens() {
        for piece in token.split(['-', '_']) {
            let Some(&label) = ADMONITION_LABELS
                .iter()
                .find(|label| piece.eq_ignore_ascii_case(label))
            else {
                continue;
            };
            if matches!(label, "admonition" | "callout") {
                generic = Some(label);
            } else {
                return Some(label);
            }
        }
    }
    generic
}

fn quote(inner: &str, label: Option<&str>) -> String {
    let mut out = String::new();
    let mut lines = inner.lines().peekable();
    if let Some(label) = label {
        // A leading title line that repeats the label (Sphinx, MkDocs) is folded into it.
        if lines
            .peek()
            .is_some_and(|line| line.trim_matches('*').eq_ignore_ascii_case(label))
        {
            lines.next();
        }
        let mut chars = label.chars();
        let first = chars.next().unwrap_or_default().to_ascii_uppercase();
        let _ = writeln!(out, "> **{first}{}**", chars.as_str());
    }
    for line in lines {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            let _ = writeln!(out, "> {line}");
        }
    }
    out.trim_end().to_owned()
}

fn indent(inner: &str, pad: &str) -> String {
    inner
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{pad}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/html_tests.rs");
}
