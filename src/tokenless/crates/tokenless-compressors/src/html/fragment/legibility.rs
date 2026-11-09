//! Whether every text of a fragment stays legible on screen.
//!
//! The view prints no styling, so it is faithful only while styling cannot
//! hide text from a reader. Text is hidden by colour when it barely differs
//! from what is painted behind it, and by size when it is too small or its
//! lines are stacked onto one another. [`text_is_legible`] follows both
//! through the fragment's own cascade: inline styles in their `!important`
//! order, presentational attributes, the user agent defaults of links,
//! marks, buttons, headings and code, and the fragment's `<style>` sheets
//! and table column backgrounds, which may reach any element because
//! selectors and column spans are not matched. The page behind the
//! fragment is taken to be a browser's default: white, with black 16px
//! text. Values a browser could read differently, and boxes that could
//! move text off its background, were already rejected by [`super::css`].

use std::collections::HashMap;
use std::rc::Rc;

use super::color::{self, Color, Paint};
use super::css::{self, Background, Declaration, FontSize, LineHeight};
use super::{Node, NodeKind, text_of};

/// Contrast ratio below which text counts as hidden: on white, anything
/// fainter than about `#d0d0d0` (1.54).
const MIN_CONTRAST: f64 = 1.5;
/// Smallest font size, in CSS pixels, that counts as readable.
const MIN_FONT_PX: f64 = 6.0;
/// Smallest line height, as a multiple of the font size, before adjacent
/// lines overprint each other.
const MIN_LINE_HEIGHT: f64 = 0.5;
/// Distinct colours that may be behind one element's text before the check
/// gives up.
const MAX_BACKDROPS: usize = 64;
/// Distinct pairs of text colour and backdrop one fragment may compare
/// before the check gives up. Each pair is compared once, so the work stays
/// bounded however many texts repeat the same colours, and a fragment that
/// meets thousands of distinct backdrops with thousands of sheet colours
/// passes through instead of stalling the compressor.
const MAX_COMPARISONS: usize = 1 << 16;

const DEFAULT_FONT_PX: f64 = 16.0;
/// Browsers size a button's font 13.333px, whatever its parent's size.
const BUTTON_FONT_PX: f64 = 40.0 / 3.0;
/// Links before and after a visit, unless the `body` says otherwise.
const LINK: Color = Color::rgb(0x00_00_ee);
const VISITED: Color = Color::rgb(0x55_1a_8b);
const MARK: Color = Color::rgb(0xff_ff_00);
const BUTTON_FACE: Color = Color::rgb(0xef_ef_ef);

/// Whether every visible text of the document rooted at `html` stays
/// legible: contrast of at least [`MIN_CONTRAST`] against every colour that
/// may be behind it, no image behind it, a font of at least
/// [`MIN_FONT_PX`], and lines at least [`MIN_LINE_HEIGHT`] apart.
pub(super) fn text_is_legible(nodes: &[Node], html: usize) -> bool {
    walk(nodes, html).is_some()
}

fn walk(nodes: &[Node], html: usize) -> Option<()> {
    let body = nodes[html]
        .children
        .iter()
        .find(|&&id| nodes[id].is_element("body"))
        .map(|&id| &nodes[id]);
    let link_color = |attribute: &str, default: Color| {
        body.and_then(|body| body.attr(attribute))
            .and_then(color::legacy)
            .unwrap_or(default)
    };
    let page = Style {
        colors: vec![Color::BLACK],
        backdrop: Backdrop::Colors(vec![Color::WHITE]),
        font: Range::exact(DEFAULT_FONT_PX),
        leading: Leading::Normal,
    };
    let mut cascade = Cascade {
        loose: Loose::of(nodes)?,
        links: vec![link_color("link", LINK), link_color("vlink", VISITED)],
        root_font: page.font,
        contrasts: Contrasts::default(),
    };
    let mut stack = vec![(html, Rc::new(page))];
    while let Some((id, inherited)) = stack.pop() {
        let node = &nodes[id];
        match &node.kind {
            NodeKind::Text(text) => {
                if !text.trim().is_empty() && !cascade.is_legible(&inherited) {
                    return None;
                }
            }
            // Head content and style text are never drawn.
            NodeKind::Element { .. } if node.is_element("head") || node.is_element("style") => {}
            NodeKind::Element { .. } => {
                let style = Rc::new(cascade.style(node, &inherited)?);
                if node.is_element("html") {
                    cascade.root_font = style.font;
                }
                stack.extend(
                    node.children
                        .iter()
                        .rev()
                        .map(|&child| (child, Rc::clone(&style))),
                );
            }
            _ => {}
        }
    }
    Some(())
}

/// Bounds of a size in CSS pixels. Sheets that may or may not reach an
/// element widen them.
#[derive(Debug, Clone, Copy)]
struct Range {
    low: f64,
    high: f64,
}

impl Range {
    fn exact(px: f64) -> Self {
        Self { low: px, high: px }
    }

    fn scale(self, factor: f64) -> Self {
        Self {
            low: self.low * factor,
            high: self.high * factor,
        }
    }

    fn widen(self, other: Self) -> Self {
        Self {
            low: self.low.min(other.low),
            high: self.high.max(other.high),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Leading {
    Normal,
    /// A multiple of the font size of the text itself.
    Factor(f64),
    /// Fixed when declared, whatever font size the text has.
    Px(Range),
}

#[derive(Debug, Clone)]
enum Backdrop {
    /// Opaque colours the element's own and inherited backgrounds may put
    /// behind the text. Sheet backgrounds, which may be behind any text,
    /// are kept apart in [`Loose`], so each element does not repeat them.
    Colors(Vec<Color>),
    /// An image or gradient may be.
    Unknown,
}

/// What an element passes to its text and children.
#[derive(Debug, Clone)]
struct Style {
    /// Colours the text may have.
    colors: Vec<Color>,
    backdrop: Backdrop,
    font: Range,
    leading: Leading,
}

/// Verdicts of colour comparisons already made in one fragment, so text
/// that repeats its colours and backdrops costs lookups, not comparisons.
struct Contrasts {
    /// Whether a text colour stays legible on a backdrop.
    pairs: HashMap<([u8; 4], [u8; 4]), bool>,
    /// Whether a backdrop keeps every sheet colour legible.
    under_sheet_colors: HashMap<[u8; 4], bool>,
    /// Whether a text colour stays legible on every sheet backdrop.
    over_sheet_backdrops: HashMap<[u8; 4], bool>,
    /// Whether every sheet colour stays legible on every sheet backdrop.
    sheets: Option<bool>,
    /// Comparisons left before [`MAX_COMPARISONS`] is reached.
    budget: usize,
}

impl Default for Contrasts {
    fn default() -> Self {
        Self {
            pairs: HashMap::new(),
            under_sheet_colors: HashMap::new(),
            over_sheet_backdrops: HashMap::new(),
            sheets: None,
            budget: MAX_COMPARISONS,
        }
    }
}

impl Contrasts {
    /// Whether `text` stays legible on `backdrop`; `false` once the budget
    /// is spent.
    fn legible(&mut self, text: Color, backdrop: Color) -> bool {
        let key = (text.key(), backdrop.key());
        if let Some(&legible) = self.pairs.get(&key) {
            return legible;
        }
        let Some(budget) = self.budget.checked_sub(1) else {
            return false;
        };
        self.budget = budget;
        let legible = text.over(backdrop).contrast(backdrop) >= MIN_CONTRAST;
        self.pairs.insert(key, legible);
        legible
    }

    /// Whether text in any of `colors`, or in any colour a sheet sets, stays
    /// legible on each of `backdrops` and on any background a sheet sets.
    fn all_legible(&mut self, colors: &[Color], backdrops: &[Color], loose: &Loose) -> bool {
        backdrops.iter().all(|&backdrop| {
            colors.iter().all(|&text| self.legible(text, backdrop))
                && self.under_sheet_colors(&loose.colors, backdrop)
        }) && colors
            .iter()
            .all(|&text| self.over_sheet_backdrops(text, &loose.backdrops))
            && self.sheets_legible(loose)
    }

    fn sheets_legible(&mut self, loose: &Loose) -> bool {
        if let Some(legible) = self.sheets {
            return legible;
        }
        let legible = loose.backdrops.iter().all(|&backdrop| {
            loose
                .colors
                .iter()
                .all(|&text| self.legible(text, backdrop))
        });
        self.sheets = Some(legible);
        legible
    }

    fn under_sheet_colors(&mut self, colors: &[Color], backdrop: Color) -> bool {
        if let Some(&legible) = self.under_sheet_colors.get(&backdrop.key()) {
            return legible;
        }
        let legible = colors.iter().all(|&text| self.legible(text, backdrop));
        self.under_sheet_colors.insert(backdrop.key(), legible);
        legible
    }

    fn over_sheet_backdrops(&mut self, text: Color, backdrops: &[Color]) -> bool {
        if let Some(&legible) = self.over_sheet_backdrops.get(&text.key()) {
            return legible;
        }
        let legible = backdrops
            .iter()
            .all(|&backdrop| self.legible(text, backdrop));
        self.over_sheet_backdrops.insert(text.key(), legible);
        legible
    }
}

/// Values the fragment's `<style>` sheets and table columns may give any
/// element.
#[derive(Debug, Default)]
struct Loose {
    colors: Vec<Color>,
    /// Opaque, so they hide whatever is under them wherever they apply.
    backdrops: Vec<Color>,
    font: Option<Range>,
}

impl Loose {
    /// Reads every sheet and column background, set by a column's style or
    /// its presentational attributes. `None` when one sets what
    /// cannot be followed into every element it may reach: a background
    /// image, a translucent or `currentcolor` background, which would
    /// depend on what is under it, a clip to the glyphs, which would move
    /// an inline background out from behind its text, a relative font
    /// size, which compounds with nesting, or a line height other than a
    /// number.
    fn of(nodes: &[Node]) -> Option<Self> {
        let mut loose = Self::default();
        for (id, node) in nodes.iter().enumerate() {
            if node.is_element("style") {
                for declaration in css::sheet_declarations(&text_of(nodes, id))? {
                    loose.add(&declaration)?;
                }
            } else if node.is_element("col") || node.is_element("colgroup") {
                // Browsers paint a column's `background` and `bgcolor`
                // attributes as they paint a background its style sets.
                if node
                    .attr("background")
                    .is_some_and(|url| !url.trim().is_empty())
                {
                    return None;
                }
                if let Some(color) = node.attr("bgcolor").and_then(color::legacy) {
                    loose.backdrop(Some(Paint::Color(color)))?;
                }
                for declaration in css::declarations(node.attr("style").unwrap_or_default())? {
                    if declaration.property.starts_with("background") {
                        loose.add(&declaration)?;
                    }
                }
            }
        }
        dedupe(&mut loose.colors);
        dedupe(&mut loose.backdrops);
        Some(loose)
    }

    fn add(&mut self, declaration: &Declaration) -> Option<()> {
        let value = declaration.value.as_str();
        match declaration.property.as_str() {
            "color" => {
                // Inherited and current colours are tracked where they come from.
                if let Some(Paint::Color(color)) = color::parse(value) {
                    self.colors.push(color);
                }
            }
            "background" => {
                let background = css::background(value)?;
                if background.image || background.clip_text {
                    return None;
                }
                self.backdrop(background.color)?;
            }
            "background-color" => match value {
                "initial" | "unset" => {}
                _ => self.backdrop(Some(color::parse(value)?))?,
            },
            "background-image" if !matches!(value, "none" | "initial" | "unset") => return None,
            // Clipped to the glyphs, a background an inline style paints
            // would no longer be behind the text it was judged against.
            "background-clip" if value == "inherit" || clips_to_text(value) => return None,
            "font-size" => self.font_size(css::font_size(value)?)?,
            "font" => {
                let font = css::font(value)?;
                self.font_size(font.size)?;
                self.line_height(font.line_height)?;
            }
            "line-height" => self.line_height(css::line_height(value)?)?,
            _ => {}
        }
        Some(())
    }

    fn backdrop(&mut self, paint: Option<Paint>) -> Option<()> {
        match paint {
            None => {}
            Some(Paint::Color(color)) if color.is_invisible() => {}
            Some(Paint::Color(color)) if color.is_opaque() => self.backdrops.push(color),
            Some(_) => return None,
        }
        Some(())
    }

    fn font_size(&mut self, size: FontSize) -> Option<()> {
        match size {
            FontSize::Inherit => {}
            FontSize::Px(px) if px >= MIN_FONT_PX => {
                let px = Range::exact(px);
                self.font = Some(self.font.map_or(px, |font| font.widen(px)));
            }
            _ => return None,
        }
        Some(())
    }

    fn line_height(&self, line_height: LineHeight) -> Option<()> {
        match line_height {
            LineHeight::Normal | LineHeight::Inherit => Some(()),
            LineHeight::Factor(factor) if factor >= MIN_LINE_HEIGHT => Some(()),
            _ => None,
        }
    }
}

struct Cascade {
    loose: Loose,
    /// Colours a link takes from the `body` or the browser.
    links: Vec<Color>,
    /// The `html` element's font size, which `rem` units refer to.
    root_font: Range,
    contrasts: Contrasts,
}

impl Cascade {
    /// Whether text styled by `style`, or by any colour a sheet may give
    /// it, is legible.
    fn is_legible(&mut self, style: &Style) -> bool {
        let Backdrop::Colors(backdrops) = &style.backdrop else {
            return false;
        };
        let spaced = match style.leading {
            Leading::Normal => true,
            Leading::Factor(factor) => factor >= MIN_LINE_HEIGHT,
            Leading::Px(px) => px.low >= MIN_LINE_HEIGHT * style.font.high,
        };
        style.font.low >= MIN_FONT_PX
            && spaced
            && self
                .contrasts
                .all_legible(&style.colors, backdrops, &self.loose)
    }

    /// The style `node` passes on, given its parent's. `None` when a value
    /// cannot be followed.
    fn style(&self, node: &Node, parent: &Style) -> Option<Style> {
        let name = node.local_name();
        let mut colors = parent.colors.clone();
        let mut font = parent.font;
        let mut leading = parent.leading;
        let mut layer = None;
        // User agent defaults that replace what the element inherits.
        let painted = |color| {
            Some(Background {
                color: Some(Paint::Color(color)),
                ..Background::default()
            })
        };
        match name {
            "a" if node.attr("href").is_some() => colors.clone_from(&self.links),
            "mark" => {
                colors = vec![Color::BLACK];
                layer = painted(MARK);
            }
            "button" => {
                colors = vec![Color::BLACK];
                layer = painted(BUTTON_FACE);
                font = Range::exact(BUTTON_FONT_PX);
                leading = Leading::Normal;
            }
            "small" => font = font.scale(1.0 / 1.2),
            // Nested in sections, an `h1` shrinks down to an `h6`'s size.
            "h1" => {
                font = Range {
                    low: font.low * 0.67,
                    high: font.high * 2.0,
                };
            }
            "h2" => font = font.scale(1.5),
            "h3" => font = font.scale(1.17),
            "h5" => font = font.scale(0.83),
            "h6" => font = font.scale(0.67),
            // The default monospace font is drawn at 13px where others are 16px.
            "code" | "kbd" | "samp" | "pre" => font.low *= 13.0 / 16.0,
            _ => {}
        }
        // Presentational attributes, which rank below inline styles.
        match name {
            "font" => {
                if let Some(color) = node.attr("color").and_then(color::legacy) {
                    colors = vec![color];
                }
                if let Some(px) = node.attr("size").and_then(legacy_font_size) {
                    font = Range::exact(px);
                }
            }
            "body" => {
                if let Some(color) = node.attr("text").and_then(color::legacy) {
                    colors = vec![color];
                }
            }
            _ => {}
        }
        if matches!(
            name,
            "body" | "table" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th"
        ) {
            let color = node.attr("bgcolor").and_then(color::legacy);
            let image = node
                .attr("background")
                .is_some_and(|url| !url.trim().is_empty());
            if color.is_some() || image {
                layer = Some(Background {
                    color: color.map(Paint::Color),
                    image,
                    clip_text: false,
                });
            }
        }

        let declarations = match node.attr("style") {
            Some(style) => css::declarations(style)?,
            None => Vec::new(),
        };
        let mut color_value = None;
        let mut size = None;
        let mut line_height = None;
        // Within one block an important declaration wins over any
        // normal one; otherwise the last one wins.
        let ordered = declarations
            .iter()
            .filter(|declaration| !declaration.important)
            .chain(
                declarations
                    .iter()
                    .filter(|declaration| declaration.important),
            );
        for declaration in ordered {
            let value = declaration.value.as_str();
            match declaration.property.as_str() {
                "color" => color_value = Some(value),
                "background" => layer = Some(css::background(value)?),
                "background-color" => {
                    layer.get_or_insert_default().color = match value {
                        "initial" | "unset" => None,
                        _ => Some(color::parse(value)?),
                    };
                }
                "background-image" => {
                    // `inherit` may repeat an image of the parent's.
                    layer.get_or_insert_default().image =
                        !matches!(value, "none" | "initial" | "unset");
                }
                "background-clip" => {
                    // Whether the parent's clip reaches the glyphs is unknown.
                    if value == "inherit" {
                        return None;
                    }
                    layer.get_or_insert_default().clip_text = clips_to_text(value);
                }
                "font" => {
                    let shorthand = css::font(value)?;
                    size = Some(shorthand.size);
                    line_height = Some(shorthand.line_height);
                }
                "font-size" => size = Some(css::font_size(value)?),
                "line-height" => line_height = Some(css::line_height(value)?),
                _ => {}
            }
        }

        if let Some(value) = color_value {
            colors = match value {
                "inherit" | "unset" => parent.colors.clone(),
                "initial" => vec![Color::BLACK],
                _ => match color::parse(value)? {
                    Paint::Color(color) => vec![color],
                    Paint::Current => parent.colors.clone(),
                },
            };
        }
        dedupe(&mut colors);
        // Relative sizes refer to the parent, not to a default of the element.
        font = match size {
            None => font,
            Some(FontSize::Px(px)) => Range::exact(px),
            Some(FontSize::Scale(factor)) => parent.font.scale(factor),
            Some(FontSize::Root(factor)) => self.root_font.scale(factor),
            Some(FontSize::Inherit) => parent.font,
        };
        if let Some(sheet) = self.loose.font {
            font = font.widen(sheet);
        }
        leading = match line_height {
            None => leading,
            Some(LineHeight::Normal) => Leading::Normal,
            Some(LineHeight::Inherit) => parent.leading,
            Some(LineHeight::Factor(factor)) => Leading::Factor(factor),
            Some(LineHeight::Px(px)) => Leading::Px(Range::exact(px)),
            Some(LineHeight::Em(factor)) => Leading::Px(font.scale(factor)),
            Some(LineHeight::Rem(factor)) => Leading::Px(self.root_font.scale(factor)),
        };

        let mut backdrop = paint_over(layer, &parent.backdrop, &self.loose.backdrops, &colors);
        if let Backdrop::Colors(backdrops) = &mut backdrop {
            dedupe(backdrops);
            if backdrops.len() + self.loose.backdrops.len() > MAX_BACKDROPS {
                return None;
            }
        }
        Some(Style {
            colors,
            backdrop,
            font,
            leading,
        })
    }
}

/// What is behind an element's text once its own background `layer` is
/// painted over its parent's `backdrop`, or over a sheet's background.
/// `colors` are the element's text colours, which `currentcolor` refers to.
fn paint_over(
    layer: Option<Background>,
    backdrop: &Backdrop,
    sheets: &[Color],
    colors: &[Color],
) -> Backdrop {
    let Some(layer) = layer else {
        return backdrop.clone();
    };
    if layer.image {
        return Backdrop::Unknown;
    }
    let paints = match layer.color {
        // Clipped to the glyphs, the colour is under the text, not behind it.
        _ if layer.clip_text => return backdrop.clone(),
        None => return backdrop.clone(),
        Some(Paint::Color(color)) => vec![color],
        Some(Paint::Current) => colors.to_vec(),
    };
    let mut painted = Vec::new();
    for paint in paints {
        if paint.is_opaque() {
            painted.push(paint);
        } else if let Backdrop::Colors(under) = backdrop {
            painted.extend(under.iter().chain(sheets).map(|&under| paint.over(under)));
        } else {
            return Backdrop::Unknown;
        }
    }
    Backdrop::Colors(painted)
}

/// The size a `font` element's `size` attribute sets, read as the HTML
/// standard's legacy font size parser does: `1` to `7`, or relative to
/// `3` with a sign. `None` where the attribute has no effect.
fn legacy_font_size(value: &str) -> Option<f64> {
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (sign, digits) = match value.as_bytes().first() {
        Some(b'+') => (1, &value[1..]),
        Some(b'-') => (-1, &value[1..]),
        _ => (0, value),
    };
    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        return None;
    }
    // A number too long to parse is clamped like any large one.
    let number = digits[..end].parse::<i64>().unwrap_or(i64::MAX);
    let size = match sign {
        1 => number.saturating_add(3),
        -1 => 3_i64.saturating_sub(number),
        _ => number,
    }
    .clamp(1, 7);
    const SIZES: [f64; 7] = [10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];
    usize::try_from(size - 1).ok().map(|index| SIZES[index])
}

/// Whether a `background-clip` value clips the background colour, which the
/// last layer's clip decides, to the glyphs.
fn clips_to_text(value: &str) -> bool {
    value
        .rsplit(',')
        .next()
        .is_some_and(|clip| clip.trim() == "text")
}

fn dedupe(colors: &mut Vec<Color>) {
    colors.sort_by_key(|color| color.key());
    colors.dedup_by_key(|color| color.key());
}
