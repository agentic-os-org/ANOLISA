use super::wrap::display_width;

pub(super) struct StreamingCardFrame {
    content_width: usize,
}

impl StreamingCardFrame {
    pub(super) fn new(content_width: usize) -> Self {
        Self { content_width }
    }

    pub(super) fn top(&self, title: &str) -> String {
        let label = format!(" {title} ");
        let width = self.content_width + 4;
        let dash_count = width.saturating_sub(display_width(&label) + 2);
        format!("╭{label}{}╮", "─".repeat(dash_count))
    }

    pub(super) fn bottom(&self) -> String {
        format!("╰{}╯", "─".repeat(self.content_width + 2))
    }

    pub(super) fn line(&self, line: &str) -> String {
        let padding = self.content_width.saturating_sub(display_width(line));
        format!("│ {line}{} │", " ".repeat(padding))
    }

    pub(super) fn finish_partial_line(&self, current_width: usize) -> String {
        let padding = self.content_width.saturating_sub(current_width);
        format!("{} │", " ".repeat(padding))
    }
}
