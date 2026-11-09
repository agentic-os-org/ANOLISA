//! CSS a fragment view may drop.
//!
//! A Markdown view prints no styling, which is faithful only while the
//! styling changes how text looks rather than what it says or whether it
//! is seen. Each declaration is therefore checked against an allowlist of
//! cosmetic properties, and anything else rejects the fragment: struck
//! prices, raised exponents, hidden or generated text, list markers,
//! preserved line breaks, symbol fonts that draw letters as pictures, boxes
//! whose text can overflow onto other content, inline boxes tall enough to
//! paint over neighbouring lines, and every property or spelling the check
//! cannot read, including escapes, vendor and custom properties, and
//! functions other than colours and images, such as `calc()` or `var()`,
//! which can compute or substitute any value. Colours and sizes pass here
//! when they can be read; whether the text they produce is legible is
//! decided by [`super::legibility`], which follows them through the
//! fragment's cascade. Selectors are not matched, so a `<style>` sheet must be
//! cosmetic for any element it could reach. Stylesheets outside the
//! fragment are unknown to every reader of the value and are not
//! considered.

use super::color;
use super::starts_with_ignore_ascii_case;

/// Tallest vertical padding or border, in CSS pixels, a non-replaced inline
/// element may have. Neither moves its line, so a taller one paints over
/// the lines around it.
const MAX_INLINE_EXTENT_PX: f64 = 8.0;

/// Widest letter or word spacing, in CSS pixels, that still reads as one
/// word or line: wider spacing scatters it across the screen.
const MAX_SPACING_PX: f64 = 32.0;

/// Functions whose results the checks read: colours, which
/// [`color::parse`] validates, and images, which [`super::legibility`]
/// takes as unknown backdrops. Any other function, such as `calc()`,
/// `min()` or `var()`, can compute or substitute any value, so a check
/// reading its text would see a different value from a browser.
const KNOWN_FUNCTIONS: [&str; 11] = [
    "conic-gradient",
    "hsl",
    "hsla",
    "linear-gradient",
    "radial-gradient",
    "repeating-conic-gradient",
    "repeating-linear-gradient",
    "repeating-radial-gradient",
    "rgb",
    "rgba",
    "url",
];

/// Line styles a border side shorthand may name beside a width and colour.
const BORDER_STYLES: [&str; 10] = [
    "none", "hidden", "dotted", "dashed", "solid", "double", "groove", "ridge", "inset", "outset",
];

/// Decoration lines other than `line-through`, which strikes text out.
const DECORATION_LINES: [&str; 6] = [
    "none",
    "underline",
    "overline",
    "blink",
    "spelling-error",
    "grammar-error",
];

/// Line styles and thickness words of a `text-decoration` shorthand; a
/// thickness set as a length is left out, as a thick line covers text.
const DECORATION_STYLES: [&str; 7] = [
    "solid",
    "double",
    "dotted",
    "dashed",
    "wavy",
    "auto",
    "from-font",
];

/// Fonts that draw letters as pictures or other scripts, such as Outlook's
/// Wingdings smiley typed as `J`. The view would print the letters.
const SYMBOL_FONTS: [&str; 13] = [
    "bookshelf symbol 7",
    "itc zapf dingbats",
    "marlett",
    "ms outlook",
    "ms reference specialty",
    "mt extra",
    "symbol",
    "webdings",
    "wingdings",
    "wingdings 2",
    "wingdings 3",
    "zapf dingbats",
    "zapfdingbats",
];

/// One declaration with its property and value lowercased.
pub(super) struct Declaration {
    pub(super) property: String,
    pub(super) value: String,
    /// Whether the value ended in `!important`, now removed. Within one
    /// block an important declaration wins over any normal one.
    pub(super) important: bool,
}

/// What an inline `style` attribute styles; the closures run only when a
/// declaration needs them.
pub(super) struct Styled<'a> {
    /// A non-replaced inline element: width and height do not apply to it,
    /// and its vertical padding and borders do not move its line.
    pub(super) inline: bool,
    /// A table part or image, which grows to fit its content whatever size
    /// it is given.
    pub(super) grows: bool,
    /// Whether the element's text, descendants included, has any visible
    /// character.
    pub(super) has_text: &'a dyn Fn() -> bool,
    /// Whether that text has line breaks, tabs or space runs that
    /// preserving whitespace would show.
    pub(super) keeps_whitespace: &'a dyn Fn() -> bool,
}

/// Whether an inline `style` attribute only changes how its element looks.
pub(super) fn inline_is_cosmetic(block: &str, styled: &Styled<'_>) -> bool {
    declarations(block).is_some_and(|declarations| {
        declarations
            .iter()
            .all(|declaration| is_cosmetic(declaration, Some(styled)))
    })
}

/// Whether a `<style>` sheet only changes how text looks, whatever element
/// each rule reaches. See [`sheet_declarations`] for the rules it reads.
pub(super) fn sheet_is_cosmetic(sheet: &str) -> bool {
    sheet_declarations(sheet).is_some_and(|declarations| {
        declarations
            .iter()
            .all(|declaration| is_cosmetic(declaration, None))
    })
}

/// Declarations of a `style` attribute or rule block, comments removed and
/// empty ones skipped. `None` when one has no `:`, a string is
/// unterminated or a bracket unbalanced.
pub(super) fn declarations(block: &str) -> Option<Vec<Declaration>> {
    let block = without_comments(block)?;
    split_top_level(&block, ';')?
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .map(declaration)
        .collect()
}

/// Declarations of every rule in a `<style>` sheet, in source order.
/// Rules inside an `@media` block are read like the rest; any other
/// at-rule, nested `@media` or rule, or unbalanced block makes the sheet
/// unreadable (`None`), which also keeps the reading linear in its length.
pub(super) fn sheet_declarations(sheet: &str) -> Option<Vec<Declaration>> {
    let sheet = without_comments(sheet)?;
    let mut declarations = Vec::new();
    collect_rules(&sheet, false, &mut declarations)?;
    Some(declarations)
}

fn collect_rules(mut sheet: &str, in_media: bool, out: &mut Vec<Declaration>) -> Option<()> {
    loop {
        sheet = sheet.trim_start();
        if sheet.is_empty() {
            return Some(());
        }
        // A statement ending in `;` before any block is an at-rule such as
        // `@import`, whose rules the view cannot see.
        let open = find_top_level(sheet, '{')?;
        if find_top_level(&sheet[..open], ';').is_some() {
            return None;
        }
        let prelude = sheet[..open].trim();
        let close = matching_close(sheet, open)?;
        let block = &sheet[open + 1..close];
        if let Some(rest) = prelude.strip_prefix('@') {
            if in_media || !starts_with_ignore_ascii_case(rest.as_bytes(), b"media") {
                return None;
            }
            collect_rules(block, true, out)?;
        } else {
            if block.contains('{') {
                return None;
            }
            out.extend(declarations(block)?);
        }
        sheet = &sheet[close + 1..];
    }
}

fn declaration(text: &str) -> Option<Declaration> {
    let (property, value) = text.split_once(':')?;
    let mut value = value.trim().to_ascii_lowercase();
    let mut important = false;
    if let Some(bang) = value.rfind('!')
        && value[bang + 1..].trim() == "important"
    {
        value.truncate(bang);
        value.truncate(value.trim_end().len());
        important = true;
    }
    Some(Declaration {
        property: property.trim().to_ascii_lowercase(),
        value,
        important,
    })
}

/// Whether one declaration is cosmetic. `styled` describes the element it
/// styles, or is `None` in a sheet, where any element may be styled.
fn is_cosmetic(declaration: &Declaration, styled: Option<&Styled<'_>>) -> bool {
    let Declaration {
        property, value, ..
    } = declaration;
    // An escape or a function can spell any value, so neither is read.
    if property.contains('\\') || value.contains('\\') || !calls_known_functions(value) {
        return false;
    }
    let value = value.as_str();
    match property.as_str() {
        "color" | "background-color" => color_value(value),
        "background" => background(value).is_some(),
        "font" => font(value).is_some_and(|font| !has_symbol_font(&font.family)),
        "font-family" => !has_symbol_font(value),
        "font-size" => font_size(value).is_some(),
        "line-height" => line_height(value).is_some(),
        // Negative spacing stacks glyphs onto each other, and wide spacing
        // scatters them.
        "letter-spacing" | "word-spacing" => {
            is_global(value)
                || value == "normal"
                || length(value).is_some_and(|px| (0.0..=MAX_SPACING_PX).contains(&px))
        }
        "text-decoration" => tokens(value).iter().all(|token| {
            is_global(token)
                || DECORATION_LINES.contains(token)
                || DECORATION_STYLES.contains(token)
                || color::parse(token).is_some()
        }),
        "text-decoration-line" => tokens(value)
            .iter()
            .all(|token| is_global(token) || DECORATION_LINES.contains(token)),
        // Raised or lowered text reads as an exponent or index once flattened.
        "vertical-align" => matches!(
            value,
            "baseline" | "top" | "middle" | "bottom" | "text-top" | "text-bottom"
        ),
        // Preserved whitespace is invisible only while there is none to keep.
        "white-space" => {
            matches!(value, "normal" | "nowrap")
                || styled.is_some_and(|styled| !(styled.keeps_whitespace)())
        }
        // A negative margin can move text out of view, above or left of
        // where a page scrolls. Each value must read as a size of at least
        // zero: a sign test alone would miss `0-9999px`, which a browser
        // reads as two values.
        _ if property == "margin" || property.starts_with("margin-") => {
            tokens(value).iter().all(|&token| {
                token == "auto"
                    || is_global(token)
                    || dimension(token).is_some_and(|(number, unit)| {
                        number >= 0.0 && (unit == "%" || length(token).is_some())
                    })
            })
        }
        // Text overflowing a box it does not fit lands on whatever is drawn
        // next to it, or under a later image.
        "width" | "height" | "max-width" | "max-height" => {
            matches!(value, "auto" | "none" | "100%" | "initial" | "unset")
                || styled
                    .is_some_and(|styled| styled.inline || styled.grows || !(styled.has_text)())
        }
        _ if property.starts_with("padding") || property.starts_with("border") => {
            box_edge_is_cosmetic(property, value, styled.is_none_or(|styled| styled.inline))
        }
        "background-image"
        | "background-position"
        | "background-position-x"
        | "background-position-y"
        | "background-size"
        | "background-repeat"
        | "background-attachment"
        | "background-origin"
        | "background-clip"
        | "background-blend-mode"
        | "font-style"
        | "font-weight"
        | "font-stretch"
        | "text-align"
        | "text-decoration-color"
        | "text-decoration-style"
        | "min-width"
        | "min-height"
        | "box-sizing"
        | "word-break"
        | "word-wrap"
        | "overflow-wrap" => true,
        _ => false,
    }
}

/// Whether a padding or border declaration only grows its box. On an
/// element that is (or, in a sheet, may be) a non-replaced inline one, its
/// vertical parts are capped at [`MAX_INLINE_EXTENT_PX`].
fn box_edge_is_cosmetic(property: &str, value: &str, inline: bool) -> bool {
    let tokens = tokens(value);
    let (sides, shorthand) = match property {
        "padding" | "border-width" => (sides_of_box(&tokens), false),
        "padding-top"
        | "padding-bottom"
        | "padding-block"
        | "padding-block-start"
        | "padding-block-end"
        | "border-top-width"
        | "border-bottom-width"
        | "border-block-width"
        | "border-block-start-width"
        | "border-block-end-width" => (Some(tokens.clone()), false),
        "padding-left"
        | "padding-right"
        | "padding-inline"
        | "padding-inline-start"
        | "padding-inline-end"
        | "border-left-width"
        | "border-right-width"
        | "border-inline-width"
        | "border-inline-start-width"
        | "border-inline-end-width" => (Some(Vec::new()), false),
        // A side shorthand mixes width, style and colour.
        "border" | "border-top" | "border-bottom" | "border-block" | "border-block-start"
        | "border-block-end" => (Some(tokens.clone()), true),
        "border-left"
        | "border-right"
        | "border-inline"
        | "border-inline-start"
        | "border-inline-end" => (Some(Vec::new()), true),
        "border-color" | "border-style" | "border-radius" | "border-collapse"
        | "border-spacing" => (Some(Vec::new()), false),
        _ if property.starts_with("border-")
            && ["-color", "-style", "-radius"]
                .iter()
                .any(|suffix| property.ends_with(suffix))
            && !property.starts_with("border-image") =>
        {
            (Some(Vec::new()), false)
        }
        _ => (None, false),
    };
    let Some(vertical) = sides else {
        return false;
    };
    // Every word of a shorthand must be read as a width, a style or a
    // colour, so a width spelled any other way is not taken for a style.
    let is_word = |token: &str| BORDER_STYLES.contains(&token) || color::parse(token).is_some();
    if shorthand
        && !tokens
            .iter()
            .all(|&token| is_global(token) || is_word(token) || edge_extent(token).is_some())
    {
        return false;
    }
    !inline
        || vertical.iter().all(|&token| {
            let extent = if shorthand && is_word(token) {
                Some(0.0)
            } else {
                edge_extent(token)
            };
            extent.is_some_and(|px| (0.0..=MAX_INLINE_EXTENT_PX).contains(&px))
        })
}

/// The size of one padding or border width value in CSS pixels. `None`
/// when unreadable, or for `inherit`, as the parent's edge may be any size.
fn edge_extent(token: &str) -> Option<f64> {
    match token {
        "initial" | "unset" => Some(0.0),
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        _ => length(token),
    }
}

/// Whether every function `value` calls is one of [`KNOWN_FUNCTIONS`].
/// Brackets inside strings, such as in a quoted `url()`, call nothing.
fn calls_known_functions(value: &str) -> bool {
    let mut index = 0;
    while let Some(c) = value[index..].chars().next() {
        match c {
            '"' | '\'' => match string_end(&value[index..]) {
                Some(end) => {
                    index += end;
                    continue;
                }
                None => return false,
            },
            '(' => {
                let name_start = value[..index]
                    .char_indices()
                    .rev()
                    .find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '-'))
                    .map_or(0, |(at, c)| at + c.len_utf8());
                if !KNOWN_FUNCTIONS.contains(&&value[name_start..index]) {
                    return false;
                }
            }
            _ => {}
        }
        index += c.len_utf8();
    }
    true
}

/// The top and bottom values of a one- to four-value box shorthand.
fn sides_of_box<'a>(tokens: &[&'a str]) -> Option<Vec<&'a str>> {
    match tokens {
        [all] => Some(vec![all]),
        [vertical, _] => Some(vec![vertical]),
        [top, _, bottom] | [top, _, bottom, _] => Some(vec![top, bottom]),
        _ => None,
    }
}

fn is_global(value: &str) -> bool {
    matches!(value, "inherit" | "initial" | "unset")
}

fn color_value(value: &str) -> bool {
    is_global(value) || color::parse(value).is_some()
}

/// A `font-size` value before the parent's size resolves it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum FontSize {
    /// CSS pixels.
    Px(f64),
    /// A multiple of the parent's size.
    Scale(f64),
    /// A multiple of the root element's size.
    Root(f64),
    /// The parent's size.
    Inherit,
}

/// Reads a `font-size` value. `None` for negative sizes, units relative to
/// the viewport or container, and anything else a browser could read
/// differently or the check cannot size.
pub(super) fn font_size(value: &str) -> Option<FontSize> {
    let px = match value {
        "inherit" | "unset" => return Some(FontSize::Inherit),
        "larger" => return Some(FontSize::Scale(1.2)),
        "smaller" => return Some(FontSize::Scale(1.0 / 1.2)),
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" | "initial" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        _ => {
            let (number, unit) = dimension(value)?;
            if number < 0.0 {
                return None;
            }
            return match unit {
                "em" => Some(FontSize::Scale(number)),
                "%" => Some(FontSize::Scale(number / 100.0)),
                "ex" | "ch" => Some(FontSize::Scale(number / 2.0)),
                "rem" => Some(FontSize::Root(number)),
                _ => absolute_px(number, unit).map(FontSize::Px),
            };
        }
    };
    Some(FontSize::Px(px))
}

/// A `line-height` value before the element's font size resolves it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum LineHeight {
    Normal,
    /// A multiple of the font size, inherited as a multiple.
    Factor(f64),
    /// CSS pixels.
    Px(f64),
    /// A multiple of the element's own font size, inherited as pixels.
    Em(f64),
    /// A multiple of the root element's font size.
    Rem(f64),
    Inherit,
}

/// Reads a `line-height` value; `None` as for [`font_size`].
pub(super) fn line_height(value: &str) -> Option<LineHeight> {
    match value {
        "normal" | "initial" => return Some(LineHeight::Normal),
        "inherit" | "unset" => return Some(LineHeight::Inherit),
        _ => {}
    }
    let (number, unit) = dimension(value)?;
    if number < 0.0 {
        return None;
    }
    match unit {
        "" => Some(LineHeight::Factor(number)),
        "em" => Some(LineHeight::Em(number)),
        "%" => Some(LineHeight::Em(number / 100.0)),
        "ex" | "ch" => Some(LineHeight::Em(number / 2.0)),
        "rem" => Some(LineHeight::Rem(number)),
        _ => absolute_px(number, unit).map(LineHeight::Px),
    }
}

/// The parts of a `font` shorthand the checks need.
pub(super) struct Font {
    pub(super) size: FontSize,
    /// `Normal` when the shorthand omits it, as the shorthand resets it.
    pub(super) line_height: LineHeight,
    pub(super) family: String,
}

/// Reads a `font` shorthand: optional style, variant, weight and stretch
/// keywords, a size, an optional `/` line height, then a family list.
/// `None` for system fonts such as `caption`, a global keyword, or any
/// other spelling.
pub(super) fn font(value: &str) -> Option<Font> {
    // `12px/1.5`, `12px / 1.5` and `12px /1.5` all read alike once the
    // slash is its own token; family names never hold one unquoted.
    let spaced = value.replace('/', " / ");
    let tokens = tokens(&spaced);
    let mut index = tokens
        .iter()
        .take_while(|token| is_font_prefix(token))
        .count();
    let size = *tokens.get(index)?;
    index += 1;
    let mut leading = None;
    if tokens.get(index) == Some(&"/") {
        leading = Some(*tokens.get(index + 1)?);
        index += 2;
    }
    if is_global(size) || leading.is_some_and(is_global) {
        return None;
    }
    let family = tokens
        .get(index..)
        .filter(|rest| !rest.is_empty())?
        .join(" ");
    Some(Font {
        size: font_size(size)?,
        line_height: leading.map_or(Some(LineHeight::Normal), line_height)?,
        family,
    })
}

fn is_font_prefix(token: &str) -> bool {
    matches!(
        token,
        "normal"
            | "italic"
            | "oblique"
            | "small-caps"
            | "bold"
            | "bolder"
            | "lighter"
            | "ultra-condensed"
            | "extra-condensed"
            | "condensed"
            | "semi-condensed"
            | "semi-expanded"
            | "expanded"
            | "extra-expanded"
            | "ultra-expanded"
    ) || number(token).is_some_and(|weight| (1.0..=1000.0).contains(&weight))
}

/// Whether a family list, as in `font-family` or a `font` element's `face`,
/// names a font from [`SYMBOL_FONTS`]. An unreadable list counts as one.
pub(super) fn has_symbol_font(families: &str) -> bool {
    let Some(families) = split_top_level(families, ',') else {
        return true;
    };
    families.iter().any(|family| {
        let family = family.trim();
        let family = family
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .or_else(|| family.strip_prefix('\'')?.strip_suffix('\''))
            .unwrap_or(family);
        let name = family
            .split_ascii_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        SYMBOL_FONTS.contains(&name.as_str())
    })
}

/// The parts of a `background` shorthand that decide what is painted
/// behind text.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct Background {
    pub(super) color: Option<color::Paint>,
    /// Any layer has an image or gradient, whose colours are unknown.
    pub(super) image: bool,
    /// The colour is clipped to the glyphs, so it is not behind the text.
    pub(super) clip_text: bool,
}

/// Reads a `background` shorthand. `None` for `inherit`, a colour outside
/// the last layer, or any keyword the check does not know.
pub(super) fn background(value: &str) -> Option<Background> {
    match value {
        "initial" | "unset" | "none" => {
            return Some(Background::default());
        }
        "inherit" => return None,
        _ => {}
    }
    let layers = split_top_level(value, ',')?;
    let mut background = Background::default();
    for (index, layer) in layers.iter().enumerate() {
        let last = index + 1 == layers.len();
        for token in tokens(layer) {
            if let Some(paint) = color::parse(token) {
                if !last || background.color.is_some() {
                    return None;
                }
                background.color = Some(paint);
            } else if token.contains('(') {
                background.image = true;
            } else if token == "text" {
                background.clip_text |= last;
            } else if !(matches!(
                token,
                "none"
                    | "/"
                    | "left"
                    | "right"
                    | "top"
                    | "bottom"
                    | "center"
                    | "auto"
                    | "cover"
                    | "contain"
                    | "repeat"
                    | "repeat-x"
                    | "repeat-y"
                    | "no-repeat"
                    | "space"
                    | "round"
                    | "scroll"
                    | "fixed"
                    | "local"
                    | "border-box"
                    | "padding-box"
                    | "content-box"
            ) || dimension(token).is_some())
            {
                return None;
            }
        }
    }
    Some(background)
}

/// A length in CSS pixels, with font-relative units at the default 16px.
/// `0` needs no unit. `None` for percentages and other units.
fn length(token: &str) -> Option<f64> {
    let (number, unit) = dimension(token)?;
    match unit {
        "" if number == 0.0 => Some(0.0),
        "em" | "rem" => Some(number * 16.0),
        "ex" | "ch" => Some(number * 8.0),
        _ => absolute_px(number, unit),
    }
}

fn absolute_px(number: f64, unit: &str) -> Option<f64> {
    match unit {
        "" if number == 0.0 => Some(0.0),
        "px" => Some(number),
        "pt" => Some(number * 4.0 / 3.0),
        "pc" => Some(number * 16.0),
        "in" => Some(number * 96.0),
        "cm" => Some(number * 96.0 / 2.54),
        "mm" => Some(number * 96.0 / 25.4),
        "q" => Some(number * 96.0 / 101.6),
        _ => None,
    }
}

/// A number and the unit after it, as `("12", "px")` from `12px`; the unit
/// is empty for a bare number. `None` unless the number is CSS syntax.
pub(super) fn dimension(token: &str) -> Option<(f64, &str)> {
    let bytes = token.as_bytes();
    let digits_from = |mut at: usize| {
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        at
    };
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    end = digits_from(end);
    if bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end = digits_from(end + 1);
    }
    // `e` starts an exponent only before a digit; otherwise it is `em`.
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(bytes.get(end + 1), Some(b'+' | b'-')));
        if bytes.get(end + 1 + sign).is_some_and(u8::is_ascii_digit) {
            end = digits_from(end + 1 + sign);
        }
    }
    Some((number(&token[..end])?, &token[end..]))
}

/// A CSS `<number>`: optional sign, digits with an optional fraction (or a
/// fraction alone), and an optional exponent. Rust also accepts forms CSS
/// rejects, such as `1.`, `inf` or `nan`, so the syntax is checked first.
pub(super) fn number(text: &str) -> Option<f64> {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(at) => (&unsigned[..at], Some(&unsigned[at + 1..])),
        None => (unsigned, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((integer, fraction)) => (integer.is_empty() || digits(integer)) && digits(fraction),
        None => digits(mantissa),
    };
    let exponent_ok = exponent
        .is_none_or(|exponent| digits(exponent.strip_prefix(['+', '-']).unwrap_or(exponent)));
    if !(mantissa_ok && exponent_ok) {
        return None;
    }
    text.parse::<f64>().ok().filter(|number| number.is_finite())
}

/// Parts of `value` separated by whitespace outside strings and brackets,
/// so `rgb(0 0 0)` stays one token.
pub(super) fn tokens(value: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = value.trim_start();
    while !rest.is_empty() {
        let end = find_top_level_by(rest, |c| c.is_ascii_whitespace()).unwrap_or(rest.len());
        tokens.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    tokens
}

/// CSS with comments and the legacy `<!--`/`-->` markers removed, leaving
/// strings intact so a comment opener inside one hides nothing. `None` when
/// a string is unterminated.
fn without_comments(css: &str) -> Option<String> {
    let mut output = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(c) = rest.chars().next() {
        if c == '"' || c == '\'' {
            let end = string_end(rest)?;
            output.push_str(&rest[..end]);
            rest = &rest[end..];
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |end| &after[end + 2..]);
            output.push(' ');
        } else if let Some(after) = rest.strip_prefix("<!--") {
            rest = after;
        } else if let Some(after) = rest.strip_prefix("-->") {
            rest = after;
        } else {
            output.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    Some(output)
}

/// Byte length of the quoted string at the start of `css`, quotes included.
fn string_end(css: &str) -> Option<usize> {
    let quote = css.chars().next()?;
    let mut escaped = false;
    for (index, c) in css.char_indices().skip(1) {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '\n' => return None,
            _ if c == quote => return Some(index + 1),
            _ => {}
        }
    }
    None
}

/// Parts of `css` separated by `separator` outside strings, parentheses and
/// blocks, so `url(data:...;base64,...)` stays one value. `None` when a
/// string is unterminated or a bracket unbalanced.
fn split_top_level(css: &str, separator: char) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut rest = css;
    while let Some(at) = find_top_level(rest, separator) {
        let absolute = css.len() - rest.len() + at;
        parts.push(&css[start..absolute]);
        start = absolute + separator.len_utf8();
        rest = &css[start..];
    }
    if !balanced(rest) {
        return None;
    }
    parts.push(&css[start..]);
    Some(parts)
}

/// Byte offset of the first `target` outside strings, parentheses and
/// blocks.
fn find_top_level(css: &str, target: char) -> Option<usize> {
    find_top_level_by(css, |c| c == target)
}

fn find_top_level_by(css: &str, is_target: impl Fn(char) -> bool) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = 0;
    while let Some(c) = css[index..].chars().next() {
        if depth == 0 && is_target(c) {
            return Some(index);
        }
        match c {
            '"' | '\'' => {
                index += string_end(&css[index..])?;
                continue;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
        index += c.len_utf8();
    }
    None
}

/// Byte offset of the `}` closing the block opened at `open`.
fn matching_close(css: &str, open: usize) -> Option<usize> {
    let inner = open + 1;
    find_top_level(&css[inner..], '}').map(|at| inner + at)
}

/// Whether strings terminate and brackets balance.
fn balanced(css: &str) -> bool {
    let mut depth = 0usize;
    let mut index = 0;
    while let Some(c) = css[index..].chars().next() {
        match c {
            '"' | '\'' => match string_end(&css[index..]) {
                Some(end) => {
                    index += end;
                    continue;
                }
                None => return false,
            },
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => match depth.checked_sub(1) {
                Some(next) => depth = next,
                None => return false,
            },
            _ => {}
        }
        index += c.len_utf8();
    }
    depth == 0
}
