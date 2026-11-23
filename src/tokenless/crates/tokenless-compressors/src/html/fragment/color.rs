//! Colours as a browser paints them: CSS values, legacy HTML attribute
//! values, compositing over a backdrop, and the contrast between two
//! colours.

use super::css::number;

/// An sRGB colour with straight alpha, every channel in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Color {
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
}

/// A CSS `<color>` value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Paint {
    Color(Color),
    /// `currentcolor`: the element's own text colour.
    Current,
}

impl Color {
    pub(super) const BLACK: Self = Self::rgb(0x00_00_00);
    pub(super) const WHITE: Self = Self::rgb(0xff_ff_ff);
    const TRANSPARENT: Self = Self {
        red: 0.0,
        green: 0.0,
        blue: 0.0,
        alpha: 0.0,
    };

    pub(super) const fn rgb(hex: u32) -> Self {
        Self {
            red: ((hex >> 16) & 0xff) as f64 / 255.0,
            green: ((hex >> 8) & 0xff) as f64 / 255.0,
            blue: (hex & 0xff) as f64 / 255.0,
            alpha: 1.0,
        }
    }

    fn clamped(red: f64, green: f64, blue: f64, alpha: f64) -> Self {
        Self {
            red: red.clamp(0.0, 1.0),
            green: green.clamp(0.0, 1.0),
            blue: blue.clamp(0.0, 1.0),
            alpha: alpha.clamp(0.0, 1.0),
        }
    }

    pub(super) fn is_opaque(self) -> bool {
        self.alpha >= 1.0
    }

    pub(super) fn is_invisible(self) -> bool {
        self.alpha <= 0.0
    }

    /// The opaque colour seen where this one is painted over `backdrop`.
    pub(super) fn over(self, backdrop: Self) -> Self {
        let mix = |top: f64, bottom: f64| top * self.alpha + bottom * (1.0 - self.alpha);
        Self {
            red: mix(self.red, backdrop.red),
            green: mix(self.green, backdrop.green),
            blue: mix(self.blue, backdrop.blue),
            alpha: 1.0,
        }
    }

    /// WCAG contrast ratio between two opaque colours, from 1 to 21.
    pub(super) fn contrast(self, other: Self) -> f64 {
        let (one, two) = (self.luminance(), other.luminance());
        (one.max(two) + 0.05) / (one.min(two) + 0.05)
    }

    fn luminance(self) -> f64 {
        let linear = |channel: f64| {
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.red) + 0.7152 * linear(self.green) + 0.0722 * linear(self.blue)
    }

    /// The colour at eight bits per channel, for deduplication.
    pub(super) fn key(self) -> [u8; 4] {
        // The channels are clamped to 0..=1, so each rounds into a byte.
        let byte = |channel: f64| (channel * 255.0).round() as u8;
        [
            byte(self.red),
            byte(self.green),
            byte(self.blue),
            byte(self.alpha),
        ]
    }
}

/// Reads a lowercased CSS colour. `None` for global keywords, which mean
/// different things per property, and for notations the check does not
/// read: system colours, functions other than `rgb()` and `hsl()`, and any
/// value some browser could read differently, such as channels mixing
/// numbers and percentages or the `none` keyword. A browser that drops a
/// declaration keeps the inherited colour, so reading one it drops would
/// judge the wrong colour.
pub(super) fn parse(value: &str) -> Option<Paint> {
    match value {
        "currentcolor" => return Some(Paint::Current),
        "transparent" => return Some(Paint::Color(Color::TRANSPARENT)),
        _ => {}
    }
    if let Some(hex) = value.strip_prefix('#') {
        return hex_color(hex).map(Paint::Color);
    }
    if let Some((name, rest)) = value.split_once('(') {
        return function(name, rest.strip_suffix(')')?).map(Paint::Color);
    }
    named(value).map(Paint::Color)
}

/// Reads a colour attribute such as `bgcolor` or a `font` element's
/// `color` the way the HTML standard's legacy colour parser does, which
/// turns almost any string into some colour: `chucknorris` is a dark red.
/// `None` where the attribute has no effect.
pub(super) fn legacy(value: &str) -> Option<Color> {
    let value = value.trim_matches(|c: char| c.is_ascii_whitespace());
    if value.is_empty() || value.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if let Some(color) = named(&value.to_ascii_lowercase()) {
        return Some(color);
    }
    if let Some(hex) = value.strip_prefix('#')
        && hex.len() == 3
        && let Some(color) = hex_color(hex)
    {
        return Some(color);
    }
    let mut digits: Vec<u8> = value
        .chars()
        .flat_map(|c| {
            if c.len_utf16() > 1 {
                vec![b'0', b'0']
            } else {
                vec![
                    u8::try_from(c)
                        .ok()
                        .filter(u8::is_ascii_hexdigit)
                        .unwrap_or(b'0'),
                ]
            }
        })
        .take(128)
        .collect();
    if value.starts_with('#') {
        digits.remove(0);
    }
    while digits.is_empty() || !digits.len().is_multiple_of(3) {
        digits.push(b'0');
    }
    let mut length = digits.len() / 3;
    let mut components: Vec<&[u8]> = digits.chunks(length).collect();
    if length > 8 {
        components = components.iter().map(|part| &part[length - 8..]).collect();
        length = 8;
    }
    while length > 2 && components.iter().all(|part| part[0] == b'0') {
        components = components.iter().map(|part| &part[1..]).collect();
        length -= 1;
    }
    let channel = |part: &[u8]| -> Option<f64> {
        let digits = std::str::from_utf8(&part[..part.len().min(2)]).ok()?;
        Some(f64::from(u8::from_str_radix(digits, 16).ok()?) / 255.0)
    };
    Some(Color::clamped(
        channel(components[0])?,
        channel(components[1])?,
        channel(components[2])?,
        1.0,
    ))
}

fn hex_color(hex: &str) -> Option<Color> {
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |at: usize| u32::from_str_radix(&hex[at..=at], 16).ok();
    let pair = |at: usize| u32::from_str_radix(&hex[at..at + 2], 16).ok();
    let channels: Vec<u32> = match hex.len() {
        3 | 4 => (0..hex.len())
            .map(|at| digit(at).map(|value| value * 17))
            .collect::<Option<_>>()?,
        6 | 8 => (0..hex.len() / 2)
            .map(|index| pair(index * 2))
            .collect::<Option<_>>()?,
        _ => return None,
    };
    let channel = |index: usize| f64::from(channels.get(index).copied().unwrap_or(255)) / 255.0;
    Some(Color::clamped(
        channel(0),
        channel(1),
        channel(2),
        channel(3),
    ))
}

/// An `rgb()`, `rgba()`, `hsl()` or `hsla()` colour, in the comma syntax
/// or the space syntax with an optional `/ alpha`.
fn function(name: &str, arguments: &str) -> Option<Color> {
    let (channels, alpha): (Vec<&str>, Option<&str>) = if arguments.contains(',') {
        let parts: Vec<&str> = arguments.split(',').map(str::trim).collect();
        if !(3..=4).contains(&parts.len()) {
            return None;
        }
        (parts[..3].to_vec(), parts.get(3).copied())
    } else {
        let (channels, alpha) = match arguments.split_once('/') {
            Some((channels, alpha)) => (channels, Some(alpha.trim())),
            None => (arguments, None),
        };
        (channels.split_ascii_whitespace().collect(), alpha)
    };
    if channels.len() != 3 {
        return None;
    }
    let alpha = match alpha {
        None => 1.0,
        Some(alpha) => match alpha.strip_suffix('%') {
            Some(percent) => number(percent)? / 100.0,
            None => number(alpha)?,
        },
    };
    match name {
        "rgb" | "rgba" => {
            let percent = channels.iter().all(|channel| channel.ends_with('%'));
            let values = channels
                .iter()
                .map(|channel| match channel.strip_suffix('%') {
                    Some(value) if percent => Some(number(value)? / 100.0),
                    None if !percent => Some(number(channel)? / 255.0),
                    _ => None,
                })
                .collect::<Option<Vec<f64>>>()?;
            Some(Color::clamped(values[0], values[1], values[2], alpha))
        }
        "hsl" | "hsla" => {
            let hue = angle(channels[0])?;
            let saturation = (number(channels[1].strip_suffix('%')?)? / 100.0).clamp(0.0, 1.0);
            let lightness = (number(channels[2].strip_suffix('%')?)? / 100.0).clamp(0.0, 1.0);
            let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
            let sector = hue.rem_euclid(360.0) / 60.0;
            let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
            let (red, green, blue) = match sector {
                s if s < 1.0 => (chroma, second, 0.0),
                s if s < 2.0 => (second, chroma, 0.0),
                s if s < 3.0 => (0.0, chroma, second),
                s if s < 4.0 => (0.0, second, chroma),
                s if s < 5.0 => (second, 0.0, chroma),
                _ => (chroma, 0.0, second),
            };
            let base = lightness - chroma / 2.0;
            Some(Color::clamped(red + base, green + base, blue + base, alpha))
        }
        _ => None,
    }
}

/// A hue in degrees: a bare number or an angle in `deg`, `grad`, `rad`
/// or `turn`.
fn angle(token: &str) -> Option<f64> {
    let (number, unit) = super::css::dimension(token)?;
    match unit {
        "" | "deg" => Some(number),
        "grad" => Some(number * 0.9),
        "rad" => Some(number.to_degrees()),
        "turn" => Some(number * 360.0),
        _ => None,
    }
}

fn named(name: &str) -> Option<Color> {
    NAMED_COLORS
        .binary_search_by(|(candidate, _)| candidate.cmp(&name))
        .ok()
        .map(|index| Color::rgb(NAMED_COLORS[index].1))
}

/// The CSS named colours, sorted by name for binary search.
#[rustfmt::skip]
const NAMED_COLORS: [(&str, u32); 148] = [
    ("aliceblue", 0xf0f8ff), ("antiquewhite", 0xfaebd7), ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4), ("azure", 0xf0ffff), ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4), ("black", 0x000000), ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff), ("blueviolet", 0x8a2be2), ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887), ("cadetblue", 0x5f9ea0), ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e), ("coral", 0xff7f50), ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc), ("crimson", 0xdc143c), ("cyan", 0x00ffff),
    ("darkblue", 0x00008b), ("darkcyan", 0x008b8b), ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9), ("darkgreen", 0x006400), ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b), ("darkmagenta", 0x8b008b), ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00), ("darkorchid", 0x9932cc), ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a), ("darkseagreen", 0x8fbc8f), ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f), ("darkslategrey", 0x2f4f4f), ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3), ("deeppink", 0xff1493), ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969), ("dimgrey", 0x696969), ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222), ("floralwhite", 0xfffaf0), ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff), ("gainsboro", 0xdcdcdc), ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700), ("goldenrod", 0xdaa520), ("gray", 0x808080),
    ("green", 0x008000), ("greenyellow", 0xadff2f), ("grey", 0x808080),
    ("honeydew", 0xf0fff0), ("hotpink", 0xff69b4), ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082), ("ivory", 0xfffff0), ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa), ("lavenderblush", 0xfff0f5), ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd), ("lightblue", 0xadd8e6), ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff), ("lightgoldenrodyellow", 0xfafad2), ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90), ("lightgrey", 0xd3d3d3), ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a), ("lightseagreen", 0x20b2aa), ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899), ("lightslategrey", 0x778899), ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0), ("lime", 0x00ff00), ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6), ("magenta", 0xff00ff), ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa), ("mediumblue", 0x0000cd), ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db), ("mediumseagreen", 0x3cb371), ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a), ("mediumturquoise", 0x48d1cc), ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970), ("mintcream", 0xf5fffa), ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5), ("navajowhite", 0xffdead), ("navy", 0x000080),
    ("oldlace", 0xfdf5e6), ("olive", 0x808000), ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500), ("orangered", 0xff4500), ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa), ("palegreen", 0x98fb98), ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093), ("papayawhip", 0xffefd5), ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f), ("pink", 0xffc0cb), ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6), ("purple", 0x800080), ("rebeccapurple", 0x663399),
    ("red", 0xff0000), ("rosybrown", 0xbc8f8f), ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513), ("salmon", 0xfa8072), ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57), ("seashell", 0xfff5ee), ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0), ("skyblue", 0x87ceeb), ("slateblue", 0x6a5acd),
    ("slategray", 0x708090), ("slategrey", 0x708090), ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f), ("steelblue", 0x4682b4), ("tan", 0xd2b48c),
    ("teal", 0x008080), ("thistle", 0xd8bfd8), ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0), ("violet", 0xee82ee), ("wheat", 0xf5deb3),
    ("white", 0xffffff), ("whitesmoke", 0xf5f5f5), ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;
    include!("../../tests/html_color_tests.rs");
}
