//! Shared style tokens for grouped reference panels (`/help`, `/hooks`, ...).
//!
//! These tokens are the team's *default* reference-panel visual convention
//! (originally inspired by upstream ratatui examples, which are not a
//! stability contract): bold+underlined section headers, cyan emphasis for
//! the primary token, dark-gray de-emphasis for metadata, gray body text.
//! Panels adopt them for consistency by default; a panel with genuinely
//! different semantics may override or refine individual tokens rather
//! than being constrained by this module.

use ratatui::style::{Color, Modifier, Style};

/// Top-level section headers (e.g. `Config`, `Agent Hooks`).
pub(super) fn reference_section_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

/// Second-level group headers (e.g. hook event names).
pub(super) fn reference_group_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// The primary token of an entry (e.g. command name, enabled marker).
pub(super) fn reference_emphasis_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// De-emphasized metadata (arguments, scope tags, extension names).
pub(super) fn reference_muted_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Regular body/summary text.
pub(super) fn reference_body_style() -> Style {
    Style::default().fg(Color::Gray)
}
