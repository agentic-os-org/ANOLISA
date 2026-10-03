//! Small ANSI color helper for human CLI output.
//!
//! JSON output must stay machine-stable, so handlers only use this module
//! on their human rendering path. `--no-color` disables styling by returning
//! the raw string unchanged.

use std::fmt::Display;

use console::{Style, measure_text_width};

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    enabled: bool,
}

impl Palette {
    /// Create a palette, disabling ANSI styling when `no_color` is set.
    pub fn new(no_color: bool) -> Self {
        Self { enabled: !no_color }
    }

    /// Style table headers.
    pub fn header(&self, text: impl Display) -> String {
        self.paint(text, Style::new().bold().cyan())
    }

    /// Style field labels.
    pub fn label(&self, text: impl Display) -> String {
        self.paint(text, Style::new().cyan().bold())
    }

    /// Style successful values.
    pub fn ok(&self, text: impl Display) -> String {
        self.paint(text, Style::new().green().bold())
    }

    /// Style warning values.
    pub fn warn(&self, text: impl Display) -> String {
        self.paint(text, Style::new().yellow().bold())
    }

    /// Style error values.
    pub fn err(&self, text: impl Display) -> String {
        self.paint(text, Style::new().red().bold())
    }

    /// Style muted secondary text.
    pub fn muted(&self, text: impl Display) -> String {
        self.paint(text, Style::new().dim())
    }

    /// Style filesystem paths and URLs.
    pub fn path(&self, text: impl Display) -> String {
        self.paint(text, Style::new().cyan())
    }

    /// Style identifiers such as operation IDs.
    pub fn id(&self, text: impl Display) -> String {
        self.paint(text, Style::new().magenta())
    }

    /// Style command names.
    pub fn command(&self, text: impl Display) -> String {
        self.paint(text, Style::new().bold())
    }

    /// Style status-like values by meaning.
    pub fn status(&self, text: impl Display) -> String {
        let raw = text.to_string();
        let key = raw.trim().to_ascii_lowercase();
        match key.as_str() {
            "adopted" | "installed" | "ok" | "ready" | "succeeded" | "true" => self.ok(raw),
            "degraded" | "disabled" | "partial" | "warn" | "warning" => self.warn(raw),
            "blocked" | "error" | "fail" | "failed" | "false" | "referent_mismatch" => {
                self.err(raw)
            }
            "-" | "not_installed" | "unknown" => self.muted(raw),
            _ => self.paint(raw, Style::new().cyan()),
        }
    }

    /// Style log severity values by severity.
    pub fn severity(&self, text: impl Display) -> String {
        let raw = text.to_string();
        let key = raw.trim().to_ascii_lowercase();
        match key.as_str() {
            "debug" => self.muted(raw),
            "info" => self.paint(raw, Style::new().blue()),
            "warn" => self.warn(raw),
            "error" => self.err(raw),
            _ => raw,
        }
    }

    /// Style boolean values using the status palette.
    pub fn bool_value(&self, text: impl Display) -> String {
        self.status(text)
    }

    fn paint(&self, text: impl Display, style: Style) -> String {
        let text = text.to_string();
        if self.enabled {
            style.force_styling(true).apply_to(text).to_string()
        } else {
            text
        }
    }
}

/// Pad a string on the right to `width` terminal display columns.
pub fn pad_right(text: impl AsRef<str>, width: usize) -> String {
    let text = text.as_ref();
    let len = measure_text_width(text);
    if len >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - len))
    }
}

#[cfg(test)]
mod tests {
    use super::pad_right;

    #[test]
    fn pad_right_counts_cjk_display_width() {
        let padded = pad_right("状态", 6);
        assert_eq!(console::measure_text_width(&padded), 6);
    }
}
