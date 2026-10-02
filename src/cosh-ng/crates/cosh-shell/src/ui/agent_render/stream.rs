use std::io::{self, Write};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Widget, Wrap},
};

use super::buffer_to_styled_lines;
use super::card::StreamingCardFrame;
use super::markdown::{is_table_row, is_table_separator_row, MarkdownRenderModel};
use super::wrap::{
    char_width, display_width, is_cjk_breakable, is_line_closing_punctuation, line_to_string,
    should_buffer_word_char, strip_ansi_escape, wrap_plain_line,
};
use super::RatatuiInlineRenderer;
use crate::types::CardKind;

pub struct StreamingAgentBlock {
    width: usize,
    plain: bool,
    title: String,
    current_width: usize,
    current_line: String,
    started: bool,
    seen_text: bool,
    line_has_visible: bool,
    pending_word: String,
    pending_cjk_group: String,
    pending_star: bool,
    pending_backticks: usize,
    skip_until_newline: bool,
}

pub struct MarkdownStreamBlock {
    renderer: RatatuiInlineRenderer,
    pending: String,
    started: bool,
}

impl StreamingAgentBlock {
    pub(super) fn new(width: usize, plain: bool, title: &str) -> Self {
        Self {
            width,
            plain,
            title: title.to_string(),
            current_width: 0,
            current_line: String::new(),
            started: false,
            seen_text: false,
            line_has_visible: false,
            pending_word: String::new(),
            pending_cjk_group: String::new(),
            pending_star: false,
            pending_backticks: 0,
            skip_until_newline: false,
        }
    }

    pub fn write_delta<W: Write>(&mut self, output: &mut W, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        let clean_text = strip_ansi_escape(text);
        for ch in clean_text.chars() {
            if self.skip_until_newline {
                if ch == '\n' {
                    self.skip_until_newline = false;
                    self.line_has_visible = false;
                    self.current_width = 0;
                    self.current_line.clear();
                }
                continue;
            }

            if ch == '`' {
                self.pending_backticks += 1;
                continue;
            }

            if self.pending_backticks > 0 && self.flush_backticks(ch)? {
                continue;
            }

            if ch == '*' {
                self.pending_star = !self.pending_star;
                continue;
            }

            if self.pending_star {
                self.write_text_char(output, '*')?;
                self.pending_star = false;
            }

            self.write_text_char(output, ch)?;
        }

        Ok(())
    }

    pub fn finish<W: Write>(&mut self, output: &mut W, footer: Option<&str>) -> io::Result<bool> {
        if self.pending_star {
            self.write_text_char(output, '*')?;
            self.pending_star = false;
        }
        self.flush_pending_word(output)?;
        self.flush_pending_cjk_group(output)?;
        self.pending_backticks = 0;

        if !self.started {
            return Ok(false);
        }

        if self.plain {
            writeln!(output)?;
        } else {
            self.finish_rich_line(output)?;
            writeln!(output)?;
        }
        if let Some(footer) = footer {
            for line in wrap_plain_line(footer, self.width) {
                if self.plain {
                    writeln!(output, "  {line}")?;
                } else {
                    writeln!(output, "{}", self.frame().line(&line))?;
                }
            }
        }
        if !self.plain {
            writeln!(output, "{}", self.frame().bottom())?;
        }
        self.started = false;
        self.seen_text = false;
        self.line_has_visible = false;
        self.current_width = 0;
        self.current_line.clear();
        self.pending_cjk_group.clear();
        Ok(true)
    }

    fn write_text_char<W: Write>(&mut self, output: &mut W, ch: char) -> io::Result<()> {
        if ch == '\r' {
            return Ok(());
        }

        if ch == '\n' || ch.is_whitespace() {
            self.flush_pending_word(output)?;
            return self.write_visible_char(output, ch);
        }

        if should_buffer_word_char(ch) {
            if !self.pending_cjk_group.is_empty() {
                if is_line_closing_punctuation(ch) {
                    return self.write_visible_char(output, ch);
                }
                self.flush_pending_cjk_group(output)?;
            }
            self.pending_word.push(ch);
            return Ok(());
        }

        self.flush_pending_word(output)?;
        self.write_visible_char(output, ch)
    }

    fn flush_pending_word<W: Write>(&mut self, output: &mut W) -> io::Result<()> {
        if self.pending_word.is_empty() {
            return Ok(());
        }

        let word = std::mem::take(&mut self.pending_word);
        let word_width = display_width(&word);
        if word_width <= self.width
            && self.current_width > 0
            && self.current_width + word_width > self.width
        {
            self.write_line_break(output)?;
        }

        for ch in word.chars() {
            self.write_visible_char(output, ch)?;
        }
        Ok(())
    }

    fn write_line_break<W: Write>(&mut self, output: &mut W) -> io::Result<()> {
        if self.plain {
            writeln!(output)?;
            write!(output, "  ")?;
        } else {
            self.finish_rich_line(output)?;
            writeln!(output)?;
            write!(output, "│ ")?;
        }
        self.current_width = 0;
        self.current_line.clear();
        self.line_has_visible = false;
        Ok(())
    }

    fn finish_rich_line<W: Write>(&self, output: &mut W) -> io::Result<()> {
        write!(
            output,
            "{}",
            self.frame().finish_partial_line(self.current_width)
        )
    }

    fn flush_backticks(&mut self, next_ch: char) -> io::Result<bool> {
        let count = self.pending_backticks;
        self.pending_backticks = 0;

        if count >= 3 && !self.line_has_visible {
            if next_ch == '\n' {
                self.line_has_visible = false;
                self.current_width = 0;
            } else {
                self.skip_until_newline = true;
            }
            return Ok(true);
        }

        Ok(false)
    }

    fn write_visible_char<W: Write>(&mut self, output: &mut W, ch: char) -> io::Result<()> {
        if !self.pending_cjk_group.is_empty() {
            if self.pending_unit_continues_with(ch) {
                self.pending_cjk_group.push(ch);
                return Ok(());
            }

            if is_line_closing_punctuation(ch) {
                self.pending_cjk_group.push(ch);
                return Ok(());
            }
            self.flush_pending_cjk_group(output)?;
        }

        if !self.started && ch.is_whitespace() {
            return Ok(());
        }

        if self.started && !self.line_has_visible && ch != '\n' && ch.is_whitespace() {
            return Ok(());
        }

        if !self.started {
            writeln!(output)?;
            if self.plain {
                writeln!(output, "{}:", self.title)?;
                write!(output, "  ")?;
            } else {
                writeln!(output, "{}", self.frame().top(&self.title))?;
                write!(output, "│ ")?;
            }
            self.started = true;
        }

        if ch == '\n' {
            return self.write_line_break(output);
        }

        if is_cjk_breakable(ch) && !is_line_closing_punctuation(ch) {
            self.pending_cjk_group.push(ch);
            self.seen_text = true;
            self.line_has_visible = true;
            return Ok(());
        }

        let next_width = self.current_line_width_with(ch);
        if next_width > 0 && self.current_width > 0 && next_width > self.width {
            self.write_line_break(output)?;
        }

        write!(output, "{ch}")?;
        self.current_line.push(ch);
        self.current_width = display_width(&self.current_line);
        if !ch.is_whitespace() {
            self.seen_text = true;
            self.line_has_visible = true;
        }
        Ok(())
    }

    fn pending_unit_continues_with(&self, ch: char) -> bool {
        if ch.is_whitespace() || ch.is_control() {
            return false;
        }
        if char_width(ch) == 0 {
            return true;
        }

        let pending_width = display_width(&self.pending_cjk_group);
        let mut combined = self.pending_cjk_group.clone();
        combined.push(ch);
        display_width(&combined) <= pending_width
    }

    fn flush_pending_cjk_group<W: Write>(&mut self, output: &mut W) -> io::Result<()> {
        if self.pending_cjk_group.is_empty() {
            return Ok(());
        }

        let pending = std::mem::take(&mut self.pending_cjk_group);
        let pending_width = display_width(&pending);
        if pending_width <= self.width {
            if self.current_width > 0 && self.current_width + pending_width > self.width {
                self.write_line_break(output)?;
            }
            return self.write_buffered_text(output, &pending);
        }

        if self.current_width > 0 {
            self.write_line_break(output)?;
        }

        for ch in pending.chars() {
            if self.current_width > 0 && self.current_line_width_with(ch) > self.width {
                self.write_line_break(output)?;
            }
            let mut encoded = [0; 4];
            self.write_buffered_text(output, ch.encode_utf8(&mut encoded))?;
        }
        Ok(())
    }

    fn write_buffered_text<W: Write>(&mut self, output: &mut W, text: &str) -> io::Result<()> {
        write!(output, "{text}")?;
        self.current_line.push_str(text);
        self.current_width = display_width(&self.current_line);
        if text.chars().any(|ch| !ch.is_whitespace()) {
            self.seen_text = true;
            self.line_has_visible = true;
        }
        Ok(())
    }

    fn current_line_width_with(&self, ch: char) -> usize {
        let mut line = self.current_line.clone();
        line.push(ch);
        display_width(&line)
    }

    fn frame(&self) -> StreamingCardFrame {
        StreamingCardFrame::new(self.width)
    }
}

impl MarkdownStreamBlock {
    pub(super) fn new(renderer: RatatuiInlineRenderer) -> Self {
        Self {
            renderer,
            pending: String::new(),
            started: false,
        }
    }

    pub fn write_delta<W: Write>(&mut self, output: &mut W, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        self.pending.push_str(&strip_ansi_escape(text));
        while let Some(split_at) = stable_markdown_split(&self.pending) {
            let stable = self.pending[..split_at].to_string();
            self.pending.replace_range(..split_at, "");
            self.write_markdown_fragment(output, &stable)?;
        }
        Ok(())
    }

    pub fn finish<W: Write>(&mut self, output: &mut W, footer: Option<&str>) -> io::Result<bool> {
        if !self.pending.trim().is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.write_markdown_fragment(output, &pending)?;
        }

        if !self.started {
            return Ok(false);
        }

        if let Some(footer) = footer {
            self.write_markdown_fragment(output, footer)?;
        }
        if !self.renderer.plain {
            writeln!(output, "{}", self.frame().bottom())?;
        }
        self.started = false;
        Ok(true)
    }

    pub fn has_started(&self) -> bool {
        self.started
    }

    pub fn has_buffered_text(&self) -> bool {
        !self.pending.trim().is_empty()
    }

    fn write_markdown_fragment<W: Write>(&mut self, output: &mut W, text: &str) -> io::Result<()> {
        if self.renderer.styled && !self.renderer.plain {
            return self.write_styled_markdown_fragment(output, text);
        }

        let lines = self.renderer.markdown_text_lines(text);
        if lines.is_empty() || lines.iter().all(|line| line.trim().is_empty()) {
            return Ok(());
        }

        if !self.started {
            if self.renderer.plain {
                writeln!(
                    output,
                    "{}:",
                    CardKind::AgentResponse
                        .title(self.renderer.i18n().t(crate::MessageId::AgentResponseTitle))
                )?;
            } else {
                writeln!(
                    output,
                    "{}",
                    self.frame().top(
                        &CardKind::AgentResponse
                            .title(self.renderer.i18n().t(crate::MessageId::AgentResponseTitle)),
                    )
                )?;
            }
            self.started = true;
        }

        for line in lines {
            write_card_line(
                output,
                self.renderer.plain,
                self.renderer.content_width(),
                &line,
            )?;
        }
        Ok(())
    }

    fn write_styled_markdown_fragment<W: Write>(
        &mut self,
        output: &mut W,
        text: &str,
    ) -> io::Result<()> {
        let lines = MarkdownRenderModel::parse_with_language(
            text,
            self.renderer.content_width(),
            self.renderer.language,
        )
        .styled_lines();
        if lines.is_empty()
            || lines
                .iter()
                .all(|line| line_to_string(line).trim().is_empty())
        {
            return Ok(());
        }

        if !self.started {
            writeln!(
                output,
                "{}",
                self.frame().top(
                    &CardKind::AgentResponse
                        .title(self.renderer.i18n().t(crate::MessageId::AgentResponseTitle)),
                )
            )?;
            self.started = true;
        }

        for line in lines {
            write_styled_card_line(output, self.renderer.content_width(), line)?;
        }
        Ok(())
    }

    fn frame(&self) -> StreamingCardFrame {
        StreamingCardFrame::new(self.renderer.content_width())
    }
}

fn write_card_line<W: Write>(
    output: &mut W,
    plain: bool,
    content_width: usize,
    line: &str,
) -> io::Result<()> {
    if plain {
        if line.trim().is_empty() {
            writeln!(output)
        } else {
            writeln!(output, "  {line}")
        }
    } else if line.trim().is_empty() {
        writeln!(
            output,
            "{}",
            StreamingCardFrame::new(content_width).line("")
        )
    } else {
        writeln!(
            output,
            "{}",
            StreamingCardFrame::new(content_width).line(line)
        )
    }
}

fn write_styled_card_line<W: Write>(
    output: &mut W,
    content_width: usize,
    line: Line<'static>,
) -> io::Result<()> {
    for row in styled_line_rows(content_width, line) {
        writeln!(output, "{}", styled_frame_line(content_width, &row))?;
    }
    Ok(())
}

fn styled_line_rows(content_width: usize, line: Line<'static>) -> Vec<String> {
    let width = content_width.max(1);
    let plain = line_to_string(&line);
    let height = display_width(&plain).max(1).div_ceil(width).max(1);
    let area = Rect::new(0, 0, width as u16, height as u16);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(line)
        .wrap(Wrap { trim: true })
        .render(area, &mut buffer);
    buffer_to_styled_lines(&buffer, area)
}

fn styled_frame_line(content_width: usize, content: &str) -> String {
    if content.trim().is_empty() {
        return StreamingCardFrame::new(content_width).line("");
    }

    let visible_width = display_width(&strip_ansi_escape(content));
    let padding = " ".repeat(content_width.saturating_sub(visible_width));
    format!("│ {content}{padding} │")
}

fn stable_markdown_split(text: &str) -> Option<usize> {
    if text.trim().is_empty() {
        return None;
    }

    if markdown_fence_is_open(text) {
        return None;
    }

    if let Some(idx) = text.rfind("\n\n") {
        return Some(idx + 2);
    }

    if table_block_is_open(text) {
        return None;
    }

    if let Some(idx) = stable_complete_markdown_line_split(text) {
        return Some(idx);
    }

    sentence_boundary(text)
}

fn stable_complete_markdown_line_split(text: &str) -> Option<usize> {
    let split_at = text.rfind('\n')? + 1;
    let line = text[..split_at].lines().last()?.trim_end();
    if complete_markdown_line_can_flush(line) {
        Some(split_at)
    } else {
        None
    }
}

fn complete_markdown_line_can_flush(line: &str) -> bool {
    let trimmed = line.trim_start();
    heading_line(trimmed)
        || block_quote_line(trimmed)
        || list_item_line(trimmed)
        || indented_code_line(line)
}

fn markdown_fence_is_open(text: &str) -> bool {
    text.lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
        % 2
        == 1
}

fn table_block_is_open(text: &str) -> bool {
    let tail = text
        .rsplit_once("\n\n")
        .map(|(_, tail)| tail)
        .unwrap_or(text);
    let rows = tail
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return false;
    }

    for line in &rows {
        if !is_table_row(line) {
            return false;
        }
    }

    match rows.len() {
        1 => true,
        _ => is_table_separator_row(rows[1]),
    }
}

fn heading_line(line: &str) -> bool {
    let level = line.chars().take_while(|ch| *ch == '#').count();
    level > 0 && level <= 6 && line[level..].starts_with(' ')
}

fn block_quote_line(line: &str) -> bool {
    line.starts_with('>')
}

fn list_item_line(line: &str) -> bool {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .is_some()
        || ordered_list_item_line(line)
}

fn ordered_list_item_line(line: &str) -> bool {
    let Some((digits, rest)) = line.split_once('.') else {
        return false;
    };
    !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit()) && rest.starts_with(' ')
}

fn indented_code_line(line: &str) -> bool {
    line.starts_with("    ") || line.starts_with('\t')
}

fn sentence_boundary(text: &str) -> Option<usize> {
    let mut last_boundary = None;
    for (idx, ch) in text.char_indices() {
        if matches!(ch, '。' | '！' | '？') {
            last_boundary = Some(idx + ch.len_utf8());
        }
    }

    let boundary = last_boundary?;
    if boundary < 24 {
        return None;
    }
    if text[boundary..].chars().count() > 16 {
        return None;
    }
    Some(boundary)
}
