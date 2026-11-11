use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use super::{Error, GuideStore, RuntimeState, SetGuideError};
use crate::config::{
    detect_language_from_env, load::copilot_shell_cosh_dir, parse_language_setting, private_state,
    resolve_language_setting,
};
use crate::i18n::{I18n, MessageId};

struct UserPaths {
    cosh_dir: Option<PathBuf>,
    runtime_base: Option<PathBuf>,
}

#[derive(Debug)]
struct CommandOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    fn ok(lines: Vec<String>) -> Self {
        Self {
            exit_code: 0,
            stdout: format!("{}\n", lines.join("\n")),
            stderr: String::new(),
        }
    }

    fn error(exit_code: i32, message: String) -> Self {
        Self {
            exit_code,
            stdout: String::new(),
            stderr: format!("{message}\n"),
        }
    }
}

enum Action {
    Set(bool),
    Status,
}

pub(crate) fn run_cli(args: &[OsString]) -> i32 {
    let paths = UserPaths {
        cosh_dir: copilot_shell_cosh_dir(),
        runtime_base: std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    };
    let language = std::env::var("COSH_SHELL_LANG")
        .ok()
        .and_then(|value| parse_language_setting(&value))
        .map(resolve_language_setting)
        .unwrap_or_else(detect_language_from_env);
    let output = run_with_paths(args, &paths, I18n::new(language));
    if std::io::stdout()
        .write_all(output.stdout.as_bytes())
        .is_err()
        || std::io::stderr()
            .write_all(output.stderr.as_bytes())
            .is_err()
    {
        return 1;
    }
    output.exit_code
}

fn parse_action(args: &[OsString]) -> Option<Action> {
    let [leaf] = args else { return None };
    match leaf.to_str()? {
        "on" => Some(Action::Set(true)),
        "off" => Some(Action::Set(false)),
        "status" => Some(Action::Status),
        _ => None,
    }
}

fn run_with_paths(args: &[OsString], paths: &UserPaths, i18n: I18n) -> CommandOutput {
    if args.is_empty() || matches!(args, [leaf] if matches!(leaf.to_str(), Some("--help" | "-h"))) {
        return CommandOutput::ok(vec![i18n.t(MessageId::GuideUsage).to_string()]);
    }
    let Some(action) = parse_action(args) else {
        return CommandOutput::error(2, i18n.t(MessageId::GuideUsage).to_string());
    };
    let Some(cosh_dir) = &paths.cosh_dir else {
        return CommandOutput::error(1, i18n.t(MessageId::ConfigHomeMissingBody).to_string());
    };
    let store = match GuideStore::open(cosh_dir.join("guide")) {
        Ok(store) => store,
        Err(error) => {
            return state_error(
                i18n,
                match action {
                    Action::Status => MessageId::GuideReadFailed,
                    Action::Set(_) => MessageId::GuideSaveFailed,
                },
                &error,
            )
        }
    };
    match action {
        Action::Status => status(&store, paths, i18n),
        Action::Set(enabled) => set(&store, paths, enabled, i18n),
    }
}

fn set(store: &GuideStore, paths: &UserPaths, enabled: bool, i18n: I18n) -> CommandOutput {
    let runtime = paths
        .runtime_base
        .as_deref()
        .map(RuntimeState::open)
        .transpose();
    let trusted_runtime = runtime.as_ref().ok().and_then(Option::as_ref);
    match store.set_guide(trusted_runtime, enabled) {
        Ok(outcome) => {
            let mut lines = vec![if !enabled && outcome.runtime.is_some() {
                i18n.t(MessageId::GuideDisabledSaved).to_string()
            } else {
                i18n.format(
                    MessageId::GuideSaved,
                    &[("state", state_label(i18n, enabled))],
                )
            }];
            if outcome.runtime.is_none() {
                lines.push(unconfirmed(i18n, runtime.as_ref().err()));
            } else if enabled {
                lines.push(i18n.t(MessageId::GuideAvailableConditional).to_string());
            }
            lines.push(i18n.t(MessageId::GuideIndependentHint).to_string());
            CommandOutput::ok(lines)
        }
        Err(SetGuideError::Runtime {
            disabled_published,
            error,
        }) => state_error(
            i18n,
            if disabled_published {
                MessageId::GuideRuntimePublishPartial
            } else {
                MessageId::GuideRuntimePublishFailed
            },
            &error,
        ),
        Err(SetGuideError::Persistence {
            runtime: Some(runtime),
            error,
        }) => CommandOutput::error(
            1,
            i18n.format(
                MessageId::GuideRuntimeSaveFailed,
                &[
                    ("state", state_label(i18n, runtime.enabled)),
                    ("error", &error_reason(i18n, &error)),
                ],
            ),
        ),
        Err(SetGuideError::Persistence {
            runtime: None,
            error,
        }) => state_error(i18n, MessageId::GuideSaveFailed, &error),
    }
}

fn status(store: &GuideStore, paths: &UserPaths, i18n: I18n) -> CommandOutput {
    let preferences = match store.load_preferences() {
        Ok(preferences) => preferences,
        Err(error) => return state_error(i18n, MessageId::GuideReadFailed, &error),
    };
    let enabled = preferences
        .as_ref()
        .is_none_or(|preferences| preferences.guide_enabled);
    let mut lines = vec![if preferences.is_some() {
        i18n.format(
            MessageId::GuidePreferenceSaved,
            &[("state", state_label(i18n, enabled))],
        )
    } else {
        i18n.t(MessageId::GuidePreferenceDefault).to_string()
    }];
    let mut failure = None;
    if enabled {
        let runtime = paths
            .runtime_base
            .as_deref()
            .map(RuntimeState::open)
            .transpose();
        match runtime {
            Ok(Some(runtime)) => match runtime.read() {
                Ok(Some(state)) => lines.push(
                    i18n.t(if state.enabled {
                        MessageId::GuideAvailableConditional
                    } else {
                        MessageId::GuideClosed
                    })
                    .to_string(),
                ),
                Ok(None) => lines.push(unconfirmed(i18n, None)),
                Err(error) => failure = Some(error),
            },
            Ok(None) => lines.push(unconfirmed(i18n, None)),
            Err(error) => failure = Some(error),
        }
    }
    lines.push(i18n.t(MessageId::GuideIndependentHint).to_string());
    let mut output = CommandOutput::ok(lines);
    if let Some(error) = failure {
        output.exit_code = 1;
        output.stderr = format!("{}\n", unconfirmed(i18n, Some(&error)));
    }
    output
}

fn state_label(i18n: I18n, enabled: bool) -> &'static str {
    i18n.t(if enabled {
        MessageId::GuideStateOn
    } else {
        MessageId::GuideStateOff
    })
}

fn unconfirmed(i18n: I18n, error: Option<&Error>) -> String {
    match error {
        Some(error) => i18n.format(
            MessageId::GuideRuntimeUnconfirmed,
            &[("error", &error_reason(i18n, error))],
        ),
        None => i18n.t(MessageId::GuideUnconfirmed).to_string(),
    }
}

fn state_error(i18n: I18n, id: MessageId, error: &Error) -> CommandOutput {
    CommandOutput::error(1, i18n.format(id, &[("error", &error_reason(i18n, error))]))
}

fn error_reason(i18n: I18n, error: &Error) -> String {
    let id = match error {
        Error::State(private_state::Error::Io(std::io::ErrorKind::PermissionDenied)) => {
            MessageId::HealthUnavailablePermissionDenied
        }
        Error::State(private_state::Error::Io(kind)) => {
            return i18n.format(
                MessageId::GuideErrorIo,
                &[("error", &std::io::Error::from(*kind).to_string())],
            )
        }
        Error::State(private_state::Error::UnsafePath) => MessageId::GuideErrorUnsafe,
        Error::State(private_state::Error::LockBusy) => MessageId::GuideErrorBusy,
        Error::State(private_state::Error::TooLarge) | Error::Capacity => {
            MessageId::GuideErrorTooLarge
        }
        Error::Corrupt => MessageId::GuideErrorCorrupt,
        Error::UnsupportedVersion => MessageId::GuideErrorVersion,
        Error::RuntimeUnavailable => MessageId::GuideRuntimeUnavailable,
        Error::InvalidRequest => MessageId::GuideUsage,
    };
    i18n.t(id).to_string()
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
