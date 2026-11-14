//! Markdown views of complete HTML documents. Only enumerated non-content
//! elements are removed; every removal is counted in the view header and the
//! original page stays retrievable. No text-density or link-density scoring.

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;

use html5ever::interface::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{Attribute, LocalName, Namespace, ParseOpts, QualName, parse_document};

mod fragment;

/// Rendered bodies shorter than this are extraction failures, not pages.
/// Calibrated on 44 technical-documentation, error and SPA pages: app shells
/// render 0 chars, nav fragments of truncated pages at most 51, and the
/// smallest real page (example.com) 167.
const MIN_BODY_CHARS: usize = 64;

/// Element nesting beyond this depth is flattened to its collapsed text
/// instead of rendered recursively, so a deeply nested page cannot exhaust
/// the stack. Flattening loses block structure, including the line breaks
/// and indentation of code blocks; the words stay.
const MAX_DEPTH: usize = 128;

/// Parsing stops once the tree nests deeper than this: the HTML5 tree
/// builder scans the open-element stack per start tag, so parse time grows
/// quadratically with depth (about 1 s at 10 000 levels, 4 min at 160 000).
/// Every element the tree builder pushes counts, including void elements
/// and the implied `tbody` and `tr` of tables, so a table nested inside a
/// table cell costs three or four levels. Browsers cap the tree at 512;
/// real pages stay under 50.
///
/// This bounds depth, and with it parse time and the cost of measuring the
/// depth; it does not bound how many nodes a wide page allocates or the
/// memory they take.
///
/// Measuring the depth costs an ancestor walk per inserted element, so the
/// same page of 200 short paragraphs renders in 0.32 ms flat, 1.13 ms nested
/// 128 deep and 5.47 ms nested 500 deep (release build, x86-64). The walk
/// stops at this limit and a page over it is abandoned at the next piece
/// boundary, which bounds the overhead to one piece of maximally nested
/// markup; see [`Dom::max_depth`] for what would make it constant.
const MAX_PARSE_DEPTH: usize = 512;

/// Input is fed to the parser in pieces of this size and the depth checked
/// after each, so a page over the limit costs at most one more piece of
/// parsing. The depth comes from the parser's own tree, not a pre-scan:
/// foreign content, raw text, comments and attribute quoting all follow
/// the tokenizer's rules without a second implementation of them.
const PARSE_PIECE_BYTES: usize = 4096;

/// Elements whose content the tokenizer reads as raw text, so a tag inside
/// them is not a tag. `noscript` is one because the parser runs with
/// scripting enabled.
const RAW_TEXT: [&str; 7] = [
    "script",
    "style",
    "textarea",
    "title",
    "xmp",
    "plaintext",
    "noscript",
];

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
    /// Header line, title, URL, rendered body and end marker, followed by
    /// any text the input carried after the document's `</html>` end tag.
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
    /// Renders the page as Markdown. Text after the `</html>` end tag is not
    /// part of the page and follows the end marker as written, except that
    /// lines imitating the view's wrapper are escaped like page text. Returns
    /// `None` when the rendered body is too short to be a page or the markup
    /// nests deeper than the parse limit; callers keep the original then.
    #[must_use]
    pub fn render(&self, input: &str) -> Option<HtmlView> {
        let (document, trailer) = split_trailer(input);
        let nodes = parse(document)?;
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
        let mut renderer = Renderer::new(&nodes, false);
        if let Some(head) = head {
            renderer.count_removals(head, false);
        }
        // Escaped once here, where every rendering path ends up, and once
        // for the trailer below, so the view boundary can only come from the
        // renderer.
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
        let trailer = trailer.trim_start();
        if !trailer.is_empty() {
            output.push('\n');
            output.push_str(&escape_wrapper_lines(trailer));
        }
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

/// Parses markup into the node arena; index 0 is the document. Returns
/// `None` once the tree nests deeper than [`MAX_PARSE_DEPTH`].
fn parse(markup: &str) -> Option<Vec<Node>> {
    let mut parser = parse_document(Dom::default(), ParseOpts::default());
    let mut rest = markup;
    while !rest.is_empty() {
        let mut cut = PARSE_PIECE_BYTES.min(rest.len());
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (piece, tail) = rest.split_at(cut);
        parser.process(StrTendril::from_slice(piece));
        if parser.tokenizer.sink.sink.max_depth.get() > MAX_PARSE_DEPTH {
            return None;
        }
        rest = tail;
    }
    // Finishing only closes what is open; it inserts no elements.
    Some(parser.finish().nodes.into_inner())
}

/// Splits the input after the first `</html>` end tag that the tokenizer
/// would see as one, so a literal inside a comment or a script string does
/// not end the document. The parser would fold anything after the tag into
/// the body, where a root other than `<body>` drops it from the view: a
/// build log or grep listing printed after a fetched page would vanish. The
/// first tag is used because a trailer can itself contain one (a second
/// page, a grep over the page).
fn split_trailer(input: &str) -> (&str, &str) {
    let end = tags(input)
        .find(|tag| tag.closing && tag.name == "html")
        .map_or(input.len(), |tag| tag.end);
    input.split_at(end)
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
    /// Deepest element inserted so far, counting `<html>` as 1, from a walk
    /// over the ancestors at each insertion. Nothing is cached: the adoption
    /// agency appends to and reparents into elements that are not attached
    /// yet, so a depth stored then would be wrong for the whole subtree. An
    /// element inserted under a detached one is counted from that root and
    /// caught by the next insertion after the root is attached. The open
    /// element stack is a subsequence of the current node's ancestors, so
    /// the walk understates it by at most one, under foster parenting.
    ///
    /// The walk is linear in the depth of every element inserted (numbers in
    /// [`MAX_PARSE_DEPTH`]) and capped at that limit, which is what keeps it
    /// affordable. Caching a depth per node instead needs a generation counter
    /// to tell a stale depth from a fresh one, bumped at the three places that
    /// move existing nodes — a successful [`Dom::detach`],
    /// [`Dom::remove_from_parent`] and [`Dom::reparent_children`] — with the
    /// depths found by a walk written back along it, which would make the walk
    /// amortised constant. Left out as bookkeeping the cap makes unnecessary.
    max_depth: Cell<usize>,
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
                self.insert(&mut self.nodes.borrow_mut(), parent, index, id)
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

    // Children are searched from the end: in every shape measured the target
    // is the last child or, under foster parenting, the one before it, because
    // the tree builder removes and inserts near the current node. Nothing
    // enforces that — `remove_from_parent` also reaches here from the adoption
    // agency's furthest block and from a `<frameset>` body, and `Vec::remove`
    // moves everything after the index — so the time bounds in
    // `nesting_beyond_the_parse_limit_is_not_parsed`, which cover the wide
    // table and the adoption-agency shape, are the alarm should a future
    // html5ever move those targets away from the end. A scan from the front
    // would be quadratic in the width of a table whatever the position.
    fn detach(nodes: &mut [Node], id: usize) {
        if let Some(parent) = nodes[id].parent.take() {
            let children = &mut nodes[parent].children;
            if let Some(index) = children.iter().rposition(|&child| child == id) {
                children.remove(index);
            }
        }
    }

    fn insert(&self, nodes: &mut [Node], parent: usize, index: usize, id: usize) {
        Self::detach(nodes, id);
        nodes[id].parent = Some(parent);
        nodes[parent].children.insert(index, id);
        if !matches!(nodes[id].kind, NodeKind::Element { .. }) {
            return;
        }
        // Capped one past the limit: the page is abandoned at that point,
        // so a walk never costs more than the limit allows.
        let mut depth = 1;
        let mut ancestor = parent;
        while let Some(next) = nodes[ancestor].parent
            && depth <= MAX_PARSE_DEPTH
        {
            depth += 1;
            ancestor = next;
        }
        self.max_depth.set(self.max_depth.get().max(depth));
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
        let id = self.push(NodeKind::Element {
            name,
            attrs,
            template,
        });
        // Template contents hang off the template, so their depth counts on
        // from it and a `<template>` cannot restart the count. The contents
        // get a parent but never enter the template's `children`: the depth
        // walk climbs through `parent`, while rendering and the tree searches
        // descend through `children`, so contents count towards the parse
        // limit and are never rendered. No other node in the tree has a parent
        // that does not list it as a child.
        if let Some(contents) = template {
            self.nodes.borrow_mut()[contents].parent = Some(id);
        }
        id
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
            .rposition(|&child| child == *sibling)
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

/// A tag found by [`tags`]: lowercase name and the offset just past its `>`.
struct Tag {
    name: String,
    closing: bool,
    end: usize,
}

/// Scans raw markup for the tags the tokenizer would see, without building
/// a tree: comments and raw-text elements are skipped whole, so a literal
/// tag inside `<!-- -->` or a `<script>` string is content, not a tag. An
/// end tag may carry whitespace or attributes before its `>`. Raw text is
/// skipped only when the tokenizer surely reads it as such: a self-closing
/// start tag closes the element in foreign content, and an unterminated one
/// has no end to skip to. In doubt the scanner sees tags, because a missed
/// `</html>` silently drops the trailer, while a false split only leaves
/// markup after the view.
///
/// A comment with no `-->` does not end the scan either: the scanner carries
/// on seeing tags after it, because the alternative is missing a real
/// `</html>` and the trailer that follows it.
///
/// The `/>` test reads the two bytes before the first `>` and so is blind to
/// attribute values: `<script src="a/>">`, whose tag ends after the quoted
/// value, and `<script src=a/>`, whose slash the tokenizer ignores in HTML
/// content, both look self-closing. The scanner then sees tags inside the
/// element, where a literal `</html>` in a string ends the document early:
/// what follows becomes the trailer, or the body falls under
/// [`MIN_BODY_CHARS`] and the caller keeps the original. No text is lost
/// either way, and telling those two from a foreign self-closing tag takes
/// the tokenizer's attribute states, which this scanner exists to avoid.
fn tags(input: &str) -> impl Iterator<Item = Tag> + '_ {
    let bytes = input.as_bytes();
    let find = move |from: usize, needle: &[u8]| {
        bytes[from..]
            .windows(needle.len())
            .position(|w| w.eq_ignore_ascii_case(needle))
            .map(|at| from + at + needle.len())
    };
    let mut i = 0usize;
    // Per raw-text name, and for the comment terminator, the offset from which
    // that needle is known to be absent. A failed search covered everything
    // after the offset it started from and `i` only grows, so every later
    // search for the same needle fails too; without the note each unterminated
    // comment or raw-text start tag would rescan to the end of the input and
    // the whole scan would be quadratic in their number.
    let mut absent = [usize::MAX; RAW_TEXT.len()];
    let mut comment_absent = usize::MAX;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            if bytes[i] != b'<' {
                i += 1;
                continue;
            }
            if bytes[i..].starts_with(b"<!--") {
                if i < comment_absent {
                    match find(i + 4, b"-->") {
                        Some(after) => i = after,
                        // Jumping to the end here would hide every later tag,
                        // including a real `</html>` and the trailer after it.
                        None => {
                            comment_absent = i + 4;
                            i += 4;
                        }
                    }
                } else {
                    i += 4;
                }
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
            if !closing
                && let Some(kind) = RAW_TEXT.iter().position(|raw| *raw == name)
                && !bytes[..i].ends_with(b"/>")
                && i < absent[kind]
            {
                match find(i, format!("</{name}").as_bytes()) {
                    Some(after) => {
                        i = after;
                        continue;
                    }
                    None => absent[kind] = i,
                }
            }
            return Some(Tag {
                name,
                closing,
                end: i,
            });
        }
        None
    })
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

/// How [`Renderer::text_lines`] reads newlines inside text nodes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WhiteSpace {
    /// Newlines are collapsible spaces, as in ordinary flow and inline code.
    Collapse,
    /// Newlines end lines, as in `pre`.
    Preserve,
}

/// Starts a new line at a block edge unless the current one is still empty,
/// so adjacent block edges break the line once. With preserved whitespace,
/// a line that ends with a newline has already ended there: the newline
/// becomes the break instead of adding a blank line before the block.
fn break_line(lines: &mut Vec<String>, white_space: WhiteSpace) {
    let Some(line) = lines.last_mut() else {
        return;
    };
    if white_space == WhiteSpace::Preserve && line.ends_with('\n') {
        line.pop();
    } else if line.is_empty() {
        return;
    }
    lines.push(String::new());
}

/// A link or image `title`, collapsed, when a fragment view prints it.
fn tooltip(node: &Node) -> Option<String> {
    node.attr("title")
        .map(collapse)
        .filter(|title| !title.is_empty())
}

/// The `(destination "title")` part of a Markdown link or image. A
/// fragment view prints a tooltip as the Markdown title, so a fact kept
/// only in the attribute is not lost.
fn link_tail(target: &str, title: Option<&str>) -> String {
    match title {
        Some(title) => format!(
            "({target} \"{}\")",
            title.replace('\\', "\\\\").replace('"', "\\\"")
        ),
        None => format!("({target})"),
    }
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
    /// Renders a fragment embedded in data: visible controls keep their
    /// labels and images or links without text keep their targets, because
    /// nothing visible may be removed from a fragment.
    fragment: bool,
    /// Set when structure was flattened (deep nesting, a layout-grid table,
    /// columns past the alignment cap) or, in a fragment, when ordered-list
    /// numbers would differ from a browser's or a table has more header
    /// rows than GFM can show. A page view accepts that; a fragment view is
    /// rejected instead.
    lossy: bool,
}

impl<'a> Renderer<'a> {
    fn new(nodes: &'a [Node], fragment: bool) -> Self {
        Self {
            nodes,
            removed: [0; REMOVED_CATEGORIES],
            depth: 0,
            fragment,
            lossy: false,
        }
    }

    /// Returns the removal category for a node, or `None` when it is content.
    fn removal(&self, node: &Node, in_sectioning: bool) -> Option<Removed> {
        if matches!(node.kind, NodeKind::Comment) {
            return Some(Removed::Comment);
        }
        let category = match node.local_name() {
            // A button label is visible text; its action has no Markdown form.
            "button" if self.fragment => return None,
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

    /// Text of a subtree with removable nodes dropped and counted, its lines
    /// joined by `\n`. See [`Self::text_lines`].
    fn text(
        &mut self,
        id: usize,
        in_sectioning: bool,
        sep: &str,
        white_space: WhiteSpace,
    ) -> String {
        self.text_lines(id, in_sectioning, sep, white_space)
            .join("\n")
    }

    /// Text of a subtree split where a browser breaks the line: at each `br`
    /// and at the edges of block-level elements, which carry no text node
    /// of their own. `sep` goes between text nodes of one line: empty for
    /// verbatim code, a space when flattening markup so words from adjacent
    /// elements do not run together. Newlines inside text nodes stay in
    /// their line; `white_space` says whether they already end it.
    fn text_lines(
        &mut self,
        id: usize,
        in_sectioning: bool,
        sep: &str,
        white_space: WhiteSpace,
    ) -> Vec<String> {
        enum Visit {
            Enter(usize),
            ExitBlock,
        }
        let mut lines = vec![String::new()];
        let mut stack = vec![Visit::Enter(id)];
        while let Some(visit) = stack.pop() {
            let Visit::Enter(id) = visit else {
                break_line(&mut lines, white_space);
                continue;
            };
            let node = &self.nodes[id];
            if let Some(category) = self.removal(node, in_sectioning) {
                self.removed[category as usize] += 1;
                continue;
            }
            match &node.kind {
                NodeKind::Text(value) => {
                    if let Some(line) = lines.last_mut() {
                        if !line.is_empty() {
                            line.push_str(sep);
                        }
                        line.push_str(value);
                    }
                }
                // Consecutive breaks leave the blank lines they show.
                NodeKind::Element { .. } if node.is_element("br") => lines.push(String::new()),
                NodeKind::Element { .. } if is_block(node) => {
                    break_line(&mut lines, white_space);
                    stack.push(Visit::ExitBlock);
                }
                _ => {}
            }
            stack.extend(node.children.iter().rev().map(|&child| Visit::Enter(child)));
        }
        lines
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
            self.lossy = true;
            return collapse(&self.text(id, in_sectioning, " ", WhiteSpace::Collapse));
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
                let code = self.text(id, in_sectioning, "", WhiteSpace::Preserve);
                let code = code.strip_prefix('\n').unwrap_or(&code);
                let code = code.trim_end_matches('\n');
                let fence = "`".repeat((longest_backtick_run(code) + 1).max(3));
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
        let list = &self.nodes[id];
        let ordered = list.is_element("ol");
        // CommonMark ordered lists only render decimal numbers. A list with a
        // non-decimal `type` (a/A/i/I) is still rendered, but marked lossy so
        // the caller knows the markers no longer match the source page.
        if ordered
            && matches!(
                list.attr("type").unwrap_or("").trim(),
                "a" | "A" | "i" | "I"
            )
        {
            self.lossy = true;
        }
        // A reversed list counts down, by default to 1 at its last item.
        let step: i64 = if list.attr("reversed").is_some() {
            -1
        } else {
            1
        };
        let item_count = list
            .children
            .iter()
            .filter(|&&child| self.nodes[child].is_element("li"))
            .count();
        let mut number = list
            .attr("start")
            .and_then(parse_integer)
            .unwrap_or(if step < 0 {
                i64::try_from(item_count).unwrap_or(i64::MAX)
            } else {
                1
            });
        let mut items = Vec::new();
        for &child in &self.nodes[id].children {
            let node = &self.nodes[child];
            let is_item = node.is_element("li");
            // As in a browser, every `li` takes a number, whether or not the
            // view shows the item, an item's `value` restarts the count, and
            // nothing else is numbered.
            let ordinal = (ordered && is_item).then(|| {
                if let Some(value) = node.attr("value").and_then(parse_integer) {
                    number = value;
                }
                let ordinal = number;
                number = number.saturating_add(step);
                ordinal
            });
            if let Some(category) = self.removal(node, in_sectioning) {
                self.removed[category as usize] += 1;
                continue;
            }
            // Stray inline content between items renders as its own item.
            let body = if is_item || is_block(node) {
                self.blocks(child, in_sectioning)
            } else {
                let mut text = String::new();
                self.inline(child, &mut text, in_sectioning);
                text.trim().to_owned()
            };
            // An empty item leaves a gap in the numbers, and content
            // between items has none: a fragment view shows neither.
            if self.fragment && ordered && is_item == body.is_empty() {
                self.lossy = true;
            }
            let marker = match ordinal {
                Some(ordinal) => format!("{ordinal}. "),
                None if ordered => String::new(),
                None => "- ".to_owned(),
            };
            if body.is_empty() {
                continue;
            }
            // Content between items is indented to stay inside the list.
            let pad = " ".repeat(if marker.is_empty() { 3 } else { marker.len() });
            let mut item = if marker.is_empty() {
                pad.clone()
            } else {
                marker
            };
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
        let mut remaining_rows = 0;
        let mut stack = table_children(self.nodes, id);
        stack.reverse();
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
                    // Zero rowspans grow to the group's end; no span crosses groups.
                    pending.clear();
                    remaining_rows = node
                        .children
                        .iter()
                        .filter(|&&row| self.nodes[row].is_element("tr"))
                        .count();
                    stack.extend(node.children.iter().rev());
                }
                "tr" => {
                    remaining_rows = remaining_rows.saturating_sub(1);
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
                        let requested = span(cell_node, "colspan");
                        if column.saturating_add(requested.max(1)) > MAX_TABLE_COLUMNS {
                            self.lossy = true;
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
        // GFM has one header row, at the top: any other row of headers,
        // such as a second header line or a section label further down,
        // would read as data.
        if self.fragment && rows.iter().skip(1).any(|(header, _)| *header) {
            self.lossy = true;
        }
        // A cell holding block content (a nested table, a list, several
        // paragraphs) cannot sit on one GFM line: the table is a layout grid,
        // so its cells are emitted as consecutive blocks in source order.
        if rows
            .iter()
            .flat_map(|(_, cells)| cells)
            .any(|cell| cell.contains('\n'))
        {
            self.lossy = true;
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
            self.lossy = true;
            push_collapsed(
                out,
                &self.text(id, in_sectioning, " ", WhiteSpace::Collapse),
            );
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
                    let title = if self.fragment { tooltip(node) } else { None };
                    if alt.is_empty() {
                        if self.fragment
                            && let Some(target) = node.attr("src").and_then(destination)
                        {
                            let _ = write!(out, "![]{}", link_tail(&target, title.as_deref()));
                        }
                        return;
                    }
                    if !brackets_balanced(&alt) {
                        out.push_str(&alt);
                        return;
                    }
                    match node.attr("src").and_then(destination) {
                        Some(target) => {
                            let _ = write!(out, "![{alt}]{}", link_tail(&target, title.as_deref()));
                        }
                        None => {
                            let _ = write!(out, "![{alt}]");
                        }
                    }
                }
                "a" => {
                    let mut text = String::new();
                    self.inline_children(id, &mut text, in_sectioning);
                    let trimmed = text.trim();
                    // Whitespace at the edges of the link text separates it
                    // from its neighbours, as `wrapped` keeps it for emphasis.
                    if text.starts_with(char::is_whitespace) && !out.ends_with([' ', '\n']) {
                        out.push(' ');
                    }
                    match node.attr("href").and_then(destination) {
                        Some(target)
                            if (self.fragment || !trimmed.is_empty())
                                && brackets_balanced(trimmed) =>
                        {
                            let title = if self.fragment { tooltip(node) } else { None };
                            let _ =
                                write!(out, "[{trimmed}]{}", link_tail(&target, title.as_deref()));
                        }
                        _ => out.push_str(trimmed),
                    }
                    if text.ends_with(char::is_whitespace) && !out.ends_with([' ', '\n']) {
                        out.push(' ');
                    }
                }
                "code" | "kbd" | "samp" => {
                    // A code span cannot hold a line break, so each line a
                    // `br` or block starts becomes its own span on its own
                    // line rather than running into its neighbour.
                    let lines = self.text_lines(id, in_sectioning, "", WhiteSpace::Collapse);
                    let mut spans = lines
                        .iter()
                        .map(|line| collapse(line))
                        .filter(|line| !line.is_empty());
                    if let Some(first) = spans.next() {
                        push_code_span(out, &first);
                        for line in spans {
                            out.push('\n');
                            push_code_span(out, &line);
                        }
                    }
                }
                // A fragment keeps button labels, which a browser draws as
                // separate boxes; a page view removes buttons before this.
                "label" | "button" => {
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
            | "center"
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

/// Reads an attribute as the HTML standard's rules for parsing integers
/// do: optional whitespace and sign, then digits, with anything after them
/// ignored. `None` without digits or outside `i32`, where browsers fall
/// back to the default.
fn parse_integer(value: &str) -> Option<i64> {
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
    let number = digits[..end].parse::<i32>().ok().map(i64::from)?;
    Some(if negative { -number } else { number })
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

/// Escapes lines that imitate the view's own wrapper lines. Applied once to
/// the whole rendered body, so paragraphs, code, table cells and flattened
/// deep nesting are all covered, and once to the trailer; an escaped line
/// stays escaped. Line terminators are kept as they are.
fn escape_wrapper_lines(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.starts_with("[End page]") || line.starts_with("[HTML page rendered as Markdown")
            {
                format!("\\{line}")
            } else {
                line.to_owned()
            }
        })
        .collect()
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

/// Length of the longest consecutive backtick run, in one pass over the
/// text. Code fences must outgrow that run; block fences also require at
/// least three backticks.
fn longest_backtick_run(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut longest = 0usize;
    let mut run = 0usize;
    for &b in bytes {
        if b == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
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

/// Children of a `table` in the order a browser draws them: captions above
/// the table, then the first `thead`, every other child in source order,
/// and the first `tfoot` last, wherever either sits in the source.
fn table_children(nodes: &[Node], table: usize) -> Vec<usize> {
    let children = &nodes[table].children;
    let first = |name| {
        children
            .iter()
            .copied()
            .find(|&child| nodes[child].is_element(name))
    };
    let (head, foot) = (first("thead"), first("tfoot"));
    let captions = children
        .iter()
        .copied()
        .filter(|&child| nodes[child].is_element("caption"));
    let rest = children.iter().copied().filter(|&child| {
        Some(child) != head && Some(child) != foot && !nodes[child].is_element("caption")
    });
    captions.chain(head).chain(rest).chain(foot).collect()
}

/// A `colspan`/`rowspan` value; missing or unparsable values mean 1.
fn span(node: &Node, attr: &str) -> usize {
    node.attr(attr)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(1)
}

/// Appends `text` as one code span. A code span is closed by a backtick run
/// of the same length as its fence, so the fence must outgrow the longest
/// run in the text, exactly like a code block's. One linear scan finds that
/// run; probing with `contains` for every prefix length is quadratic in the
/// run length, which untrusted HTML controls.
fn push_code_span(out: &mut String, text: &str) {
    let ticks = "`".repeat(longest_backtick_run(text) + 1);
    let _ = write!(out, "{ticks}{text}{ticks}");
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
