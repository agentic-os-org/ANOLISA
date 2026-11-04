use crate::config::Language;
use crate::ui::wrap::wrap_plain_line;
use qrcode::{Color, QrCode};
use unicode_width::UnicodeWidthStr;

const CONSOLE_URL: &str = "https://console.aliyun.com/alinux/welcome?source=cosh";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrialLayoutKind {
    Normal,
    Compact,
    Link,
    Minimal,
}

/// Terminal lines to join with CRLF; preserve QR backgrounds and reserve any trailing newline.
pub(crate) struct TrialLayout {
    pub(crate) lines: Vec<String>,
    pub(crate) height: u16,
    pub(crate) layout_kind: TrialLayoutKind,
}

/// Reserve caller-owned rows, including any trailing newline or renderer-added rows.
pub(crate) fn render_trial_waiting(
    cols: u16,
    rows: u16,
    reserved_rows: u16,
    language: Language,
) -> TrialLayout {
    let available = usize::from(rows.saturating_sub(reserved_rows));
    let width = usize::from(cols.min(58));
    let text = WaitingText::new(language);
    if width >= 2 && available > 0 {
        if let Some(qr) = qr_lines() {
            let qr_width = crate::ui::wrap::strip_ansi_escape(&qr[0]).width();
            if width >= qr_width + 4 {
                let lines = normal_lines(&text, &qr, width);
                if lines.len() <= available {
                    return layout(lines, TrialLayoutKind::Normal);
                }
            }
            if width >= qr_width {
                let mut lines = wrapped(&[text.compact_title, text.console_link], width);
                lines.extend(qr);
                lines.extend(wrapped(&[text.compact_status, text.keys], width));
                if lines.len() <= available {
                    return layout(lines, TrialLayoutKind::Compact);
                }
            }
        }
        let lines = wrapped(
            &[
                text.title,
                text.guide,
                CONSOLE_URL,
                text.no_space,
                text.auth,
                text.status,
                text.keys,
            ],
            width,
        );
        if lines.len() <= available {
            return layout(lines, TrialLayoutKind::Link);
        }
    }
    let lines = if width == 0 || available == 0 {
        Vec::new()
    } else {
        vec![match width {
            1 => "!",
            2 => "^C",
            3..=11 => "Esc",
            _ => "Esc / Ctrl+C",
        }
        .to_string()]
    };
    layout(lines, TrialLayoutKind::Minimal)
}

fn layout(lines: Vec<String>, layout_kind: TrialLayoutKind) -> TrialLayout {
    TrialLayout {
        height: lines.len() as u16,
        lines,
        layout_kind,
    }
}

fn wrapped(text: &[&str], width: usize) -> Vec<String> {
    text.iter()
        .flat_map(|line| wrap_plain_line(line, width))
        .collect()
}

fn normal_lines(text: &WaitingText, qr: &[String], width: usize) -> Vec<String> {
    let content_width = [
        text.title,
        text.feature,
        text.scan,
        CONSOLE_URL,
        text.account,
        text.auth,
        text.status,
        text.keys,
    ]
    .iter()
    .map(|line| line.width())
    .max()
    .unwrap_or(0)
    .max(37);
    let width = width.min(content_width + 4);
    let inner = width - 4;
    let title = format!("╭─ {} ", text.title);
    let mut lines = vec![format!("{title}{}╮", "─".repeat(width - title.width() - 1))];
    let mut body = wrapped(
        &[text.feature, "", text.scan, CONSOLE_URL, text.account, ""],
        inner,
    );
    body.extend(qr.iter().cloned());
    body.extend(wrapped(&["", text.auth], inner));
    for line in body {
        let cells = crate::ui::wrap::strip_ansi_escape(&line).width();
        lines.push(format!("│ {line}{} │", " ".repeat(inner - cells)));
    }
    lines.push(format!("├{}┤", "─".repeat(width - 2)));
    for line in wrapped(&[text.status, text.keys], inner) {
        let padding = inner - line.width();
        lines.push(format!("│ {line}{} │", " ".repeat(padding)));
    }
    lines.push(format!("╰{}╯", "─".repeat(width - 2)));
    lines
}

fn qr_lines() -> Option<Vec<String>> {
    let code = QrCode::new(CONSOLE_URL.as_bytes()).ok()?;
    let size = code.width();
    let width = size + 8;
    let dark = |x: usize, y: usize| {
        x >= 4 && y >= 4 && x < size + 4 && y < size + 4 && code[(x - 4, y - 4)] == Color::Dark
    };
    Some(
        (0..width)
            .step_by(2)
            .map(|y| {
                let mut line = String::from("\x1b[38;2;0;0;0;48;2;255;255;255m");
                for x in 0..width {
                    line.push(match (dark(x, y), dark(x, y + 1)) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    });
                }
                line.push_str("\x1b[0m");
                line
            })
            .collect(),
    )
}

struct WaitingText {
    title: &'static str,
    feature: &'static str,
    scan: &'static str,
    account: &'static str,
    guide: &'static str,
    auth: &'static str,
    status: &'static str,
    keys: &'static str,
    compact_title: &'static str,
    console_link: &'static str,
    compact_status: &'static str,
    no_space: &'static str,
}

impl WaitingText {
    fn new(language: Language) -> Self {
        match language {
            Language::ZhCn => Self {
                title: "AI Shell · 免配置接入",
                feature: "让 AI 帮你完成任务、排查问题。",
                scan: "扫码或打开链接，开通操作系统控制台",
                account: "请使用当前 ECS 所属账号。",
                guide: "请使用当前 ECS 所属账号，开通操作系统控制台。",
                auth: "也可用 /auth 配置其他模型",
                status: "◌ 等待开通 · 完成后自动继续回答",
                keys: "Esc 取消 · n 不再提示",
                compact_title: "AI Shell · 免配置接入 · 当前 ECS 所属账号",
                console_link:
                    "操作系统控制台 https://console.aliyun.com/alinux/welcome?source=cosh",
                compact_status: "等待开通，完成后自动继续 · /auth 配置模型",
                no_space: "终端空间不足，放大窗口后显示二维码",
            },
            Language::EnUs => Self {
                title: "AI Shell · Automatic access",
                feature: "Let AI complete tasks and troubleshoot issues.",
                scan: "Scan or open the link to activate OS Console",
                account: "Use the account that owns this ECS instance.",
                guide: "Activate OS Console with the account owning this ECS.",
                auth: "Use /auth to configure another model",
                status: "◌ Waiting for activation · Answers resume automatically",
                keys: "Esc cancel · n disable automatic access",
                compact_title: "AI Shell · Automatic access · ECS owner account",
                console_link: "OS Console https://console.aliyun.com/alinux/welcome?source=cosh",
                compact_status: "Waiting · Auto-resume · /auth: other models",
                no_space: "Not enough terminal space; enlarge to show the QR code",
            },
        }
    }
}
