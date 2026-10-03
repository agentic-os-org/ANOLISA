use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Cell, Row, Table, Widget},
};

use super::super::buffer_to_lines;
use super::super::wrap::{char_width, display_width};

pub(super) fn render_ratatui_markdown_table(
    i18n: &crate::I18n,
    rows: &[Vec<String>],
    width: usize,
) -> Vec<String> {
    let Some(table_model) = table_model(rows, width, TableRenderMode::Rich) else {
        return Vec::new();
    };

    let area = Rect::new(0, 0, table_model.width as u16, table_model.height);
    let mut buffer = Buffer::empty(area);
    let widths = table_model
        .column_widths
        .iter()
        .map(|width| Constraint::Length(*width as u16))
        .collect::<Vec<_>>();
    let mut rows = table_model
        .body_rows
        .into_iter()
        .map(|row| ratatui_table_row(row, &table_model.column_widths, Style::default()))
        .collect::<Vec<_>>();
    let header = ratatui_table_row(
        table_model.header,
        &table_model.column_widths,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let table = Table::new(rows.drain(..), widths)
        .header(header)
        .column_spacing(2)
        .block(
            Block::bordered()
                .title(Line::from(Span::styled(
                    format!(" {} ", i18n.t(crate::MessageId::MarkdownTableLabel)),
                    Style::default().add_modifier(Modifier::BOLD),
                )))
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    table.render(area, &mut buffer);
    buffer_to_lines(&buffer, area)
}

fn ratatui_table_row(cells: Vec<String>, widths: &[usize], style: Style) -> Row<'static> {
    let wrapped_cells = widths
        .iter()
        .enumerate()
        .map(|(idx, width)| {
            let cell = cells.get(idx).map(String::as_str).unwrap_or("");
            wrap_table_cell(cell, *width)
        })
        .collect::<Vec<_>>();
    let height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let cells = wrapped_cells
        .into_iter()
        .map(|lines| {
            Cell::new(Text::from(
                lines
                    .into_iter()
                    .map(|line| Line::from(Span::raw(line)))
                    .collect::<Vec<_>>(),
            ))
        })
        .collect::<Vec<_>>();
    Row::new(cells).height(height as u16).style(style)
}

pub(super) fn render_plain_markdown_table(rows: &[Vec<String>], width: usize) -> Vec<String> {
    let Some(table_model) = table_model(rows, width, TableRenderMode::Plain) else {
        return Vec::new();
    };
    let visible_rows = std::iter::once(&table_model.header)
        .chain(table_model.body_rows.iter())
        .collect::<Vec<_>>();
    let mut rendered = Vec::new();
    rendered.push(render_table_border(&table_model.column_widths));
    for (idx, row) in visible_rows.iter().enumerate() {
        rendered.extend(render_wrapped_table_row(row, &table_model.column_widths));
        if idx == 0 {
            rendered.push(render_table_border(&table_model.column_widths));
        }
    }
    rendered.push(render_table_border(&table_model.column_widths));
    rendered
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableRenderMode {
    Rich,
    Plain,
}

#[derive(Debug)]
struct MarkdownTableModel {
    header: Vec<String>,
    body_rows: Vec<Vec<String>>,
    column_widths: Vec<usize>,
    width: usize,
    height: u16,
}

fn table_model(
    rows: &[Vec<String>],
    width: usize,
    mode: TableRenderMode,
) -> Option<MarkdownTableModel> {
    if rows.is_empty() {
        return None;
    }

    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if column_count == 0 {
        return None;
    }

    let available = width.max(column_count * 4 + 1);
    let cell_budget = match mode {
        TableRenderMode::Rich => available
            .saturating_sub(2)
            .saturating_sub((column_count - 1) * 2)
            .max(column_count),
        TableRenderMode::Plain => {
            let border_width = column_count * 3 + 1;
            available.saturating_sub(border_width).max(column_count)
        }
    };
    let natural_widths = (0..column_count)
        .map(|col| {
            rows.iter()
                .filter(|row| !is_table_separator_row(row))
                .filter_map(|row| row.get(col))
                .map(|cell| display_width(cell))
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect::<Vec<_>>();
    let column_widths = fit_column_widths(natural_widths, cell_budget);
    let visible_rows = rows
        .iter()
        .filter(|row| !is_table_separator_row(row))
        .cloned()
        .collect::<Vec<_>>();
    let (header, body_rows) = visible_rows.split_first()?;
    let body_rows = body_rows.to_vec();
    let width = match mode {
        TableRenderMode::Rich => {
            2 + column_widths.iter().sum::<usize>() + (column_widths.len() - 1) * 2
        }
        TableRenderMode::Plain => width,
    }
    .min(width.max(column_count * 4 + 1));
    let rich_height = if mode == TableRenderMode::Rich {
        2 + ratatui_row_height(header, &column_widths)
            + body_rows
                .iter()
                .map(|row| ratatui_row_height(row, &column_widths))
                .sum::<u16>()
    } else {
        0
    };

    Some(MarkdownTableModel {
        header: header.clone(),
        body_rows,
        column_widths,
        width,
        height: rich_height,
    })
}

fn ratatui_row_height(row: &[String], widths: &[usize]) -> u16 {
    widths
        .iter()
        .enumerate()
        .map(|(idx, width)| {
            let cell = row.get(idx).map(String::as_str).unwrap_or("");
            wrap_table_cell(cell, *width).len().max(1) as u16
        })
        .max()
        .unwrap_or(1)
}

fn fit_column_widths(mut widths: Vec<usize>, budget: usize) -> Vec<usize> {
    if widths.is_empty() {
        return widths;
    }

    let min_width = 3;
    let min_budget = widths.len() * min_width;
    if budget <= min_budget {
        return vec![min_width; widths.len()];
    }

    for width in &mut widths {
        *width = (*width).max(min_width);
    }
    while widths.iter().sum::<usize>() > budget {
        let Some((idx, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > min_width)
            .max_by_key(|(_, width)| **width)
        else {
            break;
        };
        widths[idx] -= 1;
    }
    widths
}

fn render_table_border(widths: &[usize]) -> String {
    let mut line = String::from("+");
    for width in widths {
        line.push_str(&"-".repeat(*width));
        line.push_str("--+");
    }
    line
}

fn render_wrapped_table_row(row: &[String], widths: &[usize]) -> Vec<String> {
    let wrapped_cells = widths
        .iter()
        .enumerate()
        .map(|(idx, width)| {
            let cell = row.get(idx).map(String::as_str).unwrap_or("");
            wrap_table_cell(cell, *width)
        })
        .collect::<Vec<_>>();
    let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1).max(1);

    (0..row_height)
        .map(|line_idx| {
            let mut line = String::from("|");
            for (cell_idx, width) in widths.iter().enumerate() {
                let cell = wrapped_cells[cell_idx]
                    .get(line_idx)
                    .map(String::as_str)
                    .unwrap_or("");
                line.push(' ');
                line.push_str(cell);
                line.push_str(&" ".repeat(width.saturating_sub(display_width(cell))));
                line.push(' ');
                line.push('|');
            }
            line
        })
        .collect()
}

fn wrap_table_cell(cell: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if cell.trim().is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for ch in cell.chars() {
        let ch_width = char_width(ch);
        if current_width + ch_width > width && !current.is_empty() {
            lines.push(current.trim_end().to_string());
            current = String::new();
            current_width = 0;
            if ch.is_whitespace() {
                continue;
            }
        }
        current.push(ch);
        current_width += ch_width;
    }
    if !current.is_empty() {
        lines.push(current.trim_end().to_string());
    }
    lines
}

fn is_table_separator_row(row: &[String]) -> bool {
    !row.is_empty()
        && row.iter().all(|cell| {
            let trimmed = cell.trim();
            !trimmed.is_empty()
                && trimmed
                    .chars()
                    .all(|ch| matches!(ch, '-' | ':' | ' ' | '\t'))
                && trimmed.chars().any(|ch| ch == '-')
        })
}
