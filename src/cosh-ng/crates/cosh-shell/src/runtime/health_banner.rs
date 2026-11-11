use std::ffi::OsString;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    budget: Duration,
    disabled: Vec<String>,
    absolute_boottime_us: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Render(Options),
    Set(bool),
    Status,
}

fn parse_action(args: &[OsString]) -> Result<Action, ()> {
    let Some((head, tail)) = args.split_first() else {
        return Err(());
    };
    if head != "banner" {
        return Err(());
    }
    if let [leaf] = tail {
        match leaf.to_str() {
            Some("on") => return Ok(Action::Set(true)),
            Some("off") => return Ok(Action::Set(false)),
            Some("status") => return Ok(Action::Status),
            _ => {}
        }
    }
    let mut options = Options {
        budget: Duration::from_millis(500),
        disabled: Vec::new(),
        absolute_boottime_us: None,
    };
    let mut budget_seen = false;
    let mut index = 0;
    while index < tail.len() {
        let value = tail.get(index + 1).and_then(|arg| arg.to_str()).ok_or(())?;
        match tail[index].to_str() {
            Some("--budget-ms") if !budget_seen => {
                let budget: u64 = value.parse().map_err(|_| ())?;
                if !(1..=500).contains(&budget) {
                    return Err(());
                }
                options.budget = Duration::from_millis(budget);
                budget_seen = true;
            }
            Some("--disable") if !value.is_empty() => options.disabled.push(value.to_string()),
            Some("--bash-deadline-boottime-us") if options.absolute_boottime_us.is_none() => {
                let deadline: u64 = value.parse().map_err(|_| ())?;
                if deadline == 0 {
                    return Err(());
                }
                options.absolute_boottime_us = Some(deadline);
            }
            _ => return Err(()),
        }
        index += 2;
    }
    Ok(Action::Render(options))
}

mod cli;
mod presentation;
pub(crate) use cli::run_cli;

fn deadline_for(options: &Options, started: Instant) -> Option<Instant> {
    let mut deadline = started + options.budget;
    if let Some(absolute) = options.absolute_boottime_us {
        let before = Instant::now();
        let remaining =
            crate::diagnostics::health::runtime::bash_budget_remaining(absolute, options.budget)?;
        deadline = deadline.min(before + remaining);
    }
    (deadline > Instant::now()).then_some(deadline)
}

fn manage(
    action: Action,
    store: &crate::config::guide::GuideStore,
    language: crate::Language,
) -> Result<String, ()> {
    let enabled = match action {
        Action::Set(enabled) => store.set_banner(enabled).map_err(|_| ())?.banner_enabled,
        Action::Status => {
            store
                .load_preferences()
                .map_err(|_| ())?
                .unwrap_or_default()
                .banner_enabled
        }
        Action::Render(_) => return Err(()),
    };
    let text = match (language, enabled) {
        (crate::Language::ZhCn, true) => "Bash 登录资源摘要：开启\n不会改变 AI 引导或 Cosh 内健康检查。\n",
        (crate::Language::ZhCn, false) => "Bash 登录资源摘要：关闭\n不会改变 AI 引导或 Cosh 内健康检查。\n",
        (_, true) => "Bash login resource summary: on\nAI guidance and Cosh health settings are unchanged.\n",
        (_, false) => "Bash login resource summary: off\nAI guidance and Cosh health settings are unchanged.\n",
    };
    Ok(text.to_string())
}

pub(super) use crate::config::guide::open_optional_user_state as open_guide_state;

#[cfg(test)]
#[path = "health_banner/tests.rs"]
mod tests;
