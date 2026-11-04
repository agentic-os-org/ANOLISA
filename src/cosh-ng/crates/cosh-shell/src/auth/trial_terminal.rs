use std::io::{self, Write};

use crate::config::Language;
use crate::runtime::state::InlineState;
use crate::ui::trial_layout::render_trial_waiting;
use crate::ui::wrap::wrap_plain_line;

use super::trial_state::TrialPhase;

fn dimensions() -> (u16, u16) {
    ratatui::crossterm::terminal::size().unwrap_or((80, 24))
}

pub(super) fn clear<W: Write>(state: &mut InlineState, output: &mut W) -> io::Result<()> {
    clear_session(&mut state.auth.ecs_trial, output)
}

pub(super) fn clear_session<W: Write>(
    trial: &mut super::trial::TrialSession,
    output: &mut W,
) -> io::Result<()> {
    let (_, rows) = dimensions();
    let height = std::mem::take(&mut trial.height).min(rows.saturating_sub(1));
    if height > 0 {
        write!(output, "\r\x1b[{height}A")?;
        for index in 0..height {
            write!(output, "\r\x1b[2K")?;
            if index + 1 < height {
                write!(output, "\x1b[1B")?;
            }
        }
        if height > 1 {
            write!(output, "\x1b[{}A", height - 1)?;
        }
        write!(output, "\r\x1b[?25h")?;
    }
    trial.signature.clear();
    Ok(())
}

pub(super) fn redraw<W: Write>(state: &mut InlineState, output: &mut W) -> io::Result<()> {
    let Some(flow) = state.auth.ecs_trial.flow.as_ref() else {
        return Ok(());
    };
    if matches!(flow.phase, TrialPhase::Closed | TrialPhase::Failed) {
        return Ok(());
    }
    let (cols, rows) = dimensions();
    let signature = format!(
        "{cols}:{rows}:{:?}:{}:{}",
        flow.phase, flow.selected, flow.progress.phase
    );
    if state.auth.ecs_trial.signature == signature {
        return Ok(());
    }
    let zh = state.language == Language::ZhCn;
    let texts = if flow.phase == TrialPhase::Settings {
        vec![
            if zh {
                "免配置接入提示"
            } else {
                "Automatic access preferences"
            }
            .into(),
            if zh {
                "关闭后不再自动接入操作系统控制台模型服务。"
            } else {
                "Disabling stops automatic access to OS Console model service."
            }
            .into(),
            if zh {
                "可用 /auth 配置模型或重新启用。"
            } else {
                "Use /auth to configure a model or enable automatic access."
            }
            .into(),
            format!(
                "{} [1] {}",
                if flow.selected == 0 { ">" } else { " " },
                if zh {
                    "本次会话不再提示"
                } else {
                    "Disable for this session"
                }
            ),
            format!(
                "{} [2] {}",
                if flow.selected == 1 { ">" } else { " " },
                if zh {
                    "以后不再提示免配置接入"
                } else {
                    "Disable for future sessions"
                }
            ),
            if flow.can_resume() {
                if zh {
                    "↑↓ 选择 · Enter 确认 · Esc 返回等待"
                } else {
                    "↑↓ select · Enter confirm · Esc resume"
                }
            } else if zh {
                "↑↓ 选择 · Enter 确认 · Esc 返回 Shell"
            } else {
                "↑↓ select · Enter confirm · Esc return to Shell"
            }
            .into(),
        ]
    } else if flow.phase == TrialPhase::Waiting && flow.progress.phase == "waiting_activation" {
        Vec::new()
    } else {
        vec![match flow.phase {
            TrialPhase::Pausing => {
                if zh {
                    "正在停止派发，稍后显示提示设置…"
                } else {
                    "Stopping dispatch before opening settings…"
                }
            }
            TrialPhase::Cancelling => {
                if zh {
                    "正在取消，问题已保留…"
                } else {
                    "Cancelling; your question is retained…"
                }
            }
            _ if flow.progress.phase == "credits_preparing" => {
                if zh {
                    "正在检查可用额度… Esc 取消 · n 不再提示"
                } else {
                    "Checking available quota… Esc cancel · n disable automatic access"
                }
            }
            _ => {
                if zh {
                    "正在连接模型服务… Esc 取消 · n 不再提示"
                } else {
                    "Connecting to model service… Esc cancel · n disable automatic access"
                }
            }
        }
        .into()]
    };
    let budget = rows.saturating_sub(2) as usize;
    let lines = if texts.is_empty() {
        render_trial_waiting(cols, rows, 2, state.language).lines
    } else {
        texts
            .iter()
            .flat_map(|text| wrap_plain_line(text, usize::from(cols.max(2))))
            .collect::<Vec<_>>()
    };
    let lines = if lines.len() <= budget && cols >= 2 {
        lines
    } else if cols >= 3 && budget > 0 {
        vec!["Esc".into()]
    } else {
        Vec::new()
    };
    clear(state, output)?;
    for line in &lines {
        write!(output, "{line}\r\n")?;
    }
    state.auth.ecs_trial.height = lines.len() as u16;
    state.auth.ecs_trial.dimensions = Some((cols, rows));
    state.auth.ecs_trial.signature = signature;
    output.flush()
}
