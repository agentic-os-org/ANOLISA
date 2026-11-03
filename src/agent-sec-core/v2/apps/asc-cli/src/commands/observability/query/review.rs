//! Owner-qualified review with V1 observation rows and integrated security details.

use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde_json::{Value, json};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{Page, field, invalid_response, method, page, safe_text};

const PAGE_SIZE: u32 = 100;
const HEADER_ROWS: usize = 1;

// Signal handlers only set an atomic flag; terminal I/O stays on the UI thread.
struct Signals {
    exit_code: Arc<AtomicUsize>,
    registrations: Vec<signal_hook::SigId>,
}
impl Signals {
    fn new() -> io::Result<Self> {
        let mut guard = Self {
            exit_code: Arc::new(AtomicUsize::new(0)),
            registrations: Vec::new(),
        };
        for (signal, code) in [
            (signal_hook::consts::SIGINT, 130),
            (signal_hook::consts::SIGTERM, 143),
            (signal_hook::consts::SIGHUP, 129),
        ] {
            guard.registrations.push(signal_hook::flag::register_usize(
                signal,
                Arc::clone(&guard.exit_code),
                code,
            )?);
        }
        Ok(guard)
    }

    fn code(&self) -> u8 {
        // Only the three exit codes above can be stored by these handlers.
        u8::try_from(self.exit_code.load(Ordering::Relaxed)).unwrap_or(1)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}

struct Terminal;
impl Terminal {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, Hide, EnableMouseCapture)?;
        Ok(guard)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            Show,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

struct Screen {
    method: &'static str,
    params: Value,
    page: Page,
    offset: i64,
    selected: usize,
    horizontal: usize,
}

impl Screen {
    fn load(
        client: &impl Fn(&str, Value) -> io::Result<Value>,
        method: &'static str,
        params: Value,
        offset: i64,
    ) -> io::Result<Self> {
        let mut result = page(client, method, params.clone(), offset, PAGE_SIZE)?;
        if method == method::OBS_TIMELINE_GET {
            result.items = observation_rows(&result.items)?;
        }
        Ok(Self {
            method,
            params,
            page: result,
            offset,
            selected: 0,
            horizontal: 0,
        })
    }

    fn columns(&self) -> &[&str] {
        match self.method {
            method::OBS_SESSIONS_LIST => &["Last seen", "Session", "Turns", "Events"],
            method::OBS_RUNS_LIST => &["Started", "Run", "Preview", "Events"],
            _ => &["Time", "Hook", "Call / Tool", "Security Result"],
        }
    }

    fn row(&self, row: &Value) -> io::Result<Vec<String>> {
        Ok(match self.method {
            method::OBS_SESSIONS_LIST => vec![
                local_time(row, "last_seen_epoch")?,
                field(row, "session_id")?.into(),
                row["turn_count"].to_string(),
                row["observability_event_count"].to_string(),
            ],
            method::OBS_RUNS_LIST => vec![
                local_time(row, "started_at_epoch")?,
                field(row, "run_id")?.into(),
                row["user_input_preview"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("(no user_input)")
                    .into(),
                row["observability_event_count"].to_string(),
            ],
            _ => vec![
                local_time(row, "timestamp_epoch")?,
                field(row, "hook")?.into(),
                row["tool_call_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| row["call_id"].as_str())
                    .unwrap_or("")
                    .into(),
                security_summary(row),
            ],
        })
    }

    fn start(&self, height: usize) -> usize {
        self.selected.saturating_sub(height.saturating_sub(1))
    }

    fn draw(&self, width: usize, height: usize) -> io::Result<()> {
        let columns = self.columns();
        let rows = self
            .page
            .items
            .iter()
            .map(|row| self.row(row))
            .collect::<io::Result<Vec<_>>>()?;
        let limits: &[usize] = match self.method {
            method::OBS_SESSIONS_LIST => &[32, 40, 20, 20],
            method::OBS_RUNS_LIST => &[32, 36, 60, 20],
            _ => &[32, 22, 18, 50],
        };
        let widths: Vec<usize> = columns
            .iter()
            .enumerate()
            .map(|(index, title)| {
                rows.iter()
                    .map(|row| safe_text(&row[index]).width())
                    .max()
                    .unwrap_or(0)
                    .min(limits[index])
                    .max(title.len())
            })
            .collect();
        let table_row = |values: &[String]| {
            values
                .iter()
                .zip(&widths)
                .map(|(value, width)| {
                    let value = clipped(value, *width);
                    let padding = width.saturating_sub(value.width());
                    format!("{value}{}", " ".repeat(padding))
                })
                .collect::<Vec<_>>()
                .join("  ")
        };
        let mut lines = vec![format!(
            "  {}",
            viewport(
                &table_row(&columns.iter().map(|s| (*s).into()).collect::<Vec<_>>()),
                self.horizontal,
                width.saturating_sub(2)
            )
        )];
        for (index, row) in rows
            .iter()
            .enumerate()
            .skip(self.start(height))
            .take(height)
        {
            lines.push(format!(
                "{} {}",
                if index == self.selected { ">" } else { " " },
                viewport(&table_row(row), self.horizontal, width.saturating_sub(2))
            ));
        }
        if rows.is_empty() {
            lines.push(
                match self.method {
                    method::OBS_SESSIONS_LIST => "No observability records found.",
                    method::OBS_RUNS_LIST => "No runs recorded for this session.",
                    _ => "No events for this run.",
                }
                .into(),
            );
        }
        lines.push(format!(
            "{}-{} / {}",
            if rows.is_empty() { 0 } else { self.offset + 1 },
            self.offset + i64::try_from(rows.len()).map_err(io::Error::other)?,
            self.page.total
        ));
        draw(&lines, width)
    }
}

// Keep the daemon's observation pagination intact. Security rows belong to their
// exact owner-qualified observation, not to a nearby timestamp or business ID.
fn observation_rows(items: &[Value]) -> io::Result<Vec<Value>> {
    let mut rows = Vec::new();
    for item in items.iter().filter(|item| item["kind"] == "observability") {
        if item["id"].as_i64().is_none() {
            return Err(invalid_response());
        }
        let mut row = item.clone();
        let mut security: Vec<Value> = items
            .iter()
            .filter(|candidate| {
                candidate["kind"] == "security"
                    && candidate["observability_event_id"] == item["id"]
                    && candidate["uid"] == item["uid"]
            })
            .cloned()
            .collect();
        // V1 displays code, skill, PII for tool input and prompt, PII for runs.
        security.sort_by_key(|item| match item["event"]["category"].as_str() {
            Some("code_scan" | "prompt_scan") => 0,
            Some("skill_ledger") => 1,
            _ => 2,
        });
        row["security_events"] = json!(security);
        rows.push(row);
    }
    Ok(rows)
}

fn result_value(event: &Value) -> String {
    for object in [&event["details"]["result"], &event["details"]] {
        for key in ["verdict", "status", "valid"] {
            let value = &object[key];
            if !value.is_null() && value.as_str() != Some("") {
                return match (key, value) {
                    ("valid", Value::Bool(true)) => "pass".into(),
                    ("valid", Value::Bool(false)) => "fail".into(),
                    (_, Value::String(value)) => value.clone(),
                    _ => value.to_string(),
                };
            }
        }
    }
    event["result"].as_str().unwrap_or("-").into()
}

fn security_summary(row: &Value) -> String {
    let results: Vec<_> = row["security_events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            format!(
                "{}:{}",
                item["event"]["category"].as_str().unwrap_or(""),
                result_value(&item["event"])
            )
        })
        .collect();
    if results.is_empty() {
        "-".into()
    } else {
        results.join(", ")
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Finite range checked by chrono; subtracting floor yields a nonnegative fraction.
fn timestamp(row: &Value, key: &str) -> io::Result<DateTime<Utc>> {
    let epoch = row[key]
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(invalid_response)?;
    DateTime::from_timestamp(epoch.floor() as i64, ((epoch - epoch.floor()) * 1e9) as u32)
        .ok_or_else(invalid_response)
}

fn local_time(row: &Value, key: &str) -> io::Result<String> {
    Ok(timestamp(row, key)?
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string())
}

fn detail_text(row: &Value) -> io::Result<String> {
    let mut text = format!(
        "Hook: {}\nObserved at: {} ({})\nSession: {}\nRun: {}\nCall ID: {}\nTool call: {}\n\nMetadata:\n{}\n\nMetrics:\n{}",
        field(row, "hook")?,
        local_time(row, "timestamp_epoch")?,
        timestamp(row, "timestamp_epoch")?.to_rfc3339(),
        field(row, "session_id")?,
        field(row, "run_id")?,
        row["call_id"].as_str().unwrap_or(""),
        row["tool_call_id"].as_str().unwrap_or(""),
        serde_json::to_string_pretty(&row["metadata"])?,
        serde_json::to_string_pretty(&row["metrics"])?
    );
    if let Some(events) = row["security_events"].as_array().filter(|v| !v.is_empty()) {
        text.push_str("\n\nSecurity Events:");
        for item in events {
            let event = &item["event"];
            write!(text, "\n{} / {} result={}\nmatch={} rank={} delta={:+.3}s security_at={}\ndetails:\n{}\n",
                field(event, "category")?, field(event, "event_type")?, field(event, "result")?,
                field(&item["match"], "reason")?, item["match"]["rank"], item["match"]["time_delta_seconds"].as_f64().ok_or_else(invalid_response)?,
                local_time(item, "timestamp_epoch")?, serde_json::to_string_pretty(&event["details"])?).map_err(io::Error::other)?;
        }
    }
    Ok(text)
}

fn clipped(value: &str, width: usize) -> String {
    let value = safe_text(value);
    if value.width() <= width {
        value
    } else {
        format!("{}…", viewport(&value, 0, width.saturating_sub(1)))
    }
}

fn viewport(text: &str, offset: usize, width: usize) -> String {
    let mut position = 0;
    let mut result = String::new();
    for ch in text.chars().filter(|c| !c.is_control()) {
        let cells = ch.width().unwrap_or(0);
        if position >= offset && position + cells <= offset + width {
            result.push(ch);
        }
        position += cells;
        if position > offset + width {
            break;
        }
    }
    result
}

fn wrapped(text: &str, width: usize) -> Vec<String> {
    text.lines()
        .flat_map(|line| {
            let mut lines = Vec::new();
            let mut current = String::new();
            let mut cells = 0;
            for ch in line.chars().filter(|c| !c.is_control()) {
                let size = ch.width().unwrap_or(0);
                if cells + size > width.max(2) {
                    lines.push(std::mem::take(&mut current));
                    cells = 0;
                }
                current.push(ch);
                cells += size;
            }
            lines.push(current);
            lines
        })
        .collect()
}

fn dimensions() -> io::Result<(usize, usize)> {
    let (width, height) = terminal::size()?;
    Ok((
        usize::from(width).max(2),
        usize::from(height).saturating_sub(1).max(1),
    ))
}

fn draw(lines: &[String], width: usize) -> io::Result<()> {
    let mut output = io::stdout().lock();
    write!(output, "\x1b[2J\x1b[H")?;
    for line in lines {
        write!(output, "{}\r\n", viewport(line, 0, width.saturating_sub(1)))?;
    }
    output.flush()
}

#[derive(Debug, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Back,
    Next,
    Previous,
    Refresh,
    Quit,
    Click(usize),
    Other,
}
fn key(signals: &Signals) -> io::Result<Key> {
    loop {
        if signals.code() != 0 {
            return Ok(Key::Quit);
        }
        if event::poll(Duration::from_millis(100))? {
            break;
        }
    }
    Ok(match event::read()? {
        Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => Key::Quit,
            KeyCode::Char('q' | 'b') | KeyCode::Esc | KeyCode::Backspace => Key::Back,
            KeyCode::Up | KeyCode::Char('k') => Key::Up,
            KeyCode::Down | KeyCode::Char('j') => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Enter => Key::Enter,
            KeyCode::PageDown | KeyCode::Char('n') => Key::Next,
            KeyCode::PageUp | KeyCode::Char('p') => Key::Previous,
            KeyCode::Char('r') => Key::Refresh,
            _ => Key::Other,
        },
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => Key::Click(usize::from(mouse.row)),
            MouseEventKind::ScrollUp => Key::Up,
            MouseEventKind::ScrollDown => Key::Down,
            _ => Key::Other,
        },
        _ => Key::Other,
    })
}

pub(in crate::commands::observability) fn run(
    client: &impl Fn(&str, Value) -> io::Result<Value>,
) -> io::Result<u8> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("Error: `observability review` requires an interactive terminal.");
        return Ok(2);
    }
    let signals = Signals::new()?;
    let result = run_terminal(client, &signals);
    // run_terminal has dropped the terminal guard before the caller exits.
    match signals.code() {
        0 => result.map(|()| 0),
        code => Ok(code),
    }
}

fn run_terminal(
    client: &impl Fn(&str, Value) -> io::Result<Value>,
    signals: &Signals,
) -> io::Result<()> {
    let _terminal = Terminal::enter()?;
    let mut screen = Screen::load(client, method::OBS_SESSIONS_LIST, json!({}), 0)?;
    let mut parents = Vec::new();
    loop {
        let (width, height) = dimensions()?;
        let height = height.saturating_sub(HEADER_ROWS + 1).max(1);
        screen.draw(width, height)?;
        let mut action = key(signals)?;
        if let Key::Click(row) = action {
            action = Key::Other;
            if (HEADER_ROWS..HEADER_ROWS + height).contains(&row) {
                let selected = screen.start(height) + row - HEADER_ROWS;
                if selected < screen.page.items.len() {
                    screen.selected = selected;
                    action = Key::Enter;
                }
            }
        }
        match action {
            Key::Quit => return Ok(()),
            Key::Up => screen.selected = screen.selected.saturating_sub(1),
            Key::Down => {
                screen.selected =
                    (screen.selected + 1).min(screen.page.items.len().saturating_sub(1));
            }
            Key::Left => screen.horizontal = screen.horizontal.saturating_sub(10),
            Key::Right => screen.horizontal = screen.horizontal.saturating_add(10),
            Key::Back => match parents.pop() {
                Some(parent) => screen = parent,
                None => return Ok(()),
            },
            Key::Refresh => {
                let mut refreshed =
                    Screen::load(client, screen.method, screen.params, screen.offset)?;
                refreshed.selected = screen
                    .selected
                    .min(refreshed.page.items.len().saturating_sub(1));
                refreshed.horizontal = screen.horizontal;
                screen = refreshed;
            }
            Key::Next => {
                if let Some(next) = screen.page.next {
                    screen = Screen::load(client, screen.method, screen.params, next)?;
                }
            }
            Key::Previous => {
                screen = Screen::load(
                    client,
                    screen.method,
                    screen.params,
                    (screen.offset - i64::from(PAGE_SIZE)).max(0),
                )?;
            }
            Key::Enter => {
                if let Some(row) = screen.page.items.get(screen.selected) {
                    if screen.method == method::OBS_TIMELINE_GET {
                        if details(row, signals)? {
                            return Ok(());
                        }
                    } else {
                        let mut params = screen.params.clone();
                        let method = if screen.method == method::OBS_SESSIONS_LIST {
                            params["session_id"] = json!(field(row, "session_id")?);
                            method::OBS_RUNS_LIST
                        } else {
                            params["run_id"] = json!(field(row, "run_id")?);
                            method::OBS_TIMELINE_GET
                        };
                        let child = Screen::load(client, method, params, 0)?;
                        parents.push(screen);
                        screen = child;
                    }
                }
            }
            _ => {}
        }
    }
}

fn details(row: &Value, signals: &Signals) -> io::Result<bool> {
    let text = detail_text(row)?;
    let mut offset: usize = 0;
    loop {
        let (width, height) = dimensions()?;
        let lines = wrapped(&text, width.saturating_sub(1));
        offset = offset.min(lines.len().saturating_sub(1));
        let visible: Vec<_> = lines.iter().skip(offset).take(height).cloned().collect();
        draw(&visible, width)?;
        match key(signals)? {
            Key::Quit => return Ok(true),
            Key::Back => return Ok(false),
            Key::Down => offset = (offset + 1).min(lines.len().saturating_sub(1)),
            Key::Up => offset = offset.saturating_sub(1),
            Key::Next => offset = (offset + height).min(lines.len().saturating_sub(1)),
            Key::Previous => offset = offset.saturating_sub(height),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_integrate_only_their_own_security_results_and_details() {
        let observation = json!({"kind":"observability", "id":7,"uid":1000,"hook":"before_tool_call","session_id":"s","run_id":"r","tool_call_id":"t","timestamp_epoch":100,"metadata":{"sessionId":"s"},"metrics":{"tool_name":"shell"}});
        let security = json!({"kind":"security","observability_event_id":7,"uid":1000,"timestamp_epoch":101,"event":{"category":"code_scan","event_type":"scan","result":"succeeded","details":{"result":{"verdict":"deny"}}},"match":{"reason":"tool_call_id","rank":0,"time_delta_seconds":1.0}});
        let mut foreign = security.clone();
        foreign["uid"] = json!(2000);
        foreign["event"]["details"] = json!({"secret":"foreign"});
        let mut unrelated = security.clone();
        unrelated["observability_event_id"] = json!(8);
        let rows = observation_rows(&[foreign, security, observation, unrelated]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(security_summary(&rows[0]), "code_scan:deny");
        let details = detail_text(&rows[0]).unwrap();
        for expected in [
            "Metadata:",
            "Metrics:",
            "Security Events:",
            "result=succeeded",
            "match=tool_call_id",
            "delta=+1.000s",
            "deny",
        ] {
            assert!(details.contains(expected), "{details}");
        }
        assert!(!details.contains("foreign"));
    }

    #[test]
    fn result_projection_follows_v1_precedence() {
        for (details, expected) in [
            (
                json!({"result":{"verdict":"deny","status":"ok","valid":true}}),
                "deny",
            ),
            (json!({"result":{"verdict":"","status":"warn"}}), "warn"),
            (json!({"result":{"valid":true}}), "pass"),
            (json!({"result":{"valid":false}}), "fail"),
            (json!({"status":"drifted"}), "drifted"),
            (json!({}), "succeeded"),
        ] {
            assert_eq!(
                result_value(&json!({"details":details,"result":"succeeded"})),
                expected
            );
        }
        assert_eq!(security_summary(&json!({"security_events":[]})), "-");
    }

    #[test]
    fn unicode_wrapping_preserves_long_details_and_strips_terminal_controls() {
        let text = "中文abc\n\nend\u{1b}";
        assert_eq!(wrapped(text, 4), ["中文", "abc", "", "end"]);
        assert_eq!(viewport("中文abc", 4, 3), "abc");
    }
}
