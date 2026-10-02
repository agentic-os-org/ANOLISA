//! Compatibility exports for terminal UI wrapping helpers.

pub(super) use crate::ui::wrap::{
    char_width, compact_rendered_lines, is_cjk_breakable, is_line_closing_punctuation,
    line_is_empty, line_to_string, ordered_list_item, should_buffer_word_char, strip_ansi_escape,
};
pub(crate) use crate::ui::wrap::{display_width, wrap_plain_line, wrap_plain_line_with_prefix};
