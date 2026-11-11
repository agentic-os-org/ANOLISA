use std::ffi::OsString;
use std::io::IsTerminal;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{manage, open_guide_state, parse_action, presentation, Action, Options};
use crate::config::{
    detect_language_from_env, parse_language_setting, resolve_language_setting, LanguageSetting,
};
use crate::diagnostics::health::runtime::{
    health_check_ids, start_login_health_scan_config_entry_until, validate_health_check_ids,
};
use crate::runtime::shell_integration::write_frame;
use crate::ui::RatatuiInlineRenderer;

pub(crate) fn run_cli(args: &[OsString]) -> i32 {
    if matches!(args, [flag] if flag == crate::diagnostics::health::suppression::claim::WORKER_FLAG)
    {
        return crate::diagnostics::health::suppression::claim::run_worker();
    }
    let started = Instant::now();
    let default_deadline = started + Duration::from_millis(500);
    let action = match parse_action(args) {
        Ok(action) => action,
        Err(()) => return usage(default_deadline),
    };
    match action {
        Action::Render(options) => {
            if validate_health_check_ids(&options.disabled).is_err() {
                return usage(default_deadline);
            }
            let Some(deadline) = super::deadline_for(&options, started) else {
                return 0;
            };
            render(options, deadline)
        }
        action => {
            let language = std::env::var("COSH_SHELL_LANG")
                .ok()
                .and_then(|value| parse_language_setting(&value))
                .map(resolve_language_setting)
                .unwrap_or_else(detect_language_from_env);
            let result = crate::config::guide::open_user_store()
                .map_err(|_| ())
                .and_then(|store| manage(action, &store, language));
            match result {
                Ok(text) => i32::from(
                    write_frame(nix::libc::STDOUT_FILENO, text.as_bytes(), default_deadline)
                        .is_err(),
                ),
                Err(()) => {
                    let _ = write_frame(
                        nix::libc::STDERR_FILENO,
                        b"Unable to read or save the login resource summary preference.\n",
                        default_deadline,
                    );
                    1
                }
            }
        }
    }
}

fn usage(deadline: Instant) -> i32 {
    let _ = write_frame(
        nix::libc::STDERR_FILENO,
        b"Usage: cosh health banner [on|off|status] | [--disable CHECK_ID] [--budget-ms 1..500]\n",
        deadline,
    );
    2
}

pub(super) fn render(mut options: Options, deadline: Instant) -> i32 {
    if options.absolute_boottime_us.is_none()
        && !std::io::stdout().is_terminal()
        && !std::io::stderr().is_terminal()
    {
        return 0;
    }
    if deadline <= Instant::now() {
        return 0;
    }
    // Reject unsafe output before consuming persistent invitation state.
    if write_frame(nix::libc::STDOUT_FILENO, &[], deadline).is_err() {
        return 1;
    }
    let (store, runtime) = match open_guide_state() {
        Ok(state) => state,
        Err(_) => return 0,
    };
    let initial = match store.snapshot(runtime.as_ref()) {
        Ok(snapshot) => snapshot,
        Err(_) => return 0,
    };
    if !initial.preferences.banner_enabled && !initial.preferences.guide_enabled {
        return 0;
    }
    if !initial.preferences.banner_enabled {
        options
            .disabled
            .extend(health_check_ids().into_iter().map(str::to_string));
        options.disabled.sort();
        options.disabled.dedup();
    }
    let worker_deadline = deadline
        .checked_sub(Duration::from_millis(20))
        .unwrap_or(deadline);
    if worker_deadline <= Instant::now() {
        return 0;
    }
    let mut handle =
        match start_login_health_scan_config_entry_until(worker_deadline, &options.disabled) {
            Ok(Some(handle)) => handle,
            _ => return 0,
        };
    let wait = worker_deadline.saturating_duration_since(Instant::now());
    let Some(report) = handle.finish_within(wait) else {
        return 0;
    };
    if Instant::now() >= deadline {
        return 0;
    }
    let Some(config) = report.login_config.as_ref() else {
        return 0;
    };
    let language = resolve_language_setting(
        parse_language_setting(&config.language).unwrap_or(LanguageSetting::Auto),
    );
    let snapshot = match store.snapshot(runtime.as_ref()) {
        Ok(snapshot) => snapshot,
        Err(_) => return 0,
    };
    let Some(now_ms) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
    else {
        return 0;
    };
    let renderer = renderer(language);
    let prepared = presentation::prepare(
        &report,
        &renderer,
        (&store, runtime.as_ref(), &snapshot),
        language,
        config.ai_enabled,
        now_ms,
    );
    let text = prepared.final_text(&store, runtime.as_ref());
    if text.is_empty() || Instant::now() >= deadline {
        return 0;
    }
    i32::from(write_frame(nix::libc::STDOUT_FILENO, text.as_bytes(), deadline).is_err())
}

fn renderer(language: crate::Language) -> RatatuiInlineRenderer {
    let columns = std::env::var("COSH_SHELL_WIDTH")
        .or_else(|_| std::env::var("COLUMNS"))
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| *value > 0)
        .or_else(|| {
            ratatui::crossterm::terminal::size()
                .ok()
                .map(|(width, _)| width)
        })
        .unwrap_or(80)
        .clamp(1, 240);
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let plain = columns < 40
        || no_color
        || std::env::var("TERM").is_ok_and(|value| value.eq_ignore_ascii_case("dumb"))
        || std::env::var("COSH_SHELL_RENDER")
            .is_ok_and(|value| matches!(value.as_str(), "plain" | "text"));
    let mut renderer = if plain {
        RatatuiInlineRenderer::plain_with_width(columns)
    } else {
        RatatuiInlineRenderer::with_width(columns)
    }
    .within_columns(columns)
    .with_language(language);
    renderer.styled = !plain && !no_color;
    renderer
}
