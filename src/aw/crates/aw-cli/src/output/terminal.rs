//! Render management metadata as text without interpreting terminal controls.

use super::Color;

pub(super) fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control()
            || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

pub(super) fn use_color(choice: Option<Color>, terminal: bool) -> bool {
    color_policy(
        choice,
        terminal,
        std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
        std::env::var_os("TERM").is_some_and(|value| value == "dumb"),
    )
}

fn color_policy(choice: Option<Color>, terminal: bool, no_color: bool, dumb: bool) -> bool {
    match choice.unwrap_or(Color::Auto) {
        Color::Always => true,
        Color::Never => false,
        Color::Auto => terminal && !no_color && !dumb,
    }
}

pub(super) fn styled(value: &str, enabled: bool, failure: bool) -> String {
    if enabled {
        let code = if failure { "31" } else { "32" };
        format!("\u{1b}[{code}m{value}\u{1b}[0m")
    } else {
        value.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_controls_and_direction_overrides_are_visible_text() {
        assert_eq!(
            escape("ok\n\r\t\u{1b}[2J\0\u{202e}\u{2066}中文"),
            "ok\\n\\r\\t\\u{1b}[2J\\u{0}\\u{202e}\\u{2066}中文"
        );
        for character in [
            '\u{007f}', '\u{0085}', '\u{061c}', '\u{200e}', '\u{200f}', '\u{2028}', '\u{2029}',
            '\u{2069}',
        ] {
            assert!(!escape(&character.to_string()).contains(character));
        }
    }

    #[test]
    fn colors_require_a_terminal_unless_explicitly_requested() {
        for terminal in [false, true] {
            for no_color in [false, true] {
                for dumb in [false, true] {
                    assert_eq!(
                        color_policy(None, terminal, no_color, dumb),
                        terminal && !no_color && !dumb
                    );
                    assert!(!color_policy(Some(Color::Never), terminal, no_color, dumb));
                    assert!(color_policy(Some(Color::Always), terminal, no_color, dumb));
                }
            }
        }
    }
}
