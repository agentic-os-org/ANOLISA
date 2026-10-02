use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

#[path = "markdown/blocks.rs"]
mod blocks;
#[path = "markdown/code.rs"]
mod code;
#[path = "markdown/highlight.rs"]
mod highlight;
#[path = "markdown/inline.rs"]
mod inline;
#[path = "markdown/list.rs"]
mod list;
#[path = "markdown/table.rs"]
mod table;

use super::wrap::{compact_rendered_lines, line_is_empty, ordered_list_item, wrap_plain_line};
use blocks::{
    render_plain_heading, render_plain_quote, render_ratatui_heading, render_ratatui_quote,
};
use code::{render_plain_code_block, render_ratatui_code_block, styled_code_block_lines};
use inline::{clean_inline_markdown, inline_text, styled_inline_spans, InlineText};
use list::{render_plain_list_item, render_ratatui_list_item};
use table::{render_plain_markdown_table, render_ratatui_markdown_table};

#[derive(Debug, Clone)]
pub(super) struct MarkdownRenderModel {
    lines: Vec<MarkdownLine>,
    width: usize,
    language: crate::Language,
}

impl MarkdownRenderModel {
    #[cfg(test)]
    pub(super) fn parse(text: &str, width: usize) -> Self {
        Self::parse_with_language(text, width, crate::Language::EnUs)
    }

    pub(super) fn parse_with_language(text: &str, width: usize, language: crate::Language) -> Self {
        Self {
            lines: lines_from_markdown(text),
            width,
            language,
        }
    }

    pub(super) fn rich_text_lines(&self) -> Vec<String> {
        render_markdown_lines(
            self.lines.clone(),
            self.width,
            self.language,
            MarkdownRenderMode::Rich,
        )
    }

    pub(super) fn plain_text_lines(&self) -> Vec<String> {
        render_markdown_lines(
            self.lines.clone(),
            self.width,
            self.language,
            MarkdownRenderMode::Plain,
        )
    }

    pub(super) fn styled_lines(&self) -> Vec<Line<'static>> {
        let i18n = crate::I18n::new(self.language);
        let mut rendered = self
            .lines
            .clone()
            .into_iter()
            .flat_map(|line| styled_lines_from_markdown_line(&i18n, line, self.width))
            .collect::<Vec<_>>();
        rendered = compact_styled_lines(rendered);
        while rendered.last().is_some_and(line_is_empty) {
            rendered.pop();
        }
        rendered
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkdownRenderMode {
    Rich,
    Plain,
}

fn render_markdown_lines(
    lines: Vec<MarkdownLine>,
    width: usize,
    language: crate::Language,
    mode: MarkdownRenderMode,
) -> Vec<String> {
    let i18n = crate::I18n::new(language);
    let mut rendered = lines
        .into_iter()
        .flat_map(|line| match line {
            MarkdownLine::Heading { level, text } => match mode {
                MarkdownRenderMode::Rich => render_ratatui_heading(level, &text, width),
                MarkdownRenderMode::Plain => render_plain_heading(level, &text, width),
            },
            MarkdownLine::Text(text) => wrap_plain_line(&text.plain, width),
            MarkdownLine::Quote(text) => match mode {
                MarkdownRenderMode::Rich => render_ratatui_quote(&text.plain, width),
                MarkdownRenderMode::Plain => render_plain_quote(&text.plain, width),
            },
            MarkdownLine::ListItem {
                indent,
                marker,
                text,
            } => match mode {
                MarkdownRenderMode::Rich => {
                    render_ratatui_list_item(&indent, &marker, &text.plain, width)
                }
                MarkdownRenderMode::Plain => {
                    render_plain_list_item(&indent, &marker, &text.plain, width)
                }
            },
            MarkdownLine::Code { language, lines } => match mode {
                MarkdownRenderMode::Rich => {
                    render_ratatui_code_block(&i18n, &language, &lines, width)
                }
                MarkdownRenderMode::Plain => {
                    render_plain_code_block(&i18n, &language, &lines, width)
                }
            },
            MarkdownLine::Table(rows) => match mode {
                MarkdownRenderMode::Rich => render_ratatui_markdown_table(&i18n, &rows, width),
                MarkdownRenderMode::Plain => render_plain_markdown_table(&rows, width),
            },
        })
        .collect::<Vec<_>>();
    rendered = compact_rendered_lines(rendered);
    while rendered.last().is_some_and(|line| line.trim().is_empty()) {
        rendered.pop();
    }
    rendered
}

#[derive(Debug, Clone)]
enum MarkdownLine {
    Heading {
        level: usize,
        text: String,
    },
    Text(InlineText),
    Quote(InlineText),
    ListItem {
        indent: String,
        marker: String,
        text: InlineText,
    },
    Code {
        language: String,
        lines: Vec<String>,
    },
    Table(Vec<Vec<String>>),
}

fn lines_from_markdown(text: &str) -> Vec<MarkdownLine> {
    let mut lines = Vec::new();
    let raw_lines = text.trim().lines().collect::<Vec<_>>();
    let mut idx = 0;

    while idx < raw_lines.len() {
        let raw = raw_lines[idx];
        let line = raw.trim_end();
        if let Some(language) = code_fence_language(line) {
            if !lines.last().is_none_or(markdown_line_is_empty) {
                lines.push(MarkdownLine::Text(inline_text("")));
            }
            let mut code_lines = Vec::new();
            idx += 1;
            while idx < raw_lines.len() && code_fence_language(raw_lines[idx]).is_none() {
                code_lines.push(raw_lines[idx].trim_end().to_string());
                idx += 1;
            }
            if idx < raw_lines.len() {
                idx += 1;
            }
            lines.push(MarkdownLine::Code {
                language,
                lines: code_lines,
            });
            continue;
        }

        if line.trim().is_empty() {
            lines.push(MarkdownLine::Text(inline_text("")));
            idx += 1;
            continue;
        }

        if is_indented_code_line(line) {
            if !lines.last().is_none_or(markdown_line_is_empty) {
                lines.push(MarkdownLine::Text(inline_text("")));
            }
            let mut code_lines = Vec::new();
            while idx < raw_lines.len() {
                let code_line = raw_lines[idx].trim_end();
                if code_line.trim().is_empty() {
                    if raw_lines
                        .get(idx + 1)
                        .is_some_and(|next| is_indented_code_line(next))
                    {
                        code_lines.push(String::new());
                        idx += 1;
                        continue;
                    }
                    break;
                }
                if !is_indented_code_line(code_line) {
                    break;
                }
                code_lines.push(strip_indented_code_prefix(code_line).to_string());
                idx += 1;
            }
            lines.push(MarkdownLine::Code {
                language: String::new(),
                lines: code_lines,
            });
            continue;
        }

        if is_table_row(line) {
            if let Some((rows, next_idx)) = markdown_table_at(&raw_lines, idx) {
                lines.push(MarkdownLine::Table(rows));
                idx = next_idx;
                continue;
            }

            while idx < raw_lines.len() && is_table_row(raw_lines[idx]) {
                lines.push(MarkdownLine::Text(inline_text(raw_lines[idx].trim_end())));
                idx += 1;
            }
            continue;
        } else if let Some(item) = block_quote_item(line) {
            lines.push(MarkdownLine::Quote(inline_text(&item)));
        } else if let Some((indent, marker, item)) = list_item(line) {
            lines.push(MarkdownLine::ListItem {
                indent: indent.to_string(),
                marker: marker.to_string(),
                text: inline_text(item),
            });
        } else if let Some((level, heading)) = heading(line) {
            lines.push(MarkdownLine::Heading {
                level,
                text: clean_inline_markdown(heading),
            });
        } else {
            let mut paragraph = Vec::new();
            while idx < raw_lines.len() {
                let paragraph_line = raw_lines[idx].trim_end();
                if paragraph_line.trim().is_empty() || starts_markdown_block(paragraph_line) {
                    break;
                }
                paragraph.push(paragraph_line.trim_start().to_string());
                idx += 1;
            }
            lines.push(MarkdownLine::Text(inline_text(&paragraph.join(" "))));
            continue;
        }
        idx += 1;
    }

    while lines.first().is_some_and(markdown_line_is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(markdown_line_is_empty) {
        lines.pop();
    }

    if lines.is_empty() {
        vec![MarkdownLine::Text(inline_text(""))]
    } else {
        lines
    }
}

pub(super) fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

pub(super) fn is_table_separator_row(line: &str) -> bool {
    if !is_table_row(line) {
        return false;
    }

    split_table_cells(line.trim().trim_matches('|'))
        .into_iter()
        .all(|cell| is_table_separator_cell(cell.trim()))
}

fn markdown_table_at(raw_lines: &[&str], start: usize) -> Option<(Vec<Vec<String>>, usize)> {
    if !is_table_row(raw_lines.get(start)?) || !is_table_separator_row(raw_lines.get(start + 1)?) {
        return None;
    }

    let mut idx = start;
    let mut rows = Vec::new();
    while idx < raw_lines.len() && is_table_row(raw_lines[idx]) {
        rows.push(table_cells(raw_lines[idx]));
        idx += 1;
    }
    Some((rows, idx))
}

fn is_table_separator_cell(cell: &str) -> bool {
    let cell = cell.trim();
    let cell = cell.strip_prefix(':').unwrap_or(cell);
    let cell = cell.strip_suffix(':').unwrap_or(cell);
    cell.len() >= 3 && cell.chars().all(|ch| ch == '-')
}

fn table_cells(line: &str) -> Vec<String> {
    split_table_cells(line.trim().trim_matches('|'))
        .into_iter()
        .map(|cell| clean_inline_markdown(cell.trim()))
        .collect()
}

fn split_table_cells(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '|' {
            cells.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    if escaped {
        current.push('\\');
    }
    cells.push(current);
    cells
}

fn markdown_line_is_empty(line: &MarkdownLine) -> bool {
    match line {
        MarkdownLine::Heading { text, .. } => text.trim().is_empty(),
        MarkdownLine::Text(text) => text.plain.trim().is_empty(),
        MarkdownLine::Quote(text) => text.plain.trim().is_empty(),
        MarkdownLine::ListItem { text, .. } => text.plain.trim().is_empty(),
        MarkdownLine::Code { lines, .. } => lines.is_empty(),
        MarkdownLine::Table(rows) => rows.is_empty(),
    }
}

fn styled_lines_from_markdown_line(
    i18n: &crate::I18n,
    line: MarkdownLine,
    width: usize,
) -> Vec<Line<'static>> {
    match line {
        MarkdownLine::Heading { level, text } => render_ratatui_heading(level, &text, width)
            .into_iter()
            .map(Line::from)
            .collect(),
        MarkdownLine::Text(text) => vec![Line::from(styled_inline_spans(&text))],
        MarkdownLine::Quote(text) => render_ratatui_quote(&text.plain, width)
            .into_iter()
            .map(Line::from)
            .collect(),
        MarkdownLine::ListItem {
            indent,
            marker,
            text,
        } => vec![styled_list_line(&indent, &marker, &text)],
        MarkdownLine::Code { language, lines } => {
            styled_code_block_lines(i18n, &language, &lines, width)
        }
        MarkdownLine::Table(rows) => render_ratatui_markdown_table(i18n, &rows, width)
            .into_iter()
            .map(Line::from)
            .collect(),
    }
}

fn styled_list_line(indent: &str, marker: &str, text: &InlineText) -> Line<'static> {
    let rich_marker = if marker.ends_with(". ") {
        marker.to_string()
    } else if indent.is_empty() {
        "• ".to_string()
    } else {
        "◦ ".to_string()
    };
    let mut spans = vec![Span::styled(
        format!("{indent}{rich_marker}"),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    spans.extend(styled_inline_spans(text));
    Line::from(spans)
}

fn compact_styled_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .filter(|line| !line_is_empty(line))
        .collect()
}

fn list_item(line: &str) -> Option<(&str, &str, &str)> {
    let indent_len = line
        .char_indices()
        .take_while(|(_, ch)| *ch == ' ')
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()
        .unwrap_or(0);
    let indent = &line[..indent_len];
    let rest = &line[indent_len..];
    if let Some(item) = rest.strip_prefix("- ").or_else(|| rest.strip_prefix("* ")) {
        return Some((indent, "- ", item));
    }
    let (marker, item) = ordered_list_item(rest)?;
    Some((indent, marker, item))
}

fn block_quote_item(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix('>')
        .map(|rest| rest.trim_start().to_string())
}

fn starts_markdown_block(line: &str) -> bool {
    code_fence_language(line).is_some()
        || is_indented_code_line(line)
        || is_table_row(line)
        || block_quote_item(line).is_some()
        || list_item(line).is_some()
        || heading(line).is_some()
}

fn is_indented_code_line(line: &str) -> bool {
    line.starts_with("    ") || line.starts_with('\t')
}

fn strip_indented_code_prefix(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('\t') {
        return rest;
    }

    line.strip_prefix("    ").unwrap_or(line)
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let heading = trimmed[level..].trim_start();
    if heading.is_empty() {
        None
    } else {
        Some((level, heading))
    }
}

fn code_fence_language(line: &str) -> Option<String> {
    line.trim_start()
        .strip_prefix("```")
        .map(|language| language.trim().to_string())
}
